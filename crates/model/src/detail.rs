//! Detail resolution (#97): what a detail tab shows, read from a
//! `SessionState`'s watches with no GPUI view. A target resolves to
//! its model, the object its actions address, the delete's cascade, its
//! actions and its Related section, whose outcome (hidden, rows, empty,
//! forbidden) is decided here; the panel renders the answer. The watches a
//! target reads are named here too, so the panel starts exactly those.

use crate::builtin::{Action, BuiltinKind, DEFAULT_ACTIONS};
use crate::catalog::{CustomKind, KindResource};
use crate::projection::{
    DetailModel, ResourceTable, custom_resource_detail, definition_detail, namespace_detail,
};
use astrolabe_kube::cluster::{ClusterError, ClusterErrorKind};
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, ObjectReference};
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::ResourceExt;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::{ObjectKey, ObjectRef};

use crate::session::{CustomKindId, WatchReading, Watched};
use crate::session_state::SessionState;

/// What a detail tab shows: one object, addressed in its list's store.
/// Custom resources are version-free (#41): each resolve reads the watch at
/// the kind's current preferred version, so a version change keeps the tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DetailTarget {
    Namespace { name: String },
    Definition { name: String },
    Builtin { kind: BuiltinKind, key: ObjectKey },
    Custom { id: CustomKindId, key: ObjectKey },
}

/// Why an involved object cannot be opened as a detail target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceError {
    /// The reference has no name.
    MissingName { kind: String },
    /// The reference's kind is neither built in nor a registered custom kind.
    UnregisteredKind { kind: String, name: String },
}

impl std::fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingName { kind } => {
                write!(f, "Cannot open involved object {kind}/: name is missing")
            }
            Self::UnregisteredKind { kind, name } => write!(
                f,
                "Cannot open involved object {kind}/{name}: kind is not registered"
            ),
        }
    }
}

impl std::error::Error for ReferenceError {}

impl DetailTarget {
    /// The object `reference` names (an Event's involved object): a
    /// built-in kind, a Namespace, a CRD or a custom kind. A namespaced
    /// object without a namespace of its own is in `namespace`.
    ///
    /// # Errors
    ///
    /// The reference has no name, or its kind is neither built in nor
    /// registered.
    pub fn by_reference(
        reference: ObjectReference,
        namespace: Option<String>,
        state: &SessionState,
    ) -> Result<Self, ReferenceError> {
        let kind = reference.kind.unwrap_or_default();
        let name = reference.name.unwrap_or_default();
        if name.is_empty() {
            return Err(ReferenceError::MissingName { kind });
        }
        let namespaced_key =
            || ObjectKey::new(reference.namespace.clone().or(namespace), name.clone());
        let api_version = reference.api_version.clone();
        if let Some(kind) = BuiltinKind::from_kind(&kind)
            && api_version
                .as_ref()
                .is_none_or(|version| *version == (kind.registration().resource)().api_version)
        {
            return Ok(Self::Builtin {
                kind,
                key: namespaced_key(),
            });
        }
        match (kind.as_str(), api_version.as_deref()) {
            ("Namespace", None | Some("v1")) => Ok(Self::Namespace { name }),
            ("CustomResourceDefinition", None | Some("apiextensions.k8s.io/v1")) => {
                Ok(Self::Definition { name })
            }
            _ => {
                let group = api_version
                    .and_then(|version| version.split_once('/').map(|(group, _)| group.to_string()))
                    .unwrap_or_default();
                let id = CustomKindId { group, kind };
                if let Some(custom) = state.custom_kind(&id) {
                    let key = if custom.namespaced {
                        namespaced_key()
                    } else {
                        ObjectKey::cluster_scoped(name)
                    };
                    Ok(Self::Custom { id, key })
                } else {
                    Err(ReferenceError::UnregisteredKind {
                        kind: id.kind,
                        name,
                    })
                }
            }
        }
    }
}

impl DetailTarget {
    /// The object an operation on this target addresses, while it is still
    /// in its watch; `None` while there is none (loading, failed, denied, gone
    /// and the rest: see `resolve`). Looks the object up without
    /// projecting it.
    pub fn object(&self, state: &SessionState) -> Option<ObjectRef> {
        self.lookup(state).ok().map(|(object, _)| object)
    }

