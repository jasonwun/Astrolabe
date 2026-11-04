//! Session's state and rules (ADR-0002, #164–#166): each watch's `Watched`
//! snapshot and Stale state, the namespace selection, the list being
//! viewed, Discovery, the Catalog and the Forward list's states. Every
//! input answers with the `Command`s Session carries out; Session decides
//! nothing about them.

use std::collections::BTreeMap;

use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::pod::{ForwardEvent, ForwardRequest, ForwardTarget};
use astrolabe_kube::watch::{CrdWatchMessage, NamespaceWatchMessage, ObjectWatchMessage};

use crate::builtin::BuiltinKind;
use crate::catalog::{self, CustomGroup, CustomKind, KindResource};
use crate::discovery_cache::DiscoveryCache;
use crate::session::{
    ConnectionHealth, CustomKindId, ForwardState, ResourceKind, SnapshotChange, WatchKey, Watched,
};

/// What Session must do after an input.
#[derive(Clone, Debug)]
pub enum Command {
    /// Start the watch `key` on `resource`, and feed its messages back to
    /// the state under `key`.
    StartWatch {
        key: WatchKey,
        resource: ApiResource,
    },
    /// Start the debounce timer of `generation` (a Definition changed);
    /// when it elapses, call `SessionState::debounce_elapsed` with it.
    ScheduleDiscovery { generation: u64 },
    /// Run Discovery as `run`, and hand its answer to
    /// `SessionState::discovery_finished` under that number.
    RunDiscovery { run: u64 },
    /// Start forwarding `request`, and hand every report to
    /// `SessionState::forward_reported` under it.
    StartForward { request: ForwardRequest },
    /// Stop the forward of `request`: drop its I/O.
    StopForward { request: ForwardRequest },
}

/// A port-forward the session keeps until stopped, whether or not its
/// target's detail tab is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveForward {
    pub request: ForwardRequest,
    pub state: ForwardState,
    /// The latest connection's failure, while the listener goes on.
    pub last_error: Option<String>,
}

/// A watch an input asked for, and the commands that bring it up.
#[derive(Clone, Debug)]
pub struct Ensured {
    /// Where the watch's objects are read from.
    pub key: WatchKey,
    /// Empty when the watch already runs.
    pub commands: Vec<Command>,
}

/// The list on screen, whose watch follows the namespace selection.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Viewing {
    Builtin(BuiltinKind),
    Custom(CustomKindId),
}

/// The kind `id` names in `catalog`.
pub fn custom_kind<'a>(catalog: &'a [CustomGroup], id: &CustomKindId) -> Option<&'a CustomKind> {
    catalog
        .iter()
        .find(|group| group.group == id.group)?
        .kinds
        .iter()
        .find(|kind| kind.kind == id.kind)
}

pub struct SessionState {
    /// The Namespaces watch's key: the Cluster's identity within the state.
    key: WatchKey,
    namespaces: Watched<Namespace>,
    crds: Watched<CustomResourceDefinition>,
    objects: BTreeMap<WatchKey, Watched<DynamicObject>>,
    namespace: String,
    viewing: Option<Viewing>,
    discovery: DiscoveryCache,
    catalog: Vec<CustomGroup>,
    /// Bumped by every Definition change that needs a rediscovery; a
    /// debounce timer counts only if no later change superseded it.
    generation: u64,
    /// Bumped by every Discovery run; only the newest run's answer is kept.
    runs: u64,
    /// The Forward list, in the order the forwards started.
    forwards: Vec<ActiveForward>,
}

impl SessionState {
    /// A Cluster's state with `namespace` selected from the start: the first
    /// namespaced watch already uses it.
    pub fn new(cluster: ClusterId, namespace: impl Into<String>) -> Self {
        Self {
            namespaces: Watched::new(cluster.clone(), "namespace"),
            crds: Watched::new(cluster.clone(), "CRD"),
            key: WatchKey {
                cluster,
                kind: ResourceKind::Namespaces,
                namespace: None,
            },
            objects: BTreeMap::new(),
            namespace: namespace.into(),
            viewing: None,
            discovery: DiscoveryCache::default(),
            catalog: Vec::new(),
            generation: 0,
            runs: 0,
            forwards: Vec::new(),
        }
    }

