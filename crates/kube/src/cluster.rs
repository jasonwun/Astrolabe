//! The live Cluster handle (#147): every request to a Cluster (Discovery,
//! watches, operations, Apply, Precheck, logs, exec and port-forward) goes
//! through [`Cluster`] and fails with a [`ClusterError`].
//!
//! Requests run on this crate's own tokio runtime, because GPUI's executor
//! has no tokio reactor; each answers through a future, receiver or
//! stream handle that any executor can poll.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

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

use crate::apply::{self, ApplyMode, ApplyReport};
use crate::discovery::{self, ServedResources};
use crate::object::{ObjectKey, Operation};
use crate::pod::{
    self, ExecOutput, ExecRequest, ExecSession, ForwardEvent, ForwardRequest, ForwardTarget,
    LogRequest, LogStream, PortForward,
};
use crate::resource;
use crate::service;
use crate::watch::{self, Watch};

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
    /// failed. A later token refresh that fails fails each request as
    /// [`Other`](ClusterErrorKind::Other) instead.
    Connect,
    /// The apiserver could not be reached after a successful connect: the
    /// transport failed (connection refused, a timeout), so a
    /// view can say "cluster unreachable" instead of a generic error.
    Unreachable,
    /// The apiserver refused the request (HTTP 403), which some views show
    /// as an empty state instead of a connection problem.
    Forbidden,
    /// Any other failure.
    Other,
}

/// Why a request to a Cluster failed.
///
/// Its text is the underlying error's, unchanged: the app shows it as is.
/// It keeps that error as its [`source`](std::error::Error::source) when
/// there is one. It is `Clone` because every request waiting on one failed
/// connect receives the same error; two errors are equal when their kind
/// and text are.
#[derive(Clone, Debug)]
pub struct ClusterError {
    kind: ClusterErrorKind,
    message: String,
    source: Option<Arc<dyn std::error::Error + Send + Sync + 'static>>,
}

impl ClusterError {
    /// An [`Other`](ClusterErrorKind::Other) failure: what a scripted
    /// cluster answers with.
    pub fn new(message: impl Into<String>) -> Self {
        Self::message_only(ClusterErrorKind::Other, message)
    }

    /// A [`Forbidden`](ClusterErrorKind::Forbidden) failure: what a
    /// scripted cluster answers an RBAC denial with.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::message_only(ClusterErrorKind::Forbidden, message)
    }

    /// A [`Connect`](ClusterErrorKind::Connect) failure: what a scripted
    /// cluster answers a failed first connect with.
    pub fn connect(message: impl Into<String>) -> Self {
        Self::message_only(ClusterErrorKind::Connect, message)
    }

    /// An [`Unreachable`](ClusterErrorKind::Unreachable) failure: what a
    /// scripted cluster answers a transport failure with.
    pub fn unreachable(message: impl Into<String>) -> Self {
        Self::message_only(ClusterErrorKind::Unreachable, message)
    }

    fn message_only(kind: ClusterErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// A failure of `kind` caused by `source`, whose text is the error's.
    pub(crate) fn caused_by(
        kind: ClusterErrorKind,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message: source.to_string(),
            source: Some(Arc::new(source)),
        }
    }

    /// An [`Other`](ClusterErrorKind::Other) failure caused by `source`.
    pub(crate) fn other(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::caused_by(ClusterErrorKind::Other, source)
    }

    /// An [`Other`](ClusterErrorKind::Other) failure caused by `source`,
    /// whose text is `context: source`.
    pub(crate) fn other_in(
        context: &str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind: ClusterErrorKind::Other,
            message: format!("{context}: {source}"),
            source: Some(Arc::new(source)),
        }
    }

    /// A [`Connect`](ClusterErrorKind::Connect) failure caused by `source`.
    pub(crate) fn connect_failed(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::caused_by(ClusterErrorKind::Connect, source)
    }

    pub fn kind(&self) -> ClusterErrorKind {
        self.kind
    }

    /// Whether the apiserver refused the request (HTTP 403).
    pub fn is_forbidden(&self) -> bool {
        self.kind == ClusterErrorKind::Forbidden
    }
}

