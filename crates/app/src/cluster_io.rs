//! The session's one I/O seam: every scripted-vs-live decision lives here,
//! so `Session` starts watches, discovers, operates and hands out its client
//! without knowing which kind of cluster it talks to. The live arm wraps
//! `astrolabe_kube::cluster::Cluster`; no other app module names it. An
//! enum, not a trait: this crate knows every adapter and no other crate
//! adds one (docs/adr/0002).

use std::collections::HashMap;

use astrolabe_kube::cluster::{Cluster, ClusterError};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::Operation;
use astrolabe_kube::pod::{
    ExecInput, ExecOutput, ExecRequest, ExecSession, ForwardEvent, ForwardRequest, LogChunk,
    LogRequest, LogStream, PortForward,
};
use astrolabe_kube::watch::{CrdWatchMessage, NamespaceWatchMessage, ObjectWatchMessage};
use futures::future::{self, BoxFuture};
use futures::{FutureExt as _, channel::mpsc};

use crate::session::WatchKey;
pub use astrolabe_kube::cluster::SharedClient;

type DiscoverFn = Box<dyn FnMut() -> Result<ServedResources, ClusterError>>;
type OperateFn = Box<dyn FnMut(&Operation) -> Result<(), ClusterError>>;
type LogsFn = Box<dyn FnMut(&LogRequest) -> Vec<LogChunk>>;
type ForwardFn = Box<dyn FnMut(&ForwardRequest) -> Vec<ForwardEvent>>;
type ExecFn = Box<dyn FnMut(&ExecRequest, mpsc::UnboundedReceiver<ExecInput>) -> Vec<ExecOutput>>;

/// Hand-fed cluster I/O for tests and demos: watch events come from
/// channels, discovery, operations, log chunks, exec output and
/// port-forward reports from closures.
pub struct ScriptedCluster {
    pub(crate) namespaces: Option<mpsc::Receiver<NamespaceWatchMessage>>,
    pub(crate) crds: Option<mpsc::Receiver<CrdWatchMessage>>,
    pub(crate) watches: HashMap<WatchKey, mpsc::Receiver<ObjectWatchMessage>>,
    discover: DiscoverFn,
    operate: OperateFn,
    logs: LogsFn,
    exec: ExecFn,
    port_forward: ForwardFn,
}

impl ScriptedCluster {
    /// Keep the sender behind `namespaces` alive while the session should
    /// stay connected: dropping it ends the watch, which reads as Stale.
    pub fn new(namespaces: mpsc::Receiver<NamespaceWatchMessage>) -> Self {
        Self {
            namespaces: Some(namespaces),
            crds: None,
            watches: HashMap::new(),
            discover: Box::new(|| Ok(ServedResources::default())),
            operate: Box::new(|_| Ok(())),
            logs: Box::new(|_| Vec::new()),
            exec: Box::new(|_, _| Vec::new()),
            port_forward: Box::new(|_| Vec::new()),
        }
    }

    pub fn crds(mut self, crds: mpsc::Receiver<CrdWatchMessage>) -> Self {
        self.crds = Some(crds);
        self
    }

    /// Events for the object watch `key` (a custom resource or built-in
    /// kind), once a view opens it.
    pub fn watch(mut self, key: WatchKey, events: mpsc::Receiver<ObjectWatchMessage>) -> Self {
        self.watches.insert(key, events);
        self
    }

    pub fn discover(
        mut self,
        discover: impl FnMut() -> Result<ServedResources, ClusterError> + 'static,
    ) -> Self {
        self.discover = Box::new(discover);
        self
    }

    pub fn operate(
        mut self,
        operate: impl FnMut(&Operation) -> Result<(), ClusterError> + 'static,
    ) -> Self {
        self.operate = Box::new(operate);
        self
    }

    /// The chunks a log request streams before the stream closes.
    pub fn logs(mut self, logs: impl FnMut(&LogRequest) -> Vec<LogChunk> + 'static) -> Self {
        self.logs = Box::new(logs);
        self
    }

    /// The output an exec request sends; the closure also gets the
    /// session's input.
    pub fn exec(
        mut self,
        exec: impl FnMut(&ExecRequest, mpsc::UnboundedReceiver<ExecInput>) -> Vec<ExecOutput> + 'static,
    ) -> Self {
        self.exec = Box::new(exec);
        self
    }

