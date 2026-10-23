use futures::Stream;
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{
    Api, Client,
    core::DynamicObject,
    discovery::ApiResource,
    runtime::{WatchStreamExt, watcher},
};

pub fn namespaces(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<Namespace>, watcher::Error>> + Send + use<> {
    tracing::debug!("starting namespace watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default()).default_backoff()
}

/// The resident, cluster-scoped CRD watch behind Custom Resources navigation.
pub fn custom_resource_definitions(
    client: &Client,
) -> impl Stream<Item = Result<watcher::Event<CustomResourceDefinition>, watcher::Error>> + Send + use<>
{
    tracing::debug!("starting CRD watch stream");
    watcher(Api::all(client.clone()), watcher::Config::default()).default_backoff()
}

/// One resource's objects, untyped (custom resources and built-in kinds
/// alike): `None` watches a cluster-scoped kind, `Some` one namespace of a
/// namespaced kind (#8: never cluster-wide for those).
pub fn objects(
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
pub fn is_forbidden(error: &watcher::Error) -> bool {
    match error {
        watcher::Error::InitialListFailed(kube::Error::Api(status))
        | watcher::Error::WatchStartFailed(kube::Error::Api(status))
        | watcher::Error::WatchFailed(kube::Error::Api(status)) => status.code == 403,
        watcher::Error::WatchError(status) => status.code == 403,
        _ => false,
    }
}
