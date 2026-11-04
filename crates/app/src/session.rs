use std::rc::Rc;
use std::time::Duration;

use astrolabe_kube::apply::{ApplyMode, ApplyReport};
use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::object::Operation;
use astrolabe_kube::pod::{ExecRequest, ExecSession, ForwardRequest, LogRequest, LogStream};
use astrolabe_kube::watch::{NamespaceWatchMessage, WatchMessage};
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::session::{CustomKindId, WatchKey};
use astrolabe_model::session_state::{Command, SessionState};
use futures::future::BoxFuture;
use futures::{FutureExt as _, StreamExt, channel::mpsc};
use gpui_kit::{Context, Task};

use crate::cluster_io::ClusterIo;
pub use crate::cluster_io::ScriptedCluster;
use crate::perf;

/// CRD add/update/delete → rediscovery debounce (#16). Updates count too: a
/// new CRD is served only once it turns Established, which arrives as an
/// update of the same object.
pub const CRD_DEBOUNCE: Duration = Duration::from_millis(400);

/// The I/O of one running forward; dropping it stops the forward.
struct ForwardTask {
    request: ForwardRequest,
    _reports: Task<()>,
}

/// The namespace a session starts in unless told otherwise.
pub const DEFAULT_NAMESPACE: &str = "default";

pub struct Session {
    state: SessionState,
    forward_tasks: Vec<ForwardTask>,
    io: ClusterIo,
}

impl Session {
    /// The session's data, for the model layer's rules.
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Connect to `cluster`, with `namespace` selected from the start: the
    /// first namespaced watch already uses it.
    pub fn new(cluster: ClusterId, namespace: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let io = ClusterIo::connect(&cluster);
        Self::start(cluster, io, namespace.into(), cx)
    }

    /// Namespace events only: CRDs never arrive, discovery finds nothing.
    pub fn from_events(
        cluster: ClusterId,
        receiver: mpsc::Receiver<NamespaceWatchMessage>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::scripted(
            cluster,
            DEFAULT_NAMESPACE,
            ScriptedCluster::new(receiver),
            cx,
        )
    }

    pub fn scripted(
        cluster: ClusterId,
        namespace: impl Into<String>,
        scripted: ScriptedCluster,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::start(cluster, ClusterIo::Scripted(scripted), namespace.into(), cx)
    }

    fn start(cluster: ClusterId, io: ClusterIo, namespace: String, cx: &mut Context<Self>) -> Self {
        let mut session = Self {
            state: SessionState::new(cluster, namespace),
            forward_tasks: Vec::new(),
            io,
        };
        spawn_drain(
            session.io.watch_namespaces(),
            cx,
            |session, message| {
                session.state.namespaces_message(message);
                Vec::new()
            },
            |session| session.state.namespaces_ended(),
        );
        spawn_drain(
            session.io.watch_crds(),
            cx,
            |session, message| session.state.crds_message(message),
            |session| session.state.crds_ended(),
        );
        let commands = session.state.connected();
        session.carry_out(commands, cx);
        session
    }

