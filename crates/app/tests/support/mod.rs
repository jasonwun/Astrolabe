//! The workspace UI on a scripted cluster, shared by the built-in kind
//! view tests.
#![allow(dead_code)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astrolabe::clock::Clock;
use astrolabe::session::ScriptedCluster;
use astrolabe::workspace::{Preselection, WorkspaceView};
use astrolabe_kube::apply::{ApplyMode, ApplyReport, DocError, DocOutcome};
use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_kube::object::{ObjectRef, Operation};
use astrolabe_kube::pod::{
    ExecInput, ExecOutput, ExecRequest, ForwardEvent, ForwardRequest, LogRequest,
};
use astrolabe_kube::watch::ObjectWatchMessage;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::session::{ResourceKind, WatchKey};
use futures::channel::mpsc;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{Modifiers, MouseButton, TestAppContext, VisualTestContext};
use tracing_subscriber::util::SubscriberInitExt as _;

pub type Sender<K> = mpsc::Sender<Result<Event<K>, ClusterError>>;

pub fn key(kind: BuiltinKind, namespace: &str) -> WatchKey {
    key_on("kind-a", kind, namespace)
}

pub fn key_on(cluster: &str, kind: BuiltinKind, namespace: &str) -> WatchKey {
    WatchKey {
        cluster: ClusterId::named(cluster),
        kind: ResourceKind::Builtin(kind),
        namespace: Some(namespace.to_string()),
    }
}

pub fn snapshot<K>(sender: &mut Sender<K>, objects: Vec<K>) {
    sender.try_send(Ok(Event::Init)).unwrap();
    for object in objects {
        sender.try_send(Ok(Event::InitApply(object))).unwrap();
    }
    sender.try_send(Ok(Event::InitDone)).unwrap();
}

pub fn object(value: serde_json::Value) -> DynamicObject {
    serde_json::from_value(value).unwrap()
}

/// Forwarding this port reports `LONG_ERROR` as a failed connection.
pub const LONG_ERROR_PORT: u16 = 7001;
/// Forwarding this port ends in a `Failed` state with `LONG_ERROR`.
pub const FAILED_PORT: u16 = 7002;
/// Synthetic (modelled on a kubelet/CNI message, not captured): an error
/// far wider than any window, with no break but spaces.
pub const LONG_ERROR: &str = "error forwarding port 7001: an error occurred forwarding 18080 -> 8080: error forwarding port 8080 to pod 0123456789abcdef, uid : failed to execute portforward in network namespace \"/var/run/netns/cni-0123456789\": failed to connect to localhost:8080 inside namespace: dial tcp4 127.0.0.1:8080: connect: connection refused";

pub type Deleted = Rc<RefCell<Vec<(String, Option<String>, String)>>>;

/// What `Deleted` records of a Delete: `apiVersion/Kind`, namespace, name.
pub fn deletion(object: &ObjectRef) -> (String, Option<String>, String) {
    let resource = &object.resource;
    (
        format!("{}/{}", resource.api_version, resource.kind),
        object.key.namespace.clone(),
        object.key.name.clone(),
    )
}
pub type Operated = Rc<RefCell<Vec<Operation>>>;
pub type Logged = Rc<RefCell<Vec<LogRequest>>>;
pub type Forwarded = Rc<RefCell<Vec<ForwardRequest>>>;
pub type Applied = Rc<RefCell<Vec<ApplyMode>>>;
pub type Execed = Rc<RefCell<Vec<(ExecRequest, mpsc::UnboundedReceiver<ExecInput>)>>>;

pub struct Ui<'a> {
    pub cx: &'a mut VisualTestContext,
    pub namespaces: Sender<Namespace>,
    pub deleted: Deleted,
    pub operated: Operated,
    pub logged: Logged,
    pub execed: Execed,
    pub forwarded: Forwarded,
    pub applied: Applied,
}

