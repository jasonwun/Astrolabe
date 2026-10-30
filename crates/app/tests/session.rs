use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use astrolabe::session::{ConnectionHealth, ResourceKind, Session, WatchError};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::{AppContext, TestAppContext};

fn namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

fn names(session: &Session) -> Vec<String> {
    session
        .namespaces()
        .into_iter()
        .map(|namespace| namespace.metadata.name.clone().unwrap())
        .collect()
}

#[gpui_kit::test]
fn session_drains_namespace_events_and_tracks_stale_recovery(cx: &mut TestAppContext) {
    let (mut sender, receiver) = mpsc::channel(32);
    let session = cx.update(|cx| cx.new(|cx| Session::from_events("kind-a", receiver, cx)));

    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(session.watch_key().cluster, "kind-a");
        assert_eq!(session.watch_key().kind, ResourceKind::Namespaces);
        assert_eq!(session.watch_key().namespace, None);
        assert_eq!(session.health(), ConnectionHealth::Connecting);
        assert!(!session.show_stale_warning());
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    sender
        .try_send(Ok(Event::InitApply(namespace("default"))))
        .unwrap();
    sender.try_send(Ok(Event::InitDone)).unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Healthy);
        assert!(!session.show_stale_warning());
    });

    sender
        .try_send(Err(WatchError::new("connection lost")))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Stale);
        assert!(session.show_stale_warning());
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    sender
        .try_send(Ok(Event::InitApply(namespace("kube-system"))))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Connecting);
        assert!(session.show_stale_warning());
    });

    sender.try_send(Ok(Event::InitDone)).unwrap();
    sender
        .try_send(Ok(Event::Apply(namespace("default"))))
        .unwrap();
    sender
        .try_send(Ok(Event::Delete(namespace("kube-system"))))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Healthy);
        assert!(!session.show_stale_warning());
    });

    drop(sender);
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Stale);
        assert!(session.show_stale_warning());
    });
}

