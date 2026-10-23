//! Pod streams, exec exit codes and port-forward listeners against a mock apiserver (synthetic responses, never
//! captured from a real apiserver).

use std::sync::{Arc, Mutex};

use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::pod::{
    ExecError, LOG_TAIL_LINES, bind_local, exec, exit_code, logs, port_forward,
};
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

#[tokio::test]
async fn exec_asks_for_an_interactive_tty_in_one_container() {
    // A plain 403 instead of a WebSocket upgrade: the request shape is what
    // this checks; the live stream is covered by the kind e2e.
    let (client, seen) = mock_client(
        403,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"pods \"web-1\" is forbidden: User \"dev\" cannot create resource \"pods/exec\""}"#,
    );
    let error = exec(
        &client,
        "team-a",
        "web-1",
        "app",
        &["bash".to_string()],
        futures::stream::empty(),
        |_| {},
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ExecError::Connect(_)), "{error}");
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "/api/v1/namespaces/team-a/pods/web-1/exec?&stdin=true&stdout=true&tty=true&container=app&command=bash"
        ]
    );
}

/// Synthetic remotecommand v4 status (plausible shape, not captured from a
/// server).
fn status(json: &str) -> Status {
    serde_json::from_str(json).unwrap()
}

#[test]
fn a_successful_command_exits_zero() {
    assert_eq!(exit_code(Some(&status(r#"{"status":"Success"}"#))), Ok(0));
}

#[test]
fn a_closed_status_channel_is_success() {
    assert_eq!(exit_code(None), Ok(0));
}

#[test]
fn a_non_zero_exit_code_comes_from_its_cause() {
    let failed = status(
        r#"{"status":"Failure","message":"command terminated with non-zero exit code: error executing command [sh -c exit 3], exit code 3","reason":"NonZeroExitCode","details":{"causes":[{"reason":"ExitCode","message":"3"}]}}"#,
    );
    assert_eq!(exit_code(Some(&failed)), Ok(3));
}

#[test]
fn a_non_zero_exit_without_its_cause_is_a_protocol_error() {
    let failed = status(r#"{"status":"Failure","reason":"NonZeroExitCode","details":{}}"#);
    assert_eq!(
        exit_code(Some(&failed)),
        Err("error stream protocol error: no ExitCode cause given".to_string())
    );
    let failed = status(r#"{"status":"Failure","reason":"NonZeroExitCode"}"#);
    assert_eq!(
        exit_code(Some(&failed)),
        Err("error stream protocol error: details must be set".to_string())
    );
    let failed = status(
        r#"{"status":"Failure","reason":"NonZeroExitCode","details":{"causes":[{"reason":"ExitCode","message":"256"}]}}"#,
    );
    assert_eq!(
        exit_code(Some(&failed)),
        Err("error stream protocol error: invalid exit code value \"256\"".to_string())
    );
}

#[test]
fn another_failure_is_its_message() {
    let failed = status(
        r#"{"status":"Failure","message":"executable file not found in $PATH","reason":"InternalError"}"#,
    );
    assert_eq!(
        exit_code(Some(&failed)),
        Err("executable file not found in $PATH".to_string())
    );
}

#[test]
fn an_unknown_status_is_a_protocol_error() {
    assert_eq!(
        exit_code(Some(&status("{}"))),
        Err("error stream protocol error: unknown error".to_string())
    );
}

#[tokio::test]
async fn a_free_local_port_is_used_as_asked() {
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listener = bind_local(free).await.unwrap();
    assert_eq!(listener.local_addr().unwrap().port(), free);
}

#[tokio::test]
async fn a_taken_local_port_falls_back_to_a_free_one() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let listener = bind_local(port).await.unwrap();
    let local = listener.local_addr().unwrap();
    assert!(local.ip().is_loopback());
    assert_ne!(local.port(), port);
}

#[tokio::test]
async fn each_local_connection_forwards_to_the_pod_port() {
    // A plain 403 instead of a WebSocket upgrade: each connection fails,
    // and the listener keeps accepting.
    let (client, seen) = mock_client(
        403,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"pods \"web-1\" is forbidden: User \"dev\" cannot create resource \"pods/portforward\""}"#,
    );
    let listener = bind_local(0).await.unwrap();
    let local = listener.local_addr().unwrap();
    let (failures, mut failed) = tokio::sync::mpsc::unbounded_channel();
    let forward = tokio::spawn(async move {
        port_forward(&client, "team-a", "web-1", 8080, listener, move |error| {
            let _ = failures.send(error);
        })
        .await
    });

    for _ in 0..2 {
        let _connection = tokio::net::TcpStream::connect(local).await.unwrap();
        let error = failed.recv().await.unwrap();
        // kube-rs reports a refused upgrade by its status, not its body.
        assert!(error.contains("403 Forbidden"), "{error}");
    }
    assert_eq!(
        *seen.lock().unwrap(),
        ["/api/v1/namespaces/team-a/pods/web-1/portforward?&ports=8080"; 2]
    );
    assert!(!forward.is_finished());
    forward.abort();
}
