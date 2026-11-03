use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use astrolabe_kube::apply::{ApplyMode, ApplyReport};
use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::ResourceExt;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::Operation;
use astrolabe_kube::pod::{
    ExecRequest, ExecSession, ForwardEvent, ForwardRequest, ForwardTarget, LogRequest, LogStream,
};
use astrolabe_kube::watch::{NamespaceWatchMessage, WatchMessage};
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::catalog::{self, CustomGroup, CustomKind, KindResource};
use astrolabe_model::discovery_cache::DiscoveryCache;
use astrolabe_model::session::{
    ConnectionHealth, CustomKindId, ForwardState, ResourceKind, SnapshotChange, WatchKey, Watched,
};
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

/// The list on screen, whose watch follows the namespace selection.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Viewing {
    Builtin(BuiltinKind),
    Custom(CustomKindId),
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

/// The namespace a session starts in unless told otherwise.
pub const DEFAULT_NAMESPACE: &str = "default";

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
            namespace,
            viewing: None,
            forwards: Vec::new(),
            io,
        };
        spawn_drain(
            session.io.watch_namespaces(),
            cx,
            |session| Some(&mut session.namespaces),
            |_, _, _| {},
        );
        spawn_drain(
            session.io.watch_crds(),
            cx,
            |session| Some(&mut session.crds),
            Self::crds_changed,
        );
        session.run_discovery(cx);
        session
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
        self.catalog = catalog::catalog(&self.crds.objects(), self.discovery.served());
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
        self.ensure_watch(key.clone(), &(kind.registration().resource)(), cx);
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

    /// The watch holding `kind`'s objects in `namespace`, once started.
    pub fn builtin_watch(
        &self,
        kind: BuiltinKind,
        namespace: &str,
    ) -> Option<&Watched<DynamicObject>> {
        self.object_watch(&self.builtin_watch_key(kind, namespace))
    }

    /// The object `name` in `namespace` of `kind`, from its watch.
    pub fn builtin_object(
        &self,
        kind: BuiltinKind,
        namespace: &str,
        name: &str,
    ) -> Option<&DynamicObject> {
        self.builtin_watch(kind, namespace)?
            .get(Some(namespace), name)
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
        self.ensure_watch(key.clone(), &resource, cx);
        Some(key)
    }

    /// Start the object watch `key` on `resource` unless it runs already.
    fn ensure_watch(&mut self, key: WatchKey, resource: &ApiResource, cx: &mut Context<Self>) {
        if self.objects.contains_key(&key) {
            return;
        }
        self.objects.insert(
            key.clone(),
            Watched::new(key.cluster.clone(), resource.kind.clone()),
        );
        spawn_drain(
            self.io.watch_objects(&key, resource),
            cx,
            move |session| session.objects.get_mut(&key),
            |_, _, _| {},
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
            ForwardEvent::ConnectionFailed(error) => {
                forward.last_error = Some(error.to_string());
            }
            ForwardEvent::Failed(error) => forward.state = ForwardState::Failed(error.to_string()),
        }
        cx.notify();
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

    pub fn discovery_failure(&self) -> Option<&ClusterError> {
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
/// closing marks the watch stale. No receiver (a scripted cluster with no
/// script for the watch) starts nothing.
fn spawn_drain<K: ResourceExt + 'static>(
    receiver: Option<mpsc::Receiver<WatchMessage<K>>>,
    cx: &mut Context<Session>,
    watched: impl Fn(&mut Session) -> Option<&mut Watched<K>> + 'static,
    changed: impl Fn(&mut Session, SnapshotChange, &mut Context<Session>) + 'static,
) {
    let Some(mut receiver) = receiver else {
        return;
    };
    let watched = Rc::new(watched);
    let apply_to = watched.clone();
    let apply = move |session: &mut Session, message, cx: &mut Context<Session>| {
        if let Some(watch) = apply_to(session) {
            perf::count(perf::Event::WatchEvent);
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
                    perf::count(perf::Event::Notify);
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
