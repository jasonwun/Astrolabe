//! What the launch opens (#106): the preselected cluster, namespace and tabs.

use astrolabe_model::builtin::BuiltinKind;

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

/// A tab the launch session opens, in its namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchTab {
    /// The list tab shows this kind.
    List(BuiltinKind),
    /// The detail tab of the object of this kind and name.
    Detail(BuiltinKind, String),
}

impl LaunchTab {
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
