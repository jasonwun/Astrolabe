//! The session's one I/O seam: every scripted-vs-live decision lives here,
//! so `Session` starts watches, discovers, operates and hands out its client
//! without knowing which kind of cluster it talks to. The live arm wraps
//! `astrolabe_kube::cluster::Cluster`; no other app module names it. An
//! enum, not a trait: this crate knows every adapter and no other crate
//! adds one (docs/adr/0002).

use std::collections::HashMap;
use std::time::Duration;

use astrolabe_kube::cluster::{Cluster, ClusterError};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::runtime::watcher::{self, Event};
use astrolabe_kube::object::Operation;
use astrolabe_kube::pod::ExecInput;
use futures::future::{self, BoxFuture};
use futures::{FutureExt as _, SinkExt, Stream, StreamExt, channel::mpsc};
use tokio::sync::oneshot;

use crate::kube_bridge::{spawn_cluster, spawn_cluster_task};
use crate::session::{
    CrdWatchMessage, ExecRequest, ForwardRequest, ForwardTarget, LogRequest, NamespaceWatchMessage,
    ObjectWatchMessage, WatchError, WatchKey, WatchMessage,
};
pub use astrolabe_kube::cluster::SharedClient;

/// Retry interval for a watch the apiserver refused with 403.
const FORBIDDEN_RETRY: Duration = Duration::from_secs(30);

type DiscoverFn = Box<dyn FnMut() -> Result<ServedResources, ClusterError>>;
type OperateFn = Box<dyn FnMut(&Operation) -> Result<(), String>>;
type LogsFn = Box<dyn FnMut(&LogRequest) -> Vec<LogChunk>>;
type ForwardFn = Box<dyn FnMut(&ForwardRequest) -> Vec<ForwardEvent>>;
type ExecFn = Box<dyn FnMut(&ExecRequest, mpsc::UnboundedReceiver<ExecInput>) -> Vec<ExecOutput>>;

/// A piece of a log stream, or why the stream stopped.
pub type LogChunk = Result<Vec<u8>, String>;

