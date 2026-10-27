use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use astrolabe_kube::crd::{self, CustomGroup, CustomKind, KindResource};
use astrolabe_kube::discovery::{DiscoveryCache, ServedResources};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::{self, Event};
use astrolabe_kube::kube::{Client, ResourceExt};
use futures::{Stream, StreamExt, channel::mpsc};
use gpui_kit::{Context, Task};
use tokio::sync::oneshot;

use crate::builtin::BuiltinKind;
use crate::cluster_io::ClusterIo;
pub use crate::cluster_io::{
    ExecOutput, ExecSession, ForwardEvent, LogChunk, LogStream, PortForward, ScriptedCluster,
    SharedClient,
};
use crate::store::ObjectKey;
pub use astrolabe_kube::pod::ExecInput;

/// CRD add/update/delete → rediscovery debounce (#16). Updates count too: a
/// new CRD is served only once it turns Established, which arrives as an
/// update of the same object.
pub const CRD_DEBOUNCE: Duration = Duration::from_millis(400);

/// A failed watch, as the session sees it. `forbidden` marks an RBAC
/// denial (HTTP 403), which some views present as an empty state instead
/// of a connection problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchError {
    pub message: String,
    pub forbidden: bool,
}

impl WatchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            forbidden: false,
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            forbidden: true,
        }
    }

    pub(crate) fn from_watcher(error: &watcher::Error) -> Self {
        Self {
            message: error.to_string(),
            forbidden: astrolabe_kube::watch::is_forbidden(error),
        }
    }
}

pub type WatchMessage<K> = Result<Event<K>, WatchError>;
pub type NamespaceWatchMessage = WatchMessage<Namespace>;
pub type CrdWatchMessage = WatchMessage<CustomResourceDefinition>;
pub type ObjectWatchMessage = WatchMessage<DynamicObject>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKind {
    Namespaces,
    CustomResourceDefinitions,
    /// A built-in kind with a registered view (#17).
    Builtin(BuiltinKind),
    /// A CRD kind at its preferred version.
    Custom {
        group: String,
        version: String,
        kind: String,
    },
}

/// One watcher: `namespace: None` is a cluster-scoped watch (#16 extends
/// #8's `(cluster, kind, ns)` key to `(cluster, kind, Option<ns>)`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WatchKey {
    pub cluster: String,
    pub kind: ResourceKind,
    pub namespace: Option<String>,
}

/// A CRD kind as navigation addresses it: version-free, so a preferred
/// version change keeps the same nav entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CustomKindId {
    pub group: String,
    pub kind: String,
}

impl CustomKindId {
    pub fn of(kind: &CustomKind) -> Self {
        Self {
            group: kind.group.clone(),
            kind: kind.kind.clone(),
        }
    }
}

/// The list on screen, whose watch follows the namespace selection.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Viewing {
    Builtin(BuiltinKind),
    Custom(CustomKindId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionHealth {
    Connecting,
    Healthy,
    Stale,
}

/// What one watch message did to the watcher's object set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotChange {
    Unchanged,
    /// The first complete list arrived.
    FirstSnapshot,
    /// An object changed, or a relist replaced the snapshot (it may hide
    /// changes missed while disconnected).
    Changed,
}

/// One watcher's snapshot plus its connection state: old data stays visible
/// (stale) through a relist until the complete replacement arrives.
pub struct Watched<K> {
    cluster: String,
    label: String,
    /// The snapshot readers see.
    committed: BTreeMap<ObjectKey, K>,
    /// A relist in progress, swapped in whole on `InitDone`.
    staging: BTreeMap<ObjectKey, K>,
    health: ConnectionHealth,
    has_snapshot: bool,
    stale: bool,
    forbidden: bool,
}

