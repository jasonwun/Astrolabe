//! The live Cluster handle (#147): every request to a Cluster (Discovery,
//! watches, operations, Apply, Precheck, logs, exec and port-forward) goes
//! through [`Cluster`] and fails with a [`ClusterError`].
//!
//! Requests run on this crate's own tokio runtime, because GPUI's executor
//! has no tokio reactor; each answers through a future, receiver or
//! stream handle that any executor can poll.

use std::future::Future;
use std::sync::{Mutex, OnceLock, PoisonError};

use futures::channel::mpsc;
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt as _, StreamExt as _};
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::Client;
use kube::client::UpgradeConnectionError;
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use kube::runtime::watcher;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::apply::{self, ApplyMode, ApplyReport};
use crate::discovery::{self, ServedResources};
use crate::object::Operation;
use crate::pod::{
    self, ExecOutput, ExecRequest, ExecSession, ForwardEvent, ForwardRequest, ForwardTarget,
    LogRequest, LogStream, PortForward,
};
use crate::resource;
use crate::service;
use crate::watch::{self, WatchMessage};

/// The cluster runtime, built on first use.
///
/// # Panics
///
/// When the operating system refuses the runtime's threads or I/O driver.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("astrolabe-cluster")
            .build()
            .expect("failed to create the cluster runtime")
    })
}

/// Start `request` on `runtime` now; the returned future resolves with its
/// answer, or with `panicked` if the task panicked.
fn spawn<T: Send + 'static>(
    runtime: &Handle,
    request: impl Future<Output = Result<T, ClusterError>> + Send + 'static,
    panicked: ClusterError,
) -> impl Future<Output = Result<T, ClusterError>> + Send + 'static {
    let (sender, receiver) = oneshot::channel();
    runtime.spawn(async move {
        let _ = sender.send(request.await);
    });
    receiver.map(|answer| answer.unwrap_or(Err(panicked)))
}

/// What kind of failure a [`ClusterError`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClusterErrorKind {
    /// The client for the Cluster could not be built on first connect: its
    /// kubeconfig is missing or wrong, or a credential plugin's first run
    /// failed. An apiserver that cannot be reached, or a later token
    /// refresh that fails, fails each request as
    /// [`Other`](ClusterErrorKind::Other) instead.
    Connect,
    /// The apiserver refused the request (HTTP 403), which some views show
    /// as an empty state instead of a connection problem.
    Forbidden,
    /// Any other failure.
    Other,
}

/// Why a request to a Cluster failed.
///
/// Its text is the underlying error's, unchanged: the app shows it as is.
/// It is `Clone` because every request waiting on one failed connect
/// receives the same error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ClusterError {
    kind: ClusterErrorKind,
    message: String,
}

