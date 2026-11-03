//! Service port-forward resolution (#18): which Pod a Service forward
//! reaches and which container port its service port maps to, as kubectl
//! resolves `port-forward svc/name`. Synthetic objects (plausible shapes,
//! never captured from a real apiserver).

use astrolabe_kube::cluster::{Cluster, ClusterError, ClusterErrorKind, ClusterId};
use astrolabe_kube::k8s_openapi::api::core::v1::{Container, Pod, Service};
use astrolabe_kube::object::ObjectKey;
use astrolabe_kube::pod::{ForwardEvent, ForwardRequest, ForwardTarget};
use astrolabe_kube::service::{ResolvedForward, forward_target};

fn service(spec: &serde_json::Value) -> Service {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": "web", "namespace": "team-a"},
        "spec": spec
    }))
    .unwrap()
}

fn web_service() -> Service {
    service(&serde_json::json!({
        "selector": {"app": "web"},
        "ports": [
            {"name": "http", "port": 80, "targetPort": 8080},
            {"name": "admin", "port": 9000, "targetPort": "admin"},
            {"name": "same", "port": 7000}
        ]
    }))
}

fn running(name: &str) -> Pod {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": name, "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {
            "nodeName": "node-a",
            "containers": [{"name": "app", "ports": [{"name": "admin", "containerPort": 9901}]}]
        },
        "status": {"phase": "Running",
                   "conditions": [{"type": "Ready", "status": "True"}]}
    }))
    .unwrap()
}

fn target(pod: &str, port: u16) -> ResolvedForward {
    ResolvedForward {
        pod: pod.to_string(),
        port,
    }
}

#[test]
fn a_numeric_target_port_is_the_container_port() {
    assert_eq!(
        forward_target(&web_service(), &[running("web-1")], 80),
        Ok(target("web-1", 8080))
    );
}

#[test]
fn an_omitted_target_port_is_the_service_port() {
    assert_eq!(
        forward_target(&web_service(), &[running("web-1")], 7000),
        Ok(target("web-1", 7000))
    );
}

#[test]
fn a_named_target_port_is_looked_up_in_the_pod() {
    assert_eq!(
        forward_target(&web_service(), &[running("web-1")], 9000),
        Ok(target("web-1", 9901))
    );
}

#[test]
fn a_named_target_port_may_be_declared_by_an_init_container() {
    let mut pod = running("web-1");
    let spec = pod.spec.as_mut().unwrap();
    spec.init_containers = Some(std::mem::take(&mut spec.containers));
    spec.containers = vec![Container::default()];
    assert_eq!(
        forward_target(&web_service(), &[pod], 9000),
        Ok(target("web-1", 9901))
    );
}

#[test]
fn a_named_target_port_the_pod_lacks_is_an_error() {
    let mut pod = running("web-1");
    pod.spec.as_mut().unwrap().containers[0].ports = None;
    assert_eq!(
        forward_target(&web_service(), &[pod], 9000),
        Err(ClusterError::new(
            "Pod 'web-1' does not have a named port 'admin'"
        ))
    );
}

#[test]
fn a_headless_service_forwards_to_the_same_port() {
    let mut headless = web_service();
    headless.spec.as_mut().unwrap().cluster_ip = Some("None".to_string());
    assert_eq!(
        forward_target(&headless, &[running("web-1")], 9000),
        Ok(target("web-1", 9000))
    );
}

#[test]
fn a_headless_service_forwards_an_undeclared_port_as_is() {
    let mut headless = web_service();
    headless.spec.as_mut().unwrap().cluster_ip = Some("None".to_string());
    assert_eq!(
        forward_target(&headless, &[running("web-1")], 81),
        Ok(target("web-1", 81))
    );
}

#[test]
fn an_undeclared_service_port_is_an_error() {
    assert_eq!(
        forward_target(&web_service(), &[running("web-1")], 81),
        Err(ClusterError::new(
            "Service web does not have a service port 81"
        ))
    );
}

#[test]
fn a_service_without_a_selector_has_no_pod_to_forward_to() {
    let bare = service(&serde_json::json!({"ports": [{"port": 80}]}));
    assert_eq!(
        forward_target(&bare, &[running("web-1")], 80),
        Err(ClusterError::new(
            "invalid service 'web': Service is defined without a selector"
        ))
    );
}

#[test]
fn no_matching_pod_is_an_error() {
    assert_eq!(
        forward_target(&web_service(), &[], 80),
        Err(ClusterError::new("no Pods match Service web's selector"))
    );
}

#[test]
fn the_chosen_pod_must_be_running() {
    let mut pending = running("web-1");
    pending.status.as_mut().unwrap().phase = Some("Pending".to_string());
    assert_eq!(
        forward_target(&web_service(), &[pending], 80),
        Err(ClusterError::new(
            "unable to forward port because pod is not running. Current status=Pending"
        ))
    );
}

fn with(mut pod: Pod, change: impl FnOnce(&mut Pod)) -> Pod {
    change(&mut pod);
    pod
}

