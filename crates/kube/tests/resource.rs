//! One-shot object operations against a mock apiserver (synthetic
//! responses, never captured from a real apiserver).

use std::sync::{Arc, Mutex};

use astrolabe_kube::k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use astrolabe_kube::k8s_openapi::api::batch::v1::CronJob;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::resource::{delete, rollout_restart, scale, suspend, trigger_cron_job};

/// One request the mock apiserver saw.
#[derive(Debug)]
struct Seen {
    line: String,
    content_type: String,
    body: serde_json::Value,
}

type Log = Arc<Mutex<Vec<Seen>>>;

fn mock_client(status: u16, reply: &'static str) -> (Client, Log) {
    let seen: Log = Arc::default();
    let log = seen.clone();
    let service = tower::service_fn(move |req: http::Request<Body>| {
        let log = log.clone();
        async move {
            let line = format!("{} {}", req.method(), req.uri().path());
            let content_type = req
                .headers()
                .get("content-type")
                .map(|value| value.to_str().unwrap().to_string())
                .unwrap_or_default();
            let bytes = req.into_body().collect_bytes().await.unwrap();
            let body = serde_json::from_slice(&bytes).unwrap_or_default();
            log.lock().unwrap().push(Seen {
                line: line.clone(),
                content_type,
                body,
            });
            let response = if reply == CRONJOB && line.starts_with("POST") {
                JOB
            } else {
                reply
            };
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(response.as_bytes().to_vec()))
                    .unwrap(),
            )
        }
    });
    (Client::new(service, "default"), seen)
}

fn lines(log: &Log) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .map(|seen| seen.line.clone())
        .collect()
}

fn crontabs() -> ApiResource {
    ApiResource {
        group: "stable.example.com".to_string(),
        version: "v1".to_string(),
        api_version: "stable.example.com/v1".to_string(),
        kind: "CronTab".to_string(),
        plural: "crontabs".to_string(),
    }
}

const DELETED: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Success","code":200}"#;
const FORBIDDEN: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"crontabs \"nightly\" is forbidden"}"#;

#[tokio::test]
async fn delete_targets_the_namespaced_or_cluster_scoped_object() {
    let (client, seen) = mock_client(200, DELETED);

    delete(&client, &crontabs(), Some("team-a"), "nightly")
        .await
        .unwrap();
    delete(&client, &crontabs(), None, "global").await.unwrap();

    assert_eq!(
        lines(&seen),
        [
            "DELETE /apis/stable.example.com/v1/namespaces/team-a/crontabs/nightly",
            "DELETE /apis/stable.example.com/v1/crontabs/global",
        ]
    );
}

#[tokio::test]
async fn delete_reports_server_refusals() {
    let (client, _) = mock_client(403, FORBIDDEN);

    let error = delete(&client, &crontabs(), Some("team-a"), "nightly")
        .await
        .unwrap_err();

    assert!(error.to_string().contains("forbidden"), "{error}");
}

/// Synthetic Scale and Deployment replies (plausible shape, not captured
/// from a server).
const SCALE: &str = r#"{"apiVersion":"autoscaling/v1","kind":"Scale","metadata":{"name":"web","namespace":"team-a"},"spec":{"replicas":5}}"#;
const DEPLOYMENT: &str = r#"{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"team-a"}}"#;

#[tokio::test]
async fn scale_patches_the_scale_subresource() {
    let (client, seen) = mock_client(200, SCALE);

    scale(
        &client,
        &ApiResource::erase::<Deployment>(&()),
        "team-a",
        "web",
        5,
    )
    .await
    .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].line,
        "PATCH /apis/apps/v1/namespaces/team-a/deployments/web/scale"
    );
    assert_eq!(seen[0].content_type, "application/merge-patch+json");
    assert_eq!(seen[0].body, serde_json::json!({"spec": {"replicas": 5}}));
}

#[tokio::test]
async fn rollout_restart_stamps_the_pod_template_as_kubectl_does() {
    let (client, seen) = mock_client(200, DEPLOYMENT);
    let at: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();

    rollout_restart(
        &client,
        &ApiResource::erase::<Deployment>(&()),
        "team-a",
        "web",
        at,
    )
    .await
    .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].line,
        "PATCH /apis/apps/v1/namespaces/team-a/deployments/web"
    );
    assert_eq!(seen[0].content_type, "application/merge-patch+json");
    assert_eq!(
        seen[0].body,
        serde_json::json!({"spec": {"template": {"metadata": {"annotations": {
            "kubectl.kubernetes.io/restartedAt": "2026-10-22T12:00:00Z"
        }}}}})
    );
}

#[tokio::test]
async fn scale_and_restart_report_server_refusals() {
    let (client, _) = mock_client(403, FORBIDDEN);
    let at: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();

    let scaled = scale(
        &client,
        &ApiResource::erase::<Deployment>(&()),
        "team-a",
        "web",
        5,
    )
    .await
    .unwrap_err();
    let restarted = rollout_restart(
        &client,
        &ApiResource::erase::<Deployment>(&()),
        "team-a",
        "web",
        at,
    )
    .await
    .unwrap_err();

    assert!(scaled.to_string().contains("forbidden"), "{scaled}");
    assert!(restarted.to_string().contains("forbidden"), "{restarted}");
}

