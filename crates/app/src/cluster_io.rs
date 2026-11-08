//! The session's one I/O seam: every scripted-vs-live decision lives here,
//! so `Session` starts watches, discovers, operates and applies without
//! knowing which kind of cluster it talks to. The live arm wraps
//! `astrolabe_kube::cluster::Cluster`; no other app module names it. An
//! enum, not a trait: this crate knows every adapter and no other crate
//! adds one (docs/adr/0002).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use astrolabe_kube::apply::{ApplyMode, ApplyReport};
use astrolabe_kube::cluster::{Cluster, ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::Operation;
use astrolabe_kube::pod::{
    ExecInput, ExecOutput, ExecRequest, ExecSession, ForwardEvent, ForwardRequest, LogChunk,
    LogRequest, LogStream, PortForward,
};
use astrolabe_kube::watch::{
    CrdWatchMessage, NamespaceWatchMessage, ObjectWatchMessage, Watch, WatchMessage,
};
use futures::future::{self, BoxFuture};
use futures::{FutureExt as _, channel::mpsc};

use astrolabe_model::session::WatchKey;

use crate::session::DEFAULT_NAMESPACE;

type DiscoverFn = Box<dyn FnMut() -> Result<ServedResources, ClusterError>>;
type OperateFn = Box<dyn FnMut(&Operation) -> Result<(), ClusterError>>;
type ApplyFn = Box<dyn FnMut(&str, ApplyMode) -> Result<ApplyReport, ClusterError>>;
type PrecheckFn = Box<dyn FnMut(&str) -> Result<ApplyReport, ClusterError>>;
type LogsFn = Box<dyn FnMut(&LogRequest) -> Vec<LogChunk>>;
type ForwardFn = Box<dyn FnMut(&ForwardRequest) -> Vec<ForwardEvent>>;
type ExecFn = Box<dyn FnMut(&ExecRequest, mpsc::UnboundedReceiver<ExecInput>) -> Vec<ExecOutput>>;

/// Hand-fed cluster I/O for tests and demos: watch events come from
/// channels, discovery, operations, Apply and Precheck reports, log chunks,
/// exec output and port-forward reports from closures.
pub struct ScriptedCluster {
    pub(crate) namespaces: Option<mpsc::Receiver<NamespaceWatchMessage>>,
    pub(crate) crds: Option<mpsc::Receiver<CrdWatchMessage>>,
    pub(crate) watches: HashMap<WatchKey, mpsc::Receiver<ObjectWatchMessage>>,
    discover: DiscoverFn,
    operate: OperateFn,
    apply: ApplyFn,
    precheck: PrecheckFn,
    logs: LogsFn,
    exec: ExecFn,
    port_forward: ForwardFn,
    /// Set while the scripted connect fails; a reconnect clears it.
    connect_failure: Rc<RefCell<Option<ClusterError>>>,
    /// Whether a failed connect leaves the watches open, so that they
    /// deliver their events once it is cleared, as a live Cluster's do.
    recovers: bool,
    /// Where the senders of the Logs streams and Exec sessions go, when
    /// they should stay open; see [`OpenStreams`].
    open_streams: Option<OpenStreams>,
}

/// The senders behind the Logs streams and Exec sessions of a
/// [`ScriptedCluster`] that keeps them open: dropping a sender is what ends
/// a stream, so the test holds them and calls [`OpenStreams::close`].
#[derive(Clone, Default)]
pub struct OpenStreams(Rc<RefCell<HeldSenders>>);

#[derive(Default)]
struct HeldSenders {
    logs: Vec<mpsc::UnboundedSender<LogChunk>>,
    execs: Vec<mpsc::UnboundedSender<ExecOutput>>,
}

impl OpenStreams {
    fn hold_log(&self, sender: mpsc::UnboundedSender<LogChunk>) {
        self.0.borrow_mut().logs.push(sender);
    }

    fn hold_exec(&self, sender: mpsc::UnboundedSender<ExecOutput>) {
        self.0.borrow_mut().execs.push(sender);
    }

    /// End every stream and session held open so far.
    pub fn close(&self) {
        let mut held = self.0.borrow_mut();
        held.logs.clear();
        held.execs.clear();
    }
}

/// Clears the connect failure of a [`ScriptedCluster`] that recovers, as a
/// live Cluster's does once kubelogin is ready.
#[derive(Clone)]
pub struct ConnectRecovery(Rc<RefCell<Option<ClusterError>>>);

impl ConnectRecovery {
    /// The connect works from now on.
    pub fn recover(&self) {
        self.0.borrow_mut().take();
    }
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
            apply: Box::new(|_, _| Ok(clean_report())),
            precheck: Box::new(|_| Ok(clean_report())),
            logs: Box::new(|_| Vec::new()),
            exec: Box::new(|_, _| Vec::new()),
            port_forward: Box::new(|_| Vec::new()),
            connect_failure: Rc::default(),
            recovers: false,
            open_streams: None,
        }
    }

    /// The watch of a failed connect: the error, then the end. `None` when
    /// the connect does not fail.
    fn failed_watch<K>(&self) -> Option<Watch<K>> {
        let error = self.connect_failure.borrow().clone()?;
        let (mut sender, receiver) = mpsc::channel(1);
        let _ = sender.try_send(Err(error));
        Some(Watch::new(receiver))
    }

    /// The watch reading `events`. While a recovering connect fails, it
    /// reports the error first and stays open.
    fn scripted_watch<K>(
        &self,
        events: Option<mpsc::Receiver<WatchMessage<K>>>,
    ) -> Option<Watch<K>> {
        let events = events?;
        let failure = self.connect_failure.borrow().clone();
        Some(Watch::with_prelude(failure.map(Err), events))
    }

    /// Whether the connect fails and its watches end on it.
    fn watches_fail(&self) -> bool {
        !self.recovers && self.connect_failure.borrow().is_some()
    }

    /// The first connect fails with `error`, as a live one does when
    /// kubelogin is not ready: each watch delivers it as its only message
    /// and ends, and Discovery, operations, Apply and Precheck answer
    /// with it. The scripted watches stay unused until a reconnect, which
    /// connects.
    pub fn connect_fails(mut self, error: ClusterError) -> Self {
        *self.connect_failure.borrow_mut() = Some(error);
        self.recovers = false;
        self
    }

    /// The first connect fails with `error` and recovers by itself, as a
    /// live one does once kubelogin is ready: each watch delivers the
    /// error, stays open, and delivers its scripted events; Discovery
    /// answers with the error. Nothing calls `reconnect`: the returned
    /// handle ends the failure.
    pub fn connect_fails_until_recovered(mut self, error: ClusterError) -> (Self, ConnectRecovery) {
        *self.connect_failure.borrow_mut() = Some(error);
        self.recovers = true;
        let recovery = ConnectRecovery(self.connect_failure.clone());
        (self, recovery)
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

    /// The report an Apply of a snapshot answers with; by default every
    /// document applies cleanly.
    pub fn apply(
        mut self,
        apply: impl FnMut(&str, ApplyMode) -> Result<ApplyReport, ClusterError> + 'static,
    ) -> Self {
        self.apply = Box::new(apply);
        self
    }

    /// The report a Precheck of a snapshot answers with; by default it
    /// finds nothing.
    pub fn precheck(
        mut self,
        precheck: impl FnMut(&str) -> Result<ApplyReport, ClusterError> + 'static,
    ) -> Self {
        self.precheck = Box::new(precheck);
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

    /// Keep the Logs streams and Exec sessions open after their scripted
    /// chunks are sent, until `open_streams` is closed.
    pub fn keep_streams_open(mut self, open_streams: &OpenStreams) -> Self {
        self.open_streams = Some(open_streams.clone());
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

/// A report with no document in it and nothing wrong.
fn clean_report() -> ApplyReport {
    ApplyReport::new(Vec::new())
}

pub(crate) enum ClusterIo {
    Live(Cluster),
    Scripted(ScriptedCluster),
}

impl ClusterIo {
    /// Connect to `cluster`; every request waits on the shared connect.
    pub(crate) fn connect(cluster: &ClusterId) -> Self {
        Self::Live(Cluster::connect(cluster))
    }

    /// The namespace a session starts in: the context's, or `"default"`
    /// for a scripted cluster.
    pub(crate) fn default_namespace(&self) -> String {
        match self {
            Self::Live(cluster) => cluster.default_namespace().to_owned(),
            Self::Scripted(_) => DEFAULT_NAMESPACE.to_owned(),
        }
    }

    /// Connect again if the last connect failed: a live Cluster starts a
    /// new attempt, a scripted one stops failing.
    pub(crate) fn reconnect(&mut self) {
        match self {
            Self::Scripted(cluster) => {
                cluster.connect_failure.borrow_mut().take();
            }
            Self::Live(cluster) => cluster.reconnect(),
        }
    }

    /// The Namespace watch; `None` when a scripted cluster scripts none and
    /// its connect does not fail.
    pub(crate) fn watch_namespaces(&mut self) -> Option<Watch<Namespace>> {
        match self {
            Self::Scripted(cluster) if cluster.watches_fail() => cluster.failed_watch(),
            Self::Scripted(cluster) => {
                let events = cluster.namespaces.take();
                cluster.scripted_watch(events)
            }
            Self::Live(cluster) => Some(cluster.watch_namespaces()),
        }
    }

    /// The CRD watch; `None` when a scripted cluster scripts none and its
    /// connect does not fail.
    pub(crate) fn watch_crds(&mut self) -> Option<Watch<CustomResourceDefinition>> {
        match self {
            Self::Scripted(cluster) if cluster.watches_fail() => cluster.failed_watch(),
            Self::Scripted(cluster) => {
                let events = cluster.crds.take();
                cluster.scripted_watch(events)
            }
            Self::Live(cluster) => Some(cluster.watch_crds()),
        }
    }

    /// The object watch `key` on `resource`; `None` when a scripted cluster
    /// scripts none for `key` and its connect does not fail.
    pub(crate) fn watch_objects(
        &mut self,
        key: &WatchKey,
        resource: &ApiResource,
    ) -> Option<Watch<DynamicObject>> {
        match self {
            Self::Scripted(cluster) if cluster.watches_fail() => cluster.failed_watch(),
            Self::Scripted(cluster) => {
                let events = cluster.watches.remove(key);
                cluster.scripted_watch(events)
            }
            Self::Live(cluster) => Some(cluster.watch_objects(resource, key.namespace.as_deref())),
        }
    }

    /// API discovery; a scripted cluster's answer is ready at once.
    pub(crate) fn discover(&mut self) -> BoxFuture<'static, Result<ServedResources, ClusterError>> {
        match self {
            Self::Scripted(cluster) => {
                future::ready(match cluster.connect_failure.borrow().clone() {
                    Some(error) => Err(error),
                    None => (cluster.discover)(),
                })
                .boxed()
            }
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

    /// Apply every document of `snapshot`; a scripted cluster's answer is
    /// ready at once.
    pub(crate) fn apply(
        &mut self,
        snapshot: &str,
        mode: ApplyMode,
        served: ServedResources,
    ) -> BoxFuture<'static, Result<ApplyReport, ClusterError>> {
        match self {
            Self::Scripted(cluster) => future::ready((cluster.apply)(snapshot, mode)).boxed(),
            Self::Live(cluster) => cluster.apply(snapshot, mode, served).boxed(),
        }
    }

    /// Dry-run `snapshot` with strict field validation; a scripted
    /// cluster's answer is ready at once.
    pub(crate) fn precheck(
        &mut self,
        snapshot: &str,
        served: ServedResources,
    ) -> BoxFuture<'static, Result<ApplyReport, ClusterError>> {
        match self {
            Self::Scripted(cluster) => future::ready((cluster.precheck)(snapshot)).boxed(),
            Self::Live(cluster) => cluster.precheck(snapshot, served).boxed(),
        }
    }

    /// Follow one container's log; a scripted cluster's chunks are all
    /// sent at once, and the stream closes unless it is kept open.
    pub(crate) fn logs(&mut self, request: LogRequest) -> LogStream {
        match self {
            Self::Scripted(cluster) => {
                let (sender, chunks) = mpsc::unbounded();
                for chunk in (cluster.logs)(&request) {
                    let _ = sender.unbounded_send(chunk);
                }
                if let Some(open) = &cluster.open_streams {
                    open.hold_log(sender);
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
                if let Some(open) = &cluster.open_streams {
                    open.hold_exec(sender);
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
}
