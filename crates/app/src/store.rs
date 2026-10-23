use std::collections::BTreeMap;
use std::fmt;

use astrolabe_kube::kube::{core::ResourceExt, runtime::watcher::Event};

/// An object's identity within one resource's store: its namespace (`None`
/// when cluster-scoped) and name. Displays as `namespace/name` or `name`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectKey {
    pub namespace: Option<String>,
    pub name: String,
}

impl ObjectKey {
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Self {
        Self {
            namespace,
            name: name.into(),
        }
    }

    pub fn cluster_scoped(name: impl Into<String>) -> Self {
        Self::new(None, name)
    }

    pub fn of<K: ResourceExt>(object: &K) -> Self {
        Self::new(object.namespace(), object.name_any())
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{namespace}/{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

pub struct Store<K> {
    committed: BTreeMap<ObjectKey, K>,
    staging: BTreeMap<ObjectKey, K>,
}

impl<K: ResourceExt> Store<K> {
    pub fn new() -> Self {
        Self {
            committed: BTreeMap::new(),
            staging: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, event: Event<K>) {
        match event {
            Event::Init => self.staging.clear(),
            Event::InitApply(object) => {
                self.staging.insert(ObjectKey::of(&object), object);
            }
            Event::InitDone => self.committed = std::mem::take(&mut self.staging),
            Event::Apply(object) => {
                self.committed.insert(ObjectKey::of(&object), object);
            }
            Event::Delete(object) => {
                self.committed.remove(&ObjectKey::of(&object));
            }
        }
    }

    pub fn snapshot(&self) -> Vec<&K> {
        self.committed.values().collect()
    }

    pub fn get(&self, namespace: Option<&str>, name: &str) -> Option<&K> {
        self.committed
            .get(&ObjectKey::new(namespace.map(str::to_string), name))
    }
}

impl<K: ResourceExt> Default for Store<K> {
    fn default() -> Self {
        Self::new()
    }
}