impl std::fmt::Display for ClusterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClusterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

impl PartialEq for ClusterError {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.message == other.message
    }
}

impl Eq for ClusterError {}

/// The kind a failed request to the apiserver is: a 403 is
/// [`Forbidden`](ClusterErrorKind::Forbidden), also when it refuses an
/// exec's or a port-forward's WebSocket upgrade, which kube-rs reports by
/// status alone; a transport failure is
/// [`Unreachable`](ClusterErrorKind::Unreachable); anything else is
/// [`Other`](ClusterErrorKind::Other).
pub(crate) fn kind_of(error: &kube::Error) -> ClusterErrorKind {
    match error {
        kube::Error::Api(status) if status.code == 403 => ClusterErrorKind::Forbidden,
        kube::Error::UpgradeConnection(UpgradeConnectionError::ProtocolSwitch(status))
            if status.as_u16() == 403 =>
        {
            ClusterErrorKind::Forbidden
        }
        // The service stack's failure: the transport, unless its auth
        // layer failed to refresh a token.
        kube::Error::Service(inner) if !caused_by_auth(inner.as_ref()) => {
            ClusterErrorKind::Unreachable
        }
        _ => ClusterErrorKind::Other,
    }
}

/// Whether `error` or anything in its source chain is a client auth error.
fn caused_by_auth(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        if error.is::<kube::client::AuthError>() {
            return true;
        }
        next = error.source();
    }
    false
}

impl From<kube::Error> for ClusterError {
    fn from(error: kube::Error) -> Self {
        Self::caused_by(kind_of(&error), error)
    }
}

/// A watch refused with HTTP 403 is
/// [`Forbidden`](ClusterErrorKind::Forbidden); one whose request failed on
/// the transport is [`Unreachable`](ClusterErrorKind::Unreachable);
/// anything else is [`Other`](ClusterErrorKind::Other).
impl From<watcher::Error> for ClusterError {
    fn from(error: watcher::Error) -> Self {
        let kind = match &error {
            watcher::Error::InitialListFailed(inner)
            | watcher::Error::WatchStartFailed(inner)
            | watcher::Error::WatchFailed(inner) => kind_of(inner),
            watcher::Error::WatchError(status) if status.code == 403 => ClusterErrorKind::Forbidden,
            _ => ClusterErrorKind::Other,
        };
        Self::caused_by(kind, error)
    }
}

/// A Cluster the app can open, named without saying where it comes from.
///
/// Today every Cluster is a kubeconfig context and the id wraps its name;
/// a second source changes only this type's inside and this crate. The app
/// takes its ids from [`clusters`](crate::clusters). It orders by name, so
/// that a list keyed by it is stable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterId {
    context: String,
}

impl ClusterId {
    /// The Cluster named `name`, whether or not any source lists it: what
    /// tests and scripted clusters name a Cluster with. Today that is the
    /// kubeconfig context `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            context: name.into(),
        }
    }

    /// The name the user knows the Cluster by.
    pub fn name(&self) -> &str {
        &self.context
    }

    /// The kubeconfig context the Cluster is.
    pub(crate) fn context(&self) -> &str {
        &self.context
    }
}

/// The Cluster's name.
impl std::fmt::Display for ClusterId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.context)
    }
}

/// One connect attempt, shared by every request that waits on it.
type SharedClient = Shared<BoxFuture<'static, Result<Client, ClusterError>>>;

/// Starts a connect attempt, at once.
type Connector = Box<dyn Fn() -> SharedClient + Send + Sync>;

/// The connect attempts of one Cluster: the latest one, and how to start
/// the next. Shared by the handle and its watches, so a watch retrying
/// after a failed connect joins the attempt a request started.
struct Connection {
    /// The latest connect attempt; replaced once it has failed.
    client: Mutex<SharedClient>,
    connector: Connector,
    runtime: Handle,
}

impl Connection {
    /// Start connecting, with the first attempt now.
    fn new(runtime: Handle, connector: Connector) -> Self {
        Self {
            client: Mutex::new(Self::attempt(&runtime, &connector)),
            connector,
            runtime,
        }
    }

