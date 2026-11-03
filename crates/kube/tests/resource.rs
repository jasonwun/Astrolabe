//! One-shot object operations through `Cluster::operate` against a mock
//! apiserver (synthetic responses, never captured from a real apiserver):
//! each Operation's request, and its failures as `ClusterError`.

use std::sync::{Arc, Mutex};

use astrolabe_kube::cluster::{Cluster, ClusterErrorKind};
use astrolabe_kube::k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use astrolabe_kube::k8s_openapi::api::batch::v1::CronJob;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::{ObjectKey, ObjectRef, Operation};

/// One request the mock apiserver saw.
#[derive(Debug)]
struct Seen {
    line: String,
    content_type: String,
    body: serde_json::Value,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// `Cluster` over an apiserver that answers every request with `status`
/// and `reply`; a POST after a CronJob reply gets the created Job.
fn mock_cluster(status: u16, reply: &'static str) -> (Cluster, Log) {
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
    (
        Cluster::from_client(Client::new(service, "default"), "mock"),
        seen,
    )
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

fn object(resource: ApiResource, namespace: Option<&str>, name: &str) -> ObjectRef {
    ObjectRef {
        resource,
        key: ObjectKey::new(namespace.map(str::to_string), name),
    }
}

fn web() -> ObjectRef {
    object(ApiResource::erase::<Deployment>(&()), Some("team-a"), "web")
}

fn nightly() -> ObjectRef {
    object(
        ApiResource::erase::<CronJob>(&()),
        Some("team-a"),
        "nightly",
    )
}

const DELETED: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Success","code":200}"#;
const FORBIDDEN: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"crontabs \"nightly\" is forbidden"}"#;
const NOT_FOUND: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound","message":"deployments \"web\" not found"}"#;

#[tokio::test]
async fn delete_targets_the_namespaced_or_cluster_scoped_object() {
    let (cluster, seen) = mock_cluster(200, DELETED);

    cluster
        .operate(&Operation::Delete {
            object: object(crontabs(), Some("team-a"), "nightly"),
        })
        .await
        .unwrap();
    cluster
        .operate(&Operation::Delete {
            object: object(crontabs(), None, "global"),
        })
        .await
        .unwrap();

    assert_eq!(
        lines(&seen),
        [
            "DELETE /apis/stable.example.com/v1/namespaces/team-a/crontabs/nightly",
            "DELETE /apis/stable.example.com/v1/crontabs/global",
        ]
    );
}

/// Synthetic Scale and Deployment replies (plausible shape, not captured
/// from a server).
const SCALE: &str = r#"{"apiVersion":"autoscaling/v1","kind":"Scale","metadata":{"name":"web","namespace":"team-a"},"spec":{"replicas":5}}"#;
const DEPLOYMENT: &str = r#"{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"team-a"}}"#;

#[tokio::test]
async fn scale_patches_the_scale_subresource() {
    let (cluster, seen) = mock_cluster(200, SCALE);

    cluster
        .operate(&Operation::Scale {
            object: web(),
            replicas: 5,
        })
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
async fn rollout_restart_stamps_the_pod_template_with_the_current_time() {
    let (cluster, seen) = mock_cluster(200, DEPLOYMENT);

    cluster
        .operate(&Operation::RolloutRestart { object: web() })
        .await
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].line,
        "PATCH /apis/apps/v1/namespaces/team-a/deployments/web"
    );
    assert_eq!(seen[0].content_type, "application/merge-patch+json");
    let mut body = seen[0].body.clone();
    let stamp =
        body["spec"]["template"]["metadata"]["annotations"]["kubectl.kubernetes.io/restartedAt"]
            .take();
    assert_eq!(
        body,
        serde_json::json!({"spec": {"template": {"metadata": {"annotations": {
            "kubectl.kubernetes.io/restartedAt": null
        }}}}})
    );
    // The stamp is the cluster layer's own clock reading, so only its
    // plausibility is pinned: later than when this test was written.
    let stamp: Timestamp = stamp
        .as_str()
        .expect("a restartedAt stamp")
        .parse()
        .unwrap();
    assert!(stamp > "2026-11-01T00:00:00Z".parse().unwrap(), "{stamp}");
}

#[tokio::test]
async fn a_refused_operation_is_forbidden() {
    let (cluster, _) = mock_cluster(403, FORBIDDEN);

    for operation in [
        Operation::Delete {
            object: object(crontabs(), Some("team-a"), "nightly"),
        },
        Operation::Scale {
            object: web(),
            replicas: 5,
        },
        Operation::RolloutRestart { object: web() },
    ] {
        let error = cluster.operate(&operation).await.unwrap_err();

        assert_eq!(error.kind(), ClusterErrorKind::Forbidden, "{operation:?}");
        assert!(error.to_string().contains("forbidden"), "{error}");
    }
}

#[tokio::test]
async fn a_missing_object_is_other() {
    let (cluster, _) = mock_cluster(404, NOT_FOUND);

    let error = cluster
        .operate(&Operation::Scale {
            object: web(),
            replicas: 1,
        })
        .await
        .unwrap_err();

    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert!(error.to_string().contains("not found"), "{error}");
}

/// No kubeconfig names this context, on CI or a dev machine.
const NO_SUCH_CONTEXT: &str = "astrolabe-test-no-such-context";

