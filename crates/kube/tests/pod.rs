//! Pod streams against a mock apiserver (synthetic responses, never
//! captured from a real apiserver).

use std::sync::{Arc, Mutex};

use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::pod::{LOG_TAIL_LINES, logs};
use futures::TryStreamExt;

type Uris = Arc<Mutex<Vec<String>>>;

fn mock_client(status: u16, reply: &'static str) -> (Client, Uris) {
    let seen: Uris = Arc::default();
    let log = seen.clone();
    let service = tower::service_fn(move |req: http::Request<Body>| {
        let log = log.clone();
        async move {
            log.lock().unwrap().push(req.uri().to_string());
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(status)
                    .body(Body::from(reply.as_bytes().to_vec()))
                    .unwrap(),
            )
        }
    });
    (Client::new(service, "default"), seen)
}

#[tokio::test]
async fn logs_follow_one_container_from_a_bounded_tail() {
    let (client, seen) = mock_client(200, "starting\n\x1b[32mready\x1b[0m\n");

    let chunks: Vec<Vec<u8>> = logs(&client, "team-a", "web-1", "app")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();

    assert_eq!(chunks.concat(), b"starting\n\x1b[32mready\x1b[0m\n");
    assert_eq!(
        *seen.lock().unwrap(),
        [format!(
            "/api/v1/namespaces/team-a/pods/web-1/log?&container=app&follow=true&tailLines={LOG_TAIL_LINES}"
        )]
    );
}

#[tokio::test]
async fn a_refused_log_request_is_an_error() {
    let (client, _) = mock_client(
        400,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":400,"reason":"BadRequest","message":"container \"db\" is not valid for pod \"web-1\""}"#,
    );
    let error = logs(&client, "team-a", "web-1", "db").await.err().unwrap();
    assert!(
        error.to_string().contains("is not valid for pod"),
        "{error}"
    );
}
