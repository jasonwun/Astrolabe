use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use astrolabe_kube::crd::{self, CustomGroup, CustomKind, KindResource};
use astrolabe_kube::discovery::{DiscoveryCache, ServedResources};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::{self, Event};
use astrolabe_kube::kube::{Client, ResourceExt};
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt as _, SinkExt, Stream, StreamExt, channel::mpsc};
use gpui_kit::Context;
use tokio::sync::oneshot;

use crate::kube_bridge::{spawn_cluster, spawn_cluster_task};
use crate::store::Store;

/// CRD add/update/delete → rediscovery debounce (#16). Updates count too: a
/// new CRD is served only once it turns Established, which arrives as an
/// update of the same object.
pub const CRD_DEBOUNCE: Duration = Duration::from_millis(400);

/// Retry interval for a watch the apiserver refused with 403.
const FORBIDDEN_RETRY: Duration = Duration::from_secs(30);

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

    fn from_watcher(error: &watcher::Error) -> Self {
        Self {
            message: error.to_string(),
            forbidden: astrolabe_kube::watch::is_forbidden(error),
        }
    }
}

pub type WatchMessage<K> = Result<Event<K>, WatchError>;
pub type NamespaceWatchMessage = WatchMessage<Namespace>;
pub type CrdWatchMessage = WatchMessage<CustomResourceDefinition>;
pub type CustomWatchMessage = WatchMessage<DynamicObject>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKind {
    Namespaces,
    CustomResourceDefinitions,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionHealth {
    Connecting,
    Healthy,
    Stale,
}

/// One watcher's store plus its connection state: old data stays visible
/// (stale) through a relist until the complete replacement arrives.
pub struct Watched<K> {
    label: String,
    store: Store<K>,
    health: ConnectionHealth,
    has_snapshot: bool,
    stale: bool,
    forbidden: bool,
}

impl<K: ResourceExt> Watched<K> {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            store: Store::new(),
            health: ConnectionHealth::Connecting,
            has_snapshot: false,
            stale: false,
            forbidden: false,
        }
    }

    fn set_health(&mut self, cluster: &str, health: ConnectionHealth) {
        if self.health != health {
            tracing::info!(
                cluster = %cluster,
                from = ?self.health,
                to = ?health,
                "{} session health changed",
                self.label
            );
            self.health = health;
        }
    }

    fn mark_stale(&mut self, cluster: &str) {
        self.set_health(cluster, ConnectionHealth::Stale);
        self.stale = self.has_snapshot;
    }

    fn apply(&mut self, cluster: &str, message: WatchMessage<K>) {
        match message {
            Ok(event) => {
                let relist_completed = matches!(&event, Event::InitDone);
                match &event {
                    Event::Init => {
                        self.set_health(cluster, ConnectionHealth::Connecting);
                        self.stale = self.has_snapshot;
                    }
                    Event::InitApply(_) => {}
                    Event::InitDone | Event::Apply(_) | Event::Delete(_) => {
                        self.has_snapshot = true;
                        self.forbidden = false;
                        self.set_health(cluster, ConnectionHealth::Healthy);
                        self.stale = false;
                    }
                }
                self.store.apply(event);
                if relist_completed {
                    tracing::info!(
                        cluster = %cluster,
                        objects = self.store.snapshot().len(),
                        "{} snapshot replaced",
                        self.label
                    );
                }
            }
            Err(error) => {
                self.forbidden = error.forbidden;
                self.mark_stale(cluster);
            }
        }
    }

    pub fn objects(&self) -> Vec<&K> {
        self.store.snapshot()
    }

    pub fn get(&self, namespace: Option<&str>, name: &str) -> Option<&K> {
        self.store.get(namespace, name)
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

type DiscoverFn = Box<dyn FnMut() -> Result<ServedResources, String>>;
type DeleteFn = Box<dyn FnMut(&ApiResource, Option<&str>, &str) -> Result<(), String>>;

/// Hand-fed cluster I/O for tests and demos: watch events come from
/// channels, discovery and delete from closures.
pub struct ScriptedCluster {
    namespaces: mpsc::Receiver<NamespaceWatchMessage>,
    crds: Option<mpsc::Receiver<CrdWatchMessage>>,
    custom: HashMap<WatchKey, mpsc::Receiver<CustomWatchMessage>>,
    discover: DiscoverFn,
    delete: DeleteFn,
}

impl ScriptedCluster {
    pub fn new(namespaces: mpsc::Receiver<NamespaceWatchMessage>) -> Self {
        Self {
            namespaces,
            crds: None,
            custom: HashMap::new(),
            discover: Box::new(|| Ok(ServedResources::default())),
            delete: Box::new(|_, _, _| Ok(())),
        }
    }

    pub fn crds(mut self, crds: mpsc::Receiver<CrdWatchMessage>) -> Self {
        self.crds = Some(crds);
        self
    }

    /// Events for the custom resource watch `key`, once a view opens it.
    pub fn custom(mut self, key: WatchKey, events: mpsc::Receiver<CustomWatchMessage>) -> Self {
        self.custom.insert(key, events);
        self
    }

    pub fn discover(
        mut self,
        discover: impl FnMut() -> Result<ServedResources, String> + 'static,
    ) -> Self {
        self.discover = Box::new(discover);
        self
    }

    pub fn delete(
        mut self,
        delete: impl FnMut(&ApiResource, Option<&str>, &str) -> Result<(), String> + 'static,
    ) -> Self {
        self.delete = Box::new(delete);
        self
    }
}

/// The session's cluster client, connected once and shared by every
/// watcher and one-shot operation.
pub type SharedClient = Shared<BoxFuture<'static, Result<Client, String>>>;

enum ClusterIo {
    Live {
        client: SharedClient,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    },
    Scripted(ScriptedCluster),
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
    custom: BTreeMap<WatchKey, Watched<DynamicObject>>,
    namespace: String,
    viewing: Option<CustomKindId>,
    io: ClusterIo,
}

impl Session {
    pub fn new(cluster: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let cluster = cluster.into();
        let client = {
            let cluster = cluster.clone();
            let connect = spawn_cluster(async move {
                astrolabe_kube::client_for_context(&cluster)
                    .await
                    .map_err(|error| error.to_string())
            });
            async move {
                connect
                    .await
                    .unwrap_or_else(|_| Err("cluster connect task dropped".to_string()))
            }
            .boxed()
            .shared()
        };
        let (namespaces, namespace_task) = spawn_watch(
            cluster.clone(),
            "namespace",
            client.clone(),
            astrolabe_kube::watch::namespaces,
        );
        let (crds, crd_task) = spawn_watch(
            cluster.clone(),
            "CRD",
            client.clone(),
            astrolabe_kube::watch::custom_resource_definitions,
        );
        let io = ClusterIo::Live {
            client,
            tasks: vec![namespace_task, crd_task],
        };
        Self::start(cluster, namespaces, Some(crds), io, cx)
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
        mut scripted: ScriptedCluster,
        cx: &mut Context<Self>,
    ) -> Self {
        let (_, placeholder) = mpsc::channel(1);
        let namespaces = std::mem::replace(&mut scripted.namespaces, placeholder);
        let crds = scripted.crds.take();
        Self::start(
            cluster.into(),
            namespaces,
            crds,
            ClusterIo::Scripted(scripted),
            cx,
        )
    }

    fn start(
        cluster: String,
        namespaces: mpsc::Receiver<NamespaceWatchMessage>,
        crds: Option<mpsc::Receiver<CrdWatchMessage>>,
        io: ClusterIo,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut session = Self {
            watch_key: WatchKey {
                cluster,
                kind: ResourceKind::Namespaces,
                namespace: None,
            },
            namespaces: Watched::new("namespace"),
            crds: Watched::new("CRD"),
            discovery: DiscoveryCache::default(),
            crd_generation: 0,
            discovery_runs: 0,
            catalog: Vec::new(),
            custom: BTreeMap::new(),
            namespace: "default".to_string(),
            viewing: None,
            io,
        };
        spawn_drain(
            namespaces,
            cx,
            |session, message, _| {
                let cluster = session.watch_key.cluster.clone();
                session.namespaces.apply(&cluster, message);
            },
            |session| {
                let cluster = session.watch_key.cluster.clone();
                session.namespaces.mark_stale(&cluster);
            },
        );
        if let Some(crds) = crds {
            spawn_drain(crds, cx, Self::apply_crd, |session| {
                let cluster = session.watch_key.cluster.clone();
                session.crds.mark_stale(&cluster);
            });
        }
        session.run_discovery(cx);
        session
    }

    fn apply_crd(&mut self, message: CrdWatchMessage, cx: &mut Context<Self>) {
        let changed = match &message {
            Ok(Event::Apply(_) | Event::Delete(_)) => true,
            // A relist after a disconnect may hide missed changes; the
            // first snapshot does not, connect-time discovery covers it.
            Ok(Event::InitDone) => self.crds.has_snapshot(),
            _ => false,
        };
        let first_snapshot = matches!(&message, Ok(Event::InitDone)) && !self.crds.has_snapshot();
        let cluster = self.watch_key.cluster.clone();
        self.crds.apply(&cluster, message);
        self.rebuild_catalog(cx);
        // Connect-time discovery races the first CRD list: a CRD created in
        // between would stay "not served" until the next CRD change.
        let unresolved = first_snapshot
            && self.catalog.iter().any(|group| {
                group
                    .kinds
                    .iter()
                    .any(|kind| kind.resource == KindResource::NotServed)
            });
        if changed || unresolved {
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
        match &mut self.io {
            ClusterIo::Scripted(scripted) => {
                let result = (scripted.discover)();
                self.finish_discovery(run, result, cx);
            }
            ClusterIo::Live { client, .. } => {
                let client = client.clone();
                let result = spawn_cluster(async move {
                    let client = client.await?;
                    astrolabe_kube::discovery::discover(&client)
                        .await
                        .map_err(|error| error.to_string())
                });
                cx.spawn(async move |this, cx| {
                    let result = result
                        .await
                        .unwrap_or_else(|_| Err("discovery task dropped".to_string()));
                    this.update(cx, |session, cx| session.finish_discovery(run, result, cx))
                        .ok();
                })
                .detach();
            }
        }
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
        if let Some(id) = self.viewing.clone() {
            self.ensure_custom_watch(&id, cx);
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
        self.viewing = Some(id.clone());
        self.ensure_custom_watch(id, cx)
    }

    /// Leave the instance lists (another view is shown).
    pub fn close_custom_list(&mut self) {
        self.viewing = None;
    }

    fn ensure_custom_watch(
        &mut self,
        id: &CustomKindId,
        cx: &mut Context<Self>,
    ) -> Option<WatchKey> {
        let kind = self.custom_kind(id)?;
        let KindResource::Served(resource) = &kind.resource else {
            return None;
        };
        let resource = resource.clone();
        let key = self.custom_watch_key(kind);
        if self.custom.contains_key(&key) {
            return Some(key);
        }
        self.custom
            .insert(key.clone(), Watched::new(resource.kind.clone()));
        let events = match &mut self.io {
            ClusterIo::Scripted(scripted) => scripted.custom.remove(&key),
            ClusterIo::Live { client, tasks } => {
                let namespace = key.namespace.clone();
                let watch_resource = resource.clone();
                let (events, task) = spawn_watch(
                    key.cluster.clone(),
                    "custom resource",
                    client.clone(),
                    move |client| {
                        astrolabe_kube::watch::custom_resources(
                            client,
                            &watch_resource,
                            namespace.as_deref(),
                        )
                    },
                );
                tasks.push(task);
                Some(events)
            }
        };
        if let Some(events) = events {
            let apply_key = key.clone();
            let closed_key = key.clone();
            spawn_drain(
                events,
                cx,
                move |session, message, _| {
                    let cluster = session.watch_key.cluster.clone();
                    if let Some(watch) = session.custom.get_mut(&apply_key) {
                        watch.apply(&cluster, message);
                    }
                },
                move |session| {
                    let cluster = session.watch_key.cluster.clone();
                    if let Some(watch) = session.custom.get_mut(&closed_key) {
                        watch.mark_stale(&cluster);
                    }
                },
            );
        }
        cx.notify();
        Some(key)
    }

    /// The watch a kind's list reads for the current namespace selection:
    /// namespaced kinds follow it, cluster-scoped kinds ignore it.
    pub fn custom_watch_key(&self, kind: &CustomKind) -> WatchKey {
        WatchKey {
            cluster: self.watch_key.cluster.clone(),
            kind: ResourceKind::Custom {
                group: kind.group.clone(),
                version: kind.version.clone(),
                kind: kind.kind.clone(),
            },
            namespace: kind.namespaced.then(|| self.namespace.clone()),
        }
    }

    pub fn set_namespace(&mut self, namespace: String, cx: &mut Context<Self>) {
        if self.namespace == namespace {
            return;
        }
        self.namespace = namespace;
        if let Some(id) = self.viewing.clone() {
            self.ensure_custom_watch(&id, cx);
        }
        cx.notify();
    }

    /// Delete one object; resolves with the server's answer.
    pub fn delete(
        &mut self,
        resource: ApiResource,
        namespace: Option<String>,
        name: String,
    ) -> oneshot::Receiver<Result<(), String>> {
        match &mut self.io {
            ClusterIo::Scripted(scripted) => {
                let (tx, rx) = oneshot::channel();
                let _ = tx.send((scripted.delete)(&resource, namespace.as_deref(), &name));
                rx
            }
            ClusterIo::Live { client, .. } => {
                let client = client.clone();
                spawn_cluster(async move {
                    let client = client.await?;
                    astrolabe_kube::resource::delete(
                        &client,
                        &resource,
                        namespace.as_deref(),
                        &name,
                    )
                    .await
                    .map_err(|error| error.to_string())
                })
            }
        }
    }

    /// The live session's shared client; `None` for scripted sessions.
    pub fn client(&self) -> Option<SharedClient> {
        match &self.io {
            ClusterIo::Live { client, .. } => Some(client.clone()),
            ClusterIo::Scripted(_) => None,
        }
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

    pub fn custom_watch(&self, key: &WatchKey) -> Option<&Watched<DynamicObject>> {
        self.custom.get(key)
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

impl Drop for Session {
    fn drop(&mut self) {
        if let ClusterIo::Live { tasks, .. } = &mut self.io {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

/// Resident drain loop (#8 bridge): applies every queued message, then
/// notifies once; the channel closing marks the watch stale.
fn spawn_drain<K: 'static>(
    mut receiver: mpsc::Receiver<WatchMessage<K>>,
    cx: &mut Context<Session>,
    apply: impl Fn(&mut Session, WatchMessage<K>, &mut Context<Session>) + 'static,
    closed: impl Fn(&mut Session) + 'static,
) {
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

/// Resident watcher on the cluster runtime. Any stream error restarts the
/// watcher from a fresh LIST (#28): a silent watch resumption would emit no
/// `InitDone`, leaving the UI stale.
fn spawn_watch<K, S>(
    cluster: String,
    label: &'static str,
    client: SharedClient,
    stream: impl Fn(&Client) -> S + Send + 'static,
) -> (mpsc::Receiver<WatchMessage<K>>, tokio::task::JoinHandle<()>)
where
    K: Send + 'static,
    S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
{
    let (mut sender, receiver) = mpsc::channel(256);
    let task = spawn_cluster_task(async move {
        let client = match client.await {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(cluster = %cluster, error = %error, "{label} watch client setup failed");
                let _ = sender.send(Err(WatchError::new(error))).await;
                return;
            }
        };
        loop {
            let events = stream(&client);
            futures::pin_mut!(events);
            let mut report_end = true;
            let mut forbidden = false;
            while let Some(event) = events.next().await {
                let event = event.map_err(|error| {
                    tracing::warn!(cluster = %cluster, error = %error, "{label} watch failed");
                    WatchError::from_watcher(&error)
                });
                let failed = event.is_err();
                forbidden = matches!(&event, Err(error) if error.forbidden);
                if sender.send(event).await.is_err() {
                    return;
                }
                if failed {
                    report_end = false;
                    break;
                }
            }
            if report_end
                && sender
                    .send(Err(WatchError::new(format!("{label} watch ended"))))
                    .await
                    .is_err()
            {
                return;
            }
            tracing::info!(cluster = %cluster, "restarting {label} watch");
            // RBAC denials do not clear by retrying fast; back off further.
            let delay = if forbidden {
                FORBIDDEN_RETRY
            } else {
                Duration::from_secs(1)
            };
            tokio::time::sleep(delay).await;
        }
    });
    (receiver, task)
}