    // Inputs

    /// A message of the Namespaces watch.
    pub fn namespaces_message(&mut self, message: NamespaceWatchMessage) -> SnapshotChange {
        self.namespaces.apply(message)
    }

    /// The Namespaces watch ended: its snapshot stays, flagged Stale.
    pub fn namespaces_ended(&mut self) {
        self.namespaces.mark_stale();
    }

    /// A message of the Definitions watch: the Catalog follows it, and a
    /// change that Discovery may not have seen schedules a rediscovery.
    pub fn crds_message(&mut self, message: CrdWatchMessage) -> Vec<Command> {
        let change = self.crds.apply(message);
        let mut commands = self.rebuild_catalog();
        // Connect-time Discovery races the first Definition list: a
        // Definition created in between would stay "not served" until the
        // next change.
        let unresolved = change == SnapshotChange::FirstSnapshot
            && self.catalog.iter().any(|group| {
                group
                    .kinds
                    .iter()
                    .any(|kind| kind.resource == KindResource::NotServed)
            });
        // A relist after a disconnect may hide missed changes; the first
        // snapshot does not, connect-time Discovery covers it.
        if change == SnapshotChange::Changed || unresolved {
            self.generation += 1;
            commands.push(Command::ScheduleDiscovery {
                generation: self.generation,
            });
        }
        commands
    }

    /// The Definitions watch ended: its snapshot stays, flagged Stale.
    pub fn crds_ended(&mut self) {
        self.crds.mark_stale();
    }

    /// A message of the object watch `key`; a key nobody started changes
    /// nothing.
    pub fn object_message(
        &mut self,
        key: &WatchKey,
        message: ObjectWatchMessage,
    ) -> SnapshotChange {
        self.objects
            .get_mut(key)
            .map_or(SnapshotChange::Unchanged, |watched| watched.apply(message))
    }

    /// The object watch `key` ended: its snapshot stays, flagged Stale.
    pub fn object_ended(&mut self, key: &WatchKey) {
        if let Some(watched) = self.objects.get_mut(key) {
            watched.mark_stale();
        }
    }

    /// Connected: run the first Discovery.
    pub fn connected(&mut self) -> Vec<Command> {
        vec![self.start_run()]
    }

    /// The debounce timer of `generation` elapsed: Discovery runs unless a
    /// later Definition change superseded the timer.
    pub fn debounce_elapsed(&mut self, generation: u64) -> Vec<Command> {
        if generation == self.generation {
            vec![self.start_run()]
        } else {
            Vec::new()
        }
    }

    /// Discovery run `run` answered. A newer run in flight or done wins, so
    /// an older answer changes nothing; a failure keeps the last result.
    pub fn discovery_finished(
        &mut self,
        run: u64,
        result: Result<ServedResources, ClusterError>,
    ) -> Vec<Command> {
        if run != self.runs {
            return Vec::new();
        }
        if let Err(error) = &result {
            tracing::warn!(cluster = %self.cluster(), error = %error, "discovery failed");
        }
        self.discovery.record(result);
        self.rebuild_catalog()
    }

    /// Enter a built-in kind's list: its watch for the current namespace
    /// starts on first visit (lazy, then resident for the session).
    pub fn open_builtin_list(&mut self, kind: BuiltinKind) -> Ensured {
        self.viewing = Some(Viewing::Builtin(kind));
        let namespace = self.namespace.clone();
        self.watch_builtin(kind, &namespace)
    }