    /// The object in its watch and what addresses it, or the outcome that
    /// says why there is none (never `Shown`).
    fn lookup<'a>(&self, state: &'a SessionState) -> Result<(ObjectRef, Shown<'a>), DetailOutcome> {
        let (resource, key, shown) = match self {
            Self::Namespace { name } => (
                ApiResource::erase::<Namespace>(&()),
                ObjectKey::cluster_scoped(name.clone()),
                Shown::Namespace(in_watch(Some(state.namespaces_watch()), None, name)?),
            ),
            Self::Definition { name } => (
                ApiResource::erase::<CustomResourceDefinition>(&()),
                ObjectKey::cluster_scoped(name.clone()),
                Shown::Definition(in_watch(Some(state.crds()), None, name)?),
            ),
            Self::Builtin { kind, key } => {
                let watch = state.builtin_watch(*kind, key.namespace.as_deref());
                let object = in_watch(watch, key.namespace.as_deref(), &key.name)?;
                (
                    (kind.registration().resource)(),
                    key.clone(),
                    Shown::Builtin(*kind, object),
                )
            }
            Self::Custom { id, key } => {
                let kind = state.custom_kind(id).ok_or(DetailOutcome::Undefined)?;
                let resource = match &kind.resource {
                    KindResource::Served(resource) => resource,
                    KindResource::Undiscovered => return Err(DetailOutcome::Loading),
                    KindResource::NotServed => return Err(DetailOutcome::NotServed),
                };
                let namespace = key.namespace.as_deref();
                let object = in_watch(
                    state.custom_object_watch(kind, namespace),
                    namespace,
                    &key.name,
                )?;
                (resource.clone(), key.clone(), Shown::Custom(kind, object))
            }
        };
        Ok((ObjectRef { resource, key }, shown))
    }
}

/// `name` in `watch` by the one rule every reader follows
/// (`Watched::reading`): loading, failed or forbidden until the watch holds
/// a snapshot, then gone when the object is absent from it.
fn in_watch<'a, K: ResourceExt>(
    watch: Option<&'a Watched<K>>,
    namespace: Option<&str>,
    name: &str,
) -> Result<&'a K, DetailOutcome> {
    match (Watched::reading(watch), watch) {
        (WatchReading::Rows, Some(watch)) => watch.get(namespace, name).ok_or(DetailOutcome::Gone),
        (WatchReading::Failed(error), _) => Err(DetailOutcome::Failed(error.cloned())),
        (WatchReading::Forbidden, _) => Err(DetailOutcome::Forbidden),
        (WatchReading::Loading | WatchReading::Rows, _) => Err(DetailOutcome::Loading),
    }
}

