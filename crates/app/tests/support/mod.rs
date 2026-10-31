//! The workspace UI on a scripted cluster, shared by the built-in kind
//! view tests.
#![allow(dead_code)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astrolabe::builtin::BuiltinKind;
use astrolabe::clock::Clock;
use astrolabe::session::{
    ExecInput, ExecOutput, ExecRequest, ForwardEvent, ForwardRequest, LogRequest, ObjectRef,
    ObjectWatchMessage, Operation, ResourceKind, ScriptedCluster, WatchError, WatchKey,
};
use astrolabe::workspace::{Preselection, WorkspaceView};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext};
use tracing_subscriber::util::SubscriberInitExt as _;

pub type Sender<K> = mpsc::Sender<Result<Event<K>, WatchError>>;

pub fn key(kind: BuiltinKind, namespace: &str) -> WatchKey {
    key_on("kind-a", kind, namespace)
}

pub fn key_on(cluster: &str, kind: BuiltinKind, namespace: &str) -> WatchKey {
    WatchKey {
        cluster: cluster.to_string(),
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
pub type Operated = Rc<RefCell<Vec<Operation>>>;
pub type Logged = Rc<RefCell<Vec<LogRequest>>>;
pub type Forwarded = Rc<RefCell<Vec<ForwardRequest>>>;
pub type Execed = Rc<RefCell<Vec<(ExecRequest, mpsc::UnboundedReceiver<ExecInput>)>>>;

pub struct Ui<'a> {
    pub cx: &'a mut VisualTestContext,
    pub namespaces: Sender<Namespace>,
    pub deleted: Deleted,
    pub operated: Operated,
    pub logged: Logged,
    pub execed: Execed,
    pub forwarded: Forwarded,
}

pub fn open(
    cx: &mut TestAppContext,
    watches: Vec<(WatchKey, mpsc::Receiver<ObjectWatchMessage>)>,
) -> Ui<'_> {
    cx.update(|cx| {
        gpui_kit::init(cx);
        astrolabe::keymap::bind(cx);
    });
    let (namespaces, namespace_events) = mpsc::channel(32);
    let deleted: Deleted = Rc::default();
    let operated: Operated = Rc::default();
    let logged: Logged = Rc::default();
    let execed: Execed = Rc::default();
    let forwarded: Forwarded = Rc::default();
    let mut cluster = ScriptedCluster::new(namespace_events)
        .delete({
            let deleted = deleted.clone();
            move |object: &ObjectRef| {
                let resource = &object.resource;
                deleted.borrow_mut().push((
                    format!("{}/{}", resource.api_version, resource.kind),
                    object.key.namespace.clone(),
                    object.key.name.clone(),
                ));
                Ok(())
            }
        })
        .operate({
            let operated = operated.clone();
            move |operation: &Operation| {
                operated.borrow_mut().push(operation.clone());
                match operation {
                    Operation::Scale { replicas: 0, .. } => Err("scale refused".to_string()),
                    _ => Ok(()),
                }
            }
        })
        .logs({
            let logged = logged.clone();
            move |request: &LogRequest| {
                logged.borrow_mut().push(request.clone());
                vec![Ok(b"starting\n".to_vec()), Err("stream reset".to_string())]
            }
        })
        .exec({
            let execed = execed.clone();
            move |request: &ExecRequest, input| {
                execed.borrow_mut().push((request.clone(), input));
                let exit = match request.shell.as_str() {
                    "bash" => Ok(0),
                    _ => Err("container not found (\"app\")".to_string()),
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
                if request.port == 9901 {
                    events.push(ForwardEvent::ConnectionFailed(
                        "error forwarding port 9901: connection refused".to_string(),
                    ));
                }
                events
            }
        });
    for (key, events) in watches {
        cluster = cluster.watch(key, events);
    }
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        WorkspaceView::from_clusters(
            vec![("kind-a".to_string(), cluster)],
            Preselection::default(),
            window,
            cx,
        )
    });
    Ui {
        cx,
        namespaces,
        deleted,
        operated,
        logged,
        execed,
        forwarded,
    }
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
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(kind.nav_id(), cx));
    snapshot(watch, objects);
    ui.settle();
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

/// Shared buffer behind a `tracing` subscriber, so tests can assert on the
/// log lines a developer would see via RUST_LOG.
#[derive(Clone, Default)]
pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
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
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .with_env_filter("astrolabe=debug,astrolabe_kube=debug,kube_runtime=debug")
            .with_ansi(false)
            .with_writer(self.clone())
            .finish()
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

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
