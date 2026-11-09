//! List resolution (#237): what the viewed list shows, read from a
//! `SessionState`'s watches with no GPUI view, the counterpart of
//! `detail::resolve`. The outcome (rows, loading, failed, forbidden,
//! undefined, not served) and the Stale flag are decided here; the panel
//! renders the answer.

use std::collections::BTreeSet;

use astrolabe_kube::cluster::ClusterError;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::ResourceExt;

use crate::catalog::KindResource;
use crate::detail::{OpenRowError, forbidden_message};
use crate::notice::{Intent, Notice, failed_message};
use crate::projection::{
    ResourceTable, project_custom_resources, project_definitions, project_namespaces,
};
use crate::session::{StaleCause, WatchReading, Watched};
use crate::session_state::{SessionState, Viewing};

/// The text shown where listing Definitions is denied.
pub const CRD_DENIED: &str = "No permission to list CustomResourceDefinitions";

/// The nav's note where listing Definitions is denied.
pub fn crd_denied_notice() -> Notice {
    Notice::new("nav-crd-denied", CRD_DENIED, Intent::Muted)
}

/// The list's note that opening a row failed.
pub fn open_row_notice(error: &OpenRowError) -> Notice {
    Notice::new("list-notice", error.to_string(), Intent::Warning)
}

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

impl ListOutcome {
    /// What the list shows instead of rows; none for rows.
    pub fn notice(&self) -> Option<Notice> {
        Some(match self {
            Self::Rows(_) => return None,
            Self::Loading(text) | Self::Forbidden(text) | Self::Undefined(text) => {
                Notice::new("list-message", text.clone(), Intent::Muted)
            }
            Self::Failed(error) => Notice::new(
                "list-error",
                failed_message("list", error.as_ref()),
                Intent::Danger,
            ),
            Self::NotServed(text) => Notice::new("list-error", text.clone(), Intent::Danger),
        })
    }
}

/// The viewed list as of `now`.
#[derive(Debug, PartialEq)]
pub struct Resolved {
    pub outcome: ListOutcome,
    /// Why the list shows an old snapshot: its watch lost its connection, or
    /// RBAC denies it. None when the rows are current.
    pub stale: Option<StaleCause>,
}

/// The list `state` views as of `now`. `groups` narrows Definitions to
/// those API groups (empty: all).
pub fn resolve(state: &SessionState, groups: &BTreeSet<String>, now: Timestamp) -> Resolved {
    match state.viewing() {
        Viewing::Namespaces => from_watch(
            Some(state.namespaces()),
            "namespaces",
            |_| forbidden_message("namespaces"),
            |objects| project_namespaces(objects, now),
        ),
        Viewing::Builtin(kind) => {
            let registration = kind.registration();
            from_watch(
                state.builtin_watch(*kind, Some(state.namespace())),
                registration.label,
                forbidden_message,
                |objects| (registration.list)(objects, now),
            )
        }
        Viewing::Definitions => from_watch(
            Some(state.crds()),
            "custom resource definitions",
            |_| CRD_DENIED.to_string(),
            |objects| project_definitions(objects, groups, now),
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
    rows: impl FnOnce(&[&K]) -> ResourceTable,
) -> Resolved {
    let outcome = match (Watched::reading(watch), watch) {
        (WatchReading::Rows, Some(watch)) => {
            ListOutcome::Rows(rows(&watch.objects().collect::<Vec<_>>()))
        }
        (WatchReading::Failed(error), _) => ListOutcome::Failed(error.cloned()),
        (WatchReading::Forbidden, _) => ListOutcome::Forbidden(forbidden(label)),
        (WatchReading::Loading | WatchReading::Rows, _) => {
            ListOutcome::Loading(format!("Loading {}…", label.to_lowercase()))
        }
    };
    Resolved {
        outcome,
        stale: watch.and_then(|watch| watch.status().stale_cause()),
    }
}

fn custom(state: &SessionState, id: &crate::session::CustomKindId, now: Timestamp) -> Resolved {
    let unresolved = |outcome| Resolved {
        outcome,
        stale: None,
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
            |objects| project_custom_resources(kind, objects, now),
        ),
    }
}
