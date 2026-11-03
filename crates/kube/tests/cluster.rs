//! The `Cluster` handle over a mock apiserver. Every payload here is
//! synthetic: hand-written minimal shapes, never captured from a real
//! apiserver.

use astrolabe_kube::cluster::{Cluster, ClusterError, ClusterErrorKind};
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;

const AGGREGATED_CORE: &str = r#"{"kind":"APIGroupDiscoveryList","apiVersion":"apidiscovery.k8s.io/v2","items":[
    {"metadata":{"name":""},"versions":[{"version":"v1","resources":[
        {"resource":"namespaces","responseKind":{"group":"","version":"v1","kind":"Namespace"},"scope":"Cluster","singularResource":"namespace","verbs":["get","list","watch"]}
    ]}]}
]}"#;
const AGGREGATED_GROUPS: &str = r#"{"kind":"APIGroupDiscoveryList","apiVersion":"apidiscovery.k8s.io/v2","items":[
    {"metadata":{"name":"stable.example.com"},"versions":[{"version":"v1","resources":[
        {"resource":"crontabs","responseKind":{"group":"stable.example.com","version":"v1","kind":"CronTab"},"scope":"Namespaced","singularResource":"crontab","verbs":["get","list","watch","delete","patch"]}
    ]}]}
]}"#;

fn reply(status: u16, body: &str) -> http::Response<Body> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.as_bytes().to_vec()))
        .unwrap()
}

fn status(code: u16, reason: &str) -> http::Response<Body> {
    reply(
        code,
        &format!(
            r#"{{"kind":"Status","apiVersion":"v1","status":"Failure","code":{code},"reason":"{reason}","message":"{reason} by the mock"}}"#
        ),
    )
}

/// `Cluster` over a canned handler keyed by request path.
fn mock_cluster(handler: impl Fn(&str) -> http::Response<Body> + Send + Sync + 'static) -> Cluster {
    let service = tower::service_fn(move |req: http::Request<Body>| {
        std::future::ready(Ok::<_, std::convert::Infallible>(handler(req.uri().path())))
    });
    Cluster::from_client(Client::new(service, "default"), "mock")
}

fn aggregated_server(path: &str) -> http::Response<Body> {
    match path {
        "/api" => reply(200, AGGREGATED_CORE),
        "/apis" => reply(200, AGGREGATED_GROUPS),
        _ => status(404, "NotFound"),
    }
}

/// Every request is refused, so both discovery paths fail on their first
/// request.
fn refusing_server(
    code: u16,
    reason: &'static str,
) -> impl Fn(&str) -> http::Response<Body> + Send + Sync + 'static {
    move |_| status(code, reason)
}

#[tokio::test]
async fn discover_lists_the_served_kinds() {
    let cluster = mock_cluster(aggregated_server);

    let served = cluster.discover().await.unwrap();

    let crontab = served
        .resolve("stable.example.com", "v1", "CronTab")
        .expect("CronTab served");
    assert_eq!(crontab.resource.plural, "crontabs");
    assert!(crontab.namespaced);
}

#[tokio::test]
async fn a_refused_discovery_is_forbidden() {
    let cluster = mock_cluster(refusing_server(403, "Forbidden"));

    let error = cluster.discover().await.unwrap_err();

    assert!(error.is_forbidden(), "{error}");
    assert_eq!(error.kind(), ClusterErrorKind::Forbidden);
    assert!(
        error.to_string().contains("Forbidden by the mock"),
        "{error}"
    );
}

#[tokio::test]
async fn a_failed_discovery_is_other() {
    let cluster = mock_cluster(refusing_server(500, "InternalError"));

    let error = cluster.discover().await.unwrap_err();

    assert!(!error.is_forbidden(), "{error}");
    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert!(
        error.to_string().contains("InternalError by the mock"),
        "{error}"
    );
}

/// No kubeconfig names this context, on CI or a dev machine, so building
/// the client fails whatever the ambient kubeconfig holds.
const NO_SUCH_CONTEXT: &str = "astrolabe-test-no-such-context";

#[tokio::test]
async fn connect_returns_at_once_and_its_failure_surfaces_per_request() {
    let cluster = Cluster::connect(NO_SUCH_CONTEXT);

    let first = cluster.discover().await.unwrap_err();
    let second = cluster.discover().await.unwrap_err();

    assert_eq!(first.kind(), ClusterErrorKind::Connect);
    assert!(!first.is_forbidden());
    assert_eq!(first, second);
}

/// GPUI's executor has no tokio reactor: a request must resolve when
/// awaited outside any tokio runtime.
#[test]
fn discover_resolves_outside_a_tokio_runtime() {
    let cluster = Cluster::connect(NO_SUCH_CONTEXT);

    let error = futures::executor::block_on(cluster.discover()).unwrap_err();

    assert_eq!(error.kind(), ClusterErrorKind::Connect);
}

#[test]
fn a_new_error_is_other_and_shows_its_message() {
    let error = ClusterError::new("scripted failure");

    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert!(!error.is_forbidden());
    assert_eq!(error.to_string(), "scripted failure");
}

/// The YAML editors' Apply still runs outside the handle (#160): it must
/// reach a tokio reactor when awaited from an executor without one.
#[test]
fn a_request_not_behind_the_handle_yet_runs_on_the_cluster_runtime() {
    let answer =
        futures::executor::block_on(astrolabe_kube::cluster::run_on_cluster_runtime(async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            42
        }));

    assert_eq!(answer, Some(42));
}
