//! Session's state and rules for watches (ADR-0002, #164): each watch's
//! `Watched` snapshot and Stale state, the namespace selection and the list
//! being viewed. Every input answers with the `Command`s Session carries
//! out; Session decides nothing about them.

use std::collections::BTreeMap;

use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::watch::{CrdWatchMessage, NamespaceWatchMessage, ObjectWatchMessage};

use crate::builtin::BuiltinKind;
use crate::catalog::{CustomGroup, CustomKind, KindResource};
use crate::session::{
    ConnectionHealth, CustomKindId, ResourceKind, SnapshotChange, WatchKey, Watched,
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

    /// A message of the Definitions watch.
    pub fn crds_message(&mut self, message: CrdWatchMessage) -> SnapshotChange {
        self.crds.apply(message)
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

    /// Enter a built-in kind's list: its watch for the current namespace
    /// starts on first visit (lazy, then resident for the session).
    pub fn open_builtin_list(&mut self, kind: BuiltinKind) -> Ensured {
        self.viewing = Some(Viewing::Builtin(kind));
        let namespace = self.namespace.clone();
        self.watch_builtin(kind, &namespace)
    }

    /// Enter a custom kind's instance list, as `catalog` resolves it. `None`
    /// while the kind is unknown or not served.
    pub fn open_custom_list(
        &mut self,
        id: &CustomKindId,
        catalog: &[CustomGroup],
    ) -> Option<Ensured> {
        self.viewing = Some(Viewing::Custom(id.clone()));
        let namespace = self.namespace.clone();
        self.watch_custom_object(id, Some(&namespace), catalog)
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
        catalog: &[CustomGroup],
    ) -> Option<Ensured> {
        let kind = custom_kind(catalog, id)?;
        let KindResource::Served(resource) = &kind.resource else {
            return None;
        };
        let key = self.custom_object_watch_key(kind, namespace);
        Some(self.ensure(key, resource))
    }

    /// Select `namespace`; the viewed list's watch follows it.
    pub fn set_namespace(&mut self, namespace: String, catalog: &[CustomGroup]) -> Vec<Command> {
        if self.namespace == namespace {
            return Vec::new();
        }
        self.namespace = namespace;
        self.follow_viewed_list(catalog)
    }

    /// The catalog changed: a viewed custom kind may have become served, or
    /// moved to a new preferred version.
    pub fn catalog_changed(&mut self, catalog: &[CustomGroup]) -> Vec<Command> {
        if matches!(self.viewing, Some(Viewing::Custom(_))) {
            self.follow_viewed_list(catalog)
        } else {
            Vec::new()
        }
    }

    // Queries

    /// The Namespaces watch's key; it names the Cluster.
    pub fn watch_key(&self) -> &WatchKey {
        &self.key
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
        self.crds.forbidden()
    }

    pub fn health(&self) -> ConnectionHealth {
        self.namespaces.health()
    }

    /// The Namespaces watch lost its connection and shows its old snapshot.
    pub fn namespaces_stale(&self) -> bool {
        self.namespaces.show_stale_warning()
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

    fn follow_viewed_list(&mut self, catalog: &[CustomGroup]) -> Vec<Command> {
        let namespace = self.namespace.clone();
        let ensured = match self.viewing.clone() {
            Some(Viewing::Builtin(kind)) => Some(self.watch_builtin(kind, &namespace)),
            Some(Viewing::Custom(id)) => self.watch_custom_object(&id, Some(&namespace), catalog),
            None => None,
        };
        ensured.map(|ensured| ensured.commands).unwrap_or_default()
    }
}