impl<K: ResourceExt> Watched<K> {
    /// `label` names the watched resource in logs.
    pub fn new(cluster: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            cluster: cluster.into(),
            label: label.into(),
            committed: BTreeMap::new(),
            staging: BTreeMap::new(),
            health: ConnectionHealth::Connecting,
            has_snapshot: false,
            stale: false,
            forbidden: false,
        }
    }

    fn set_health(&mut self, health: ConnectionHealth) {
        if self.health != health {
            tracing::info!(
                cluster = %self.cluster,
                from = ?self.health,
                to = ?health,
                "{} session health changed",
                self.label
            );
            self.health = health;
        }
    }

    fn mark_healthy(&mut self) {
        self.has_snapshot = true;
        self.forbidden = false;
        self.set_health(ConnectionHealth::Healthy);
        self.stale = false;
    }

    fn mark_stale(&mut self) {
        self.set_health(ConnectionHealth::Stale);
        self.stale = self.has_snapshot;
    }

    /// The single place a watch message is interpreted.
    pub fn apply(&mut self, message: WatchMessage<K>) -> SnapshotChange {
        match message {
            Ok(Event::Init) => {
                self.set_health(ConnectionHealth::Connecting);
                self.stale = self.has_snapshot;
                self.staging.clear();
                SnapshotChange::Unchanged
            }
            Ok(Event::InitApply(object)) => {
                self.staging.insert(ObjectKey::of(&object), object);
                SnapshotChange::Unchanged
            }
            Ok(Event::InitDone) => {
                let change = if self.has_snapshot {
                    SnapshotChange::Changed
                } else {
                    SnapshotChange::FirstSnapshot
                };
                self.mark_healthy();
                self.committed = std::mem::take(&mut self.staging);
                tracing::info!(
                    cluster = %self.cluster,
                    objects = self.committed.len(),
                    "{} snapshot replaced",
                    self.label
                );
                change
            }
            Ok(Event::Apply(object)) => {
                self.mark_healthy();
                self.committed.insert(ObjectKey::of(&object), object);
                SnapshotChange::Changed
            }
            Ok(Event::Delete(object)) => {
                self.mark_healthy();
                self.committed.remove(&ObjectKey::of(&object));
                SnapshotChange::Changed
            }
            Err(error) => {
                self.forbidden = error.forbidden;
                self.mark_stale();
                SnapshotChange::Unchanged
            }
        }
    }

    pub fn objects(&self) -> Vec<&K> {
        self.committed.values().collect()
    }

    pub fn get(&self, namespace: Option<&str>, name: &str) -> Option<&K> {
        self.committed
            .get(&ObjectKey::new(namespace.map(str::to_string), name))
    }

    pub fn health(&self) -> ConnectionHealth {
        self.health
    }

    pub fn has_snapshot(&self) -> bool {
        self.has_snapshot
    }

    pub fn show_stale_warning(&self) -> bool {
        self.stale
    }

    /// The latest failure was an RBAC denial.
    pub fn forbidden(&self) -> bool {
        self.forbidden
    }
}

/// One object of one resource: what a delete addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRef {
    pub resource: ApiResource,
    pub key: ObjectKey,
}

/// A one-shot change to a registered workload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Scale { object: ObjectRef, replicas: i32 },
    RolloutRestart { object: ObjectRef },
    SetSuspend { object: ObjectRef, suspend: bool },
    TriggerCronJob { object: ObjectRef },
}

/// Follow `container`'s log in Pod `pod` (#17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRequest {
    pub pod: ObjectKey,
    pub container: String,
}

/// Run `shell` interactively in `container` of Pod `pod` (#17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    pub pod: ObjectKey,
    pub container: String,
    pub shell: String,
}

/// What a port-forward reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardTarget {
    /// A container port of the Pod (#17).
    Pod(ObjectKey),
    /// A service port of the Service, reaching one of its Pods (#18).
    Service(ObjectKey),
}

/// Forward a localhost port to `port` of `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardRequest {
    pub target: ForwardTarget,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardState {
    Starting,
    /// Accepting on this localhost port.
    Listening(u16),
    Failed(String),
}