    /// Enter a custom kind's instance list, as the Catalog resolves it. `None`
    /// while the kind is unknown or not served.
    pub fn open_custom_list(&mut self, id: &CustomKindId) -> Option<Ensured> {
        self.viewing = Some(Viewing::Custom(id.clone()));
        let namespace = self.namespace.clone();
        self.watch_custom_object(id, Some(&namespace))
    }

    /// Leave the watched lists (Namespaces or Definitions is shown).
    pub fn close_list(&mut self) {
        self.viewing = None;
    }

    /// The watch holding `kind`'s objects in `namespace`, whatever the
    /// namespace selection (a detail tab or related section reads it).
    pub fn watch_builtin(&mut self, kind: BuiltinKind, namespace: &str) -> Ensured {
        let key = self.builtin_watch_key(kind, namespace);
        let resource = (kind.registration().resource)();
        self.ensure(key, &resource)
    }

    /// The watch an object of kind `id` in `namespace` (`None` when
    /// cluster-scoped) is read from, at the kind's current preferred
    /// version, whatever the namespace selection (#41). `None` while the
    /// kind is unknown or not served.
    pub fn watch_custom_object(
        &mut self,
        id: &CustomKindId,
        namespace: Option<&str>,
    ) -> Option<Ensured> {
        let kind = custom_kind(&self.catalog, id)?;
        let KindResource::Served(resource) = &kind.resource else {
            return None;
        };
        let resource = resource.clone();
        let key = self.custom_object_watch_key(kind, namespace);
        Some(self.ensure(key, &resource))
    }

    /// Select `namespace`; the viewed list's watch follows it.
    pub fn set_namespace(&mut self, namespace: String) -> Vec<Command> {
        if self.namespace == namespace {
            return Vec::new();
        }
        self.namespace = namespace;
        self.follow_viewed_list()
    }

    /// Forward `request`, unless it already is; a failed forward starts
    /// over, at the end of the list.
    pub fn forward_port(&mut self, request: ForwardRequest) -> Vec<Command> {
        let mut commands = Vec::new();
        if let Some(index) = self.forward_index(&request) {
            if !matches!(self.forwards[index].state, ForwardState::Failed(_)) {
                return Vec::new();
            }
            self.forwards.remove(index);
            commands.push(Command::StopForward {
                request: request.clone(),
            });
        }
        self.forwards.push(ActiveForward {
            request: request.clone(),
            state: ForwardState::Starting,
            last_error: None,
        });
        commands.push(Command::StartForward { request });
        commands
    }

    /// Stop the forward of `request`; one not in the list changes nothing.
    pub fn stop_forward(&mut self, request: &ForwardRequest) -> Vec<Command> {
        let Some(index) = self.forward_index(request) else {
            return Vec::new();
        };
        self.forwards.remove(index);
        vec![Command::StopForward {
            request: request.clone(),
        }]
    }

    /// A report of the forward of `request`; one no longer in the list
    /// changes nothing.
    pub fn forward_reported(&mut self, request: &ForwardRequest, event: ForwardEvent) {
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
    }

    // Queries

