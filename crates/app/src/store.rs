use std::fmt;

use astrolabe_kube::kube::core::ResourceExt;

/// An object's identity within one resource's snapshot: its namespace (`None`
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
