//! Session's value types: what a watcher is keyed by, what it holds and how
//! its connection fares.

use std::collections::BTreeMap;

use astrolabe_kube::cluster::{ClusterError, ClusterId};
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

/// What a reader shows for one watch (see `Watched::reading`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchReading<'a> {
    /// No snapshot yet: the first list is in progress, or not started.
    Loading,
    /// The watch failed or ended before any snapshot. The error is none when
    /// the watch ended without one.
    Failed(Option<&'a ClusterError>),
    /// RBAC denies the watch.
    Forbidden,
    /// The snapshot is shown, empty or not; the status says whether stale.
    Rows,
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

/// What `ObjectKey` and a borrowed lookup key have in common, so a
/// `BTreeMap<ObjectKey, _>` can be searched without building an owned key.
trait KeyView {
    fn namespace(&self) -> Option<&str>;
    fn name(&self) -> &str;
}

impl KeyView for ObjectKey {
    fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
    fn name(&self) -> &str {
        &self.name
    }
}

struct KeyRef<'a> {
    namespace: Option<&'a str>,
    name: &'a str,
}

impl KeyView for KeyRef<'_> {
    fn namespace(&self) -> Option<&str> {
        self.namespace
    }
    fn name(&self) -> &str {
        self.name
    }
}

impl<'a> std::borrow::Borrow<dyn KeyView + 'a> for ObjectKey {
    fn borrow(&self) -> &(dyn KeyView + 'a) {
        self
    }
}

// Same order as `ObjectKey`'s derived `Ord`: namespace (`None` first), then
// name.
impl PartialEq for dyn KeyView + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for dyn KeyView + '_ {}

impl PartialOrd for dyn KeyView + '_ {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for dyn KeyView + '_ {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.namespace(), self.name()).cmp(&(other.namespace(), other.name()))
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
    /// The failure that ended the watch, until a message shows it live
    /// again.
    last_error: Option<ClusterError>,
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
            last_error: None,
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
        if status == WatchStatus::Live {
            self.last_error = None;
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
                self.last_error = Some(error);
                SnapshotChange::Unchanged
            }
        }
    }

    /// The committed objects in key order (namespace, then name). Allocates
    /// nothing; callers that need a `Vec` collect it themselves.
    pub fn objects(&self) -> impl ExactSizeIterator<Item = &K> {
        self.committed.values()
    }

    /// Looks an object up by identity. Allocates nothing: the lookup borrows
    /// `namespace` and `name` instead of building an owned `ObjectKey`.
    pub fn get(&self, namespace: Option<&str>, name: &str) -> Option<&K> {
        let probe = KeyRef { namespace, name };
        self.committed.get(&probe as &dyn KeyView)
    }

    pub fn status(&self) -> WatchStatus {
        self.status
    }

    /// The latest failure of the watch, kind and source kept; none once it
    /// is live again.
    pub fn last_error(&self) -> Option<&ClusterError> {
        self.last_error.as_ref()
    }

    /// What a reader of this watch can show: the one rule the list and the
    /// detail share (#276). A watch that is not there is [`Loading`] too.
    ///
    /// [`Loading`]: WatchReading::Loading
    pub fn reading(watch: Option<&Self>) -> WatchReading<'_> {
        let Some(watch) = watch else {
            return WatchReading::Loading;
        };
        match watch.status {
            WatchStatus::Connecting => WatchReading::Loading,
            WatchStatus::Unreachable => WatchReading::Failed(watch.last_error.as_ref()),
            WatchStatus::Forbidden { .. } => WatchReading::Forbidden,
            WatchStatus::Live | WatchStatus::Relisting | WatchStatus::Stale => WatchReading::Rows,
        }
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
    Failed(ClusterError),
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{KeyRef, KeyView};

    fn key<'a>(namespace: Option<&'a str>, name: &'a str) -> KeyRef<'a> {
        KeyRef { namespace, name }
    }

    fn view<'a>(key: &'a KeyRef<'a>) -> &'a dyn KeyView {
        key
    }

    #[test]
    fn key_views_compare_by_namespace_then_name() {
        let early = key(Some("a"), "z");
        let late = key(Some("b"), "a");
        assert!(view(&early) != view(&late));
        assert_eq!(view(&early).partial_cmp(view(&late)), Some(Ordering::Less));
        assert_eq!(
            view(&late).partial_cmp(view(&early)),
            Some(Ordering::Greater)
        );

        let cluster = key(None, "m");
        assert_eq!(
            view(&cluster).partial_cmp(view(&early)),
            Some(Ordering::Less)
        );

        let first = key(Some("a"), "m");
        let second = key(Some("a"), "n");
        let same = key(Some("a"), "m");
        assert!(view(&first) != view(&second));
        assert!(view(&first) == view(&same));
        assert_eq!(view(&first).partial_cmp(view(&same)), Some(Ordering::Equal));
    }
}
