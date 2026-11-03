//! Session's value types: what a watcher is keyed by, what it holds and how
//! its connection fares.

use std::collections::BTreeMap;

use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::kube::ResourceExt;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_kube::object::ObjectKey;
use astrolabe_kube::watch::WatchMessage;

use crate::builtin::BuiltinKind;
use crate::catalog::CustomKind;

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
    pub cluster: ClusterId,
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
    cluster: ClusterId,
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
    pub fn new(cluster: ClusterId, label: impl Into<String>) -> Self {
        Self {
            cluster,
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

    /// The watch failed or ended: keep the snapshot, flag it stale.
    pub fn mark_stale(&mut self) {
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
                self.forbidden = error.is_forbidden();
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardState {
    Starting,
    /// Accepting on this localhost port.
    Listening(u16),
    Failed(String),
}
