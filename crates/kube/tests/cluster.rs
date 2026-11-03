//! The `Cluster` handle over a mock apiserver. Every payload here is
//! synthetic: hand-written minimal shapes, never captured from a real
//! apiserver.

use astrolabe_kube::apply::{ApplyError, ApplyMode};
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

/// The Workspace YAML editor's Apply still runs outside the handle (#161):
/// it must reach a tokio reactor when awaited from an executor without one.
#[test]
fn a_request_not_behind_the_handle_yet_runs_on_the_cluster_runtime() {
    // A plain executor, with no tokio reactor of its own: the sleep only
    // completes on the cluster runtime.
    let (sender, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(futures::executor::block_on(
            astrolabe_kube::cluster::run_on_cluster_runtime(async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                42
            }),
        ));
    });

    let answer = answer.recv_timeout(std::time::Duration::from_secs(5));

    assert_eq!(answer, Ok(Some(42)));
}

// ---- Apply and Precheck ---------------------------------------------------

const CONFIGMAPS_DISCOVERY: &str = r#"{"kind":"APIResourceList","apiVersion":"v1","groupVersion":"v1","resources":[
    {"name":"configmaps","singularName":"configmap","namespaced":true,"kind":"ConfigMap","verbs":["get","list","patch"]}
]}"#;
const CONFIGMAP: &str =
    r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"cm","namespace":"default"}}"#;
const CONFIGMAP_YAML: &str =
    "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\ndata:\n  key: value\n";
const CONFLICT: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":409,"reason":"Conflict",
    "message":"Apply failed with 1 conflict: conflict with \"kubectl\" using v1: .data.key"}"#;
const UNKNOWN_FIELD: &str = r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":422,"reason":"Invalid",
    "message":"ConfigMap \"cm\" is invalid","details":{"causes":[
    {"reason":"FieldValueInvalid","message":"unknown field \"data.key\"","field":"data.key"}]}}"#;
const PATCH_PATH: &str = "/api/v1/namespaces/default/configmaps/cm";

/// The `PATCH` requests' query strings, in the order the mock saw them.
type Patches = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// `Cluster` serving ConfigMap discovery, answering each `PATCH` of `cm`
/// with `patched`, and recording the `PATCH` queries.
fn configmap_cluster(patched: fn() -> http::Response<Body>) -> (Cluster, Patches) {
    let recorded = Patches::default();
    let seen = recorded.clone();
    let service = tower::service_fn(move |req: http::Request<Body>| {
        let response = match (req.method().as_str(), req.uri().path()) {
            ("GET", "/api/v1") => reply(200, CONFIGMAPS_DISCOVERY),
            ("PATCH", PATCH_PATH) => {
                let query = req.uri().query().unwrap_or_default().to_owned();
                seen.lock().unwrap().push(query);
                patched()
            }
            _ => status(404, "NotFound"),
        };
        std::future::ready(Ok::<_, std::convert::Infallible>(response))
    });
    (
        Cluster::from_client(Client::new(service, "default"), "mock"),
        recorded,
    )
}

fn applied() -> http::Response<Body> {
    reply(200, CONFIGMAP)
}

fn conflicted() -> http::Response<Body> {
    reply(409, CONFLICT)
}

fn unknown_field() -> http::Response<Body> {
    reply(422, UNKNOWN_FIELD)
}

fn queries(patches: &Patches) -> Vec<String> {
    patches.lock().unwrap().clone()
}

#[tokio::test]
async fn apply_sends_the_snapshot_as_a_server_side_apply() {
    let (cluster, patches) = configmap_cluster(applied);

    let report = cluster
        .apply(CONFIGMAP_YAML, ApplyMode::Normal)
        .await
        .unwrap();

    assert!(report.ok);
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.outcomes[0].doc, "v1 ConfigMap/cm");
    let queries = queries(&patches);
    assert_eq!(queries.len(), 1, "{queries:?}");
    assert!(queries[0].contains("fieldManager=astrolabe"), "{queries:?}");
    assert!(!queries[0].contains("force"), "{queries:?}");
    assert!(!queries[0].contains("dryRun"), "{queries:?}");
}

#[tokio::test]
async fn a_conflicting_apply_reports_the_conflict() {
    let (cluster, _) = configmap_cluster(conflicted);

    let report = cluster
        .apply(CONFIGMAP_YAML, ApplyMode::Normal)
        .await
        .unwrap();

    assert!(!report.ok);
    let error = report.outcomes[0].result.as_ref().unwrap_err();
    match &error.error {
        ApplyError::Conflict(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].manager, "kubectl");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn force_apply_takes_over_the_conflicting_fields() {
    let (cluster, patches) = configmap_cluster(applied);

    let report = cluster
        .apply(CONFIGMAP_YAML, ApplyMode::Force)
        .await
        .unwrap();

    assert!(report.ok);
    let queries = queries(&patches);
    assert!(queries[0].contains("force=true"), "{queries:?}");
    assert!(queries[0].contains("fieldManager=astrolabe"), "{queries:?}");
}

#[tokio::test]
async fn precheck_is_a_strict_dry_run() {
    let (cluster, patches) = configmap_cluster(applied);

    let report = cluster.precheck(CONFIGMAP_YAML).await.unwrap();

    assert!(report.ok);
    let queries = queries(&patches);
    assert_eq!(queries.len(), 1, "{queries:?}");
    assert!(queries[0].contains("dryRun=All"), "{queries:?}");
    assert!(queries[0].contains("fieldValidation=Strict"), "{queries:?}");
    assert!(!queries[0].contains("force"), "{queries:?}");
}

#[tokio::test]
async fn a_precheck_reports_what_the_apply_would_hit() {
    let (cluster, _) = configmap_cluster(conflicted);

    let report = cluster.precheck(CONFIGMAP_YAML).await.unwrap();

    assert!(!report.ok);
    assert!(report.outcomes[0].result.is_err());
}

#[tokio::test]
async fn a_precheck_flags_an_unknown_field_as_strict_decoding() {
    let (cluster, _) = configmap_cluster(unknown_field);

    let report = cluster.precheck(CONFIGMAP_YAML).await.unwrap();

    let error = report.outcomes[0].result.as_ref().unwrap_err();
    match &error.error {
        ApplyError::Validation(causes) => {
            assert_eq!(causes.len(), 1);
            assert_eq!(causes[0].raw_field, "data.key");
            assert!(causes[0].is_strict_decoding());
        }
        other => panic!("expected a validation failure, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unparsable_snapshot_fails_before_anything_is_sent() {
    let (cluster, patches) = configmap_cluster(applied);

    let apply = cluster
        .apply("kind: [unclosed", ApplyMode::Normal)
        .await
        .unwrap_err();
    let precheck = cluster.precheck("kind: [unclosed").await.unwrap_err();

    assert_eq!(apply.kind(), ClusterErrorKind::Other);
    assert_eq!(precheck.kind(), ClusterErrorKind::Other);
    assert_eq!(queries(&patches), Vec::<String>::new());
}

#[tokio::test]
async fn apply_and_precheck_fail_with_the_connect_failure() {
    let cluster = Cluster::connect(NO_SUCH_CONTEXT);

    let apply = cluster
        .apply(CONFIGMAP_YAML, ApplyMode::Normal)
        .await
        .unwrap_err();
    let precheck = cluster.precheck(CONFIGMAP_YAML).await.unwrap_err();

    assert_eq!(apply.kind(), ClusterErrorKind::Connect);
    assert_eq!(precheck.kind(), ClusterErrorKind::Connect);
}
