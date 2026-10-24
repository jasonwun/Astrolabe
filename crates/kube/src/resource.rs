//! One-shot operations on a single object of any kind.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, Patch, PatchParams};
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

/// Set a Deployment's desired replicas through its `scale` subresource.
pub async fn scale(
    client: &Client,
    namespace: &str,
    name: &str,
    replicas: i32,
) -> kube::Result<()> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let patch = serde_json::json!({"spec": {"replicas": replicas}});
    api.patch_scale(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

/// `kubectl rollout restart`: stamp the pod template with kubectl's
/// `restartedAt` annotation, so the Deployment rolls out new Pods.
pub async fn rollout_restart(
    client: &Client,
    namespace: &str,
    name: &str,
    at: Timestamp,
) -> kube::Result<()> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let patch = serde_json::json!({"spec": {"template": {"metadata": {"annotations": {
        "kubectl.kubernetes.io/restartedAt": at.to_string()
    }}}}});
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}
