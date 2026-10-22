use std::collections::BTreeMap;

use astrolabe_kube::kube::{core::ResourceExt, runtime::watcher::Event};

pub struct Store<K> {
    committed: BTreeMap<(Option<String>, String), K>,
    staging: BTreeMap<(Option<String>, String), K>,
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
                self.staging.insert(key(&object), object);
            }
            Event::InitDone => self.committed = std::mem::take(&mut self.staging),
            Event::Apply(object) => {
                self.committed.insert(key(&object), object);
            }
            Event::Delete(object) => {
                self.committed.remove(&key(&object));
            }
        }
    }

    pub fn snapshot(&self) -> Vec<&K> {
        self.committed.values().collect()
    }
}

impl<K: ResourceExt> Default for Store<K> {
    fn default() -> Self {
        Self::new()
    }
}

fn key<K: ResourceExt>(object: &K) -> (Option<String>, String) {
    (object.namespace(), object.name_any())
}
