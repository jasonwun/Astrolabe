//! Detail resolution (#97): what a detail tab shows, read from the session's
//! watches with no GPUI view. A target resolves to its model, the object its
//! actions address, the delete's cascade, its actions and its Related
//! section, whose outcome (hidden, rows, empty, forbidden) is decided here;
//! the panel renders the answer. The watches a target reads are named here
//! too, so the panel starts exactly those.

use astrolabe_kube::crd::{CustomKind, KindResource};
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, ObjectReference};
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;

use crate::builtin::{Action, BuiltinKind, DEFAULT_ACTIONS};
use crate::projection::{
    DetailModel, ResourceTable, custom_resource_detail, definition_detail, namespace_detail,
};
use crate::session::{CustomKindId, ObjectRef, Session};
use crate::store::ObjectKey;

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

impl DetailTarget {
    /// The object `reference` names (an Event's involved object): a
    /// built-in kind, a Namespace, a CRD or a custom kind. A namespaced
    /// object without a namespace of its own is in `namespace`.
    pub fn by_reference(
        reference: ObjectReference,
        namespace: Option<String>,
        session: &Session,
    ) -> Result<Self, String> {
        let kind = reference.kind.unwrap_or_default();
        let name = reference.name.unwrap_or_default();
        let target = format!("{kind}/{name}");
        if name.is_empty() {
            return Err(format!(
                "Cannot open involved object {target}: name is missing"
            ));
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
                if let Some(custom) = session.custom_kind(&id) {
                    let key = if custom.namespaced {
                        namespaced_key()
                    } else {
                        ObjectKey::cluster_scoped(name)
                    };
                    Ok(Self::Custom { id, key })
                } else {
                    Err(format!(
                        "Cannot open involved object {target}: kind is not registered"
                    ))
                }
            }
        }
    }
}

impl DetailTarget {
    /// The object an operation on this target addresses, while it is still
    /// in its watch; `None` once it is gone. Looks the object up without
    /// projecting it.
    pub fn object(&self, session: &Session) -> Option<ObjectRef> {
        self.find(session).map(|(object, _)| object)
    }

    /// The object in its watch and what addresses it.
    fn find<'a>(&self, session: &'a Session) -> Option<(ObjectRef, Shown<'a>)> {
        let (resource, key, shown) = match self {
            Self::Namespace { name } => (
                ApiResource::erase::<Namespace>(&()),
                ObjectKey::cluster_scoped(name.clone()),
                Shown::Namespace(session.namespace_object(name)?),
            ),
            Self::Definition { name } => (
                ApiResource::erase::<CustomResourceDefinition>(&()),
                ObjectKey::cluster_scoped(name.clone()),
                Shown::Definition(session.crds().get(None, name)?),
            ),
            Self::Builtin { kind, key } => {
                let object = session.builtin_object(*kind, key.namespace.as_deref()?, &key.name)?;
                (
                    (kind.registration().resource)(),
                    key.clone(),
                    Shown::Builtin(*kind, object),
                )
            }
            Self::Custom { id, key } => {
                let kind = session.custom_kind(id)?;
                let KindResource::Served(resource) = &kind.resource else {
                    return None;
                };
                let namespace = key.namespace.as_deref();
                let watch = session.custom_object_watch_key(kind, namespace);
                let object = session.object_watch(&watch)?.get(namespace, &key.name)?;
                (resource.clone(), key.clone(), Shown::Custom(kind, object))
            }
        };
        Some((ObjectRef { resource, key }, shown))
    }
}

/// A target's object as its watch holds it.
enum Shown<'a> {
    Namespace(&'a Namespace),
    Definition(&'a CustomResourceDefinition),
    Builtin(BuiltinKind, &'a DynamicObject),
    Custom(&'a CustomKind, &'a DynamicObject),
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
    /// The related kind's watch is denied by RBAC.
    Forbidden(String),
}

/// The text shown where listing `label` (a kind's plural label) is denied.
pub fn forbidden_message(label: &str) -> String {
    format!("No permission to list {} here", label.to_lowercase())
}

/// A built-in watch a detail reads: `kind`'s objects in `namespace`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeededWatch {
    pub kind: BuiltinKind,
    pub namespace: String,
}

/// The built-in watches `target` reads: the shown kind's, then its Related
/// kind's, in the target's namespace. Other targets read the session's own
/// watches, or a custom kind's watch that follows its preferred version.
pub fn watches(target: &DetailTarget) -> Vec<NeededWatch> {
    let DetailTarget::Builtin { kind, key } = target else {
        return Vec::new();
    };
    let Some(namespace) = &key.namespace else {
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
            namespace: namespace.clone(),
        })
        .collect()
}

/// `target` as of `now`, or `None` once it no longer exists.
pub fn resolve(target: &DetailTarget, session: &Session, now: Timestamp) -> Option<Resolved> {
    let (object, shown) = target.find(session)?;
    match shown {
        Shown::Namespace(namespace) => Some(Resolved {
            model: namespace_detail(namespace, now),
            object,
            cascade: "Deleting a Namespace deletes every object in it.".to_string(),
            actions: DEFAULT_ACTIONS,
            related: None,
        }),
        Shown::Definition(crd) => Some(Resolved {
            model: definition_detail(crd, now),
            object,
            cascade: format!(
                "Deleting a CustomResourceDefinition also deletes every {} object in the cluster.",
                crd.spec.names.kind
            ),
            actions: DEFAULT_ACTIONS,
            related: None,
        }),
        Shown::Builtin(kind, shown) => {
            let registration = kind.registration();
            let namespace = object.key.namespace.clone()?;
            let namespace = namespace.as_str();
            let related = registration.related.as_ref().and_then(|related| {
                let watch = session.builtin_watch(related.kind, namespace);
                let candidates = watch.map(|watch| watch.objects()).unwrap_or_default();
                let selected = (related.select)(shown, &candidates);
                let outcome = if watch.is_some_and(|watch| watch.forbidden()) {
                    RelatedOutcome::Forbidden(forbidden_message(related.title))
                } else if related.hide_when_empty && selected.is_empty() {
                    return None;
                } else {
                    let table = (related.kind.registration().list)(&selected, now);
                    if table.rows.is_empty() {
                        RelatedOutcome::Empty(format!("No {}", related.title.to_lowercase()))
                    } else {
                        RelatedOutcome::Rows(table)
                    }
                };
                Some(RelatedRows {
                    title: related.title,
                    outcome,
                })
            });
            Some(Resolved {
                model: (registration.detail)(shown, now)?,
                object,
                cascade: (registration.cascade)(shown).to_string(),
                actions: registration.actions,
                related,
            })
        }
        Shown::Custom(kind, shown) => Some(Resolved {
            model: custom_resource_detail(kind, shown, now),
            object,
            cascade: "Objects that list it in ownerReferences will be garbage-collected."
                .to_string(),
            actions: DEFAULT_ACTIONS,
            related: None,
        }),
    }
}