pub fn open(
    cx: &mut TestAppContext,
    watches: Vec<(WatchKey, mpsc::Receiver<ObjectWatchMessage>)>,
) -> Ui<'_> {
    let (namespaces, namespace_events) = mpsc::channel(32);
    let deleted: Deleted = Rc::default();
    let operated: Operated = Rc::default();
    let logged: Logged = Rc::default();
    let execed: Execed = Rc::default();
    let forwarded: Forwarded = Rc::default();
    let applied: Applied = Rc::default();
    let mut cluster = ScriptedCluster::new(namespace_events)
        .operate({
            let deleted = deleted.clone();
            let operated = operated.clone();
            move |operation: &Operation| {
                if let Operation::Delete { object } = operation {
                    deleted.borrow_mut().push(deletion(object));
                    return Ok(());
                }
                operated.borrow_mut().push(operation.clone());
                match operation {
                    Operation::Scale { replicas: 0, .. } => Err(ClusterError::new("scale refused")),
                    _ => Ok(()),
                }
            }
        })
        .apply({
            let applied = applied.clone();
            move |snapshot: &str, mode| {
                applied.borrow_mut().push(mode);
                Ok(match mode {
                    ApplyMode::Normal => conflict_report(snapshot),
                    ApplyMode::Force => ApplyReport::new(vec![outcome(Ok(()))]),
                })
            }
        })
        .precheck(|snapshot: &str| Ok(unknown_field_report(snapshot)))
        .logs({
            let logged = logged.clone();
            move |request: &LogRequest| {
                logged.borrow_mut().push(request.clone());
                vec![
                    Ok(b"starting\n".to_vec()),
                    Err(ClusterError::new("stream reset")),
                ]
            }
        })
        .exec({
            let execed = execed.clone();
            move |request: &ExecRequest, input| {
                execed.borrow_mut().push((request.clone(), input));
                let exit = match request.shell.as_str() {
                    "bash" => Ok(0),
                    _ => Err(ClusterError::new("container not found (\"app\")")),
                };
                vec![
                    ExecOutput::Bytes(b"/ # ".to_vec()),
                    ExecOutput::Exited(exit),
                ]
            }
        })
        .port_forward({
            let forwarded = forwarded.clone();
            move |request: &ForwardRequest| {
                forwarded.borrow_mut().push(request.clone());
                let mut events = vec![ForwardEvent::Listening(request.port + 10_000)];
                match request.port {
                    9901 => events.push(ForwardEvent::ConnectionFailed(ClusterError::new(
                        "error forwarding port 9901: connection refused",
                    ))),
                    LONG_ERROR_PORT => events.push(ForwardEvent::ConnectionFailed(
                        ClusterError::new(LONG_ERROR),
                    )),
                    FAILED_PORT => {
                        events = vec![ForwardEvent::Failed(ClusterError::new(LONG_ERROR))];
                    }
                    _ => {}
                }
                events
            }
        });
    for (key, events) in watches {
        cluster = cluster.watch(key, events);
    }
    let cx = open_workspace(cx, vec![(ClusterId::named("kind-a"), cluster)]);
    Ui {
        cx,
        namespaces,
        deleted,
        operated,
        logged,
        execed,
        forwarded,
        applied,
    }
}

/// A Workspace window over `clusters`, with the app's key bindings. For a
/// test that needs more than [`open`]'s one Cluster.
pub fn open_workspace(
    cx: &mut TestAppContext,
    clusters: Vec<(ClusterId, ScriptedCluster)>,
) -> &mut VisualTestContext {
    cx.update(|cx| {
        gpui_kit::init(cx);
        astrolabe::keymap::bind(cx);
    });
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        WorkspaceView::from_clusters(clusters, Preselection::default(), window, cx)
    });
    cx
}

/// The one document every scripted Apply and Precheck reports on.
fn outcome(result: Result<(), DocError>) -> DocOutcome {
    DocOutcome {
        doc_index: 0,
        doc: "v1 Namespace/old".to_string(),
        result,
    }
}

/// A report whose one document failed with `status`, its causes located
/// on `snapshot`.
fn failed_report(snapshot: &str, status: serde_json::Value) -> ApplyReport {
    ApplyReport::failed(
        0,
        "v1 Namespace/old",
        snapshot,
        &serde_json::from_value(status).unwrap(),
    )
}

/// What a plain Apply answers: `kubectl` owns `metadata.name` (409).
pub fn conflict_report(snapshot: &str) -> ApplyReport {
    failed_report(
        snapshot,
        serde_json::json!({
            "status": "Failure",
            "code": 409,
            "reason": "Conflict",
            "message": "Apply failed with 1 conflict: conflict with \"kubectl\" using v1: .metadata.name",
        }),
    )
}

/// What a Precheck answers: an unknown field, which a real Apply ignores.
pub fn unknown_field_report(snapshot: &str) -> ApplyReport {
    failed_report(
        snapshot,
        serde_json::json!({
            "status": "Failure",
            "code": 422,
            "reason": "Invalid",
            "message": "Namespace \"old\" is invalid",
            "details": {"causes": [{
                "reason": "FieldValueInvalid",
                "message": "unknown field \"spec.bogus\"",
                "field": "spec.bogus",
            }]},
        }),
    )
}