    /// The reports a port-forward request makes.
    pub fn port_forward(
        mut self,
        port_forward: impl FnMut(&ForwardRequest) -> Vec<ForwardEvent> + 'static,
    ) -> Self {
        self.port_forward = Box::new(port_forward);
        self
    }
}

pub(crate) enum ClusterIo {
    Live(Cluster),
    Scripted(ScriptedCluster),
}

impl ClusterIo {
    /// Connect to kubeconfig context `context`; every request waits on the
    /// shared connect.
    pub(crate) fn connect(context: &str) -> Self {
        Self::Live(Cluster::connect(context))
    }

    /// The Namespace watch; `None` when a scripted cluster scripts none.
    pub(crate) fn watch_namespaces(&mut self) -> Option<mpsc::Receiver<NamespaceWatchMessage>> {
        match self {
            Self::Scripted(cluster) => cluster.namespaces.take(),
            Self::Live(cluster) => Some(cluster.watch_namespaces()),
        }
    }

    /// The CRD watch; `None` when a scripted cluster scripts none.
    pub(crate) fn watch_crds(&mut self) -> Option<mpsc::Receiver<CrdWatchMessage>> {
        match self {
            Self::Scripted(cluster) => cluster.crds.take(),
            Self::Live(cluster) => Some(cluster.watch_crds()),
        }
    }

    /// The object watch `key` on `resource`; `None` when a scripted cluster
    /// scripts none for `key`.
    pub(crate) fn watch_objects(
        &mut self,
        key: &WatchKey,
        resource: &ApiResource,
    ) -> Option<mpsc::Receiver<ObjectWatchMessage>> {
        match self {
            Self::Scripted(cluster) => cluster.watches.remove(key),
            Self::Live(cluster) => Some(cluster.watch_objects(resource, key.namespace.as_deref())),
        }
    }

    /// API discovery; a scripted cluster's answer is ready at once.
    pub(crate) fn discover(&mut self) -> BoxFuture<'static, Result<ServedResources, ClusterError>> {
        match self {
            Self::Scripted(cluster) => future::ready((cluster.discover)()).boxed(),
            Self::Live(cluster) => cluster.discover().boxed(),
        }
    }

    /// Run one operation on one object; a scripted cluster's answer is
    /// ready at once.
    pub(crate) fn operate(
        &mut self,
        operation: Operation,
    ) -> BoxFuture<'static, Result<(), ClusterError>> {
        match self {
            Self::Scripted(cluster) => future::ready((cluster.operate)(&operation)).boxed(),
            Self::Live(cluster) => cluster.operate(&operation).boxed(),
        }
    }

    /// Follow one container's log; a scripted cluster's chunks are all
    /// sent at once.
    pub(crate) fn logs(&mut self, request: LogRequest) -> LogStream {
        match self {
            Self::Scripted(cluster) => {
                let (sender, chunks) = mpsc::unbounded();
                for chunk in (cluster.logs)(&request) {
                    let _ = sender.unbounded_send(chunk);
                }
                LogStream::new(chunks)
            }
            Self::Live(cluster) => cluster.logs(request),
        }
    }

    /// Open an interactive shell in one container; a scripted cluster's
    /// output is all sent at once.
    pub(crate) fn exec(&mut self, request: ExecRequest) -> ExecSession {
        match self {
            Self::Scripted(cluster) => {
                let (input, inputs) = mpsc::unbounded();
                let (sender, output) = mpsc::unbounded();
                for item in (cluster.exec)(&request, inputs) {
                    let _ = sender.unbounded_send(item);
                }
                ExecSession::new(input, output)
            }
            Self::Live(cluster) => cluster.exec(request),
        }
    }

    /// Forward a localhost port to one Pod or Service port: the same port
    /// when free. A scripted cluster's reports are all sent at once.
    pub(crate) fn port_forward(&mut self, request: ForwardRequest) -> PortForward {
        match self {
            Self::Scripted(cluster) => {
                let (sender, events) = mpsc::unbounded();
                for event in (cluster.port_forward)(&request) {
                    let _ = sender.unbounded_send(event);
                }
                PortForward::new(events)
            }
            Self::Live(cluster) => cluster.port_forward(request),
        }
    }

    /// The live shared client; `None` for scripted clusters.
    pub(crate) fn client(&self) -> Option<SharedClient> {
        match self {
            Self::Live(cluster) => Some(cluster.client()),
            Self::Scripted(_) => None,
        }
    }
}
