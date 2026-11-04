//! Pod streams, exec exit codes and port-forward listeners against a mock
//! apiserver (synthetic responses, never captured from a real apiserver).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use astrolabe_kube::cluster::{Cluster, ClusterErrorKind, ClusterId};
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::object::ObjectKey;
use astrolabe_kube::pod::{
    ExecOutput, ExecRequest, ForwardEvent, ForwardRequest, ForwardTarget, LOG_TAIL_LINES,
    LogRequest, PortForward,
};
use tokio::sync::oneshot;

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

/// A client whose first request never completes. The first receiver
/// resolves once that request starts, the second once its future is
/// dropped, i.e. once its work stopped.
fn hanging_client() -> (Client, oneshot::Receiver<()>, oneshot::Receiver<()>) {
    let (started, on_start) = oneshot::channel();
    let (alive, on_drop) = oneshot::channel::<()>();
    let pending = Arc::new(Mutex::new(Some((started, alive))));
    let service = tower::service_fn(move |_: http::Request<Body>| {
        let pending = pending.lock().unwrap().take();
        async move {
            let (started, _alive) = pending.expect("one request only");
            let _ = started.send(());
            std::future::pending::<Result<http::Response<Body>, std::convert::Infallible>>().await
        }
    });
    (Client::new(service, "default"), on_start, on_drop)
}

fn web_1() -> ObjectKey {
    ObjectKey::new(Some("team-a".into()), "web-1")
}

fn log_request(container: &str) -> LogRequest {
    LogRequest {
        pod: web_1(),
        container: container.to_string(),
    }
}

fn exec_request() -> ExecRequest {
    ExecRequest {
        pod: web_1(),
        container: "app".to_string(),
        shell: "bash".to_string(),
    }
}

/// Waits for the hanging request to start, drops `handle`, and expects the
/// request's work to stop.
async fn dropping_stops_the_request<T>(
    handle: T,
    on_start: oneshot::Receiver<()>,
    on_drop: oneshot::Receiver<()>,
) {
    tokio::time::timeout(Duration::from_secs(5), on_start)
        .await
        .expect("the request starts")
        .unwrap();
    drop(handle);
    tokio::time::timeout(Duration::from_secs(5), on_drop)
        .await
        .expect("dropping the handle stops the request")
        .unwrap_err();
}

#[tokio::test]
async fn logs_follow_one_container_from_a_bounded_tail() {
    let (client, seen) = mock_client(200, "starting\n\x1b[32mready\x1b[0m\n");
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut stream = cluster.logs(log_request("app"));
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next().await {
        chunks.extend(chunk.unwrap());
    }

    assert_eq!(chunks, b"starting\n\x1b[32mready\x1b[0m\n");
    assert_eq!(
        *seen.lock().unwrap(),
        [format!(
            "/api/v1/namespaces/team-a/pods/web-1/log?&container=app&follow=true&tailLines={LOG_TAIL_LINES}"
        )]
    );
}

#[tokio::test]
async fn a_refused_log_request_is_the_streams_last_chunk() {
    let (client, _) = mock_client(
        400,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":400,"reason":"BadRequest","message":"container \"db\" is not valid for pod \"web-1\""}"#,
    );
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut stream = cluster.logs(log_request("db"));
    let error = stream.next().await.unwrap().unwrap_err();

    assert!(
        error.to_string().contains("is not valid for pod"),
        "{error}"
    );
    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert_eq!(stream.next().await, None);
}

#[tokio::test]
async fn dropping_a_log_stream_stops_its_request() {
    let (client, on_start, on_drop) = hanging_client();
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let stream = cluster.logs(log_request("app"));

    dropping_stops_the_request(stream, on_start, on_drop).await;
}

#[tokio::test]
async fn exec_asks_for_an_interactive_tty_in_one_container() {
    // A plain 403 instead of a WebSocket upgrade: the request shape is what
    // this checks; the live stream is covered by the kind e2e.
    let (client, seen) = mock_client(
        403,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"pods \"web-1\" is forbidden: User \"dev\" cannot create resource \"pods/exec\""}"#,
    );
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut session = cluster.exec(exec_request());
    let Some(ExecOutput::Exited(Err(error))) = session.next().await else {
        panic!("a refused exec ends the session with an error");
    };

    assert_eq!(error.kind(), ClusterErrorKind::Forbidden, "{error}");
    assert_eq!(session.next().await, None);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "/api/v1/namespaces/team-a/pods/web-1/exec?&stdin=true&stdout=true&tty=true&container=app&command=bash"
        ]
    );
}

#[tokio::test]
async fn an_exec_refused_another_way_is_not_forbidden() {
    let (client, _) = mock_client(
        404,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound","message":"pods \"web-1\" not found"}"#,
    );
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut session = cluster.exec(exec_request());
    let Some(ExecOutput::Exited(Err(error))) = session.next().await else {
        panic!("a refused exec ends the session with an error");
    };

    assert_eq!(error.kind(), ClusterErrorKind::Other, "{error}");
    assert!(error.to_string().contains("404 Not Found"), "{error}");
}

