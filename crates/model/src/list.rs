//! List resolution (#237): what the viewed list shows, read from a
//! `SessionState`'s watches with no GPUI view, the counterpart of
//! `detail::resolve`. The outcome (rows, loading, failed, forbidden,
//! undefined, not served) and the Stale flag are decided here; the panel
//! renders the answer.

use std::collections::BTreeSet;

use astrolabe_kube::cluster::{ClusterError, ClusterErrorKind};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::ResourceExt;

use crate::catalog::KindResource;
use crate::detail::forbidden_message;
use crate::projection::{
    ResourceTable, project_custom_resources, project_definitions, project_namespaces,
};
use crate::session::{WatchReading, Watched};
use crate::session_state::{SessionState, Viewing};

/// The text shown where listing Definitions is denied.
pub const CRD_DENIED: &str = "No permission to list CustomResourceDefinitions";

/// What the viewed list shows.
#[derive(Debug, PartialEq)]
pub enum ListOutcome {
    /// The list's columns over its objects; no rows is an empty list.
    Rows(ResourceTable),
    /// The list has nothing to show yet: its kind has not resolved
    /// (Discovery is running or failed), or its watch has no snapshot.
    Loading(String),
    /// The list's watch failed or ended before its first snapshot: the
    /// Cluster cannot be reached. The error is none when the watch ended
    /// without one.
    Failed(Option<ClusterError>),
    /// The list's watch is denied by RBAC.
    Forbidden(String),
    /// The custom kind left the Catalog.
    Undefined(String),
    /// The API server does not serve the custom kind's preferred version.
    NotServed(String),
}

/// The text shown where a list's watch failed before its first snapshot.
pub fn failed_message(error: Option<&ClusterError>) -> String {
    match error {
        None => "The connection ended before the list loaded".to_string(),
        Some(error) => match error.kind() {
            ClusterErrorKind::Connect => format!("Cannot connect to the cluster: {error}"),
            ClusterErrorKind::Unreachable => format!("Cannot reach the cluster: {error}"),
            _ => format!("Cannot load the list: {error}"),
        },
    }
}

/// The viewed list as of `now`.
#[derive(Debug, PartialEq)]
pub struct Resolved {
    pub outcome: ListOutcome,
    /// The list's watch lost its connection and shows its old snapshot.
    pub stale: bool,
}

/// The list `state` views as of `now`. `groups` narrows Definitions to
/// those API groups (empty: all).
pub fn resolve(state: &SessionState, groups: &BTreeSet<String>, now: Timestamp) -> Resolved {
    match state.viewing() {
        Viewing::Namespaces => from_watch(
            Some(state.namespaces_watch()),
            "namespaces",
            |_| forbidden_message("namespaces"),
            |watch| project_namespaces(&watch.objects().collect::<Vec<_>>(), now),
        ),
        Viewing::Builtin(kind) => {
            let registration = kind.registration();
            from_watch(
                state.builtin_watch(*kind, Some(state.namespace())),
                registration.label,
                forbidden_message,
                |watch| (registration.list)(&watch.objects().collect::<Vec<_>>(), now),
            )
        }
        Viewing::Definitions => from_watch(
            Some(state.crds()),
            "custom resource definitions",
            |_| CRD_DENIED.to_string(),
            |watch| project_definitions(&watch.objects().collect::<Vec<_>>(), groups, now),
        ),
        Viewing::Custom(id) => custom(state, id, now),
    }
}

/// The one rule every list follows (`Watched::reading`): loading, failed,
/// forbidden or rows, flagged stale when the watch is.
fn from_watch<K: ResourceExt>(
    watch: Option<&Watched<K>>,
    label: &str,
    forbidden: impl FnOnce(&str) -> String,
    rows: impl FnOnce(&Watched<K>) -> ResourceTable,
) -> Resolved {
    let outcome = match (Watched::reading(watch), watch) {
        (WatchReading::Rows, Some(watch)) => ListOutcome::Rows(rows(watch)),
        (WatchReading::Failed(error), _) => ListOutcome::Failed(error.cloned()),
        (WatchReading::Forbidden, _) => ListOutcome::Forbidden(forbidden(label)),
        (WatchReading::Loading | WatchReading::Rows, _) => {
            ListOutcome::Loading(format!("Loading {}…", label.to_lowercase()))
        }
    };
    Resolved {
        outcome,
        stale: watch.is_some_and(|watch| watch.status().is_stale()),
    }
}

fn custom(state: &SessionState, id: &crate::session::CustomKindId, now: Timestamp) -> Resolved {
    let unresolved = |outcome| Resolved {
        outcome,
        stale: false,
    };
    let Some(kind) = state.custom_kind(id) else {
        return unresolved(ListOutcome::Undefined(format!(
            "{} is no longer defined",
            id.kind
        )));
    };
    match &kind.resource {
        KindResource::Undiscovered => {
            unresolved(ListOutcome::Loading(match state.discovery_failure() {
                Some(error) => format!("API discovery failed: {error}"),
                None => "Discovering API resources…".to_string(),
            }))
        }
        KindResource::NotServed => unresolved(ListOutcome::NotServed(format!(
            "{} {} is not served by the API server",
            kind.kind, kind.version
        ))),
        KindResource::Served(_) => from_watch(
            state.custom_watch(kind),
            &kind.plural,
            forbidden_message,
            |watch| project_custom_resources(kind, &watch.objects().collect::<Vec<_>>(), now),
        ),
    }
}