impl ClusterError {
    /// An [`Other`](ClusterErrorKind::Other) failure: what a scripted
    /// cluster answers with.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: ClusterErrorKind::Other,
            message: message.into(),
        }
    }

    /// A [`Forbidden`](ClusterErrorKind::Forbidden) failure: what a
    /// scripted cluster answers an RBAC denial with.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            kind: ClusterErrorKind::Forbidden,
            message: message.into(),
        }
    }

    fn connect(message: impl Into<String>) -> Self {
        Self {
            kind: ClusterErrorKind::Connect,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ClusterErrorKind {
        self.kind
    }

    /// Whether the apiserver refused the request (HTTP 403).
    pub fn is_forbidden(&self) -> bool {
        self.kind == ClusterErrorKind::Forbidden
    }
}

/// A 403 from the apiserver is [`Forbidden`](ClusterErrorKind::Forbidden),
/// also when it refuses an exec's or a port-forward's WebSocket upgrade,
/// which kube-rs reports by status alone; anything else is
/// [`Other`](ClusterErrorKind::Other).
impl From<kube::Error> for ClusterError {
    fn from(error: kube::Error) -> Self {
        let kind = match &error {
            kube::Error::Api(status) if status.code == 403 => ClusterErrorKind::Forbidden,
            kube::Error::UpgradeConnection(UpgradeConnectionError::ProtocolSwitch(status))
                if status.as_u16() == 403 =>
            {
                ClusterErrorKind::Forbidden
            }
            _ => ClusterErrorKind::Other,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

/// A watch refused with HTTP 403 is
/// [`Forbidden`](ClusterErrorKind::Forbidden); anything else is
/// [`Other`](ClusterErrorKind::Other).
impl From<watcher::Error> for ClusterError {
    fn from(error: watcher::Error) -> Self {
        let kind = if watch::is_forbidden(&error) {
            ClusterErrorKind::Forbidden
        } else {
            ClusterErrorKind::Other
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

/// The Cluster's client, connected once and shared by every request.
type SharedClient = Shared<BoxFuture<'static, Result<Client, ClusterError>>>;

/// The live handle to one Cluster.
///
/// Connecting is lazy and shared: building the handle never blocks, every
/// request waits on the same connect, and a connect failure fails each
/// request with a [`Connect`](ClusterErrorKind::Connect) error. Dropping
/// the handle stops its watches.
pub struct Cluster {
    /// The kubeconfig context, for logs.
    name: String,
    client: SharedClient,
    /// Where requests run: the cluster runtime, or the caller's for
    /// `from_client`, so a test's paused clock drives the watch retries.
    runtime: Handle,
    watches: Mutex<Vec<AbortHandle>>,
}

impl Cluster {
    /// Start connecting to kubeconfig context `context` and return at
    /// once.
    ///
    /// # Panics
    ///
    /// When the cluster runtime cannot be created on first use.
    pub fn connect(context: &str) -> Self {
        let name = context.to_owned();
        let context = name.clone();
        let runtime = runtime().handle().clone();
        // A credential plugin's first run blocks inside config loading, so
        // the connect runs on the cluster runtime, never the caller's thread.
        let client = spawn(
            &runtime,
            async move {
                crate::client_for_context(&context)
                    .await
                    .map_err(|error| ClusterError::connect(error.to_string()))
            },
            ClusterError::connect("cluster connect task panicked"),
        );
        Self {
            name,
            client: client.boxed().shared(),
            runtime,
            watches: Mutex::default(),
        }
    }

    /// A handle named `name` over an already built `client`, such as a
    /// mock one in tests. Requests run on the caller's tokio runtime when
    /// there is one, so a test's paused clock drives them; otherwise on the
    /// cluster runtime.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, when the cluster runtime cannot be created
    /// on first use.
    pub fn from_client(client: Client, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            client: futures::future::ready(Ok(client)).boxed().shared(),
            runtime: Handle::try_current().unwrap_or_else(|_| runtime().handle().clone()),
            watches: Mutex::default(),
        }
    }

    /// API Discovery, started at once on the cluster runtime. On the
    /// per-group path, a named group's version whose
    /// `/apis/<group>/<version>` answers 404 (a CRD group mid-registration)
    /// is skipped: its kinds are not served in the result.
    ///
    /// # Errors
    ///
    /// The answer fails with:
    /// - [`Connect`](ClusterErrorKind::Connect) when the client could not be
    ///   built, or its connect task panicked;
    /// - [`Forbidden`](ClusterErrorKind::Forbidden) when per-group discovery
    ///   (the fallback after any aggregated failure) gets a 403;
    /// - [`Other`](ClusterErrorKind::Other) when it fails any other way
    ///   (a 404 from `/apis/<group>/<version>` aside), or the request's
    ///   task panicked.
    pub fn discover(
        &self,
    ) -> impl Future<Output = Result<ServedResources, ClusterError>> + Send + 'static {
        let client = self.client.clone();
        spawn(
            &self.runtime,
            async move { Ok(discovery::discover(&client.await?).await?) },
            ClusterError::new("discovery task panicked"),
        )
    }

    /// Run `operation` on its object, started at once on the cluster
    /// runtime. A rollout restart is stamped with the current system time,
    /// read here.
    ///
    /// # Errors
    ///
    /// The answer fails with:
    /// - [`Connect`](ClusterErrorKind::Connect) when the client could not be
    ///   built, or its connect task panicked;
    /// - [`Forbidden`](ClusterErrorKind::Forbidden) when the apiserver
    ///   refuses a request (403);
    /// - [`Other`](ClusterErrorKind::Other) when it fails any other way,
    ///   e.g. the object is gone (404), or the request's task panicked.
    pub fn operate(
        &self,
        operation: &Operation,
    ) -> impl Future<Output = Result<(), ClusterError>> + Send + 'static {
        let client = self.client.clone();
        let operation = operation.clone();
        spawn(
            &self.runtime,
            async move {
                let client = client.await?;
                match &operation {
                    Operation::Scale { object, replicas } => {
                        resource::scale(&client, object, *replicas).await?;
                    }
                    Operation::RolloutRestart { object } => {
                        // The restart stamp goes to the server: system time.
                        #[allow(clippy::disallowed_methods)]
                        let now = k8s_openapi::jiff::Timestamp::now();
                        resource::rollout_restart(&client, object, now).await?;
                    }
                    Operation::SetSuspend { object, suspend } => {
                        resource::suspend(&client, object, *suspend).await?;
                    }
                    Operation::TriggerCronJob { object } => {
                        resource::trigger_cron_job(&client, object).await?;
                    }
                    Operation::Delete { object } => resource::delete(&client, object).await?,
                }
                Ok(())
            },
            ClusterError::new("operation task panicked"),
        )
    }

    /// Apply every document of `snapshot` with server-side apply, started
    /// at once on the cluster runtime. Each document is sent on its own:
    /// one the server rejects does not stop the others, and its outcome in
    /// the report carries the rejection, its causes located on `snapshot`.
    ///
    /// # Errors
    ///
    /// - [`Connect`](ClusterErrorKind::Connect) when the client could not be
    ///   built, or its connect task panicked: nothing is sent;
    /// - [`Other`](ClusterErrorKind::Other) when `snapshot` does not parse
    ///   as YAML documents (nothing is sent), or the request's task
    ///   panicked.
    pub fn apply(
        &self,
        snapshot: &str,
        mode: ApplyMode,
    ) -> impl Future<Output = Result<ApplyReport, ClusterError>> + Send + 'static {
        self.send_snapshot(
            snapshot,
            "apply task panicked",
            move |client, snapshot| async move {
                apply::apply_documents(&client, &snapshot, mode).await
            },
        )
    }

    /// Precheck `snapshot`: a dry-run [`apply`](Self::apply) with strict
    /// field validation, which changes nothing on the Cluster. An unknown
    /// or duplicate field comes back as a cause that
    /// [`is_strict_decoding`](apply::FieldCause::is_strict_decoding).
    ///
    /// # Errors
    ///
    /// As for [`apply`](Self::apply).
    pub fn precheck(
        &self,
        snapshot: &str,
    ) -> impl Future<Output = Result<ApplyReport, ClusterError>> + Send + 'static {
        self.send_snapshot(
            snapshot,
            "precheck task panicked",
            |client, snapshot| async move { apply::precheck_documents(&client, &snapshot).await },
        )
    }

    /// Run `request` on `snapshot` once the client connects, on the cluster
    /// runtime: Apply's and Precheck's shared shape.
    fn send_snapshot<F>(
        &self,
        snapshot: &str,
        panicked: &'static str,
        request: impl FnOnce(Client, String) -> F + Send + 'static,
    ) -> impl Future<Output = Result<ApplyReport, ClusterError>> + Send + 'static
    where
        F: Future<Output = Result<ApplyReport, apply::PayloadError>> + Send + 'static,
    {
        let client = self.client.clone();
        let snapshot = snapshot.to_owned();
        spawn(
            &self.runtime,
            async move {
                let client = client.await?;
                request(client, snapshot)
                    .await
                    .map_err(|error| ClusterError::new(error.to_string()))
            },
            ClusterError::new(panicked),
        )
    }

    /// Follow one container's log from a bounded tail
    /// ([`LOG_TAIL_LINES`](pod::LOG_TAIL_LINES)), started at once on the
    /// cluster runtime. Dropping the stream stops the request.
    ///
    /// # Errors
    ///
    /// The stream's last chunk is an error when:
    /// - the client could not be built
    ///   ([`Connect`](ClusterErrorKind::Connect));
    /// - the apiserver refuses the request, e.g. the container does not
    ///   exist ([`Forbidden`](ClusterErrorKind::Forbidden) for a 403,
    ///   [`Other`](ClusterErrorKind::Other) otherwise);
    /// - reading the stream fails ([`Other`](ClusterErrorKind::Other)).
    ///
    /// The stream just ends when the server closes it, or when the
    /// request's task panicked.
    pub fn logs(&self, request: LogRequest) -> LogStream {
        let (sender, chunks) = mpsc::unbounded();
        let client = self.client.clone();
        let work = self.runtime.spawn(async move {
            let stream = async {
                let pod = &request.pod;
                let namespace = pod.namespace.as_deref().unwrap_or_default();
                let client = client.await?;
                Ok::<_, ClusterError>(
                    pod::logs(&client, namespace, &pod.name, &request.container).await?,
                )
            };
            let stream = match stream.await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = sender.unbounded_send(Err(error));
                    return;
                }
            };
            // A read failure is the stream's last item, so no check for it.
            futures::pin_mut!(stream);
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| ClusterError::new(error.to_string()));
                if sender.unbounded_send(chunk).is_err() {
                    return;
                }
            }
        });
        LogStream::with_work(chunks, work.abort_handle())
    }

    /// Open an interactive shell in one container, started at once on the
    /// cluster runtime. Dropping the session ends it.
    ///
    /// # Errors
    ///
    /// The session's last output is
    /// [`ExecOutput::Exited`] with an error when:
    /// - the client could not be built
    ///   ([`Connect`](ClusterErrorKind::Connect));
    /// - the apiserver refuses the request
    ///   ([`Forbidden`](ClusterErrorKind::Forbidden) for a 403,
    ///   [`Other`](ClusterErrorKind::Other) otherwise);
    /// - the remote streams fail, or the server ends the command with a
    ///   failure that is not an exit code
    ///   ([`Other`](ClusterErrorKind::Other)).
    ///
    /// The session just ends, with no `Exited`, when the request's task
    /// panicked.
    pub fn exec(&self, request: ExecRequest) -> ExecSession {
        let (input, inputs) = mpsc::unbounded();
        let (sender, output) = mpsc::unbounded();
        let client = self.client.clone();
        let work = self.runtime.spawn(async move {
            let exit = async {
                let pod = &request.pod;
                let namespace = pod.namespace.as_deref().unwrap_or_default();
                let client = client.await?;
                let output = |bytes| {
                    let _ = sender.unbounded_send(ExecOutput::Bytes(bytes));
                };
                let command = std::slice::from_ref(&request.shell);
                Ok(pod::exec(
                    &client,
                    namespace,
                    &pod.name,
                    &request.container,
                    command,
                    inputs,
                    output,
                )
                .await?)
            };
            let exit = exit.await;
            let _ = sender.unbounded_send(ExecOutput::Exited(exit));
        });
        ExecSession::with_work(input, output, work.abort_handle())
    }

    /// Forward a localhost port to one Pod or Service port, started at
    /// once on the cluster runtime: on the same port when it is free, on a
    /// free one otherwise. The forward reports
    /// [`Listening`](ForwardEvent::Listening) with the port it took, then
    /// each connection that fails. A Service forward resolves its Pod
    /// afresh for each connection. Dropping the forward closes the
    /// listener and its connections.
    ///
    /// # Errors
    ///
    /// A connection's forward that fails is reported as
    /// [`ConnectionFailed`](ForwardEvent::ConnectionFailed) and the
    /// listener goes on: [`Forbidden`](ClusterErrorKind::Forbidden) when
    /// the apiserver refuses it (403), including the Service lookup;
    /// [`Other`](ClusterErrorKind::Other) for any other failure, a Service
    /// with no running Pod or no such port included.
    ///
    /// The forward's last report is [`Failed`](ForwardEvent::Failed)
    /// when:
    /// - the client could not be built
    ///   ([`Connect`](ClusterErrorKind::Connect));
    /// - no localhost port can be bound, or the listener fails
    ///   ([`Other`](ClusterErrorKind::Other)).
    ///
    /// The forward just ends when the request's task panicked.
    pub fn port_forward(&self, request: ForwardRequest) -> PortForward {
        let (sender, events) = mpsc::unbounded();
        let client = self.client.clone();
        let work = self.runtime.spawn(async move {
            let failed = |error: ClusterError| {
                let _ = sender.unbounded_send(ForwardEvent::Failed(error));
            };
            let bind_failed =
                |error: std::io::Error| ClusterError::new(format!("bind localhost: {error}"));
            let client = match client.await {
                Ok(client) => client,
                Err(error) => return failed(error),
            };
            let port = request.port;
            let listener = match pod::bind_local(port).await {
                Ok(listener) => listener,
                Err(error) => return failed(bind_failed(error)),
            };
            match listener.local_addr() {
                Ok(local) => {
                    let _ = sender.unbounded_send(ForwardEvent::Listening(local.port()));
                }
                Err(error) => return failed(bind_failed(error)),
            }
            let connections = sender.clone();
            let on_error = move |error| {
                let _ = connections.unbounded_send(ForwardEvent::ConnectionFailed(error));
            };
            let result = match &request.target {
                ForwardTarget::Pod(pod) => {
                    let namespace = pod.namespace.as_deref().unwrap_or_default();
                    pod::port_forward(&client, namespace, &pod.name, port, listener, on_error).await
                }
                ForwardTarget::Service(service) => {
                    let namespace = service.namespace.as_deref().unwrap_or_default();
                    service::port_forward(
                        &client,
                        namespace,
                        &service.name,
                        port,
                        listener,
                        on_error,
                    )
                    .await
                }
            };
            if let Err(error) = result {
                failed(ClusterError::new(format!("listener: {error}")));
            }
        });
        PortForward::with_work(events, work.abort_handle())
    }

    /// Watch the Cluster's Namespaces, resident until the handle is
    /// dropped. See [`watch_objects`](Self::watch_objects) for the
    /// messages.
    pub fn watch_namespaces(&self) -> mpsc::Receiver<WatchMessage<Namespace>> {
        self.watch("namespace", watch::namespaces)
    }

    /// Watch the Cluster's `CustomResourceDefinition`s, resident until the
    /// handle is dropped. See [`watch_objects`](Self::watch_objects) for
    /// the messages.
    pub fn watch_crds(&self) -> mpsc::Receiver<WatchMessage<CustomResourceDefinition>> {
        self.watch("CRD", watch::custom_resource_definitions)
    }

    /// Watch `resource`'s objects, resident until the handle is dropped:
    /// in `namespace`, or cluster-wide for `None` (a cluster-scoped kind).
    ///
    /// The receiver gets each LIST as `Init`, `InitApply`… `InitDone`, then
    /// live changes. A failure, or the watch ending, arrives as an `Err`
    /// and is followed 1 s later by a fresh LIST; 30 s later after a
    /// [`Forbidden`](ClusterErrorKind::Forbidden) one. A connect failure
    /// arrives as the only message.
    pub fn watch_objects(
        &self,
        resource: &ApiResource,
        namespace: Option<&str>,
    ) -> mpsc::Receiver<WatchMessage<DynamicObject>> {
        let resource = resource.clone();
        let namespace = namespace.map(str::to_owned);
        self.watch("object", move |client| {
            watch::objects(client, &resource, namespace.as_deref())
        })
    }

    /// Start one relist loop on the handle's runtime.
    fn watch<K, F, S>(&self, label: &'static str, stream: F) -> mpsc::Receiver<WatchMessage<K>>
    where
        K: Send + 'static,
        F: Fn(&Client) -> S + Send + 'static,
        S: futures::Stream<Item = Result<watcher::Event<K>, watcher::Error>> + Send,
    {
        let (receiver, task) = watch::relist(self.name.clone(), label, self.client.clone(), stream);
        let task = self.runtime.spawn(task).abort_handle();
        let mut watches = self.watches.lock().unwrap_or_else(PoisonError::into_inner);
        watches.retain(|watch| !watch.is_finished());
        watches.push(task);
        receiver
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let watches = self
            .watches
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for watch in watches.drain(..) {
            watch.abort();
        }
    }
}