/// kubectl sorts candidates by `podutils.ActivePods`, most active first;
/// each pair differs in one criterion, the more active Pod listed second.
#[test]
fn the_most_active_pod_is_chosen() {
    let unready = |pod: &mut Pod| {
        pod.status.as_mut().unwrap().conditions = Some(
            serde_json::from_value(serde_json::json!([
                {"type": "PodScheduled", "status": "True"},
                {"type": "Ready", "status": "False"}
            ]))
            .unwrap(),
        );
    };
    let pairs = [
        (
            with(running("unassigned"), |pod| {
                pod.spec.as_mut().unwrap().node_name = None;
            }),
            "assigned",
        ),
        (
            with(running("pending"), |pod| {
                pod.status.as_mut().unwrap().phase = Some("Pending".to_string());
            }),
            "running",
        ),
        (
            with(running("unknown"), |pod| {
                pod.status.as_mut().unwrap().phase = Some("Unknown".to_string());
            }),
            "running",
        ),
        (with(running("unready"), unready), "ready"),
        (
            with(running("deleting"), |pod| {
                pod.metadata.deletion_timestamp = Some(
                    serde_json::from_value(serde_json::json!("2026-10-22T11:30:00Z")).unwrap(),
                );
            }),
            "live",
        ),
        (
            with(running("newer"), |pod| {
                pod.metadata.creation_timestamp = Some(
                    serde_json::from_value(serde_json::json!("2026-10-22T11:30:00Z")).unwrap(),
                );
            }),
            "older",
        ),
    ];
    for (less, more) in pairs {
        let less_name = less.metadata.name.clone().unwrap();
        let more = with(running(more), |_| {});
        let expected = more.metadata.name.clone().unwrap();
        for pods in [[less.clone(), more.clone()], [more.clone(), less.clone()]] {
            assert_eq!(
                forward_target(&web_service(), &pods, 80),
                Ok(target(&expected, 8080)),
                "{expected} is more active than {less_name}"
            );
        }
    }
}

#[test]
fn of_equally_active_pods_the_first_listed_is_chosen() {
    let pods = [running("web-1"), running("web-2")];
    assert_eq!(
        forward_target(&web_service(), &pods, 80),
        Ok(target("web-1", 8080))
    );
}

#[test]
fn pending_is_less_active_than_unknown() {
    let phase = |name: &str, phase: &str| {
        with(running(name), |pod| {
            pod.status.as_mut().unwrap().phase = Some(phase.to_string());
        })
    };
    for pods in [
        [phase("pending", "Pending"), phase("unknown", "Unknown")],
        [phase("unknown", "Unknown"), phase("pending", "Pending")],
    ] {
        assert_eq!(
            forward_target(&web_service(), &pods, 80),
            Err(ClusterError::new(
                "unable to forward port because pod is not running. Current status=Unknown"
            ))
        );
    }
}

type Uris = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A mock apiserver answering the Service GET with `web_service()`, the
/// Pod LIST with `pods`, and refusing the port-forward upgrade with 403.
fn mock_client(pods: &[Pod]) -> (astrolabe_kube::kube::Client, Uris) {
    use astrolabe_kube::kube::client::Body;
    let seen = Uris::default();
    let log = seen.clone();
    let list = serde_json::to_vec(&serde_json::json!({
        "apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": pods
    }))
    .unwrap();
    let service = tower::service_fn(move |req: http::Request<Body>| {
        let (log, list) = (log.clone(), list.clone());
        async move {
            let uri = req.uri().to_string();
            log.lock().unwrap().push(uri.clone());
            let (status, body) = if uri.contains("/portforward") {
                (403, br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"forbidden"}"#.to_vec())
            } else if uri.contains("/services/") {
                (200, serde_json::to_vec(&web_service()).unwrap())
            } else {
                (200, list)
            };
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(status)
                    .body(Body::from(body))
                    .unwrap(),
            )
        }
    });
    (astrolabe_kube::kube::Client::new(service, "default"), seen)
}

/// Forward Service `web`'s `port` through `client` and report the first
/// connection's failure.
async fn first_failure(client: astrolabe_kube::kube::Client, port: u16) -> ClusterError {
    use std::time::Duration;
    let cluster = Cluster::from_client(client, ClusterId::kubeconfig_context("mock"));
    let mut forward = cluster.port_forward(ForwardRequest {
        target: ForwardTarget::Service(ObjectKey::new(Some("team-a".into()), "web")),
        port,
    });
    let mut next = async || {
        tokio::time::timeout(Duration::from_secs(5), forward.next())
            .await
            .expect("the forward reports")
    };
    let Some(ForwardEvent::Listening(local)) = next().await else {
        panic!("the forward listens first");
    };
    let _connection = tokio::net::TcpStream::connect(("127.0.0.1", local))
        .await
        .unwrap();
    let Some(ForwardEvent::ConnectionFailed(error)) = next().await else {
        panic!("a failed connection is reported");
    };
    error
}

#[tokio::test]
async fn each_connection_forwards_to_the_resolved_pod_port() {
    let (client, seen) = mock_client(&[running("web-1")]);

    let error = first_failure(client, 80).await;

    assert!(error.to_string().contains("403 Forbidden"), "{error}");
    assert_eq!(error.kind(), ClusterErrorKind::Forbidden);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "/api/v1/namespaces/team-a/services/web",
            "/api/v1/namespaces/team-a/pods?&labelSelector=app%3Dweb",
            "/api/v1/namespaces/team-a/pods/web-1/portforward?&ports=8080",
        ]
    );
}

#[tokio::test]
async fn a_connection_the_service_cannot_route_fails_alone() {
    let (client, seen) = mock_client(&[]);

    let error = first_failure(client, 80).await;

    assert_eq!(error.to_string(), "no Pods match Service web's selector");
    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "no port-forward is attempted"
    );
}
