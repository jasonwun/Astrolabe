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

/// Where one watch stands. Every state says whether the watch holds a
/// snapshot, so "stale with no snapshot ever taken" cannot be written down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchStatus {
    /// The first list is in progress; no snapshot yet.
    Connecting,
    /// The snapshot is current.
    Live,
    /// A relist is in progress; the old snapshot stays visible, stale.
    Relisting,
    /// The watch failed or ended; the old snapshot stays visible, stale.
    Stale,
    /// The watch failed or ended before any snapshot arrived.
    Unreachable,
    /// The latest failure was an RBAC denial. `has_snapshot` says whether an
    /// older snapshot is still shown (stale).
    Forbidden { has_snapshot: bool },
}

impl WatchStatus {
    /// The coarse health the cluster indicator shows.
    pub fn health(self) -> ConnectionHealth {
        match self {
            Self::Connecting | Self::Relisting => ConnectionHealth::Connecting,
            Self::Live => ConnectionHealth::Healthy,
            Self::Stale | Self::Unreachable | Self::Forbidden { .. } => ConnectionHealth::Stale,
        }
    }

    /// The watch shows an old snapshot that may no longer be true.
    pub fn is_stale(self) -> bool {
        match self {
            Self::Relisting | Self::Stale => true,
            Self::Forbidden { has_snapshot } => has_snapshot,
            Self::Connecting | Self::Live | Self::Unreachable => false,
        }
    }

    /// The latest failure was an RBAC denial.
    pub fn is_forbidden(self) -> bool {
        matches!(self, Self::Forbidden { .. })
    }

    fn has_snapshot(self) -> bool {
        match self {
            Self::Live | Self::Relisting | Self::Stale => true,
            Self::Forbidden { has_snapshot } => has_snapshot,
            Self::Connecting | Self::Unreachable => false,
        }
    }

    /// A list starts. An RBAC denial stays until a snapshot lifts it.
    fn listing(self) -> Self {
        match self {
            Self::Connecting | Self::Unreachable => Self::Connecting,
            Self::Live | Self::Relisting | Self::Stale => Self::Relisting,
            Self::Forbidden { .. } => self,
        }
    }

    /// The watch failed without a denial: keep the snapshot, flag it stale.
    fn failed(self) -> Self {
        if self.has_snapshot() {
            Self::Stale
        } else {
            Self::Unreachable
        }
    }

    /// The connection ended without an error; a denial is kept.
    fn ended(self) -> Self {
        if self.is_forbidden() {
            self
        } else {
            self.failed()
        }
    }
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
    status: WatchStatus,
    /// Goes up each time the committed snapshot changes.
    revision: u64,
}

impl<K: ResourceExt> Watched<K> {
    /// `label` names the watched resource in logs.
    pub fn new(cluster: ClusterId, label: impl Into<String>) -> Self {
        Self {
            cluster,
            label: label.into(),
            committed: BTreeMap::new(),
            staging: BTreeMap::new(),
            status: WatchStatus::Connecting,
            revision: 0,
        }
    }

    fn set_status(&mut self, status: WatchStatus) {
        if self.status.health() != status.health() {
            tracing::info!(
                cluster = %self.cluster,
                from = ?self.status.health(),
                to = ?status.health(),
                "{} session health changed",
                self.label
            );
        }
        self.status = status;
    }

    /// The watch failed or ended: keep the snapshot, flag it stale.
    pub fn mark_stale(&mut self) {
        self.set_status(self.status.ended());
    }

    /// The single place a watch message is interpreted.
    pub fn apply(&mut self, message: WatchMessage<K>) -> SnapshotChange {
        let change = self.interpret(message);
        if change != SnapshotChange::Unchanged {
            self.revision += 1;
        }
        change
    }

    fn interpret(&mut self, message: WatchMessage<K>) -> SnapshotChange {
        match message {
            Ok(Event::Init) => {
                self.set_status(self.status.listing());
                self.staging.clear();
                SnapshotChange::Unchanged
            }
            Ok(Event::InitApply(object)) => {
                self.staging.insert(ObjectKey::of(&object), object);
                SnapshotChange::Unchanged
            }
            Ok(Event::InitDone) => {
                let change = if self.status.has_snapshot() {
                    SnapshotChange::Changed
                } else {
                    SnapshotChange::FirstSnapshot
                };
                self.set_status(WatchStatus::Live);
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
                self.set_status(WatchStatus::Live);
                let key = ObjectKey::of(&object);
                // Versions are opaque, so equality alone identifies a replay
                // (Freelens 5a8b68e, kube-object.store.ts).
                let replay = object.resource_version().is_some_and(|version| {
                    self.committed
                        .get(&key)
                        .and_then(ResourceExt::resource_version)
                        .is_some_and(|known| known == version)
                });
                if replay {
                    return SnapshotChange::Unchanged;
                }
                self.committed.insert(key, object);
                SnapshotChange::Changed
            }
            Ok(Event::Delete(object)) => {
                self.set_status(WatchStatus::Live);
                match self.committed.remove(&ObjectKey::of(&object)) {
                    Some(_) => SnapshotChange::Changed,
                    None => SnapshotChange::Unchanged,
                }
            }
            Err(error) => {
                let has_snapshot = self.status.has_snapshot();
                self.set_status(if error.is_forbidden() {
                    WatchStatus::Forbidden { has_snapshot }
                } else {
                    self.status.failed()
                });
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

    pub fn status(&self) -> WatchStatus {
        self.status
    }

    /// Goes up whenever the committed snapshot changes (`apply` returns
    /// anything but `Unchanged`), so a reader can tell whether it changed
    /// since it last looked. A relist always counts; an `Apply` carrying the
    /// known `resourceVersion` and a `Delete` of an absent object do not.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardState {
    Starting,
    /// Accepting on this localhost port.
    Listening(u16),
    Failed(String),
}