    /// Start a connect attempt, and poll it on `runtime` so its failure is
    /// seen even when no request awaits it: [`Shared::peek`] answers only
    /// once the attempt has been polled to its end.
    fn attempt(runtime: &Handle, connector: &Connector) -> SharedClient {
        let attempt = connector();
        runtime.spawn(attempt.clone().map(drop));
        attempt
    }

    /// The client of the latest connect attempt. An attempt that failed is
    /// not kept: a new one starts and every later request shares it.
    fn client(&self) -> SharedClient {
        let mut client = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(client.peek(), Some(Err(_))) {
            *client = Self::attempt(&self.runtime, &self.connector);
        }
        client.clone()
    }
}

/// The live handle to one Cluster.
///
/// Connecting is lazy and shared: building the handle never blocks, every
/// request waits on the same connect, and a connect failure fails each
/// request waiting on it with a [`Connect`](ClusterErrorKind::Connect)
/// error. A failed connect is not kept: the next request, a watch retrying
/// or [`reconnect`](Self::reconnect) connects again, while a successful
/// one stays the one client of the handle. A watch stops when its [`Watch`] is
/// dropped or when the handle is, whichever comes first.
pub struct Cluster {
    /// Which Cluster, for logs.
    id: ClusterId,
    /// The connect attempts, shared with the watches.
    connection: Arc<Connection>,
    /// The namespace the context names, `"default"` when it names none.
    default_namespace: String,
    /// Where requests run: the cluster runtime, or the caller's for
    /// `from_client`, so a test's paused clock drives the watch retries.
    runtime: Handle,
    /// Dropped with the handle, which ends every running watch: a watch
    /// task holds the receiving side and stops once the sender is gone.
    /// Nothing is kept per watch; its reader's [`Watch`] stops it alone.
    stop: tokio::sync::watch::Sender<()>,
}

impl Cluster {
    /// Start connecting to `cluster` and return at once, after one read of
    /// the small kubeconfig file, which gives the context's namespace and
    /// the first connect attempt; the client itself connects off the
    /// caller's thread. Each later attempt reads the file again.
    ///
    /// # Panics
    ///
    /// When the cluster runtime cannot be created on first use.
    pub fn connect(cluster: &ClusterId) -> Self {
        let id = cluster.clone();
        let runtime = runtime().handle().clone();
        let first_read = crate::client::read_kubeconfig();
        let default_namespace = crate::client::default_namespace_for(&first_read, &id);
        let connector = {
            let cluster = cluster.clone();
            let runtime = runtime.clone();
            // The first attempt takes the read above, every later one
            // reads for itself: a kubeconfig is never kept across attempts.
            let first_read = Mutex::new(Some(first_read));
            move || {
                let cluster = cluster.clone();
                let kubeconfig = first_read
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                // A credential plugin's first run blocks inside config
                // loading, so the connect runs on the cluster runtime,
                // never the caller's thread.
                spawn(
                    &runtime,
                    async move {
                        let kubeconfig = kubeconfig.unwrap_or_else(crate::client::read_kubeconfig);
                        crate::client::client_for(&cluster, kubeconfig).await
                    },
                    ClusterError::connect("cluster connect task panicked"),
                )
                .boxed()
                .shared()
            }
        };
        Self::with_connector(id, default_namespace, runtime, Box::new(connector))
    }

    /// A handle whose connect attempts `connector` starts: the first one
    /// now.
    fn with_connector(
        id: ClusterId,
        default_namespace: String,
        runtime: Handle,
        connector: Connector,
    ) -> Self {
        Self {
            id,
            connection: Arc::new(Connection::new(runtime.clone(), connector)),
            default_namespace,
            runtime,
            stop: tokio::sync::watch::channel(()).0,
        }
    }

    /// The client of the latest connect attempt; see [`Connection::client`].
    fn client(&self) -> SharedClient {
        self.connection.client()
    }

    /// Connect again if the last connect failed, started at once. Without
    /// a failed connect it changes nothing: a connect in flight or done is
    /// kept. Watches do not need it: they retry a failed connect on their
    /// own. It starts the connect now, for a caller that cannot wait for
    /// their backoff.
    pub fn reconnect(&self) {
        drop(self.client());
    }