#[tokio::test]
async fn dropping_an_exec_session_stops_its_request() {
    let (client, on_start, on_drop) = hanging_client();
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let session = cluster.exec(exec_request());

    dropping_stops_the_request(session, on_start, on_drop).await;
}

fn forward_request(port: u16) -> ForwardRequest {
    ForwardRequest {
        target: ForwardTarget::Pod(web_1()),
        port,
    }
}

/// The port the forward listens on, from its first report.
async fn listening(forward: &mut PortForward) -> u16 {
    match next_report(forward).await {
        Some(ForwardEvent::Listening(port)) => port,
        other => panic!("expected Listening first, got {other:?}"),
    }
}

async fn next_report(forward: &mut PortForward) -> Option<ForwardEvent> {
    tokio::time::timeout(Duration::from_secs(5), forward.next())
        .await
        .expect("the forward reports")
}

#[tokio::test]
async fn a_forward_listens_on_the_asked_port_when_it_is_free() {
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (client, _) = mock_client(200, "");
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut forward = cluster.port_forward(forward_request(free));

    assert_eq!(listening(&mut forward).await, free);
}

#[tokio::test]
async fn a_forward_whose_port_is_taken_listens_on_a_free_one() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let (client, _) = mock_client(200, "");
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut forward = cluster.port_forward(forward_request(port));

    let local = listening(&mut forward).await;
    assert_ne!(local, port);
    tokio::net::TcpStream::connect(("127.0.0.1", local))
        .await
        .expect("the forward accepts on the port it reported");
}

#[tokio::test]
async fn each_local_connection_forwards_to_the_pod_port() {
    // A plain 403 instead of a WebSocket upgrade: each connection fails,
    // and the listener keeps accepting.
    let (client, seen) = mock_client(
        403,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":403,"reason":"Forbidden","message":"pods \"web-1\" is forbidden: User \"dev\" cannot create resource \"pods/portforward\""}"#,
    );
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut forward = cluster.port_forward(forward_request(8080));
    let local = listening(&mut forward).await;
    for _ in 0..2 {
        let _connection = tokio::net::TcpStream::connect(("127.0.0.1", local))
            .await
            .unwrap();
        let Some(ForwardEvent::ConnectionFailed(error)) = next_report(&mut forward).await else {
            panic!("a failed connection is reported");
        };
        // kube-rs reports a refused upgrade by its status, not its body.
        assert!(error.to_string().contains("403 Forbidden"), "{error}");
        assert_eq!(error.kind(), ClusterErrorKind::Forbidden);
    }

    assert_eq!(
        *seen.lock().unwrap(),
        ["/api/v1/namespaces/team-a/pods/web-1/portforward?&ports=8080"; 2]
    );
}

#[tokio::test]
async fn a_connection_refused_another_way_is_not_forbidden() {
    let (client, _) = mock_client(
        404,
        r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":404,"reason":"NotFound","message":"pods \"web-1\" not found"}"#,
    );
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));

    let mut forward = cluster.port_forward(forward_request(8080));
    let local = listening(&mut forward).await;
    let _connection = tokio::net::TcpStream::connect(("127.0.0.1", local))
        .await
        .unwrap();
    let Some(ForwardEvent::ConnectionFailed(error)) = next_report(&mut forward).await else {
        panic!("a failed connection is reported");
    };

    assert_eq!(error.kind(), ClusterErrorKind::Other, "{error}");
    assert!(error.to_string().contains("404 Not Found"), "{error}");
}

#[tokio::test]
async fn dropping_a_port_forward_stops_its_listener_and_connections() {
    let (client, on_start, on_drop) = hanging_client();
    let cluster = Cluster::from_client(client, ClusterId::named("mock"));
    let mut forward = cluster.port_forward(forward_request(8080));
    let local = listening(&mut forward).await;
    let _connection = tokio::net::TcpStream::connect(("127.0.0.1", local))
        .await
        .unwrap();

    dropping_stops_the_request(forward, on_start, on_drop).await;

    // Dropping aborts the forward's task asynchronously, so the listener may
    // outlive the connection task's cancellation (the suspected cause of #226's
    // flake, not reproduced). Re-binding the port succeeds only once the
    // listener is gone; connecting would also start failing when a leaked
    // listener's backlog fills up.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match tokio::net::TcpListener::bind(("127.0.0.1", local)).await {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("re-binding the forward's port failed: {error}"),
            }
        }
    })
    .await
    .expect("the listener closes after the forward is dropped");
}

#[tokio::test]
async fn a_forward_that_cannot_connect_fails_without_listening() {
    // No kubeconfig names this context, so building the client fails.
    let cluster = Cluster::connect(&ClusterId::named("astrolabe-test-no-such-context"));

    let mut forward = cluster.port_forward(forward_request(8080));
    let Some(ForwardEvent::Failed(error)) = next_report(&mut forward).await else {
        panic!("a forward without a client fails first");
    };

    assert_eq!(error.kind(), ClusterErrorKind::Connect, "{error}");
    assert_eq!(next_report(&mut forward).await, None);
}
