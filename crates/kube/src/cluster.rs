//! The live Cluster handle (#147): every request the app makes of a
//! Cluster goes through [`Cluster`] and fails with a [`ClusterError`].
//!
//! Requests run on this crate's own tokio runtime, because GPUI's executor
//! has no tokio reactor; each answers through a receiver that any executor
//! can await.

use std::future::Future;
use std::sync::OnceLock;

use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use kube::Client;
use tokio::sync::oneshot;

use crate::discovery::{self, ServedResources};

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

/// Run `future` on the cluster runtime; the receiver resolves with its
/// output and errs only if the task panicked.
fn spawn<R: Send + 'static>(
    future: impl Future<Output = R> + Send + 'static,
) -> oneshot::Receiver<R> {
    let (sender, receiver) = oneshot::channel();
    runtime().spawn(async move {
        let _ = sender.send(future.await);
    });
    receiver
}

/// What kind of failure a [`ClusterError`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClusterErrorKind {
    /// The client for the Cluster could not be built: its kubeconfig is
    /// missing or wrong, or a credential plugin failed.
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

    fn connect(error: &kube::Error) -> Self {
        Self {
            kind: ClusterErrorKind::Connect,
            message: error.to_string(),
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

/// The Cluster's client, connected once and shared by every request.
pub type SharedClient = Shared<BoxFuture<'static, Result<Client, ClusterError>>>;

/// The live handle to one Cluster.
///
/// Connecting is lazy and shared: building the handle never blocks, every
/// request waits on the same connect, and a connect failure fails each
/// request with a [`Connect`](ClusterErrorKind::Connect) error.
pub struct Cluster {
    client: SharedClient,
}

impl Cluster {
    /// Start connecting to kubeconfig context `context` and return at
    /// once.
    ///
    /// # Panics
    ///
    /// When the cluster runtime cannot be created on first use.
    pub fn connect(context: &str) -> Self {
        let context = context.to_owned();
        // A credential plugin's first run blocks inside config loading, so
        // the connect runs on the cluster runtime, never the caller's thread.
        let connect = spawn(async move {
            crate::client_for_context(&context)
                .await
                .map_err(|error| ClusterError::connect(&error))
        });
        let client = async move {
            connect.await.unwrap_or_else(|_| {
                Err(ClusterError {
                    kind: ClusterErrorKind::Connect,
                    message: "cluster connect task panicked".to_owned(),
                })
            })
        };
        Self {
            client: client.boxed().shared(),
        }
    }

    /// A handle over an already built `client`, such as a mock one in
    /// tests.
    pub fn from_client(client: Client) -> Self {
        Self {
            client: futures::future::ready(Ok(client)).boxed().shared(),
        }
    }

    /// API Discovery.
    ///
    /// The receiver errs only if the request's task panicked.
    ///
    /// # Errors
    ///
    /// The answer is a [`Connect`](ClusterErrorKind::Connect) error when the
    /// client could not be built, and otherwise the per-group discovery's
    /// failure, [`Forbidden`](ClusterErrorKind::Forbidden) on a 403.
    ///
    /// # Panics
    ///
    /// When the cluster runtime cannot be created on first use.
    pub fn discover(&self) -> oneshot::Receiver<Result<ServedResources, ClusterError>> {
        let client = self.client.clone();
        spawn(async move { Ok(discovery::discover(&client.await?).await?) })
    }

    /// The shared client, for the requests that do not go through this
    /// handle yet. Watches, operations, logs, exec, port-forward and Apply
    /// move behind it in #156–#160; the last of them removes this.
    pub fn client(&self) -> SharedClient {
        self.client.clone()
    }
}