    /// The forwards to one Pod or Service, in the order they started.
    pub fn forwards<'a>(
        &'a self,
        target: &'a ForwardTarget,
    ) -> impl Iterator<Item = &'a ActiveForward> {
        self.forwards
            .iter()
            .filter(move |forward| forward.request.target == *target)
    }

    /// The Namespaces watch's key; it names the Cluster.
    pub fn watch_key(&self) -> &WatchKey {
        &self.key
    }

    /// The Cluster this state belongs to.
    pub fn cluster(&self) -> &ClusterId {
        &self.key.cluster
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
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
        self.crds.status().is_forbidden()
    }

    /// The custom kinds by API group, as Custom Resources navigation shows
    /// them.
    pub fn catalog(&self) -> &[CustomGroup] {
        &self.catalog
    }

    /// The kind `id` names in the Catalog.
    pub fn custom_kind(&self, id: &CustomKindId) -> Option<&CustomKind> {
        custom_kind(&self.catalog, id)
    }

    /// What Discovery last found served; empty before its first success.
    #[must_use]
    pub fn served_resources(&self) -> ServedResources {
        self.discovery.served().cloned().unwrap_or_default()
    }

    /// The failure of the latest Discovery refresh, if it failed; the old
    /// result stays in use.
    pub fn discovery_failure(&self) -> Option<&ClusterError> {
        self.discovery.failure()
    }

    /// The disconnect banner: the Namespaces watch lost its connection, or
    /// the latest Discovery refresh failed.
    pub fn show_stale_warning(&self) -> bool {
        self.namespaces.status().is_stale() || self.discovery.failure().is_some()
    }

    pub fn health(&self) -> ConnectionHealth {
        self.namespaces.status().health()
    }

    /// The Namespaces watch lost its connection and shows its old snapshot.
    pub fn namespaces_stale(&self) -> bool {
        self.namespaces.status().is_stale()
    }

    pub fn object_watch(&self, key: &WatchKey) -> Option<&Watched<DynamicObject>> {
        self.objects.get(key)
    }

    /// The watch holding `kind`'s objects in `namespace` (every registered
    /// built-in kind is namespaced).
    pub fn builtin_watch_key(&self, kind: BuiltinKind, namespace: &str) -> WatchKey {
        WatchKey {
            cluster: self.key.cluster.clone(),
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

    /// The watch a kind's list reads for the current namespace selection:
    /// namespaced kinds follow it, cluster-scoped kinds ignore it.
    pub fn custom_watch_key(&self, kind: &CustomKind) -> WatchKey {
        self.custom_object_watch_key(kind, Some(&self.namespace))
    }

    /// The watch holding `kind`'s objects in `namespace`, at its preferred
    /// version; cluster-scoped kinds ignore `namespace`.
    pub fn custom_object_watch_key(&self, kind: &CustomKind, namespace: Option<&str>) -> WatchKey {
        WatchKey {
            cluster: self.key.cluster.clone(),
            kind: ResourceKind::Custom {
                group: kind.group.clone(),
                version: kind.version.clone(),
                kind: kind.kind.clone(),
            },
            namespace: namespace.filter(|_| kind.namespaced).map(str::to_string),
        }
    }

    fn forward_index(&self, request: &ForwardRequest) -> Option<usize> {
        self.forwards
            .iter()
            .position(|forward| forward.request == *request)
    }

    fn start_run(&mut self) -> Command {
        self.runs += 1;
        Command::RunDiscovery { run: self.runs }
    }

    /// Rebuild the Catalog from the Definitions and the last Discovery; a
    /// viewed custom kind may have become served, or moved to a new
    /// preferred version.
    fn rebuild_catalog(&mut self) -> Vec<Command> {
        self.catalog = catalog::catalog(&self.crds.objects(), self.discovery.served());
        if matches!(self.viewing, Some(Viewing::Custom(_))) {
            self.follow_viewed_list()
        } else {
            Vec::new()
        }
    }

    /// Start the object watch `key` on `resource` unless it runs already.
    fn ensure(&mut self, key: WatchKey, resource: &ApiResource) -> Ensured {
        let mut commands = Vec::new();
        if !self.objects.contains_key(&key) {
            self.objects.insert(
                key.clone(),
                Watched::new(key.cluster.clone(), resource.kind.clone()),
            );
            commands.push(Command::StartWatch {
                key: key.clone(),
                resource: resource.clone(),
            });
        }
        Ensured { key, commands }
    }

    fn follow_viewed_list(&mut self) -> Vec<Command> {
        let namespace = self.namespace.clone();
        let ensured = match self.viewing.clone() {
            Some(Viewing::Builtin(kind)) => Some(self.watch_builtin(kind, &namespace)),
            Some(Viewing::Custom(id)) => self.watch_custom_object(&id, Some(&namespace)),
            None => None,
        };
        ensured.map(|ensured| ensured.commands).unwrap_or_default()
    }
}