/// A port-forward the session keeps until stopped, whether or not its
/// target's detail tab is open.
pub struct Forward {
    pub request: ForwardRequest,
    pub state: ForwardState,
    /// The latest connection's failure, while the listener goes on.
    pub last_error: Option<String>,
    /// Owns the I/O; dropping it stops the forward.
    _reports: Task<()>,
}

pub struct Session {
    watch_key: WatchKey,
    namespaces: Watched<Namespace>,
    crds: Watched<CustomResourceDefinition>,
    discovery: DiscoveryCache,
    /// Bumped by every CRD change; a debounce timer fires only if no
    /// later change superseded it.
    crd_generation: u64,
    discovery_runs: u64,
    catalog: Vec<CustomGroup>,
    objects: BTreeMap<WatchKey, Watched<DynamicObject>>,
    namespace: String,
    viewing: Option<Viewing>,
    forwards: Vec<Forward>,
    io: ClusterIo,
}

impl Session {
    pub fn new(cluster: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let cluster = cluster.into();
        let io = ClusterIo::connect(cluster.clone());
        Self::start(cluster, io, cx)
    }

    /// Namespace events only: CRDs never arrive, discovery finds nothing.
    pub fn from_events(
        cluster: impl Into<String>,
        receiver: mpsc::Receiver<NamespaceWatchMessage>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::scripted(cluster, ScriptedCluster::new(receiver), cx)
    }

    pub fn scripted(
        cluster: impl Into<String>,
        scripted: ScriptedCluster,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::start(cluster.into(), ClusterIo::Scripted(scripted), cx)
    }

    fn start(cluster: String, io: ClusterIo, cx: &mut Context<Self>) -> Self {
        let mut session = Self {
            namespaces: Watched::new(cluster.clone(), "namespace"),
            crds: Watched::new(cluster.clone(), "CRD"),
            watch_key: WatchKey {
                cluster,
                kind: ResourceKind::Namespaces,
                namespace: None,
            },
            discovery: DiscoveryCache::default(),
            crd_generation: 0,
            discovery_runs: 0,
            catalog: Vec::new(),
            objects: BTreeMap::new(),
            namespace: "default".to_string(),
            viewing: None,
            forwards: Vec::new(),
            io,
        };
        session.start_watch(
            "namespace",
            |scripted| scripted.namespaces.take(),
            astrolabe_kube::watch::namespaces,
            |session| Some(&mut session.namespaces),
            |_, _, _| {},
            cx,
        );
        session.start_watch(
            "CRD",
            |scripted| scripted.crds.take(),
            astrolabe_kube::watch::custom_resource_definitions,
            |session| Some(&mut session.crds),
            Self::crds_changed,
            cx,
        );
        session.run_discovery(cx);
        session
    }