/// A followed container log; dropping it stops the stream.
pub struct LogStream {
    chunks: mpsc::UnboundedReceiver<LogChunk>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl LogStream {
    /// The next chunk; `None` once the server closed the stream.
    pub async fn next(&mut self) -> Option<LogChunk> {
        self.chunks.next().await
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// What an exec session sends back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecOutput {
    /// Remote TTY output.
    Bytes(Vec<u8>),
    /// The command's exit code, or why the session failed; the last item.
    Exited(Result<u8, String>),
}

/// A running `kubectl exec -it`; dropping it ends the session.
pub struct ExecSession {
    input: mpsc::UnboundedSender<ExecInput>,
    output: mpsc::UnboundedReceiver<ExecOutput>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ExecSession {
    /// Where keystrokes and resizes go; closing it sends EOF.
    pub fn input(&self) -> mpsc::UnboundedSender<ExecInput> {
        self.input.clone()
    }

    /// The next output; `None` once the session is gone.
    pub async fn next(&mut self) -> Option<ExecOutput> {
        self.output.next().await
    }
}

impl Drop for ExecSession {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// What a port-forward reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardEvent {
    /// Accepting on this localhost port.
    Listening(u16),
    /// One connection's forward failed; the listener goes on.
    ConnectionFailed(String),
    /// The forward stopped: the client, the bind or the listener failed.
    Failed(String),
}

/// A running port-forward; dropping it closes the listener and its
/// connections.
pub struct PortForward {
    events: mpsc::UnboundedReceiver<ForwardEvent>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl PortForward {
    /// The next report; `None` once the forward is gone.
    pub async fn next(&mut self) -> Option<ForwardEvent> {
        self.events.next().await
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

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
        operate: impl FnMut(&Operation) -> Result<(), String> + 'static,
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
    Live {
        /// The kubeconfig context, for logs.
        context: String,
        cluster: Cluster,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    },
    Scripted(ScriptedCluster),
}

impl ClusterIo {
    /// Connect to kubeconfig context `context`; every request waits on the
    /// shared connect.
    pub(crate) fn connect(context: String) -> Self {
        Self::Live {
            cluster: Cluster::connect(&context),
            context,
            tasks: Vec::new(),
        }
    }

    /// Start one watch: live clusters run `stream` resident on the cluster
    /// runtime; scripted clusters hand over the channel `scripted` picks,
    /// if the test scripted one.
    pub(crate) fn watch<K, S>(
        &mut self,
        label: &'static str,
        scripted: impl FnOnce(&mut ScriptedCluster) -> Option<mpsc::Receiver<WatchMessage<K>>>,
        stream: impl Fn(&Client) -> S + Send + 'static,
    ) -> Option<mpsc::Receiver<WatchMessage<K>>>
    where
        K: Send + 'static,
        S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
    {
        match self {
            Self::Scripted(cluster) => scripted(cluster),
            Self::Live {
                context,
                cluster,
                tasks,
            } => {
                let (events, task) = spawn_watch(context.clone(), label, cluster.client(), stream);
                tasks.push(task);
                Some(events)
            }
        }
    }

    /// API discovery; a scripted cluster's answer is ready at once.
    pub(crate) fn discover(&mut self) -> BoxFuture<'static, Result<ServedResources, ClusterError>> {
        match self {
            Self::Scripted(cluster) => future::ready((cluster.discover)()).boxed(),
            Self::Live { cluster, .. } => cluster.discover().boxed(),
        }
    }

    /// Run one operation on one object.
    pub(crate) fn operate(
        &mut self,
        operation: Operation,
    ) -> oneshot::Receiver<Result<(), String>> {
        match self {
            Self::Scripted(cluster) => answered((cluster.operate)(&operation)),
            Self::Live { cluster, .. } => {
                let client = cluster.client();
                spawn_cluster(async move {
                    let client = client.await.map_err(|error| error.to_string())?;
                    let result = match &operation {
                        Operation::Scale { object, replicas } => {
                            astrolabe_kube::resource::scale(&client, object, *replicas).await
                        }
                        // The restart stamp goes to the server: system time.
                        #[allow(clippy::disallowed_methods)]
                        Operation::RolloutRestart { object } => {
                            astrolabe_kube::resource::rollout_restart(
                                &client,
                                object,
                                astrolabe_kube::k8s_openapi::jiff::Timestamp::now(),
                            )
                            .await
                        }
                        Operation::SetSuspend { object, suspend } => {
                            astrolabe_kube::resource::suspend(&client, object, *suspend).await
                        }
                        Operation::TriggerCronJob { object } => {
                            astrolabe_kube::resource::trigger_cron_job(&client, object).await
                        }
                        Operation::Delete { object } => {
                            astrolabe_kube::resource::delete(&client, object).await
                        }
                    };
                    result.map_err(|error| error.to_string())
                })
            }
        }
    }

    /// Follow one container's log.
    pub(crate) fn logs(&mut self, request: LogRequest) -> LogStream {
        let (sender, chunks) = mpsc::unbounded();
        match self {
            Self::Scripted(cluster) => {
                for chunk in (cluster.logs)(&request) {
                    let _ = sender.unbounded_send(chunk);
                }
                LogStream { chunks, task: None }
            }
            Self::Live { cluster, .. } => {
                let client = cluster.client();
                let task = spawn_cluster_task(async move {
                    let failed = |error: String| {
                        let _ = sender.unbounded_send(Err(error));
                    };
                    let client = match client.await {
                        Ok(client) => client,
                        Err(error) => return failed(error.to_string()),
                    };
                    let pod = &request.pod;
                    let stream = astrolabe_kube::pod::logs(
                        &client,
                        pod.namespace.as_deref().unwrap_or_default(),
                        &pod.name,
                        &request.container,
                    )
                    .await;
                    let stream = match stream {
                        Ok(stream) => stream,
                        Err(error) => return failed(error.to_string()),
                    };
                    futures::pin_mut!(stream);
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.map_err(|error| error.to_string());
                        let stop = chunk.is_err();
                        if sender.unbounded_send(chunk).is_err() || stop {
                            return;
                        }
                    }
                });
                LogStream {
                    chunks,
                    task: Some(task),
                }
            }
        }
    }

    /// Open an interactive shell in one container.
    pub(crate) fn exec(&mut self, request: ExecRequest) -> ExecSession {
        let (input, inputs) = mpsc::unbounded();
        let (sender, output) = mpsc::unbounded();
        match self {
            Self::Scripted(cluster) => {
                for item in (cluster.exec)(&request, inputs) {
                    let _ = sender.unbounded_send(item);
                }
                ExecSession {
                    input,
                    output,
                    task: None,
                }
            }
            Self::Live { cluster, .. } => {
                let client = cluster.client();
                let task = spawn_cluster_task(async move {
                    let exit = match client.await {
                        Ok(client) => {
                            let pod = &request.pod;
                            astrolabe_kube::pod::exec(
                                &client,
                                pod.namespace.as_deref().unwrap_or_default(),
                                &pod.name,
                                &request.container,
                                std::slice::from_ref(&request.shell),
                                inputs,
                                |bytes| {
                                    let _ = sender.unbounded_send(ExecOutput::Bytes(bytes));
                                },
                            )
                            .await
                            .map_err(|error| error.to_string())
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    let _ = sender.unbounded_send(ExecOutput::Exited(exit));
                });
                ExecSession {
                    input,
                    output,
                    task: Some(task),
                }
            }
        }
    }

    /// Forward a localhost port to one Pod or Service port: the same port
    /// when free.
    pub(crate) fn port_forward(&mut self, request: ForwardRequest) -> PortForward {
        let (sender, events) = mpsc::unbounded();
        match self {
            Self::Scripted(cluster) => {
                for event in (cluster.port_forward)(&request) {
                    let _ = sender.unbounded_send(event);
                }
                PortForward { events, task: None }
            }
            Self::Live { cluster, .. } => {
                let client = cluster.client();
                let task = spawn_cluster_task(async move {
                    let failed = |error: String| {
                        let _ = sender.unbounded_send(ForwardEvent::Failed(error));
                    };
                    let client = match client.await {
                        Ok(client) => client,
                        Err(error) => return failed(error.to_string()),
                    };
                    let port = request.port;
                    let listener = match astrolabe_kube::pod::bind_local(port).await {
                        Ok(listener) => listener,
                        Err(error) => return failed(format!("bind localhost: {error}")),
                    };
                    match listener.local_addr() {
                        Ok(local) => {
                            let _ = sender.unbounded_send(ForwardEvent::Listening(local.port()));
                        }
                        Err(error) => return failed(format!("bind localhost: {error}")),
                    }
                    let connections = sender.clone();
                    let on_error = move |error| {
                        let _ = connections.unbounded_send(ForwardEvent::ConnectionFailed(error));
                    };
                    let result = match &request.target {
                        ForwardTarget::Pod(pod) => {
                            astrolabe_kube::pod::port_forward(
                                &client,
                                pod.namespace.as_deref().unwrap_or_default(),
                                &pod.name,
                                port,
                                listener,
                                on_error,
                            )
                            .await
                        }
                        ForwardTarget::Service(service) => {
                            astrolabe_kube::service::port_forward(
                                &client,
                                service.namespace.as_deref().unwrap_or_default(),
                                &service.name,
                                port,
                                listener,
                                on_error,
                            )
                            .await
                        }
                    };
                    if let Err(error) = result {
                        failed(format!("listener: {error}"));
                    }
                });
                PortForward {
                    events,
                    task: Some(task),
                }
            }
        }
    }

    /// The live shared client; `None` for scripted clusters.
    pub(crate) fn client(&self) -> Option<SharedClient> {
        match self {
            Self::Live { cluster, .. } => Some(cluster.client()),
            Self::Scripted(_) => None,
        }
    }
}

impl Drop for ClusterIo {
    fn drop(&mut self) {
        if let Self::Live { tasks, .. } = self {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

fn answered<T>(value: T) -> oneshot::Receiver<T> {
    let (tx, rx) = oneshot::channel();
    let _ = tx.send(value);
    rx
}

/// Resident watcher on the cluster runtime. Any stream error restarts the
/// watcher from a fresh LIST (#28): a silent watch resumption would emit no
/// `InitDone`, leaving the UI stale.
fn spawn_watch<K, S>(
    context: String,
    label: &'static str,
    client: SharedClient,
    stream: impl Fn(&Client) -> S + Send + 'static,
) -> (mpsc::Receiver<WatchMessage<K>>, tokio::task::JoinHandle<()>)
where
    K: Send + 'static,
    S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
{
    let (mut sender, receiver) = mpsc::channel(256);
    let task = spawn_cluster_task(async move {
        let client = match client.await {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(cluster = %context, error = %error, "{label} watch client setup failed");
                let _ = sender.send(Err(WatchError::new(error.to_string()))).await;
                return;
            }
        };
        loop {
            let events = stream(&client);
            futures::pin_mut!(events);
            let mut report_end = true;
            let mut forbidden = false;
            while let Some(event) = events.next().await {
                let event = event.map_err(|error| {
                    tracing::warn!(cluster = %context, error = %error, "{label} watch failed");
                    WatchError::from_watcher(&error)
                });
                let failed = event.is_err();
                forbidden = matches!(&event, Err(error) if error.forbidden);
                if sender.send(event).await.is_err() {
                    return;
                }
                if failed {
                    report_end = false;
                    break;
                }
            }
            if report_end
                && sender
                    .send(Err(WatchError::new(format!("{label} watch ended"))))
                    .await
                    .is_err()
            {
                return;
            }
            tracing::info!(cluster = %context, "restarting {label} watch");
            // RBAC denials do not clear by retrying fast; back off further.
            let delay = if forbidden {
                FORBIDDEN_RETRY
            } else {
                Duration::from_secs(1)
            };
            tokio::time::sleep(delay).await;
        }
    });
    (receiver, task)
}
