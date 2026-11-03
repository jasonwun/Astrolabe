//! One-shot operations on a single object of any kind.

use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::jiff::Timestamp;
use k8s_openapi::serde::Serialize;
use k8s_openapi::serde::de::DeserializeOwned;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use kube::{Api, Client};

use crate::object::ObjectRef;

/// Delete one object with the server's default propagation, so dependents
/// owned through `ownerReferences` are garbage-collected.
///
/// # Errors
///
/// A [`kube::Error`] when the request fails, e.g. the object is gone (404)
/// or RBAC forbids the delete (403).
pub(crate) async fn delete(client: &Client, object: &ObjectRef) -> kube::Result<()> {
    api(client, object)
        .delete(&object.key.name, &DeleteParams::default())
        .await?;
    Ok(())
}

/// Set a scalable workload's desired replicas through its `scale` subresource.
///
/// # Errors
///
/// A [`kube::Error`] when the patch fails, e.g. the object is gone (404),
/// the kind has no `scale` subresource, or RBAC forbids the patch (403).
pub(crate) async fn scale(client: &Client, object: &ObjectRef, replicas: i32) -> kube::Result<()> {
    let patch = serde_json::json!({"spec": {"replicas": replicas}});
    api(client, object)
        .patch_scale(
            &object.key.name,
            &PatchParams::default(),
            &Patch::Merge(&patch),
        )
        .await?;
    Ok(())
}

/// `kubectl rollout restart`: stamp the pod template with kubectl's
/// `restartedAt` annotation, so the controller rolls out new Pods.
///
/// # Errors
///
/// A [`kube::Error`] when the patch fails, e.g. the object is gone (404)
/// or RBAC forbids the patch (403).
pub(crate) async fn rollout_restart(
    client: &Client,
    object: &ObjectRef,
    at: Timestamp,
) -> kube::Result<()> {
    merge_patch(
        client,
        object,
        serde_json::json!({"spec": {"template": {"metadata": {"annotations": {
            "kubectl.kubernetes.io/restartedAt": at.to_string()
        }}}}}),
    )
    .await
}

/// Set a CronJob's suspend flag.
///
/// # Errors
///
/// A [`kube::Error`] when the patch fails, e.g. the object is gone (404)
/// or RBAC forbids the patch (403).
pub(crate) async fn suspend(
    client: &Client,
    object: &ObjectRef,
    suspended: bool,
) -> kube::Result<()> {
    merge_patch(
        client,
        object,
        serde_json::json!({"spec": {"suspend": suspended}}),
    )
    .await
}

async fn merge_patch(
    client: &Client,
    object: &ObjectRef,
    patch: serde_json::Value,
) -> kube::Result<()> {
    api(client, object)
        .patch(
            &object.key.name,
            &PatchParams::default(),
            &Patch::Merge(&patch),
        )
        .await?;
    Ok(())
}

fn api(client: &Client, object: &ObjectRef) -> Api<DynamicObject> {
    api_in(client, &object.resource, object.key.namespace.as_deref())
}

/// The one place a namespace picks a request's URL: a namespaced object is
/// addressed in its namespace, a cluster-scoped one (no namespace) across
/// the cluster. `ObjectKey` carries `None` only for cluster-scoped objects,
/// so no namespace is ever made up here.
fn api_in(client: &Client, resource: &ApiResource, namespace: Option<&str>) -> Api<DynamicObject> {
    match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, resource),
        None => Api::all_with(client.clone(), resource),
    }
}

/// Create one Job from a CronJob's current template, as `kubectl create job
/// --from=cronjob/...` does. `object` addresses a CronJob, so it carries a
/// namespace; the Job is created in the same one.
///
/// # Errors
///
/// A [`kube::Error`] when getting the CronJob or creating the Job fails,
/// a [`kube::Error::SerdeError`] when the object read does not decode as a
/// CronJob, and a 400 `BadRequest` [`kube::Error::Api`] when the CronJob's
/// Job template has no spec.
pub(crate) async fn trigger_cron_job(client: &Client, object: &ObjectRef) -> kube::Result<()> {
    let name = &object.key.name;
    let cronjob: CronJob = convert(&api(client, object).get(name).await?)?;
    let owner = OwnerReference {
        api_version: "batch/v1".to_string(),
        kind: "CronJob".to_string(),
        name: name.clone(),
        uid: cronjob.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };
    let template = cronjob.spec.job_template;
    let spec = template.spec.ok_or_else(|| {
        kube::Error::Api(Box::new(kube::core::Status {
            message: format!("CronJob {} has no Job template spec", object.key),
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
    let jobs = api_in(
        client,
        &ApiResource::erase::<Job>(&()),
        object.key.namespace.as_deref(),
    );
    jobs.create(&PostParams::default(), &convert::<_, DynamicObject>(&job)?)
        .await?;
    Ok(())
}

/// Re-express an object between its typed and dynamic forms: both requests
/// go through [`api_in`]'s dynamic `Api`, so the CronJob arrives and the
/// Job leaves as a `DynamicObject`.
fn convert<T: Serialize, U: DeserializeOwned>(object: &T) -> kube::Result<U> {
    serde_json::to_value(object)
        .and_then(serde_json::from_value)
        .map_err(kube::Error::SerdeError)
}