    /// A handle to `id` over an already built `client`, such as a
    /// mock one in tests. Requests run on the caller's tokio runtime when
    /// there is one, so a test's paused clock drives them; otherwise on the
    /// cluster runtime.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, when the cluster runtime cannot be created
    /// on first use.
    pub fn from_client(client: Client, id: ClusterId) -> Self {
        let default_namespace = client.default_namespace().to_owned();
        let runtime = Handle::try_current().unwrap_or_else(|_| runtime().handle().clone());
        let connector = move || futures::future::ready(Ok(client.clone())).boxed().shared();
        Self::with_connector(id, default_namespace, runtime, Box::new(connector))
    }

    /// The namespace the context names, `"default"` when it names none:
    /// where a Session starts and where Apply sends a document that names
    /// no namespace, as `kubectl` does.
    pub fn default_namespace(&self) -> &str {
        &self.default_namespace
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
    /// - [`Unreachable`](ClusterErrorKind::Unreachable) when the apiserver
    ///   cannot be reached after the connect (every request can fail this
    ///   way);
    /// - [`Other`](ClusterErrorKind::Other) when it fails any other way
    ///   (a 404 or 5xx from `/apis/<group>/<version>` aside), or the request's
    ///   task panicked.
    pub fn discover(
        &self,
    ) -> impl Future<Output = Result<ServedResources, ClusterError>> + Send + 'static {
        let client = self.client();
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
    /// - [`Unreachable`](ClusterErrorKind::Unreachable) when the apiserver
    ///   cannot be reached after the connect;
    /// - [`Other`](ClusterErrorKind::Other) when it fails any other way,
    ///   e.g. the object is gone (404), or the request's task panicked.
    pub fn operate(
        &self,
        operation: &Operation,
    ) -> impl Future<Output = Result<(), ClusterError>> + Send + 'static {
        let client = self.client();
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
    /// at once on the cluster runtime. Each document's kind is resolved
    /// from `served` first; the apiserver is asked only for a kind `served`
    /// lacks, such as a CRD Discovery has not seen yet. Each document is
    /// sent on its own: one the server rejects does not stop the others, and
    /// its outcome in the report carries the rejection, its causes located
    /// on `snapshot`.
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
        served: ServedResources,
    ) -> impl Future<Output = Result<ApplyReport, ClusterError>> + Send + 'static {
        self.send_snapshot(
            snapshot,
            "apply task panicked",
            move |client, snapshot| async move {
                apply::apply_documents(&client, &snapshot, mode, &served).await
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
        served: ServedResources,
    ) -> impl Future<Output = Result<ApplyReport, ClusterError>> + Send + 'static {
        self.send_snapshot(
            snapshot,
            "precheck task panicked",
            move |client, snapshot| async move {
                apply::precheck_documents(&client, &snapshot, &served).await
            },
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
        let client = self.client();
        let snapshot = snapshot.to_owned();
        spawn(
            &self.runtime,
            async move {
                let client = client.await?;
                request(client, snapshot).await.map_err(ClusterError::other)
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
    ///   [`Unreachable`](ClusterErrorKind::Unreachable) when it cannot be
    ///   reached, [`Other`](ClusterErrorKind::Other) otherwise);
    /// - reading the stream fails ([`Other`](ClusterErrorKind::Other)).
    ///
    /// The stream just ends when the server closes it, or when the
    /// request's task panicked.
    pub fn logs(&self, request: LogRequest) -> LogStream {
        let (sender, chunks) = mpsc::unbounded();
        let client = self.client();
        let work = self.runtime.spawn(async move {
            let stream = async {
                let pod = &request.pod;
                let namespace = namespace_of(pod)?;
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
                let chunk = chunk.map_err(ClusterError::other);
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
    ///   [`Unreachable`](ClusterErrorKind::Unreachable) when it cannot be
    ///   reached, [`Other`](ClusterErrorKind::Other) otherwise);
    /// - the remote streams fail, or the server ends the command with a
    ///   failure that is not an exit code
    ///   ([`Other`](ClusterErrorKind::Other)).
    ///
    /// The session just ends, with no `Exited`, when the request's task
    /// panicked.
    pub fn exec(&self, request: ExecRequest) -> ExecSession {
        let (input, inputs) = mpsc::unbounded();
        let (sender, output) = mpsc::unbounded();
        let client = self.client();
        let work = self.runtime.spawn(async move {
            let exit = async {
                let pod = &request.pod;
                let namespace = namespace_of(pod)?;
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
    /// [`Unreachable`](ClusterErrorKind::Unreachable) when it cannot be
    /// reached; [`Other`](ClusterErrorKind::Other) for any other failure,
    /// a Service with no running Pod or no such port included.
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
        let client = self.client();
        let work = self.runtime.spawn(async move {
            let failed = |error: ClusterError| {
                let _ = sender.unbounded_send(ForwardEvent::Failed(error));
            };
            let bind_failed =
                |error: std::io::Error| ClusterError::other_in("bind localhost", error);
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
            let listener_failed = |error: std::io::Error| ClusterError::other_in("listener", error);
            let forward = async {
                match &request.target {
                    ForwardTarget::Pod(pod) => {
                        let namespace = namespace_of(pod)?;
                        pod::port_forward(&client, namespace, &pod.name, port, listener, on_error)
                            .await
                            .map_err(listener_failed)
                    }
                    ForwardTarget::Service(service) => {
                        let namespace = namespace_of(service)?;
                        service::port_forward(
                            &client,
                            namespace,
                            &service.name,
                            port,
                            listener,
                            on_error,
                        )
                        .await
                        .map_err(listener_failed)
                    }
                }
            };
            if let Err(error) = forward.await {
                failed(error);
            }
        });
        PortForward::with_work(events, work.abort_handle())
    }

    /// Watch the Cluster's Namespaces. The watch stops when the returned
    /// [`Watch`] or the handle is dropped, whichever comes first. See
    /// [`watch_objects`](Self::watch_objects) for the messages.
    pub fn watch_namespaces(&self) -> Watch<Namespace> {
        self.watch("namespace", watch::namespaces)
    }

    /// Watch the Cluster's `CustomResourceDefinition`s. The watch stops
    /// when the returned [`Watch`] or the handle is dropped, whichever
    /// comes first. See [`watch_objects`](Self::watch_objects) for the
    /// messages.
    pub fn watch_crds(&self) -> Watch<CustomResourceDefinition> {
        self.watch("CRD", watch::custom_resource_definitions)
    }

    /// Watch `resource`'s objects: in `namespace`, or cluster-wide for
    /// `None` (a cluster-scoped kind).
    ///
    /// The watch is resident until the returned [`Watch`] or the handle is
    /// dropped, whichever comes first. Dropping the [`Watch`] ends the
    /// task and closes its apiserver watch at once, even when the resource
    /// is quiet and no message would ever notice the reader gone.
    ///
    /// The receiver gets each LIST as `Init`, `InitApply`… `InitDone`, then
    /// live changes. A failure, or the watch ending, arrives as an `Err`
    /// and is followed by a fresh LIST: 1 s later, then twice as long on
    /// each consecutive failure up to 30 s, starting over after a LIST
    /// completes; 30 s later after a
    /// [`Forbidden`](ClusterErrorKind::Forbidden) one. A failed connect
    /// arrives as a [`Connect`](ClusterErrorKind::Connect) `Err` too, and
    /// the watch asks the handle for its client again after the same wait
    /// (sharing the connect attempt with every other request), so its
    /// events arrive once a connect succeeds, with no call to
    /// [`reconnect`](Self::reconnect).
    pub fn watch_objects(
        &self,
        resource: &ApiResource,
        namespace: Option<&str>,
    ) -> Watch<DynamicObject> {
        let resource = resource.clone();
        let namespace = namespace.map(str::to_owned);
        self.watch("object", move |client| {
            watch::objects(client, &resource, namespace.as_deref())
        })
    }

    /// Start one relist loop on the handle's runtime.
    fn watch<K, F, S>(&self, label: &'static str, stream: F) -> Watch<K>
    where
        K: Send + 'static,
        F: Fn(&Client) -> S + Send + 'static,
        S: futures::Stream<Item = Result<watcher::Event<K>, watcher::Error>> + Send,
    {
        let connection = self.connection.clone();
        let (messages, task) =
            watch::relist(self.id.clone(), label, move || connection.client(), stream);
        let mut stopped = self.stop.subscribe();
        let task = self.runtime.spawn(async move {
            // `changed` fails once the Cluster, the only sender, is gone.
            tokio::select! {
                () = task => {}
                _ = stopped.changed() => {}
            }
        });
        Watch::with_work(messages, task.abort_handle())
    }
}

/// The namespace of a Pod or Service request's target: the one place the
/// log, exec and forward paths read it. Pods and Services are namespaced, so
/// a key without one is a caller's bug, never the empty namespace.
fn namespace_of(target: &ObjectKey) -> Result<&str, ClusterError> {
    target
        .namespace
        .as_deref()
        .ok_or_else(|| ClusterError::new(format!("{target} has no namespace")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::watch::Watch;

    fn service_error(error: impl std::error::Error + Send + Sync + 'static) -> kube::Error {
        kube::Error::Service(Box::new(error))
    }

    /// An error with a source, as a tower layer wraps the one it hit.
    #[derive(Debug, thiserror::Error)]
    #[error("layer failed")]
    struct Layered(#[source] kube::client::AuthError);

    #[test]
    fn errors_are_equal_by_kind_and_text() {
        let error = ClusterError::new("boom");
        assert_eq!(error, ClusterError::new("boom"));
        assert_ne!(error, ClusterError::new("bang"));
        assert_ne!(error, ClusterError::forbidden("boom"));
        assert_eq!(
            ClusterError::other(std::io::Error::other("boom")),
            error,
            "the source is not compared"
        );
    }

    #[test]
    fn a_transport_failure_is_unreachable() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let error = ClusterError::from(service_error(refused));
        assert_eq!(error.kind(), ClusterErrorKind::Unreachable);
    }

    #[test]
    fn an_auth_failure_is_not_unreachable_whether_wrapped_or_not() {
        let auth = || kube::client::AuthError::UnrefreshableTokenResponse;
        for error in [service_error(auth()), service_error(Layered(auth()))] {
            assert_eq!(ClusterError::from(error).kind(), ClusterErrorKind::Other);
        }
    }

    #[test]
    fn a_target_without_a_namespace_is_an_error_not_the_empty_namespace() {
        let namespaced = ObjectKey::new(Some("shop".into()), "web");
        assert_eq!(namespace_of(&namespaced).unwrap(), "shop");
        let error = namespace_of(&ObjectKey::cluster_scoped("web")).unwrap_err();
        assert_eq!(error.to_string(), "web has no namespace");
    }

    /// A client whose Namespace LIST answers `default`, whose WATCH stays
    /// open and silent, and whose every other request is a 404.
    fn mock_client() -> Client {
        let service = tower::service_fn(|request: http::Request<kube::client::Body>| {
            let target = request.uri().to_string();
            async move {
                let response = if target.contains("watch=true") {
                    return std::future::pending().await;
                } else if target.contains("/api/v1/namespaces") {
                    http::Response::builder()
                        .status(200)
                        .header("content-type", "application/json")
                        .body(kube::client::Body::from(NAMESPACE_LIST.as_bytes().to_vec()))
                } else {
                    http::Response::builder()
                        .status(404)
                        .body(kube::client::Body::empty())
                };
                Ok::<_, std::convert::Infallible>(response.unwrap())
            }
        });
        Client::new(service, "default")
    }

    /// Synthetic: a minimal shape, not captured from a server.
    const NAMESPACE_LIST: &str = r#"{"apiVersion":"v1","kind":"NamespaceList","metadata":{"resourceVersion":"1"},"items":[{"apiVersion":"v1","kind":"Namespace","metadata":{"name":"default"}}]}"#;

    /// A handle whose first `failures` connects fail; the count of connect
    /// attempts is returned beside it.
    fn flaky(failures: usize) -> (Cluster, Arc<AtomicUsize>) {
        slow_flaky(failures, std::time::Duration::ZERO)
    }

    /// As [`flaky`], but each connect takes `delay` to answer, so
    /// requests made meanwhile wait on the same attempt.
    fn slow_flaky(failures: usize, delay: std::time::Duration) -> (Cluster, Arc<AtomicUsize>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        let connector = move || {
            let attempt = counted.fetch_add(1, Ordering::SeqCst);
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if attempt < failures {
                    Err(ClusterError::connect("credential plugin not ready"))
                } else {
                    Ok(mock_client())
                }
            }
            .boxed()
            .shared()
        };
        let cluster = Cluster::with_connector(
            ClusterId::named("flaky"),
            "default".to_owned(),
            Handle::current(),
            Box::new(connector),
        );
        (cluster, attempts)
    }

    #[tokio::test]
    async fn a_failed_connect_is_not_kept() {
        let (cluster, attempts) = flaky(1);

        let first = cluster.discover().await.unwrap_err();
        assert_eq!(first.kind(), ClusterErrorKind::Connect);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // The next request connects again: the mock answers 404, which is
        // no longer a connect failure.
        let second = cluster.discover().await;
        assert_ne!(
            second.err().map(|error| error.kind()),
            Some(ClusterErrorKind::Connect)
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn reconnect_replaces_a_failure_no_request_awaited() {
        let (cluster, attempts) = flaky(1);
        tokio::task::yield_now().await;

        cluster.reconnect();

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_successful_connect_is_shared() {
        let (cluster, attempts) = flaky(0);

        let _ = cluster.discover().await;
        let _ = cluster.discover().await;
        cluster.reconnect();

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reconnect_connects_again_after_a_failure() {
        let (cluster, attempts) = flaky(1);
        let _ = cluster.discover().await;

        cluster.reconnect();
        cluster.reconnect();

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// The next message of `watch`, and how long the paused clock took.
    async fn next_after<K>(watch: &mut Watch<K>) -> (watch::WatchMessage<K>, std::time::Duration) {
        let started = tokio::time::Instant::now();
        let message = watch.next().await.expect("the watch ended");
        (message, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn a_watch_whose_connects_fail_delivers_events_once_one_succeeds() {
        // The first attempt failed before the watch asked; the watch's own
        // first ask starts the second, which fails too.
        let (cluster, attempts) = flaky(2);
        let mut watch = cluster.watch_namespaces();

        let (error, _) = next_after(&mut watch).await;
        assert_eq!(error.unwrap_err().kind(), ClusterErrorKind::Connect);

        // Nobody called `reconnect`: the next attempt is the watch's, after
        // the first backoff.
        let (init, waited) = next_after(&mut watch).await;
        assert!(matches!(init, Ok(watcher::Event::Init)));
        assert_eq!(waited, std::time::Duration::from_secs(1));
        let (applied, _) = next_after(&mut watch).await;
        assert!(matches!(applied, Ok(watcher::Event::InitApply(_))));
        let (done, _) = next_after(&mut watch).await;
        assert!(matches!(done, Ok(watcher::Event::InitDone)));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn connect_retries_back_off_to_the_relist_cap() {
        let (cluster, _) = flaky(usize::MAX);
        let mut watch = cluster.watch_namespaces();

        let (first, _) = next_after(&mut watch).await;
        assert!(first.is_err());
        let mut waits = Vec::new();
        for _ in 0..8 {
            let (message, waited) = next_after(&mut watch).await;
            assert_eq!(message.unwrap_err().kind(), ClusterErrorKind::Connect);
            waits.push(waited.as_secs());
        }

        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[tokio::test(start_paused = true)]
    async fn watches_retrying_after_one_failure_share_one_connect() {
        let (cluster, attempts) = slow_flaky(1, std::time::Duration::from_millis(10));
        let mut namespaces = cluster.watch_namespaces();
        let mut crds = cluster.watch_crds();

        // Both wait on the first attempt and report its failure.
        assert!(next_after(&mut namespaces).await.0.is_err());
        assert!(next_after(&mut crds).await.0.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Both wait out the same second, then ask again: one new attempt.
        let (init, _) = next_after(&mut namespaces).await;
        assert!(matches!(init, Ok(watcher::Event::Init)));
        let (init, _) = next_after(&mut crds).await;
        assert!(matches!(init, Ok(watcher::Event::Init)));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