/// Open `kind`'s list in the `default` namespace from the navigation with
/// `objects` in it.
pub fn open_list(
    ui: &mut Ui<'_>,
    kind: BuiltinKind,
    watch: &mut Sender<DynamicObject>,
    objects: Vec<DynamicObject>,
) {
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(kind.nav_id(), cx));
    snapshot(watch, objects);
    ui.settle();
}

/// Open `kind`'s list from the navigation with `objects` in it, then the
/// detail of `name` in the `default` namespace.
pub fn open_detail(
    ui: &mut Ui<'_>,
    kind: BuiltinKind,
    watch: &mut Sender<DynamicObject>,
    objects: Vec<DynamicObject>,
    name: &str,
) {
    open_list(ui, kind, watch, objects);
    ui.frame(|window, cx| window.click(format!("row-default/{name}"), cx));
    ui.settle();
}

/// Projects ages from `start` plus the test scheduler's elapsed time, so
/// `advance_clock` ages every object (#70). Like the system time, no two
/// reads return the same instant (each adds a nanosecond), so a panel that
/// reads the clock twice for one projection fails here as it would live.
pub fn follow_test_clock(cx: &mut TestAppContext, start: Timestamp) {
    let origin = cx.executor().now();
    let reads = std::cell::Cell::new(0);
    cx.update(|cx| {
        cx.set_global(Clock::new(move |cx| {
            reads.set(reads.get() + 1);
            start + (cx.background_executor().now() - origin) + Duration::from_nanos(reads.get())
        }))
    });
}

impl Ui<'_> {
    pub fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.executor().advance_clock(Duration::from_millis(400));
        self.cx.run_until_parked();
    }

    pub fn namespaces(&mut self, names: &[&str]) {
        let namespaces = names
            .iter()
            .map(|name| {
                serde_json::from_value(serde_json::json!({"metadata": {"name": name}})).unwrap()
            })
            .collect();
        snapshot(&mut self.namespaces, namespaces);
    }

    /// The accessible label of `id`, as the last frame drew it; empty when
    /// `id` has none.
    ///
    /// # Panics
    ///
    /// When `id` is not rendered.
    pub fn label(&mut self, id: &'static str) -> String {
        label(self.cx, id)
    }

    pub fn frame<R>(
        &mut self,
        f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
    ) -> R {
        self.cx.update(|window, cx| {
            window.render_frame(cx);
            f(window, cx)
        })
    }
}

/// The accessible label of `id` in a freshly drawn frame; empty when `id`
/// has none. For tests that build their window without [`open`].
///
/// # Panics
///
/// When `id` is not rendered.
pub fn label(cx: &mut VisualTestContext, id: &'static str) -> String {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window
            .try_find(id)
            .unwrap_or_else(|| panic!("{id} not rendered"))
            .label()
            .unwrap_or_default()
            .to_string()
    })
}

/// Press and release the left button on the centre `id` was last drawn at,
/// as a person clicks: for elements whose handlers listen for mouse events
/// rather than `window.click`, such as dock tabs and their close buttons.
pub fn mouse_click(cx: &mut VisualTestContext, id: &'static str) {
    let bounds = cx
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("{id} was not drawn"));
    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::none());
}

/// Shared buffer behind a `tracing` subscriber, so tests can assert on the
/// log lines a developer would see via RUST_LOG.
#[derive(Clone)]
pub struct LogCapture {
    buffer: Arc<Mutex<Vec<u8>>>,
    filter: &'static str,
}

impl Default for LogCapture {
    fn default() -> Self {
        Self::with_filter("astrolabe=debug,astrolabe_kube=debug,kube_runtime=debug")
    }
}

impl LogCapture {
    /// Capture what `filter` (RUST_LOG syntax) lets through.
    pub fn with_filter(filter: &'static str) -> Self {
        Self {
            buffer: Arc::default(),
            filter,
        }
    }

    /// Capture every thread's logs for the rest of the test binary; only
    /// the binary's first install captures anything.
    pub fn install(&self) {
        let _ = self.subscriber().try_init();
    }

    /// Capture what `f` logs on this thread.
    pub fn scoped<R>(&self, f: impl FnOnce() -> R) -> R {
        tracing::subscriber::with_default(self.subscriber(), f)
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.buffer.lock().unwrap().clone()).unwrap()
    }

    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .with_env_filter(self.filter)
            .with_ansi(false)
            .with_writer(self.clone())
            .finish()
    }
}

impl Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
