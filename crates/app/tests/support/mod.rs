//! The workspace UI on a scripted cluster, shared by the built-in kind
//! view tests.
#![allow(dead_code)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astrolabe::clock::Clock;
use astrolabe::launch::Preselection;
use astrolabe::session::{CRD_DEBOUNCE, OpenStreams, ScriptedCluster};
use astrolabe::workspace::WorkspaceView;
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
use astrolabe_kube::watch::{NamespaceWatchMessage, ObjectWatchMessage};
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
pub type Snapshots = Rc<RefCell<Vec<String>>>;
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
    /// The snapshot of each Apply, in step with `applied`.
    pub applied_snapshots: Snapshots,
    /// The streams a [`LogsAnswer::StaysOpen`] or [`ExecAnswer::StaysOpen`]
    /// keeps open.
    pub open_streams: OpenStreams,
}

/// How the scripted Cluster answers the operations a test triggers. The
/// default answers every one of them successfully; a test that needs a
/// failure declares it here, so the cluster's behaviour reads from the
/// test and not from a value the harness happens to branch on.
///
/// Where a knob is a list, the n-th such operation takes the n-th answer
/// and the last answer repeats; an empty list answers like the default.
#[derive(Clone, Default)]
pub struct Scenario {
    /// Each Scale operation, in order.
    pub scale: Vec<ScaleAnswer>,
    /// A plain Apply (a Force Apply always goes through).
    pub apply: ApplyAnswer,
    /// Every Precheck.
    pub precheck: PrecheckAnswer,
    /// Every Logs stream.
    pub logs: LogsAnswer,
    /// Each Exec session, in order.
    pub exec: Vec<ExecAnswer>,
    /// A Forward of a listed port; any other port just listens.
    pub forward: Vec<(u16, ForwardAnswer)>,
}

#[derive(Clone, Copy, Default)]
pub enum ScaleAnswer {
    #[default]
    Scaled,
    /// The cluster refuses with `scale refused`.
    Refused,
}

#[derive(Clone, Copy, Default)]
pub enum ApplyAnswer {
    #[default]
    Applied,
    /// A plain Apply answers 409 from `kubectl`, which offers Force Apply.
    Conflict,
}

#[derive(Clone, Copy, Default)]
pub enum PrecheckAnswer {
    #[default]
    Clean,
    /// Notes the unknown field `spec.bogus`.
    UnknownField,
}

#[derive(Clone, Copy, Default)]
pub enum LogsAnswer {
    /// One line, then the stream ends.
    #[default]
    Ends,
    /// One line, then the stream fails with `stream reset`.
    Resets,
    /// One line, then the stream stays open until `open_streams.close()`.
    StaysOpen,
}

#[derive(Clone, Copy, Default)]
pub enum ExecAnswer {
    /// A prompt, then the shell exits with code 0.
    #[default]
    Exits,
    /// A prompt, then the exec fails with `container not found`.
    Fails,
    /// A prompt, then the session stays open until `open_streams.close()`.
    StaysOpen,
}

