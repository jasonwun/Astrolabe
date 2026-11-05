//! Resident watches (#28, #147): one watch's messages, and the relist loop
//! behind [`Cluster`](crate::cluster::Cluster)'s `watch_*` methods.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::channel::mpsc;
use futures::{SinkExt as _, Stream, StreamExt as _};
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{Api, Client, core::DynamicObject, discovery::ApiResource, runtime::watcher};
use tokio::task::AbortHandle;

use crate::cluster::{ClusterError, ClusterId};

/// One watch message: an event, or why the watch failed. A failure is
/// followed by a fresh LIST, which starts with `Event::Init`.
pub type WatchMessage<K> = Result<watcher::Event<K>, ClusterError>;
pub type NamespaceWatchMessage = WatchMessage<Namespace>;
pub type CrdWatchMessage = WatchMessage<CustomResourceDefinition>;
pub type ObjectWatchMessage = WatchMessage<DynamicObject>;

/// One resident watch, as its reader sees it: the messages of
/// [`Cluster`](crate::cluster::Cluster)'s `watch_*` methods. Dropping it
/// stops the relist task and closes its apiserver watch, even when the
/// watch is quiet and would never send a message to notice.
pub struct Watch<K> {
    messages: mpsc::Receiver<WatchMessage<K>>,
    work: Option<AbortHandle>,
}

impl<K> Watch<K> {
    /// A watch reading `messages`, with no task behind it.
    pub fn new(messages: mpsc::Receiver<WatchMessage<K>>) -> Self {
        Self {
            messages,
            work: None,
        }
    }

    /// The watch reading `messages`, owning the task that feeds them.
    pub(crate) fn with_work(messages: mpsc::Receiver<WatchMessage<K>>, work: AbortHandle) -> Self {
        Self {
            messages,
            work: Some(work),
        }
    }

    /// The next message if one is already queued, without waiting.
    ///
    /// # Errors
    ///
    /// [`mpsc::TryRecvError`]: `is_closed` once the watch has ended and its
    /// queue is empty, otherwise nothing is queued yet.
    pub fn try_recv(&mut self) -> Result<WatchMessage<K>, mpsc::TryRecvError> {
        self.messages.try_recv()
    }
}

impl<K> From<mpsc::Receiver<WatchMessage<K>>> for Watch<K> {
    fn from(messages: mpsc::Receiver<WatchMessage<K>>) -> Self {
        Self::new(messages)
    }
}

impl<K> Stream for Watch<K> {
    type Item = WatchMessage<K>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.messages).poll_next(cx)
    }
}

impl<K> Drop for Watch<K> {
    fn drop(&mut self) {
        if let Some(work) = self.work.take() {
            work.abort();
        }
    }
}

/// Wait before the first relist after a failed or ended watch.
const RETRY: Duration = Duration::from_secs(1);
/// The longest wait before a relist: the wait doubles from [`RETRY`] on each
/// consecutive failure up to this, and a 403 waits this long at once (RBAC
/// denials do not clear by retrying fast).
const RETRY_CAP: Duration = Duration::from_secs(30);
/// How long the relist loop waits before its next LIST.
struct Retry {
    next: Duration,
}

impl Retry {
    fn new() -> Self {
        Self { next: RETRY }
    }

    /// The wait after a failed or ended watch; the one after it is twice
    /// as long, up to [`RETRY_CAP`].
    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(RETRY_CAP);
        delay
    }

    /// A LIST completed: the apiserver is reachable, so start over.
    fn reset(&mut self) {
        self.next = RETRY;
    }
}

/// Messages a watch buffers before its loop waits on the reader.
const CAPACITY: usize = 256;

pub(crate) fn namespaces(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<Namespace>, watcher::Error>> + Send + use<> {
    tracing::debug!("starting namespace watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default())
}

/// The resident, cluster-scoped CRD watch behind Custom Resources navigation.
pub(crate) fn custom_resource_definitions(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<CustomResourceDefinition>, watcher::Error>> + Send + use<>
{
    tracing::debug!("starting CRD watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default())
}

/// One resource's objects, untyped (custom resources and built-in kinds
/// alike): `None` watches a cluster-scoped kind, `Some` one namespace of a
/// namespaced kind (#8: never cluster-wide for those).
pub(crate) fn objects(
    client: &Client,
    resource: &ApiResource,
    namespace: Option<&str>,
) -> impl Stream<Item = Result<watcher::Event<DynamicObject>, watcher::Error>> + Send + use<> {
    tracing::debug!(kind = %resource.kind, namespace = ?namespace, "starting object watch stream");
    let api = match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, resource),
        None => Api::all_with(client.clone(), resource),
    };
    watcher(api, watcher::Config::default())
}

/// The resident relist loop for one watch, answering through the returned
/// receiver. Any stream error restarts the watcher from a fresh LIST (#28):
/// a silent watch resumption would emit no `InitDone`, leaving the reader
/// stale. The loop ends once the receiver is dropped and the loop next
/// sends; [`Watch`] stops it sooner, when its reader goes.
pub(crate) fn relist<K, F, S>(
    cluster: ClusterId,
    label: &'static str,
    client: impl Future<Output = Result<Client, ClusterError>> + Send + 'static,
    stream: F,
) -> (
    mpsc::Receiver<WatchMessage<K>>,
    impl Future<Output = ()> + Send + 'static,
)
where
    K: Send + 'static,
    F: Fn(&Client) -> S + Send + 'static,
    S: Stream<Item = Result<watcher::Event<K>, watcher::Error>> + Send,
{
    let (mut sender, receiver) = mpsc::channel(CAPACITY);
    let task = async move {
        let client = match client.await {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(cluster = %cluster, error = %error, "{label} watch client setup failed");
                let _ = sender.send(Err(error)).await;
                return;
            }
        };
        let mut retry = Retry::new();
        loop {
            let events = stream(&client);
            futures::pin_mut!(events);
            let mut forbidden = false;
            let mut failed = false;
            while let Some(event) = events.next().await {
                let event = event.map_err(|error| {
                    tracing::warn!(cluster = %cluster, error = %error, "{label} watch failed");
                    ClusterError::from(error)
                });
                match &event {
                    Err(error) => {
                        failed = true;
                        forbidden = error.is_forbidden();
                    }
                    Ok(watcher::Event::InitDone) => retry.reset(),
                    Ok(_) => {}
                }
                if sender.send(event).await.is_err() {
                    return;
                }
                if failed {
                    break;
                }
            }
            if !failed
                && sender
                    .send(Err(ClusterError::new(format!("{label} watch ended"))))
                    .await
                    .is_err()
            {
                return;
            }
            tracing::info!(cluster = %cluster, "restarting {label} watch");
            let delay = if forbidden {
                RETRY_CAP
            } else {
                retry.next_delay()
            };
            tokio::time::sleep(delay).await;
        }
    };
    (receiver, task)
}
