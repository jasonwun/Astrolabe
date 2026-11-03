//! Watches through the `Cluster` handle over a mock apiserver, on a paused
//! tokio clock: the 1 s and 30 s retries pass at once. Every payload here
//! is synthetic: hand-written minimal shapes, never captured from a real
//! apiserver.

use std::convert::Infallible;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astrolabe_kube::cluster::{Cluster, ClusterError, ClusterErrorKind};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::kube::core::Status;
use astrolabe_kube::kube::runtime::watcher::{Error, Event};
use astrolabe_kube::watch::NamespaceWatchMessage;
use futures::channel::mpsc;
use futures::future::{self, BoxFuture};
use futures::{FutureExt as _, StreamExt as _};
use tokio::time::Instant;

/// One canned answer to a namespace LIST or WATCH.
enum Answer {
    Ok(String),
    Status(u16, &'static str),
    /// The request never completes: a watch that stays open and silent.
    Hang,
}

/// The mock apiserver's script: answers in order per request type, and
/// every request's path and query in arrival order.
#[derive(Default)]
struct Apiserver {
    lists: Mutex<Vec<Answer>>,
    watches: Mutex<Vec<Answer>>,
    seen_lists: Mutex<Vec<String>>,
    seen_watches: Mutex<Vec<String>>,
}

impl Apiserver {
    fn new(lists: Vec<Answer>, watches: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self {
            lists: Mutex::new(lists),
            watches: Mutex::new(watches),
            ..Self::default()
        })
    }

    fn lists(&self) -> Vec<String> {
        self.seen_lists.lock().unwrap().clone()
    }

    fn watches(&self) -> Vec<String> {
        self.seen_watches.lock().unwrap().clone()
    }

    fn answer(&self, target: String) -> BoxFuture<'static, http::Response<Body>> {
        let (answers, seen) = if target.contains("watch=true") {
            (&self.watches, &self.seen_watches)
        } else {
            (&self.lists, &self.seen_lists)
        };
        seen.lock().unwrap().push(target);
        let mut answers = answers.lock().unwrap();
        let answer = if answers.is_empty() {
            Answer::Hang
        } else {
            answers.remove(0)
        };
        match answer {
            Answer::Ok(body) => future::ready(reply(200, &body)).boxed(),
            Answer::Status(code, reason) => future::ready(status(code, reason)).boxed(),
            Answer::Hang => future::pending().boxed(),
        }
    }
}

fn reply(code: u16, body: &str) -> http::Response<Body> {
    http::Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(Body::from(body.as_bytes().to_vec()))
        .unwrap()
}

fn status_json(code: u16, reason: &str) -> String {
    format!(
        r#"{{"kind":"Status","apiVersion":"v1","status":"Failure","code":{code},"reason":"{reason}","message":"{reason} by the mock"}}"#
    )
}

fn status(code: u16, reason: &str) -> http::Response<Body> {
    reply(code, &status_json(code, reason))
}