#[derive(Clone, Copy)]
pub enum ForwardAnswer {
    /// Listening, then a connection fails with the message.
    ConnectionFails(&'static str),
    /// The forward fails with the message and never listens.
    Fails(&'static str),
}

/// The `n`-th of `answers`, the last one repeating.
fn nth<T: Copy + Default>(answers: &[T], n: usize) -> T {
    answers
        .get(n)
        .or(answers.last())
        .copied()
        .unwrap_or_default()
}

pub fn open(
    cx: &mut TestAppContext,
    watches: Vec<(WatchKey, mpsc::Receiver<ObjectWatchMessage>)>,
    scenario: Scenario,
) -> Ui<'_> {
    let (namespaces, namespace_events) = mpsc::channel(32);
    let open_streams = OpenStreams::default();
    let (mut cluster, recorded) = scenario.cluster(namespace_events, &open_streams);
    for (key, events) in watches {
        cluster = cluster.watch(key, events);
    }
    let cx = open_workspace(cx, vec![(ClusterId::named("kind-a"), cluster)]);
    Ui {
        cx,
        namespaces,
        deleted: recorded.deleted,
        operated: recorded.operated,
        logged: recorded.logged,
        execed: recorded.execed,
        forwarded: recorded.forwarded,
        applied: recorded.applied,
        applied_snapshots: recorded.applied_snapshots,
        open_streams,
    }
}

/// What a scripted Cluster was asked to do.
#[derive(Default)]
pub struct Recorded {
    pub deleted: Deleted,
    pub operated: Operated,
    pub logged: Logged,
    pub execed: Execed,
    pub forwarded: Forwarded,
    pub applied: Applied,
    pub applied_snapshots: Snapshots,
}

impl Scenario {
    /// A scripted Cluster that answers as declared and records what it is
    /// asked, for tests that open several Clusters through
    /// [`open_workspace`]. It has no watches yet. Streams that stay open
    /// are held by `open_streams`.
    pub fn cluster(
        &self,
        namespace_events: mpsc::Receiver<NamespaceWatchMessage>,
        open_streams: &OpenStreams,
    ) -> (ScriptedCluster, Recorded) {
        let recorded = Recorded::default();
        let mut scales = 0;
        let mut execs = 0;
        let scale = self.scale.clone();
        let apply = self.apply;
        let precheck = self.precheck;
        let logs = self.logs;
        let exec = self.exec.clone();
        let forward = self.forward.clone();
        let mut cluster = ScriptedCluster::new(namespace_events)
            .operate({
                let deleted = recorded.deleted.clone();
                let operated = recorded.operated.clone();
                move |operation: &Operation| {
                    if let Operation::Delete { object } = operation {
                        deleted.borrow_mut().push(deletion(object));
                        return Ok(());
                    }
                    operated.borrow_mut().push(operation.clone());
                    if let Operation::Scale { .. } = operation {
                        let answer = nth(&scale, scales);
                        scales += 1;
                        if let ScaleAnswer::Refused = answer {
                            return Err(ClusterError::new("scale refused"));
                        }
                    }
                    Ok(())
                }
            })
            .apply({
                let applied = recorded.applied.clone();
                let snapshots = recorded.applied_snapshots.clone();
                move |snapshot: &str, mode| {
                    applied.borrow_mut().push(mode);
                    snapshots.borrow_mut().push(snapshot.to_string());
                    Ok(match (apply, mode) {
                        (ApplyAnswer::Conflict, ApplyMode::Normal) => conflict_report(snapshot),
                        _ => ApplyReport::new(Vec::new()),
                    })
                }
            })
            .precheck(move |snapshot: &str| {
                Ok(match precheck {
                    PrecheckAnswer::UnknownField => unknown_field_report(snapshot),
                    PrecheckAnswer::Clean => ApplyReport::new(Vec::new()),
                })
            })
            .logs({
                let logged = recorded.logged.clone();
                move |request: &LogRequest| {
                    logged.borrow_mut().push(request.clone());
                    let mut chunks = vec![Ok(b"starting\n".to_vec())];
                    if let LogsAnswer::Resets = logs {
                        chunks.push(Err(ClusterError::new("stream reset")));
                    }
                    chunks
                }
            })
            .exec({
                let execed = recorded.execed.clone();
                move |request: &ExecRequest, input| {
                    execed.borrow_mut().push((request.clone(), input));
                    let answer = nth(&exec, execs);
                    execs += 1;
                    let mut output = vec![ExecOutput::Bytes(b"/ # ".to_vec())];
                    match answer {
                        ExecAnswer::Exits => output.push(ExecOutput::Exited(Ok(0))),
                        ExecAnswer::Fails => output.push(ExecOutput::Exited(Err(
                            ClusterError::new("container not found (\"app\")"),
                        ))),
                        ExecAnswer::StaysOpen => {}
                    }
                    output
                }
            })
            .port_forward({
                let forwarded = recorded.forwarded.clone();
                move |request: &ForwardRequest| {
                    forwarded.borrow_mut().push(request.clone());
                    let listening = ForwardEvent::Listening(request.port + 10_000);
                    let answer = forward
                        .iter()
                        .find(|(port, _)| *port == request.port)
                        .map(|(_, answer)| *answer);
                    match answer {
                        None => vec![listening],
                        Some(ForwardAnswer::ConnectionFails(message)) => vec![
                            listening,
                            ForwardEvent::ConnectionFailed(ClusterError::new(message)),
                        ],
                        Some(ForwardAnswer::Fails(message)) => {
                            vec![ForwardEvent::Failed(ClusterError::new(message))]
                        }
                    }
                }
            });
        let keeps_open = matches!(self.logs, LogsAnswer::StaysOpen)
            || self.exec.iter().any(|a| matches!(a, ExecAnswer::StaysOpen));
        if keeps_open {
            cluster = cluster.keep_streams_open(open_streams);
        }
        (cluster, recorded)
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

/// Back to the open detail of `name` in the `default` namespace, through
/// `kind`'s list: the row click selects the open tab.
pub fn reopen_detail(ui: &mut Ui<'_>, kind: BuiltinKind, name: &str) {
    ui.frame(|window, cx| window.click(kind.nav_id(), cx));
    ui.settle();
    ui.frame(|window, cx| window.click(format!("row-default/{name}"), cx));
    ui.settle();
}

/// Open the Namespaces list holding `old`, then `old`'s detail tab.
pub fn open_namespace_detail(ui: &mut Ui<'_>) {
    ui.namespaces(&["old"]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();
}

/// Whether the detail tab of Namespace `old` is drawn.
pub fn detail_tab_shown(ui: &mut Ui<'_>) -> bool {
    ui.frame(|window, _| {
        window
            .try_find("detail-tab")
            .is_some_and(|tab| tab.label() == Some("Namespace old"))
    })
}

/// Click `id` in a freshly drawn frame, then run until parked.
pub fn click(cx: &mut VisualTestContext, id: &'static str) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    });
    cx.run_until_parked();
}

/// Whether `id` is drawn in a fresh frame.
pub fn drawn(cx: &mut VisualTestContext, id: &'static str) -> bool {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
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
    /// Runs until parked, past the CRD debounce, and parked again.
    pub fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.executor().advance_clock(CRD_DEBOUNCE);
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
