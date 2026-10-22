use futures::Stream;
use k8s_openapi::api::core::v1::Namespace;
use kube::{
    Api, Client,
    runtime::{WatchStreamExt, watcher},
};

pub fn namespaces(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<Namespace>, watcher::Error>> + Send {
    tracing::debug!("starting namespace watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default()).default_backoff()
}