fn namespace_list(names: &[&str], resource_version: &str) -> Answer {
    let items = names
        .iter()
        .map(|name| {
            format!(r#"{{"apiVersion":"v1","kind":"Namespace","metadata":{{"name":"{name}"}}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    Answer::Ok(format!(
        r#"{{"apiVersion":"v1","kind":"NamespaceList","metadata":{{"resourceVersion":"{resource_version}"}},"items":[{items}]}}"#
    ))
}

/// A watch that sends one ERROR event, then ends.
fn failing_watch() -> Answer {
    Answer::Ok(format!(
        "{{\"type\":\"ERROR\",\"object\":{}}}\n",
        status_json(500, "InternalError")
    ))
}

/// `Cluster` over `apiserver`, on the test's runtime.
fn mock_cluster(apiserver: &Arc<Apiserver>) -> Cluster {
    let apiserver = apiserver.clone();
    let service = tower::service_fn(move |request: http::Request<Body>| {
        let target = request.uri().to_string();
        apiserver.answer(target).map(Ok::<_, Infallible>)
    });
    Cluster::from_client(Client::new(service, "default"), "mock")
}

async fn next(watch: &mut mpsc::Receiver<NamespaceWatchMessage>) -> NamespaceWatchMessage {
    watch.next().await.expect("the watch ended")
}

fn name(namespace: &Namespace) -> &str {
    namespace.metadata.name.as_deref().unwrap()
}

/// Asserts the next messages are one complete LIST of `names`.
async fn assert_list(watch: &mut mpsc::Receiver<NamespaceWatchMessage>, names: &[&str]) {
    assert!(matches!(next(watch).await, Ok(Event::Init)));
    for expected in names {
        match next(watch).await {
            Ok(Event::InitApply(namespace)) => assert_eq!(name(&namespace), *expected),
            other => panic!("expected InitApply({expected}), got {other:?}"),
        }
    }
    assert!(matches!(next(watch).await, Ok(Event::InitDone)));
}

/// Captures every thread's logs for the rest of the test binary. A
/// thread-scoped subscriber would race other tests' threads over tracing's
/// per-callsite cache, so only one test in this binary installs it.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    fn install(&self) {
        tracing_subscriber::fmt()
            .with_env_filter("astrolabe_kube=debug,kube_runtime=debug")
            .with_ansi(false)
            .with_writer(self.clone())
            .init();
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(start_paused = true)]
async fn a_watch_error_forces_a_complete_relist_a_second_later() {
    let logs = Logs::default();
    logs.install();
    let apiserver = Apiserver::new(
        vec![
            namespace_list(&["default"], "1"),
            namespace_list(&["kube-system"], "2"),
        ],
        vec![failing_watch()],
    );
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();

    // 1. Initial list/watch establishes a snapshot.
    assert_list(&mut watch, &["default"]).await;

    // 2. The failing watch arrives as an error that is not a 403.
    let error = next(&mut watch).await.unwrap_err();
    let failed_at = Instant::now();
    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert!(
        error.to_string().contains("InternalError by the mock"),
        "{error}"
    );

    // 3./4. Recovery is a fresh LIST, not a silent watch resumption: the
    // relist completes from the LIST alone and replaces the snapshot.
    assert_list(&mut watch, &["kube-system"]).await;
    assert_eq!(failed_at.elapsed(), Duration::from_secs(1));
    assert_eq!(apiserver.lists().len(), 2);

    // 5. The new watch is anchored to the fresh list.
    tokio::time::sleep(Duration::from_millis(1)).await;
    let watches = apiserver.watches();
    assert_eq!(watches.len(), 2, "{watches:?}");
    assert!(watches[1].contains("resourceVersion=2"), "{watches:?}");

    // The RUST_LOG-driven event trail covers the failure and the recovery
    // without ever logging a resource payload.
    let text = logs.text();
    assert!(
        text.contains("namespace watch failed"),
        "missing watch failure log:\n{text}"
    );
    assert!(
        text.contains("restarting namespace watch"),
        "missing recovery attempt log:\n{text}"
    );
    // kube-rs emits its own watcher/backoff events at debug under the
    // kube_runtime target (verified against kube-runtime 4.2).
    assert!(
        text.contains("kube_runtime"),
        "missing kube-rs watcher/backoff events:\n{text}"
    );
    assert!(
        !text.contains("NamespaceList"),
        "log contains a resource payload:\n{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_forbidden_watch_relists_only_after_30_seconds() {
    let apiserver = Apiserver::new(
        vec![
            Answer::Status(403, "Forbidden"),
            namespace_list(&["default"], "1"),
        ],
        Vec::new(),
    );
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();

    assert!(matches!(next(&mut watch).await, Ok(Event::Init)));
    let error = next(&mut watch).await.unwrap_err();
    let failed_at = Instant::now();
    assert!(error.is_forbidden(), "{error}");
    assert!(
        error.to_string().contains("Forbidden by the mock"),
        "{error}"
    );

    tokio::time::sleep(Duration::from_secs(29)).await;
    assert_eq!(apiserver.lists().len(), 1);

    assert_list(&mut watch, &["default"]).await;
    assert_eq!(failed_at.elapsed(), Duration::from_secs(30));
    assert_eq!(apiserver.lists().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_cluster_stops_its_watches() {
    let apiserver = Apiserver::new(vec![namespace_list(&["default"], "1")], Vec::new());
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();
    assert_list(&mut watch, &["default"]).await;

    drop(cluster);

    // Bounded, so a watch that outlives its handle fails instead of
    // hanging: the paused clock reaches the bound once nothing else runs.
    let ended = tokio::time::timeout(Duration::from_secs(60), watch.next()).await;
    assert!(matches!(ended, Ok(None)), "the watch outlived its handle");
}

/// No kubeconfig names this context, on CI or a dev machine.
const NO_SUCH_CONTEXT: &str = "astrolabe-test-no-such-context";

#[tokio::test]
async fn a_connect_failure_is_the_watch_only_message() {
    let cluster = Cluster::connect(NO_SUCH_CONTEXT);
    let mut watch = cluster.watch_crds();

    let error = watch.next().await.unwrap().unwrap_err();

    assert_eq!(error.kind(), ClusterErrorKind::Connect);
    assert!(watch.next().await.is_none());
}

fn status_box(code: u16) -> Box<Status> {
    Box::new(Status {
        code,
        ..Status::default()
    })
}

#[test]
fn only_403_responses_count_as_rbac_denial() {
    let api = |code| astrolabe_kube::kube::Error::Api(status_box(code));
    let forbidden = |error: Error| ClusterError::from(error).is_forbidden();
    assert!(forbidden(Error::InitialListFailed(api(403))));
    assert!(forbidden(Error::WatchStartFailed(api(403))));
    assert!(forbidden(Error::WatchFailed(api(403))));
    assert!(forbidden(Error::WatchError(status_box(403))));

    assert!(!forbidden(Error::InitialListFailed(api(500))));
    assert!(!forbidden(Error::WatchStartFailed(api(401))));
    assert!(!forbidden(Error::WatchFailed(api(410))));
    assert!(!forbidden(Error::WatchError(status_box(410))));
    assert!(!forbidden(Error::NoResourceVersion));
}

#[test]
fn a_forbidden_error_is_forbidden_and_shows_its_message() {
    let error = ClusterError::forbidden("pods is forbidden");

    assert_eq!(error.kind(), ClusterErrorKind::Forbidden);
    assert!(error.is_forbidden());
    assert_eq!(error.to_string(), "pods is forbidden");
}
