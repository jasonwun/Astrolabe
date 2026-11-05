//! The warning per-group Discovery logs for a skipped group version (#279).
//! Its own test binary: `Logs` captures every thread's output, so no other
//! test here may reach the `warn!` it asserts.

use astrolabe_kube::cluster::{Cluster, ClusterId};
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;

mod support;

fn response(status: u16, body: &str) -> http::Response<Body> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.as_bytes().to_vec()))
        .unwrap()
}

/// A pre-aggregated apiserver whose `broken.example.com/v1` answers 503.
fn legacy_server_with_broken_group(path: &str) -> http::Response<Body> {
    match path {
        "/api" => response(
            200,
            r#"{"kind":"APIVersions","versions":["v1"],"serverAddressByClientCIDRs":[]}"#,
        ),
        "/api/v1" => response(
            200,
            r#"{"kind":"APIResourceList","apiVersion":"v1","groupVersion":"v1","resources":[
                {"name":"namespaces","singularName":"namespace","namespaced":false,"kind":"Namespace","verbs":["get"]}]}"#,
        ),
        "/apis" => response(
            200,
            r#"{"kind":"APIGroupList","apiVersion":"v1","groups":[
                {"name":"broken.example.com","versions":[{"groupVersion":"broken.example.com/v1","version":"v1"}],
                 "preferredVersion":{"groupVersion":"broken.example.com/v1","version":"v1"}}]}"#,
        ),
        "/apis/broken.example.com/v1" => response(
            503,
            r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":503,"reason":"ServiceUnavailable","message":"unavailable by the mock"}"#,
        ),
        _ => response(
            404,
            r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound"}"#,
        ),
    }
}

#[tokio::test]
async fn a_skipped_group_version_is_named_in_a_warning() {
    let logs = support::Logs::default();
    logs.install();
    let service = tower::service_fn(|req: http::Request<Body>| {
        std::future::ready(Ok::<_, std::convert::Infallible>(
            legacy_server_with_broken_group(req.uri().path()),
        ))
    });
    let client = Client::new(service, "default");

    Cluster::from_client(client, ClusterId::named("mock"))
        .discover()
        .await
        .unwrap();

    let text = logs.text();
    assert!(text.contains("WARN"), "{text}");
    assert!(text.contains("broken.example.com/v1"), "{text}");
    assert!(text.contains("code=503"), "{text}");
}
