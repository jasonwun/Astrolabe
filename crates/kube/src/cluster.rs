//! The live Cluster handle (#147): requests to a Cluster go through
//! [`Cluster`] and fail with a [`ClusterError`]. Discovery and watches do
//! today; operations, logs, exec, port-forward and Apply follow in
//! #157–#160.
//!
//! Requests run on this crate's own tokio runtime, because GPUI's executor
//! has no tokio reactor; each answers through a future or receiver that
//! any executor can poll.

use std::future::Future;
use std::sync::{Mutex, OnceLock, PoisonError};

use futures::FutureExt as _;
use futures::channel::mpsc;
use futures::future::{BoxFuture, Shared};
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::Client;
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use kube::runtime::watcher;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::discovery::{self, ServedResources};
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
    async move { receiver.await.unwrap_or(Err(panicked)) }
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

/// A 403 from the apiserver is [`Forbidden`](ClusterErrorKind::Forbidden);
/// anything else is [`Other`](ClusterErrorKind::Other).
impl From<kube::Error> for ClusterError {
    fn from(error: kube::Error) -> Self {
        let kind = match &error {
            kube::Error::Api(status) if status.code == 403 => ClusterErrorKind::Forbidden,
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
pub type SharedClient = Shared<BoxFuture<'static, Result<Client, ClusterError>>>;

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

    /// The shared client, for the requests that do not go through this
    /// handle yet. Operations, logs, exec, port-forward and Apply move
    /// behind it in #157–#160; the last of them removes this.
    pub fn client(&self) -> SharedClient {
        self.client.clone()
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