    /// Start a resident watch through the I/O seam and drain it into the
    /// `Watched` that `watched` selects; `changed` runs after each message.
    /// No-op when a scripted cluster scripts no events for it.
    fn start_watch<K, S>(
        &mut self,
        label: &'static str,
        scripted: impl FnOnce(&mut ScriptedCluster) -> Option<mpsc::Receiver<WatchMessage<K>>>,
        stream: impl Fn(&Client) -> S + Send + 'static,
        watched: impl Fn(&mut Self) -> Option<&mut Watched<K>> + 'static,
        changed: impl Fn(&mut Self, SnapshotChange, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) where
        K: ResourceExt + Send + 'static,
        S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
    {
        if let Some(events) = self.io.watch(label, scripted, stream) {
            spawn_drain(events, cx, watched, changed);
        }
    }

    fn crds_changed(&mut self, change: SnapshotChange, cx: &mut Context<Self>) {
        self.rebuild_catalog(cx);
        // Connect-time discovery races the first CRD list: a CRD created in
        // between would stay "not served" until the next CRD change.
        let unresolved = change == SnapshotChange::FirstSnapshot
            && self.catalog.iter().any(|group| {
                group
                    .kinds
                    .iter()
                    .any(|kind| kind.resource == KindResource::NotServed)
            });
        // A relist after a disconnect may hide missed changes; the first
        // snapshot does not, connect-time discovery covers it.
        if change == SnapshotChange::Changed || unresolved {
            self.schedule_discovery(cx);
        }
    }

    fn schedule_discovery(&mut self, cx: &mut Context<Self>) {
        self.crd_generation += 1;
        let generation = self.crd_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CRD_DEBOUNCE).await;
            this.update(cx, |session, cx| {
                if session.crd_generation == generation {
                    session.run_discovery(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_discovery(&mut self, cx: &mut Context<Self>) {
        self.discovery_runs += 1;
        let run = self.discovery_runs;
        let mut result = self.io.discover();
        // Scripted clusters answer before `discover` returns: finish within
        // this update, as a live answer's arrival can't be awaited here.
        if let Ok(result) = result.try_recv() {
            self.finish_discovery(run, result, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = result
                .await
                .unwrap_or_else(|_| Err("discovery task dropped".to_string()));
            this.update(cx, |session, cx| session.finish_discovery(run, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_discovery(
        &mut self,
        run: u64,
        result: Result<ServedResources, String>,
        cx: &mut Context<Self>,
    ) {
        // A newer run is in flight or done; its answer wins.
        if run != self.discovery_runs {
            return;
        }
        if let Err(error) = &result {
            tracing::warn!(cluster = %self.watch_key.cluster, error = %error, "discovery failed");
        }
        self.discovery.record(result);
        self.rebuild_catalog(cx);
        cx.notify();
    }

    fn rebuild_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog = crd::catalog(&self.crds.objects(), self.discovery.served());
        if let Some(Viewing::Custom(id)) = self.viewing.clone() {
            self.ensure_custom_watch(&id, Some(self.namespace.clone()), cx);
        }
    }

    /// Enter a kind's instance list: starts its watch for the current
    /// namespace on first visit (lazy, then resident for the session).
    /// `None` while the kind is unknown or not served.
    pub fn open_custom_list(
        &mut self,
        id: &CustomKindId,
        cx: &mut Context<Self>,
    ) -> Option<WatchKey> {
        self.viewing = Some(Viewing::Custom(id.clone()));
        self.ensure_custom_watch(id, Some(self.namespace.clone()), cx)
    }

    /// Enter a built-in kind's list: starts its watch for the current
    /// namespace on first visit (lazy, then resident for the session).
    pub fn open_builtin_list(&mut self, kind: BuiltinKind, cx: &mut Context<Self>) -> WatchKey {
        self.viewing = Some(Viewing::Builtin(kind));
        self.watch_builtin(kind, &self.namespace.clone(), cx)
    }

    /// Ensure the watch holding `kind`'s objects in `namespace`, whatever
    /// the namespace selection (a detail tab or related section reads it).
    pub fn watch_builtin(
        &mut self,
        kind: BuiltinKind,
        namespace: &str,
        cx: &mut Context<Self>,
    ) -> WatchKey {
        let key = self.builtin_watch_key(kind, namespace);
        self.ensure_watch(key.clone(), (kind.registration().resource)(), cx);
        key
    }

    /// The watch holding `kind`'s objects in `namespace` (every registered
    /// built-in kind is namespaced).
    pub fn builtin_watch_key(&self, kind: BuiltinKind, namespace: &str) -> WatchKey {
        WatchKey {
            cluster: self.watch_key.cluster.clone(),
            kind: ResourceKind::Builtin(kind),
            namespace: Some(namespace.to_string()),
        }
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
        self.ensure_custom_watch(id, namespace, cx)
    }

    /// Leave the watched lists (Namespaces or Definitions is shown).
    pub fn close_list(&mut self) {
        self.viewing = None;
    }

    fn ensure_custom_watch(
        &mut self,
        id: &CustomKindId,
        namespace: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<WatchKey> {
        let kind = self.custom_kind(id)?;
        let KindResource::Served(resource) = &kind.resource else {
            return None;
        };
        let resource = resource.clone();
        let key = self.custom_object_watch_key(kind, namespace.as_deref());
        self.ensure_watch(key.clone(), resource, cx);
        Some(key)
    }

    /// Start the object watch `key` on `resource` unless it runs already.
    fn ensure_watch(&mut self, key: WatchKey, resource: ApiResource, cx: &mut Context<Self>) {
        if self.objects.contains_key(&key) {
            return;
        }
        self.objects.insert(
            key.clone(),
            Watched::new(key.cluster.clone(), resource.kind.clone()),
        );
        let scripted_key = key.clone();
        let watched_key = key.clone();
        let namespace = key.namespace.clone();
        self.start_watch(
            "object",
            move |scripted| scripted.watches.remove(&scripted_key),
            move |client| astrolabe_kube::watch::objects(client, &resource, namespace.as_deref()),
            move |session| session.objects.get_mut(&watched_key),
            |_, _, _| {},
            cx,
        );
        cx.notify();
    }

    /// The watch a kind's list reads for the current namespace selection:
    /// namespaced kinds follow it, cluster-scoped kinds ignore it.
    pub fn custom_watch_key(&self, kind: &CustomKind) -> WatchKey {
        self.custom_object_watch_key(kind, Some(&self.namespace))
    }

    /// The watch holding `kind`'s objects in `namespace`, at its preferred
    /// version; cluster-scoped kinds ignore `namespace`.
    pub fn custom_object_watch_key(&self, kind: &CustomKind, namespace: Option<&str>) -> WatchKey {
        WatchKey {
            cluster: self.watch_key.cluster.clone(),
            kind: ResourceKind::Custom {
                group: kind.group.clone(),
                version: kind.version.clone(),
                kind: kind.kind.clone(),
            },
            namespace: namespace.filter(|_| kind.namespaced).map(str::to_string),
        }
    }

    pub fn set_namespace(&mut self, namespace: String, cx: &mut Context<Self>) {
        if self.namespace == namespace {
            return;
        }
        self.namespace = namespace;
        match self.viewing.clone() {
            Some(Viewing::Custom(id)) => {
                self.ensure_custom_watch(&id, Some(self.namespace.clone()), cx);
            }
            Some(Viewing::Builtin(kind)) => {
                self.watch_builtin(kind, &self.namespace.clone(), cx);
            }
            None => {}
        }
        cx.notify();
    }

    /// Delete one object; resolves with the server's answer.
    pub fn delete(&mut self, object: ObjectRef) -> oneshot::Receiver<Result<(), String>> {
        self.io.delete(object)
    }

    /// Run one operation; resolves with the server's answer.
    pub fn operate(&mut self, operation: Operation) -> oneshot::Receiver<Result<(), String>> {
        self.io.operate(operation)
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
        if let Some(index) = self.forward_index(&request) {
            if !matches!(self.forwards[index].state, ForwardState::Failed(_)) {
                return;
            }
            self.forwards.remove(index);
        }
        let mut forward = self.io.port_forward(request.clone());
        let reports = {
            let request = request.clone();
            cx.spawn(async move |this, cx| {
                while let Some(event) = forward.next().await {
                    let applied = this.update(cx, |session, cx| {
                        session.forward_reported(&request, event, cx)
                    });
                    if applied.is_err() {
                        return;
                    }
                }
            })
        };
        self.forwards.push(Forward {
            request,
            state: ForwardState::Starting,
            last_error: None,
            _reports: reports,
        });
        cx.notify();
    }

    pub fn stop_forward(&mut self, request: &ForwardRequest, cx: &mut Context<Self>) {
        if let Some(index) = self.forward_index(request) {
            self.forwards.remove(index);
            cx.notify();
        }
    }

    /// The forwards to one Pod or Service, in the order they started.
    pub fn forwards<'a>(&'a self, target: &'a ForwardTarget) -> impl Iterator<Item = &'a Forward> {
        self.forwards
            .iter()
            .filter(move |forward| forward.request.target == *target)
    }

    fn forward_index(&self, request: &ForwardRequest) -> Option<usize> {
        self.forwards
            .iter()
            .position(|forward| forward.request == *request)
    }

    fn forward_reported(
        &mut self,
        request: &ForwardRequest,
        event: ForwardEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.forward_index(request) else {
            return;
        };
        let forward = &mut self.forwards[index];
        match event {
            ForwardEvent::Listening(port) => forward.state = ForwardState::Listening(port),
            ForwardEvent::ConnectionFailed(error) => forward.last_error = Some(error),
            ForwardEvent::Failed(error) => forward.state = ForwardState::Failed(error),
        }
        cx.notify();
    }

    /// The live session's shared client; `None` for scripted sessions.
    pub fn client(&self) -> Option<SharedClient> {
        self.io.client()
    }

    pub fn watch_key(&self) -> &WatchKey {
        &self.watch_key
    }

    pub fn namespaces(&self) -> Vec<&Namespace> {
        self.namespaces.objects()
    }

    pub fn namespace_object(&self, name: &str) -> Option<&Namespace> {
        self.namespaces.get(None, name)
    }

    pub fn crds(&self) -> &Watched<CustomResourceDefinition> {
        &self.crds
    }

    /// RBAC denies listing CRDs: Custom Resources shows an empty-state hint
    /// (no heuristic reconstruction of kinds, #11).
    pub fn crd_access_denied(&self) -> bool {
        self.crds.forbidden()
    }

    pub fn catalog(&self) -> &[CustomGroup] {
        &self.catalog
    }

    pub fn custom_kind(&self, id: &CustomKindId) -> Option<&CustomKind> {
        self.catalog
            .iter()
            .find(|group| group.group == id.group)?
            .kinds
            .iter()
            .find(|kind| kind.kind == id.kind)
    }

    pub fn object_watch(&self, key: &WatchKey) -> Option<&Watched<DynamicObject>> {
        self.objects.get(key)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn discovery_failure(&self) -> Option<&str> {
        self.discovery.failure()
    }

    pub fn health(&self) -> ConnectionHealth {
        self.namespaces.health()
    }

    /// The disconnect banner: the namespace watch lost its connection, or
    /// the latest discovery refresh failed (old results stay in use).
    pub fn show_stale_warning(&self) -> bool {
        self.namespaces.show_stale_warning() || self.discovery.failure().is_some()
    }
}

/// Resident drain loop (#8 bridge): applies every queued message to the
/// `Watched` that `watched` selects, then notifies once; the channel
/// closing marks the watch stale.
fn spawn_drain<K: ResourceExt + 'static>(
    mut receiver: mpsc::Receiver<WatchMessage<K>>,
    cx: &mut Context<Session>,
    watched: impl Fn(&mut Session) -> Option<&mut Watched<K>> + 'static,
    changed: impl Fn(&mut Session, SnapshotChange, &mut Context<Session>) + 'static,
) {
    let watched = Rc::new(watched);
    let apply_to = watched.clone();
    let apply = move |session: &mut Session, message, cx: &mut Context<Session>| {
        if let Some(watch) = apply_to(session) {
            let change = watch.apply(message);
            changed(session, change, cx);
        }
    };
    let closed = move |session: &mut Session| {
        if let Some(watch) = watched(session) {
            watch.mark_stale();
        }
    };
    cx.spawn(async move |this, cx| {
        while let Some(message) = receiver.next().await {
            let mut ended = false;
            if this
                .update(cx, |session, cx| {
                    apply(session, message, cx);
                    loop {
                        match receiver.try_recv() {
                            Ok(message) => apply(session, message, cx),
                            Err(error) if error.is_closed() => {
                                ended = true;
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    if ended {
                        closed(session);
                    }
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            if ended {
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