    /// Carry out what `SessionState` asked for.
    fn carry_out(&mut self, commands: Vec<Command>, cx: &mut Context<Self>) {
        for command in commands {
            match command {
                Command::StartWatch { key, resource } => {
                    let message_key = key.clone();
                    let ended_key = key.clone();
                    spawn_drain(
                        self.io.watch_objects(&key, &resource),
                        cx,
                        move |session, message| {
                            session.state.object_message(&message_key, message);
                            Vec::new()
                        },
                        move |session| session.state.object_ended(&ended_key),
                    );
                    cx.notify();
                }
                Command::ScheduleDiscovery { generation } => {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(CRD_DEBOUNCE).await;
                        this.update(cx, |session, cx| {
                            let commands = session.state.debounce_elapsed(generation);
                            session.carry_out(commands, cx);
                        })
                        .ok();
                    })
                    .detach();
                }
                Command::RunDiscovery { run } => self.run_discovery(run, cx),
                Command::StartForward { request } => self.start_forward(request, cx),
                Command::StopForward { request } => {
                    self.forward_tasks.retain(|task| task.request != request);
                    cx.notify();
                }
            }
        }
    }

    fn run_discovery(&mut self, run: u64, cx: &mut Context<Self>) {
        let mut result = self.io.discover();
        // Scripted clusters answer before `discover` returns: finish within
        // this update, as a live answer's arrival can't be awaited here.
        if let Some(result) = (&mut result).now_or_never() {
            self.finish_discovery(run, result, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = result.await;
            this.update(cx, |session, cx| session.finish_discovery(run, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_discovery(
        &mut self,
        run: u64,
        result: Result<ServedResources, ClusterError>,
        cx: &mut Context<Self>,
    ) {
        let commands = self.state.discovery_finished(run, result);
        self.carry_out(commands, cx);
        cx.notify();
    }

    /// Enter a kind's instance list: starts its watch for the current
    /// namespace on first visit (lazy, then resident for the session).
    /// `None` while the kind is unknown or not served.
    pub fn open_custom_list(
        &mut self,
        id: &CustomKindId,
        cx: &mut Context<Self>,
    ) -> Option<WatchKey> {
        let ensured = self.state.open_custom_list(id)?;
        self.carry_out(ensured.commands, cx);
        Some(ensured.key)
    }

    /// Enter a built-in kind's list: starts its watch for the current
    /// namespace on first visit (lazy, then resident for the session).
    pub fn open_builtin_list(&mut self, kind: BuiltinKind, cx: &mut Context<Self>) -> WatchKey {
        let ensured = self.state.open_builtin_list(kind);
        self.carry_out(ensured.commands, cx);
        ensured.key
    }

    /// Ensure the watch holding `kind`'s objects in `namespace`, whatever
    /// the namespace selection (a detail tab or related section reads it).
    pub fn watch_builtin(
        &mut self,
        kind: BuiltinKind,
        namespace: &str,
        cx: &mut Context<Self>,
    ) -> WatchKey {
        let ensured = self.state.watch_builtin(kind, namespace);
        self.carry_out(ensured.commands, cx);
        ensured.key
    }

    /// Ensure the watch an object of kind `id` in `namespace` (`None` when
    /// cluster-scoped) is read from, at the kind's current preferred
    /// version, whatever the namespace selection (#41). Shares the list's
    /// lazy-start path, so the watch stays resident. `None` while the kind
    /// is unknown or not served.
    pub fn watch_custom_object(
        &mut self,
        id: &CustomKindId,
        namespace: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<WatchKey> {
        let ensured = self.state.watch_custom_object(id, namespace.as_deref())?;
        self.carry_out(ensured.commands, cx);
        Some(ensured.key)
    }

    /// Leave the watched lists (Namespaces or Definitions is shown). A
    /// mutation `state()` cannot expose, and it needs no Cluster command.
    pub fn close_list(&mut self) {
        self.state.close_list();
    }

    pub fn set_namespace(&mut self, namespace: String, cx: &mut Context<Self>) {
        let commands = self.state.set_namespace(namespace);
        self.carry_out(commands, cx);
        cx.notify();
    }

    /// Run one operation; resolves with the server's answer.
    pub fn operate(
        &mut self,
        operation: Operation,
    ) -> BoxFuture<'static, Result<(), ClusterError>> {
        self.io.operate(operation)
    }

    /// Apply every document of `snapshot` to the Cluster; resolves with
    /// each document's outcome.
    pub fn apply(
        &mut self,
        snapshot: &str,
        mode: ApplyMode,
    ) -> BoxFuture<'static, Result<ApplyReport, ClusterError>> {
        self.io.apply(snapshot, mode)
    }

    /// Precheck `snapshot`: a dry run that changes nothing on the Cluster.
    pub fn precheck(
        &mut self,
        snapshot: &str,
    ) -> BoxFuture<'static, Result<ApplyReport, ClusterError>> {
        self.io.precheck(snapshot)
    }

    /// Follow one container's log, from a bounded tail.
    pub fn logs(&mut self, request: LogRequest) -> LogStream {
        self.io.logs(request)
    }

    /// Open an interactive shell in one container.
    pub fn exec(&mut self, request: ExecRequest) -> ExecSession {
        self.io.exec(request)
    }

    /// Start forwarding `request`, unless it already is; a failed forward
    /// starts over.
    pub fn forward_port(&mut self, request: ForwardRequest, cx: &mut Context<Self>) {
        let commands = self.state.forward_port(request);
        self.carry_out(commands, cx);
    }

    pub fn stop_forward(&mut self, request: &ForwardRequest, cx: &mut Context<Self>) {
        let commands = self.state.stop_forward(request);
        self.carry_out(commands, cx);
    }

    fn start_forward(&mut self, request: ForwardRequest, cx: &mut Context<Self>) {
        let mut forward = self.io.port_forward(request.clone());
        let reports = {
            let request = request.clone();
            cx.spawn(async move |this, cx| {
                while let Some(event) = forward.next().await {
                    let applied = this.update(cx, |session, cx| {
                        session.state.forward_reported(&request, event);
                        cx.notify();
                    });
                    if applied.is_err() {
                        return;
                    }
                }
            })
        };
        self.forward_tasks.push(ForwardTask {
            request,
            _reports: reports,
        });
        cx.notify();
    }
}

/// Resident drain loop (#8 bridge): feeds every queued message to the state
/// through `apply`, carries out the commands it answers with, then notifies
/// once; the channel closing calls `ended`.
/// No receiver (a scripted cluster with no script for the watch) starts
/// nothing.
fn spawn_drain<K: 'static>(
    receiver: Option<mpsc::Receiver<WatchMessage<K>>>,
    cx: &mut Context<Session>,
    apply: impl Fn(&mut Session, WatchMessage<K>) -> Vec<Command> + 'static,
    ended: impl Fn(&mut Session) + 'static,
) {
    let Some(mut receiver) = receiver else {
        return;
    };
    let feed = move |session: &mut Session, message, cx: &mut Context<Session>| {
        perf::count(perf::Event::WatchEvent);
        let commands = apply(session, message);
        session.carry_out(commands, cx);
    };
    let ended = Rc::new(ended);
    let closed = ended.clone();
    cx.spawn(async move |this, cx| {
        while let Some(message) = receiver.next().await {
            let mut finished = false;
            if this
                .update(cx, |session, cx| {
                    feed(session, message, cx);
                    loop {
                        match receiver.try_recv() {
                            Ok(message) => feed(session, message, cx),
                            Err(error) if error.is_closed() => {
                                finished = true;
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    if finished {
                        ended(session);
                    }
                    perf::count(perf::Event::Notify);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            if finished {
                return;
            }
        }
        this.update(cx, |session, cx| {
            closed(session);
            cx.notify();
        })
        .ok();
    })
    .detach();
}