/// GPUI's executor has no tokio reactor: an operation must resolve when
/// awaited outside any tokio runtime, and a failed connect fails it.
#[test]
fn an_operation_on_a_failed_connect_is_a_connect_error() {
    let cluster = Cluster::connect(NO_SUCH_CONTEXT);

    let error = futures::executor::block_on(cluster.operate(&Operation::Delete { object: web() }))
        .unwrap_err();

    assert_eq!(error.kind(), ClusterErrorKind::Connect);
}

#[tokio::test]
async fn p3_scale_and_restart_target_the_registered_kind() {
    let (cluster, seen) = mock_cluster(200, SCALE);

    for operation in [
        Operation::Scale {
            object: object(ApiResource::erase::<StatefulSet>(&()), Some("team-a"), "db"),
            replicas: 5,
        },
        Operation::Scale {
            object: object(
                ApiResource::erase::<ReplicaSet>(&()),
                Some("team-a"),
                "web-abc",
            ),
            replicas: 2,
        },
        Operation::RolloutRestart {
            object: object(
                ApiResource::erase::<DaemonSet>(&()),
                Some("team-a"),
                "agent",
            ),
        },
    ] {
        cluster.operate(&operation).await.unwrap();
    }

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
async fn set_suspend_patches_the_suspend_flag_both_ways() {
    let (cluster, seen) = mock_cluster(200, CRONJOB);

    for suspend in [true, false] {
        cluster
            .operate(&Operation::SetSuspend {
                object: nightly(),
                suspend,
            })
            .await
            .unwrap();
    }

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].line,
        "PATCH /apis/batch/v1/namespaces/team-a/cronjobs/nightly"
    );
    assert_eq!(seen[0].body, serde_json::json!({"spec": {"suspend": true}}));
    assert_eq!(
        seen[1].body,
        serde_json::json!({"spec": {"suspend": false}})
    );
}

#[tokio::test]
async fn trigger_cron_job_creates_an_owned_job_from_the_template() {
    let (cluster, seen) = mock_cluster(200, CRONJOB);

    cluster
        .operate(&Operation::TriggerCronJob { object: nightly() })
        .await
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .map(|request| request.line.as_str())
            .collect::<Vec<_>>(),
        [
            "GET /apis/batch/v1/namespaces/team-a/cronjobs/nightly",
            "POST /apis/batch/v1/namespaces/team-a/jobs",
        ]
    );
    let created = &seen[1].body;
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
    let (cluster, seen) = mock_cluster(200, WITHOUT_SPEC);

    let error = cluster
        .operate(&Operation::TriggerCronJob {
            object: object(ApiResource::erase::<CronJob>(&()), Some("team-a"), "broken"),
        })
        .await
        .unwrap_err();

    assert_eq!(error.kind(), ClusterErrorKind::Other);
    // The text the detail tab shows after "Trigger failed: ".
    assert_eq!(
        error.to_string(),
        "ApiError: CronJob team-a/broken has no Job template spec: BadRequest \
         (Status { status: None, code: 400, message: \"CronJob team-a/broken has no Job \
         template spec\", metadata: None, reason: \"BadRequest\", details: None })"
    );
    assert_eq!(
        lines(&seen),
        ["GET /apis/batch/v1/namespaces/team-a/cronjobs/broken"]
    );
}

/// The `crontabs()` fixture served cluster-scoped, as a cluster-scoped
/// custom resource is.
fn clustertabs() -> ApiResource {
    ApiResource {
        plural: "clustertabs".to_string(),
        kind: "ClusterTab".to_string(),
        ..crontabs()
    }
}

/// Scale, restart, suspend and delete `object`, and return the requests'
/// method and path lines.
async fn every_operation_on(object: &ObjectRef) -> Vec<String> {
    let (cluster, seen) = mock_cluster(200, SCALE);
    let object = || object.clone();

    for operation in [
        Operation::Scale {
            object: object(),
            replicas: 2,
        },
        Operation::RolloutRestart { object: object() },
        Operation::SetSuspend {
            object: object(),
            suspend: true,
        },
        Operation::Delete { object: object() },
    ] {
        cluster.operate(&operation).await.unwrap();
    }
    lines(&seen)
}

#[tokio::test]
async fn every_operation_addresses_a_namespaced_object_in_its_namespace() {
    let nightly = object(crontabs(), Some("team-a"), "nightly");

    assert_eq!(
        every_operation_on(&nightly).await,
        [
            "PATCH /apis/stable.example.com/v1/namespaces/team-a/crontabs/nightly/scale",
            "PATCH /apis/stable.example.com/v1/namespaces/team-a/crontabs/nightly",
            "PATCH /apis/stable.example.com/v1/namespaces/team-a/crontabs/nightly",
            "DELETE /apis/stable.example.com/v1/namespaces/team-a/crontabs/nightly",
        ]
    );
}

#[tokio::test]
async fn every_operation_addresses_a_cluster_scoped_object_without_a_namespace() {
    let global = object(clustertabs(), None, "global");

    assert_eq!(
        every_operation_on(&global).await,
        [
            "PATCH /apis/stable.example.com/v1/clustertabs/global/scale",
            "PATCH /apis/stable.example.com/v1/clustertabs/global",
            "PATCH /apis/stable.example.com/v1/clustertabs/global",
            "DELETE /apis/stable.example.com/v1/clustertabs/global",
        ]
    );
}