/// A target's object as its watch holds it.
enum Shown<'a> {
    Namespace(&'a Namespace),
    Definition(&'a CustomResourceDefinition),
    Builtin(BuiltinKind, &'a DynamicObject),
    Custom(&'a CustomKind, &'a DynamicObject),
}

/// What a detail tab shows, as `ListOutcome` is for a list.
#[derive(Debug)]
pub enum DetailOutcome {
    /// The object, as of `now`.
    Shown(Box<Resolved>),
    /// Nothing to show yet: the object's watch has no snapshot, or its
    /// custom kind is not discovered yet.
    Loading,
    /// The watch failed or ended before its first snapshot. The error is
    /// none when the watch ended without one.
    Failed(Option<ClusterError>),
    /// The watch is denied by RBAC.
    Forbidden,
    /// The custom kind left the Catalog.
    Undefined,
    /// The API server does not serve the custom kind's preferred version.
    NotServed,
    /// The object is in the watch, but its kind's projection cannot parse it.
    Unreadable,
    /// The watch holds a snapshot, live or stale, without the object.
    Gone,
}

impl DetailOutcome {
    /// The shown object, if there is one.
    pub fn shown(self) -> Option<Resolved> {
        match self {
            Self::Shown(resolved) => Some(*resolved),
            _ => None,
        }
    }
}

impl DetailOutcome {
    /// What the panel shows for `name` when there is no object: an element
    /// id and the text. `None` for a shown object.
    pub fn notice(&self, name: &str) -> Option<(&'static str, String)> {
        Some(match self {
            Self::Shown(_) => return None,
            Self::Loading => ("detail-loading", format!("Loading {name}…")),
            Self::Failed(error) => ("detail-failed", failed_message(error.as_ref())),
            Self::Forbidden => (
                "detail-forbidden",
                format!("No permission to read {name} here"),
            ),
            Self::Undefined => ("detail-undefined", format!("{name} is no longer defined")),
            Self::NotServed => (
                "detail-not-served",
                format!("{name}'s kind is not served by the API server"),
            ),
            Self::Unreadable => ("detail-unreadable", format!("Cannot read {name}")),
            Self::Gone => ("detail-gone", format!("{name} no longer exists")),
        })
    }
}

/// The text shown where a detail's watch failed before its first snapshot.
pub fn failed_message(error: Option<&ClusterError>) -> String {
    match error {
        None => "The watch ended before the object loaded".to_string(),
        Some(error) => match error.kind() {
            ClusterErrorKind::Connect => format!("Cannot connect to the cluster: {error}"),
            ClusterErrorKind::Unreachable => format!("Cannot reach the cluster: {error}"),
            _ => format!("Cannot load the object: {error}"),
        },
    }
}

/// The object as of `now`, with what its actions need.
#[derive(Debug)]
pub struct Resolved {
    pub model: DetailModel,
    pub object: ObjectRef,
    pub cascade: String,
    pub actions: &'static [Action],
    /// `None` when the kind has no Related section or hides it.
    pub related: Option<RelatedRows>,
}

/// A shown Related section as of `now`.
#[derive(Debug)]
pub struct RelatedRows {
    pub title: &'static str,
    pub outcome: RelatedOutcome,
}

/// What a shown Related section holds.
#[derive(Debug, PartialEq)]
pub enum RelatedOutcome {
    /// The related kind's list columns over the selected objects.
    Rows(ResourceTable),
    /// Nothing is selected: "No {title}".
    Empty(String),
    /// The related kind's watch has no snapshot yet.
    Loading(String),
    /// The related kind's watch failed or ended before its first snapshot.
    Failed(Option<ClusterError>),
    /// The related kind's watch is denied by RBAC.
    Forbidden(String),
}

/// The text shown where listing `label` (a kind's plural label) is denied.
pub fn forbidden_message(label: &str) -> String {
    format!("No permission to list {} here", label.to_lowercase())
}

/// A built-in watch a detail reads: `kind`'s objects in `namespace` (`None`
/// when cluster-scoped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeededWatch {
    pub kind: BuiltinKind,
    pub namespace: Option<String>,
}

/// The built-in watches `target` reads: the shown kind's, then its Related
/// kind's, in the target's namespace. Other targets read the
/// state's own watches, or a custom kind's watch that follows its
/// preferred version.
pub fn watches(target: &DetailTarget) -> Vec<NeededWatch> {
    let DetailTarget::Builtin { kind, key } = target else {
        return Vec::new();
    };
    std::iter::once(*kind)
        .chain(
            kind.registration()
                .related
                .as_ref()
                .map(|related| related.kind),
        )
        .map(|kind| NeededWatch {
            kind,
            namespace: key.namespace.clone(),
        })
        .collect()
}

/// `target` as of `now`.
pub fn resolve(target: &DetailTarget, state: &SessionState, now: Timestamp) -> DetailOutcome {
    let (object, shown) = match target.lookup(state) {
        Ok(found) => found,
        Err(outcome) => return outcome,
    };
    let resolved = match shown {
        Shown::Namespace(namespace) => Resolved {
            model: namespace_detail(namespace, now),
            object,
            cascade: "Deleting a Namespace deletes every object in it.".to_string(),
            actions: DEFAULT_ACTIONS,
            related: None,
        },
        Shown::Definition(crd) => Resolved {
            model: definition_detail(crd, now),
            object,
            cascade: format!(
                "Deleting a CustomResourceDefinition also deletes every {} object in the cluster.",
                crd.spec.names.kind
            ),
            actions: DEFAULT_ACTIONS,
            related: None,
        },
        Shown::Builtin(kind, shown) => {
            let registration = kind.registration();
            let namespace = object.key.namespace.as_deref();
            let related = registration.related.as_ref().and_then(|related| {
                let watch = state.builtin_watch(related.kind, namespace);
                let candidates = watch
                    .map(|watch| watch.objects().collect::<Vec<_>>())
                    .unwrap_or_default();
                let selected = (related.select)(shown, &candidates);
                let outcome = match Watched::reading(watch) {
                    WatchReading::Loading => RelatedOutcome::Loading(format!(
                        "Loading {}…",
                        related.title.to_lowercase()
                    )),
                    WatchReading::Failed(error) => RelatedOutcome::Failed(error.cloned()),
                    WatchReading::Forbidden => {
                        RelatedOutcome::Forbidden(forbidden_message(related.title))
                    }
                    WatchReading::Rows if related.hide_when_empty && selected.is_empty() => {
                        return None;
                    }
                    WatchReading::Rows => {
                        let table = (related.kind.registration().list)(&selected, now);
                        if table.rows.is_empty() {
                            RelatedOutcome::Empty(format!("No {}", related.title.to_lowercase()))
                        } else {
                            RelatedOutcome::Rows(table)
                        }
                    }
                };
                Some(RelatedRows {
                    title: related.title,
                    outcome,
                })
            });
            let Some(model) = (registration.detail)(shown, now) else {
                return DetailOutcome::Unreadable;
            };
            Resolved {
                model,
                object,
                cascade: (registration.cascade)(shown).to_string(),
                actions: registration.actions,
                related,
            }
        }
        Shown::Custom(kind, shown) => Resolved {
            model: custom_resource_detail(kind, shown, now),
            object,
            cascade: "Objects that list it in ownerReferences will be garbage-collected."
                .to_string(),
            actions: DEFAULT_ACTIONS,
            related: None,
        },
    };
    DetailOutcome::Shown(Box::new(resolved))
}
