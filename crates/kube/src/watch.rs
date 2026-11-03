//! Resident watches (#28, #147): one watch's messages, and the relist loop
//! behind [`Cluster`](crate::cluster::Cluster)'s `watch_*` methods.

use std::time::Duration;

use futures::channel::mpsc;
use futures::{SinkExt as _, Stream, StreamExt as _};
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{
    Api, Client,
    core::DynamicObject,
    discovery::ApiResource,
    runtime::{WatchStreamExt, watcher},
};

use crate::cluster::{ClusterError, ClusterId};

/// One watch message: an event, or why the watch failed. A failure is
/// followed by a fresh LIST, which starts with `Event::Init`.
pub type WatchMessage<K> = Result<watcher::Event<K>, ClusterError>;
pub type NamespaceWatchMessage = WatchMessage<Namespace>;
pub type CrdWatchMessage = WatchMessage<CustomResourceDefinition>;
pub type ObjectWatchMessage = WatchMessage<DynamicObject>;

/// Wait before relisting after a failed or ended watch.
const RETRY: Duration = Duration::from_secs(1);
/// Wait before relisting after the apiserver refused the watch with 403:
/// RBAC denials do not clear by retrying fast.
const FORBIDDEN_RETRY: Duration = Duration::from_secs(30);
/// Messages a watch buffers before its loop waits on the reader.
const CAPACITY: usize = 256;

pub(crate) fn namespaces(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<Namespace>, watcher::Error>> + Send + use<> {
    tracing::debug!("starting namespace watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default()).default_backoff()
}

/// The resident, cluster-scoped CRD watch behind Custom Resources navigation.
pub(crate) fn custom_resource_definitions(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<CustomResourceDefinition>, watcher::Error>> + Send + use<>
{
    tracing::debug!("starting CRD watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default()).default_backoff()
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
    watcher(api, watcher::Config::default()).default_backoff()
}

/// Whether a watch failed because RBAC denies list/watch (HTTP 403).
pub(crate) fn is_forbidden(error: &watcher::Error) -> bool {
    match error {
        watcher::Error::InitialListFailed(kube::Error::Api(status))
        | watcher::Error::WatchStartFailed(kube::Error::Api(status))
        | watcher::Error::WatchFailed(kube::Error::Api(status))
        | watcher::Error::WatchError(status) => status.code == 403,
        _ => false,
    }
}

/// The resident relist loop for one watch, answering through the returned
/// receiver. Any stream error restarts the watcher from a fresh LIST (#28):
/// a silent watch resumption would emit no `InitDone`, leaving the reader
/// stale. The loop ends once the receiver is dropped and the loop next
/// sends.
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
                if let Err(error) = &event {
                    failed = true;
                    forbidden = error.is_forbidden();
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
            tokio::time::sleep(if forbidden { FORBIDDEN_RETRY } else { RETRY }).await;
        }
    };
    (receiver, task)
}
