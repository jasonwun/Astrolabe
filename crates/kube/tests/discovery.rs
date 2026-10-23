//! Discovery against a mock apiserver. Every served payload here is
//! synthetic: hand-written minimal shapes of the discovery documents, never
//! captured from a real apiserver.

use std::sync::{Arc, Mutex};

use astrolabe_kube::discovery::{DiscoveryCache, discover};
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;

const AGGREGATED_ACCEPT: &str = "as=APIGroupDiscoveryList";

fn json(body: &str) -> http::Response<Body> {
    http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(body.as_bytes().to_vec()))
        .unwrap()
}

fn not_found() -> http::Response<Body> {
    http::Response::builder()
        .status(404)
        .header("content-type", "application/json")
        .body(Body::from(
            br#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound","message":"not found"}"#.to_vec(),
        ))
        .unwrap()
}

/// `Client` over a canned handler; the log records `path [aggregated]` for
/// every request so tests can tell the two discovery paths apart.
fn mock_client(
    handler: impl Fn(&str, bool) -> http::Response<Body> + Send + Sync + 'static,
) -> (Client, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let handler = Arc::new(handler);
    let service = tower::service_fn(move |req: http::Request<Body>| {
        let aggregated = req
            .headers()
            .get(http::header::ACCEPT)
            .and_then(|accept| accept.to_str().ok())
            .is_some_and(|accept| accept.contains(AGGREGATED_ACCEPT));
        let path = req.uri().path().to_string();
        log.lock().unwrap().push(if aggregated {
            format!("{path} aggregated")
        } else {
            path.clone()
        });
        std::future::ready(Ok::<_, std::convert::Infallible>(handler(
            &path, aggregated,
        )))
    });
    (Client::new(service, "default"), seen)
}

const LEGACY_CORE_VERSIONS: &str =
    r#"{"kind":"APIVersions","versions":["v1"],"serverAddressByClientCIDRs":[]}"#;
const LEGACY_CORE_RESOURCES: &str = r#"{"kind":"APIResourceList","apiVersion":"v1","groupVersion":"v1","resources":[
    {"name":"namespaces","singularName":"namespace","namespaced":false,"kind":"Namespace","verbs":["get","list","watch"]}
]}"#;
const LEGACY_GROUPS: &str = r#"{"kind":"APIGroupList","apiVersion":"v1","groups":[
    {"name":"stable.example.com","versions":[{"groupVersion":"stable.example.com/v1","version":"v1"}],
     "preferredVersion":{"groupVersion":"stable.example.com/v1","version":"v1"}}
]}"#;
const LEGACY_CRONTABS: &str = r#"{"kind":"APIResourceList","apiVersion":"v1","groupVersion":"stable.example.com/v1","resources":[
    {"name":"crontabs","singularName":"crontab","namespaced":true,"kind":"CronTab","verbs":["get","list","watch","delete","patch"]}
]}"#;

/// A pre-aggregated apiserver: ignores the aggregated media type and
/// answers the plain `application/json` fallback that kube-client's Accept
/// header carries, so `/apis` returns an `APIGroupList` even for aggregated
/// asks. The bodies are hand-trimmed; that response shape (200,
/// `APIGroupList` / `APIVersions`) was confirmed against a v1.25.16 kind
/// apiserver via `curl` through `kubectl proxy`.
fn legacy_server(path: &str, _aggregated: bool) -> http::Response<Body> {
    match path {
        "/api" => json(LEGACY_CORE_VERSIONS),
        "/api/v1" => json(LEGACY_CORE_RESOURCES),
        "/apis" => json(LEGACY_GROUPS),
        "/apis/stable.example.com/v1" => json(LEGACY_CRONTABS),
        _ => not_found(),
    }
}

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

fn aggregated_server(path: &str, aggregated: bool) -> http::Response<Body> {
    match (path, aggregated) {
        ("/api", true) => json(AGGREGATED_CORE),
        ("/apis", true) => json(AGGREGATED_GROUPS),
        _ => not_found(),
    }
}

#[tokio::test]
async fn aggregated_discovery_resolves_custom_kinds_in_two_requests() {
    let (client, seen) = mock_client(aggregated_server);

    let served = discover(&client).await.unwrap();

    let crontab = served
        .resolve("stable.example.com", "v1", "CronTab")
        .expect("CronTab served");
    assert_eq!(crontab.resource.plural, "crontabs");
    assert_eq!(crontab.resource.api_version, "stable.example.com/v1");
    assert!(crontab.namespaced);
    let namespace = served.resolve("", "v1", "Namespace").unwrap();
    assert!(!namespace.namespaced);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn pre_aggregated_server_falls_back_to_per_group_discovery() {
    let (client, seen) = mock_client(legacy_server);

    let served = discover(&client).await.unwrap();

    let crontab = served
        .resolve("stable.example.com", "v1", "CronTab")
        .expect("CronTab served via per-group fallback");
    assert_eq!(crontab.resource.plural, "crontabs");
    assert!(crontab.namespaced);
    assert!(
        seen.lock()
            .unwrap()
            .contains(&"/apis/stable.example.com/v1".to_string())
    );
}

#[tokio::test]
async fn unresolvable_kinds_and_versions_are_not_served() {
    let (client, _) = mock_client(aggregated_server);
    let served = discover(&client).await.unwrap();

    assert!(
        served
            .resolve("stable.example.com", "v2", "CronTab")
            .is_none()
    );
    assert!(
        served
            .resolve("stable.example.com", "v1", "Other")
            .is_none()
    );
}

#[tokio::test]
async fn failed_refresh_keeps_the_previous_result() {
    let (client, _) = mock_client(aggregated_server);
    let first = discover(&client).await.map_err(|error| error.to_string());
    let mut cache = DiscoveryCache::default();
    assert!(cache.served().is_none());

    cache.record(first);
    assert!(cache.failure().is_none());

    cache.record(Err("connection refused".to_string()));
    assert_eq!(cache.failure(), Some("connection refused"));
    assert!(
        cache
            .served()
            .and_then(|served| served.resolve("stable.example.com", "v1", "CronTab"))
            .is_some()
    );

    let (client, _) = mock_client(legacy_server);
    cache.record(discover(&client).await.map_err(|error| error.to_string()));
    assert!(cache.failure().is_none());
}
