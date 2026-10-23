//! Pod streams and exec exit codes against a mock apiserver (synthetic responses, never
//! captured from a real apiserver).

use std::sync::{Arc, Mutex};

use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::pod::{ExecError, LOG_TAIL_LINES, exec, exit_code, logs};
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