#[gpui_kit::test]
fn session_coalesces_queued_events_into_one_notification(cx: &mut TestAppContext) {
    let (mut sender, receiver) = mpsc::channel(32);
    let session = cx.update(|cx| cx.new(|cx| Session::from_events("kind-a", receiver, cx)));
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
    cx.update(|cx| {
        cx.observe(&session, {
            let notifications = notifications.clone();
            move |_, _| notifications.set(notifications.get() + 1)
        })
        .detach();
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    for index in 0..10 {
        sender
            .try_send(Ok(Event::InitApply(namespace(&format!("ns-{index}")))))
            .unwrap();
    }
    sender.try_send(Ok(Event::InitDone)).unwrap();
    cx.run_until_parked();

    cx.read(|cx| {
        assert_eq!(names(session.read(cx)).len(), 10);
        assert_eq!(notifications.get(), 1);
    });
}

/// Minimal scripted apiserver for the relist scenario. Served payloads are
/// synthetic: hand-written minimal shapes, never captured from a real
/// apiserver.
struct ScriptedApiserver {
    state: Arc<ServerState>,
    port: u16,
}

#[derive(Default)]
struct ServerState {
    list_queries: Mutex<Vec<String>>,
    watch_queries: Mutex<Vec<String>>,
    fail_watch: (Mutex<bool>, Condvar),
    shutdown: AtomicBool,
    connections: Mutex<Vec<TcpStream>>,
}

impl ScriptedApiserver {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(ServerState::default());
        let accept_state = state.clone();
        thread::spawn(move || {
            for connection in listener.incoming() {
                if accept_state.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(connection) = connection else { break };
                let connection_state = accept_state.clone();
                thread::spawn(move || handle_connection(connection, connection_state));
            }
        });
        Self { state, port }
    }

    fn list_queries(&self) -> Vec<String> {
        self.state.list_queries.lock().unwrap().clone()
    }

    fn watch_queries(&self) -> Vec<String> {
        self.state.watch_queries.lock().unwrap().clone()
    }

    fn fail_active_watch(&self) {
        let (triggered, condvar) = &self.state.fail_watch;
        *triggered.lock().unwrap() = true;
        condvar.notify_all();
    }
}

impl Drop for ScriptedApiserver {
    fn drop(&mut self) {
        self.state.shutdown.store(true, Ordering::SeqCst);
        self.fail_active_watch();
        for connection in self.state.connections.lock().unwrap().drain(..) {
            connection.shutdown(Shutdown::Both).ok();
        }
        // Wake the accept loop so it observes the shutdown flag and exits.
        TcpStream::connect(("127.0.0.1", self.port)).ok();
    }
}

fn handle_connection(stream: TcpStream, state: Arc<ServerState>) {
    state
        .connections
        .lock()
        .unwrap()
        .push(stream.try_clone().unwrap());
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    while let Some(target) = read_request(&mut reader) {
        if !target.starts_with("/api/v1/namespaces") {
            // Session also discovers APIs and watches CRDs on connect:
            // answer those with an empty cluster, holding CRD watches open.
            if target.contains("watch=true") {
                write!(
                    writer,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n"
                )
                .ok();
                writer.flush().ok();
                let mut discarded = [0; 1024];
                while reader.read(&mut discarded).unwrap_or(0) > 0 {}
                return;
            }
            let body = other_response(&target);
            if write!(
                writer,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .is_err()
            {
                return;
            }
            writer.flush().ok();
            continue;
        }
        if target.contains("watch=true") {
            let watch = {
                let mut queries = state.watch_queries.lock().unwrap();
                queries.push(target);
                queries.len()
            };
            if write!(
                writer,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n"
            )
            .is_err()
            {
                return;
            }
            if watch == 1 {
                let (triggered, condvar) = &state.fail_watch;
                let mut triggered = triggered.lock().unwrap();
                while !*triggered {
                    triggered = condvar.wait(triggered).unwrap();
                }
                drop(triggered);
                write_chunk(&mut writer, WATCH_ERROR_EVENT);
                write!(writer, "0\r\n\r\n").ok();
                writer.flush().ok();
                // Close the socket: the client must not pool this connection
                // for the fresh LIST that follows the watch error.
                writer.shutdown(Shutdown::Both).ok();
                return;
            }
            // Later watches stay open and silent: the relist must complete
            // from the fresh LIST alone, with no Apply/Delete events.
            let mut discarded = [0; 1024];
            while reader.read(&mut discarded).unwrap_or(0) > 0 {}
            return;
        }
        let list = {
            let mut queries = state.list_queries.lock().unwrap();
            queries.push(target);
            queries.len()
        };
        let (names, resource_version) = if list == 1 {
            (vec!["default"], "1")
        } else {
            (vec!["kube-system"], "2")
        };
        let body = namespace_list(&names, resource_version);
        if write!(
            writer,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .is_err()
        {
            return;
        }
        writer.flush().ok();
    }
}

/// Discovery and CRD list answers for a cluster with no CRDs. A plain
/// `APIGroupList` sends discovery down the per-group fallback path.
fn other_response(target: &str) -> &'static str {
    let path = target.split('?').next().unwrap_or(target);
    match path {
        "/api" => r#"{"kind":"APIVersions","versions":["v1"],"serverAddressByClientCIDRs":[]}"#,
        "/api/v1" => {
            r#"{"kind":"APIResourceList","apiVersion":"v1","groupVersion":"v1","resources":[{"name":"namespaces","singularName":"namespace","namespaced":false,"kind":"Namespace","verbs":["list","watch"]}]}"#
        }
        "/apis" => r#"{"kind":"APIGroupList","apiVersion":"v1","groups":[]}"#,
        _ => {
            r#"{"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinitionList","metadata":{"resourceVersion":"1"},"items":[]}"#
        }
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<String> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    if request_line.is_empty() {
        return None;
    }
    let target = request_line.split_whitespace().nth(1)?.to_string();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        if header.trim().is_empty() {
            break;
        }
    }
    Some(target)
}

fn write_chunk(writer: &mut TcpStream, data: &str) {
    write!(writer, "{:x}\r\n{}\r\n", data.len(), data).ok();
    writer.flush().ok();
}

const WATCH_ERROR_EVENT: &str = concat!(
    r#"{"type":"ERROR","object":{"kind":"Status","apiVersion":"v1","status":"Failure","#,
    r#""message":"injected watch failure","reason":"InternalError","code":500}}"#,
    "\n"
);

fn namespace_list(names: &[&str], resource_version: &str) -> String {
    let items = names
        .iter()
        .map(|name| {
            format!(r#"{{"apiVersion":"v1","kind":"Namespace","metadata":{{"name":"{name}"}}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"apiVersion":"v1","kind":"NamespaceList","metadata":{{"resourceVersion":"{resource_version}"}},"items":[{items}]}}"#
    )
}

struct KubeconfigGuard {
    path: PathBuf,
    previous: Option<OsString>,
}

impl KubeconfigGuard {
    fn new(port: u16) -> Self {
        let path = env::temp_dir().join(format!("astrolabe-session-{}.yaml", std::process::id()));
        fs::write(
            &path,
            format!(
                r#"apiVersion: v1
kind: Config
current-context: scripted
clusters:
- name: scripted
  cluster:
    server: http://127.0.0.1:{port}
contexts:
- name: scripted
  context:
    cluster: scripted
    user: scripted
users:
- name: scripted
  user: {{}}
"#
            ),
        )
        .unwrap();
        let previous = env::var_os("KUBECONFIG");
        unsafe { env::set_var("KUBECONFIG", &path) };
        Self { path, previous }
    }
}

impl Drop for KubeconfigGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            unsafe { env::set_var("KUBECONFIG", previous) };
        } else {
            unsafe { env::remove_var("KUBECONFIG") };
        }
        fs::remove_file(&self.path).ok();
    }
}

fn pump_until(
    cx: &mut TestAppContext,
    what: &str,
    mut condition: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if condition(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Shared buffer behind a `tracing` subscriber, so tests can assert on the
/// log lines a developer would see via RUST_LOG.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn install(&self) {
        let writer = self.clone();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("astrolabe=debug,astrolabe_kube=debug,kube_runtime=debug")
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init();
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[gpui_kit::test]
fn session_watch_error_forces_complete_namespace_relist(cx: &mut TestAppContext) {
    // Real network I/O on the cluster runtime wakes the foreground executor
    // from tokio worker threads; allow parking so the test scheduler accepts
    // that cross-thread activity.
    cx.executor().allow_parking();
    let logs = LogCapture::default();
    logs.install();
    let server = ScriptedApiserver::spawn();
    let _kubeconfig = KubeconfigGuard::new(server.port);
    let session = cx.update(|cx| cx.new(|cx| Session::new("scripted", "default", cx)));

    // 1. Initial list/watch establishes a healthy snapshot.
    pump_until(cx, "initial snapshot", |cx| {
        cx.read(|cx| {
            let session = session.read(cx);
            names(session) == ["default"] && session.health() == ConnectionHealth::Healthy
        })
    });
    pump_until(cx, "active watch", |_| !server.watch_queries().is_empty());
    cx.read(|cx| assert!(!session.read(cx).show_stale_warning()));
    assert_eq!(server.list_queries().len(), 1);

    // 2. Fail the active watch: the old snapshot stays visible but stale.
    server.fail_active_watch();
    pump_until(cx, "stale state", |cx| {
        cx.read(|cx| {
            let session = session.read(cx);
            session.health() == ConnectionHealth::Stale && session.show_stale_warning()
        })
    });
    cx.read(|cx| assert_eq!(names(session.read(cx)), ["default"]));

    // 3. Recovery starts a fresh LIST instead of silently resuming the watch.
    pump_until(cx, "fresh LIST", |_| server.list_queries().len() == 2);

    // 4./5. The relist completes without any Apply/Delete event: InitDone
    // replaces the snapshot and clears the stale state.
    pump_until(cx, "replaced snapshot", |cx| {
        cx.read(|cx| {
            let session = session.read(cx);
            session.health() == ConnectionHealth::Healthy && !session.show_stale_warning()
        })
    });
    cx.read(|cx| assert_eq!(names(session.read(cx)), ["kube-system"]));
    pump_until(cx, "watch anchored to fresh list", |_| {
        server.watch_queries().len() == 2
    });
    assert!(server.watch_queries()[1].contains("resourceVersion=2"));

    // The RUST_LOG-driven event trail covers the whole failure/recovery arc
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
    assert!(
        text.contains("namespace session health changed"),
        "missing health transition log:\n{text}"
    );
    assert!(
        text.contains("namespace snapshot replaced"),
        "missing snapshot replacement log:\n{text}"
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
