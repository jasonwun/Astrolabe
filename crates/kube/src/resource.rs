//! One-shot operations on a single object of any kind.

use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
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

/// Set a scalable workload's desired replicas through its `scale` subresource.
pub async fn scale(
    client: &Client,
    resource: &ApiResource,
    namespace: &str,
    name: &str,
    replicas: i32,
) -> kube::Result<()> {
    merge_patch(
        client,
        resource,
        namespace,
        name,
        serde_json::json!({"spec": {"replicas": replicas}}),
        true,
    )
    .await
}

/// `kubectl rollout restart`: stamp the pod template with kubectl's
/// `restartedAt` annotation, so the controller rolls out new Pods.
pub async fn rollout_restart(
    client: &Client,
    resource: &ApiResource,
    namespace: &str,
    name: &str,
    at: Timestamp,
) -> kube::Result<()> {
    merge_patch(
        client,
        resource,
        namespace,
        name,
        serde_json::json!({"spec": {"template": {"metadata": {"annotations": {
            "kubectl.kubernetes.io/restartedAt": at.to_string()
        }}}}}),
        false,
    )
    .await
}

/// Set a CronJob's suspend flag.
pub async fn suspend(
    client: &Client,
    resource: &ApiResource,
    namespace: &str,
    name: &str,
    suspended: bool,
) -> kube::Result<()> {
    merge_patch(
        client,
        resource,
        namespace,
        name,
        serde_json::json!({"spec": {"suspend": suspended}}),
        false,
    )
    .await
}

async fn merge_patch(
    client: &Client,
    resource: &ApiResource,
    namespace: &str,
    name: &str,
    patch: serde_json::Value,
    scale: bool,
) -> kube::Result<()> {
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, resource);
    if scale {
        api.patch_scale(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
    } else {
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
    }
    Ok(())
}

/// Create one Job from a CronJob's current template, as `kubectl create job
/// --from=cronjob/...` does.
pub async fn trigger_cron_job(client: &Client, namespace: &str, name: &str) -> kube::Result<()> {
    let cronjobs: Api<CronJob> = Api::namespaced(client.clone(), namespace);
    let cronjob = cronjobs.get(name).await?;
    let owner = OwnerReference {
        api_version: "batch/v1".to_string(),
        kind: "CronJob".to_string(),
        name: name.to_string(),
        uid: cronjob.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };
    let template = cronjob.spec.job_template;
    let spec = template.spec.ok_or_else(|| {
        kube::Error::Api(Box::new(kube::core::Status {
            message: format!("CronJob {namespace}/{name} has no Job template spec"),
            reason: "BadRequest".to_string(),
            code: 400,
            ..kube::core::Status::default()
        }))
    })?;
    let metadata = template.metadata.unwrap_or_default();
    let mut annotations = metadata.annotations.unwrap_or_default();
    annotations.insert(
        "cronjob.kubernetes.io/instantiate".to_string(),
        "manual".to_string(),
    );
    let job = Job {
        metadata: ObjectMeta {
            generate_name: Some(format!("{name}-manual-")),
            annotations: Some(annotations),
            labels: metadata.labels,
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(spec),
        ..Job::default()
    };
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    jobs.create(&PostParams::default(), &job).await?;
    Ok(())
}
