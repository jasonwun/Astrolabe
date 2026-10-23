//! One-shot operations on a single object of any kind.

use kube::api::DeleteParams;
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use kube::{Api, Client};

/// Delete one object with the server's default propagation, so dependents
/// owned through `ownerReferences` are garbage-collected.
pub async fn delete(
    client: &Client,
    resource: &ApiResource,
    namespace: Option<&str>,
    name: &str,
) -> kube::Result<()> {
    let api: Api<DynamicObject> = match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, resource),
        None => Api::all_with(client.clone(), resource),
    };
    api.delete(name, &DeleteParams::default()).await?;
    Ok(())
}
