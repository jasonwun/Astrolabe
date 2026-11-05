//! List resolution (#237): what the viewed list shows, read from a
//! `SessionState`'s watches with no GPUI view, the counterpart of
//! `detail::resolve`. The outcome (rows, loading, forbidden, undefined, not
//! served) and the Stale flag are decided here; the panel renders the answer.

use std::collections::BTreeSet;

use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use crate::catalog::KindResource;
use crate::detail::forbidden_message;
use crate::projection::{
    ResourceTable, project_custom_resources, project_definitions, project_namespaces,
};
use crate::session_state::{SessionState, Viewing};

/// The text shown where listing Definitions is denied.
pub const CRD_DENIED: &str = "No permission to list CustomResourceDefinitions";

/// What the viewed list shows.
#[derive(Debug, PartialEq)]
pub enum ListOutcome {
    /// The list's columns over its objects; no rows is an empty list.
    Rows(ResourceTable),
    /// The list's kind has not resolved yet: Discovery is running or failed.
    Loading(String),
    /// The list's watch is denied by RBAC.
    Forbidden(String),
    /// The custom kind left the Catalog.
    Undefined(String),
    /// The API server does not serve the custom kind's preferred version.
    NotServed(String),
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
        Viewing::Namespaces => Resolved {
            outcome: ListOutcome::Rows(project_namespaces(&state.namespaces(), now)),
            stale: false,
        },
        Viewing::Builtin(kind) => {
            let registration = kind.registration();
            let watch = state.builtin_watch(*kind, Some(state.namespace()));
            let objects = watch.map(|watch| watch.objects()).unwrap_or_default();
            let outcome = match watch {
                Some(watch) if watch.status().is_forbidden() => {
                    ListOutcome::Forbidden(forbidden_message(registration.label))
                }
                _ => ListOutcome::Rows((registration.list)(&objects, now)),
            };
            Resolved {
                outcome,
                stale: watch.is_some_and(|watch| watch.status().is_stale()),
            }
        }
        Viewing::Definitions => {
            let crds = state.crds();
            let outcome = if state.crd_access_denied() {
                ListOutcome::Forbidden(CRD_DENIED.to_string())
            } else {
                ListOutcome::Rows(project_definitions(&crds.objects(), groups, now))
            };
            Resolved {
                outcome,
                stale: crds.status().is_stale(),
            }
        }
        Viewing::Custom(id) => custom(state, id, now),
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
        KindResource::Served(_) => {
            let watch = state.custom_watch(kind);
            let objects = watch.map(|watch| watch.objects()).unwrap_or_default();
            let outcome = match watch {
                Some(watch) if watch.status().is_forbidden() => {
                    ListOutcome::Forbidden(forbidden_message(&kind.plural))
                }
                _ => ListOutcome::Rows(project_custom_resources(kind, &objects, now)),
            };
            Resolved {
                outcome,
                stale: watch.is_some_and(|watch| watch.status().is_stale()),
            }
        }
    }
}