#[tokio::test]
async fn p3_scale_and_restart_target_the_registered_kind() {
    let (client, seen) = mock_client(200, SCALE);
    scale(
        &client,
        &ApiResource::erase::<StatefulSet>(&()),
        "team-a",
        "db",
        5,
    )
    .await
    .unwrap();
    scale(
        &client,
        &ApiResource::erase::<ReplicaSet>(&()),
        "team-a",
        "web-abc",
        2,
    )
    .await
    .unwrap();
    let at: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();
    rollout_restart(
        &client,
        &ApiResource::erase::<DaemonSet>(&()),
        "team-a",
        "agent",
        at,
    )
    .await
    .unwrap();
    assert_eq!(
        lines(&seen),
        [
            "PATCH /apis/apps/v1/namespaces/team-a/statefulsets/db/scale",
            "PATCH /apis/apps/v1/namespaces/team-a/replicasets/web-abc/scale",
            "PATCH /apis/apps/v1/namespaces/team-a/daemonsets/agent",
        ]
    );
}

const CRONJOB: &str = r#"{"apiVersion":"batch/v1","kind":"CronJob","metadata":{"name":"nightly","namespace":"team-a","uid":"cron-1"},"spec":{"schedule":"0 0 * * *","jobTemplate":{"metadata":{"labels":{"app":"nightly"}},"spec":{"template":{"metadata":{"labels":{"job":"nightly"}},"spec":{"containers":[{"name":"run","image":"busybox"}],"restartPolicy":"Never"}}}}}}"#;
const JOB: &str = r#"{"apiVersion":"batch/v1","kind":"Job","metadata":{"name":"nightly-manual-abc","namespace":"team-a"},"spec":{"template":{"spec":{"containers":[],"restartPolicy":"Never"}}}}"#;

#[tokio::test]
async fn cronjob_suspend_and_trigger_patch_and_create_the_right_objects() {
    let (client, seen) = mock_client(200, CRONJOB);
    suspend(
        &client,
        &ApiResource::erase::<CronJob>(&()),
        "team-a",
        "nightly",
        true,
    )
    .await
    .unwrap();
    trigger_cron_job(&client, "team-a", "nightly")
        .await
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .map(|request| request.line.as_str())
            .collect::<Vec<_>>(),
        [
            "PATCH /apis/batch/v1/namespaces/team-a/cronjobs/nightly",
            "GET /apis/batch/v1/namespaces/team-a/cronjobs/nightly",
            "POST /apis/batch/v1/namespaces/team-a/jobs",
        ]
    );
    assert_eq!(seen[0].body, serde_json::json!({"spec": {"suspend": true}}));
    let created = &seen[2].body;
    assert_eq!(created["apiVersion"], "batch/v1");
    assert_eq!(created["kind"], "Job");
    assert_eq!(created["metadata"]["generateName"], "nightly-manual-");
    assert_eq!(
        created["metadata"]["labels"],
        serde_json::json!({"app": "nightly"})
    );
    assert_eq!(
        created["metadata"]["annotations"]["cronjob.kubernetes.io/instantiate"],
        "manual"
    );
    assert_eq!(
        created["metadata"]["ownerReferences"],
        serde_json::json!([{
            "apiVersion": "batch/v1", "kind": "CronJob", "name": "nightly", "uid": "cron-1",
            "controller": true, "blockOwnerDeletion": true
        }])
    );
    assert_eq!(
        created["spec"]["template"]["metadata"]["labels"],
        serde_json::json!({"job": "nightly"})
    );
    assert_eq!(
        created["spec"]["template"]["spec"]["containers"][0]["image"],
        "busybox"
    );
}

#[tokio::test]
async fn triggering_a_cronjob_without_a_job_spec_is_a_bad_request() {
    const WITHOUT_SPEC: &str = r#"{"apiVersion":"batch/v1","kind":"CronJob","metadata":{"name":"broken","namespace":"team-a"},"spec":{"schedule":"0 0 * * *","jobTemplate":{}}}"#;
    let (client, seen) = mock_client(200, WITHOUT_SPEC);

    let error = trigger_cron_job(&client, "team-a", "broken")
        .await
        .unwrap_err();

    let astrolabe_kube::kube::Error::Api(status) = error else {
        panic!("API error expected")
    };
    assert_eq!(
        status.message,
        "CronJob team-a/broken has no Job template spec"
    );
    assert_eq!(status.reason, "BadRequest");
    assert_eq!(status.code, 400);
    assert_eq!(
        lines(&seen),
        ["GET /apis/batch/v1/namespaces/team-a/cronjobs/broken"]
    );
}
