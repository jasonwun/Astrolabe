//! What the launch opens (#106): the preselected cluster, namespace and tabs.

use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::object::ObjectKey;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::detail::DetailTarget;
use astrolabe_model::session_state::Viewing;
use astrolabe_model::tab::TabRequest;

/// What the launch opens instead of the first cluster in `default`; empty
/// leaves the launch as it is. Debug builds fill it from
/// `ASTROLABE_CLUSTER`, `ASTROLABE_NAMESPACE` and `ASTROLABE_OPEN`
/// (`main.rs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preselection {
    /// The name of the cluster opened and selected at launch, as the
    /// sidebar shows it, when it names a listed cluster. A name and not a
    /// [`ClusterId`]: it is what the user typed, matched against
    /// [`astrolabe_kube::clusters`].
    pub cluster: Option<String>,
    /// The namespace the session opened at launch starts in.
    pub namespace: Option<String>,
    /// Tabs the session opened at launch shows, in order (#106).
    pub open: Vec<LaunchTab>,
}

/// The Cluster the launch opens and the namespace its session starts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchTarget {
    pub cluster: ClusterId,
    /// `None` is the context's own namespace.
    pub namespace: Option<String>,
}

/// How the launch resolved its Cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchResolution {
    /// No Cluster is listed, so nothing opens.
    NoCluster,
    /// The Cluster to open.
    Open(LaunchTarget),
    /// The preselected Cluster is not listed: the first one opens, without
    /// the preselected namespace. Carries the unlisted name for the log.
    Fallback {
        unlisted: String,
        target: LaunchTarget,
    },
}

impl Preselection {
    /// Which Cluster of `clusters` the launch opens, and in which namespace.
    /// An unlisted preselected Cluster falls back to the first one and drops
    /// the preselected namespace; without a preselected Cluster the first
    /// one opens in the preselected namespace.
    pub fn resolve(&self, clusters: &[ClusterId]) -> LaunchResolution {
        let first = |namespace| {
            clusters.first().map(|cluster| LaunchTarget {
                cluster: cluster.clone(),
                namespace,
            })
        };
        match &self.cluster {
            Some(name) => match clusters.iter().find(|cluster| cluster.name() == name) {
                Some(cluster) => LaunchResolution::Open(LaunchTarget {
                    cluster: cluster.clone(),
                    namespace: self.namespace.clone(),
                }),
                None => first(None).map_or(LaunchResolution::NoCluster, |target| {
                    LaunchResolution::Fallback {
                        unlisted: name.clone(),
                        target,
                    }
                }),
            },
            None => first(self.namespace.clone())
                .map_or(LaunchResolution::NoCluster, LaunchResolution::Open),
        }
    }
}

/// What the session does for a [`LaunchTab`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchOpening {
    /// Show this list in the list tab.
    Show(Viewing),
    /// Open this tab.
    Open(TabRequest),
}

/// A tab the launch session opens, in its namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchTab {
    /// The list tab shows this kind.
    List(BuiltinKind),
    /// The detail tab of the object of this kind and name.
    Detail(BuiltinKind, String),
}

impl LaunchTab {
    /// What opening this tab does in `namespace`, the session's.
    pub fn opening(self, namespace: &str) -> LaunchOpening {
        match self {
            Self::List(kind) => LaunchOpening::Show(Viewing::Builtin(kind)),
            Self::Detail(kind, name) => {
                let key = ObjectKey::new(Some(namespace.to_string()), name);
                LaunchOpening::Open(TabRequest::Detail(DetailTarget::Builtin { kind, key }))
            }
        }
    }

    /// `ASTROLABE_OPEN`'s comma-separated tabs: a built-in `Kind` shows its
    /// list, `Kind/name` opens that object's detail. An unknown kind is
    /// logged and skipped.
    pub fn parse_all(spec: &str) -> Vec<Self> {
        spec.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .filter_map(|item| {
                let (kind, name) = match item.split_once('/') {
                    Some((kind, name)) => (kind, Some(name)),
                    None => (item, None),
                };
                let Some(kind) = BuiltinKind::from_kind(kind) else {
                    tracing::warn!(ASTROLABE_OPEN = %item, "not a built-in kind; skipping");
                    return None;
                };
                Some(match name {
                    Some(name) => Self::Detail(kind, name.to_string()),
                    None => Self::List(kind),
                })
            })
            .collect()
    }
}
