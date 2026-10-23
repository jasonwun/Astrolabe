//! One-shot object operations against a mock apiserver (synthetic
//! responses, never captured from a real apiserver).

use std::sync::{Arc, Mutex};

use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::resource::delete;

fn mock_client(status: u16, body: &'static str) -> (Client, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let service = tower::service_fn(move |req: http::Request<Body>| {
        log.lock()
            .unwrap()
            .push(format!("{} {}", req.method(), req.uri().path()));
        let response = http::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(body.as_bytes().to_vec()))
            .unwrap();
        std::future::ready(Ok::<_, std::convert::Infallible>(response))
    });
    (Client::new(service, "default"), seen)
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
        *seen.lock().unwrap(),
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
