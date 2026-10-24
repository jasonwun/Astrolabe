//! Built-in kind views (#17, #18): each kind registers its list columns, its
//! status summary and its actions on the generic list/detail pipeline
//! (#11/#16) instead of shipping a hand-written view, plus an optional
//! related section listing another kind's objects tied to this one.

use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::k8s_openapi::api::core::v1::{ConfigMap, Event, Pod, Secret, Service};
use astrolabe_kube::k8s_openapi::api::networking::v1::Ingress;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::k8s_openapi::serde::de::DeserializeOwned;
use astrolabe_kube::kube::Resource;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;

use crate::projection::config::{
    config_map_detail, project_config_maps, project_secrets, secret_detail,
};
use crate::projection::events::{event_detail, event_involves, project_events};
use crate::projection::network::{
    ingress_detail, project_ingresses, project_services, service_detail,
};
use crate::projection::workloads::{
    deployment_detail, pod_detail, project_deployments, project_pods, selector_matches,
};
use crate::projection::{DetailModel, ResourceTable};

/// A built-in kind with a registered view. Watched untyped like custom
/// resources; the registration parses objects into their typed form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinKind {
    Pods,
    Deployments,
    /// Not in navigation yet: shown as Pods' related section.
    Events,
    Services,
    Ingresses,
    ConfigMaps,
    Secrets,
}

/// A detail-tab toolbar action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Follow a container's log (container picked when several).
    Logs,
    /// Open a shell in a container (container picked when several).
    Exec,
    /// Forward a localhost port to one of the Pod's declared ports, or to
    /// one of the Service's ports.
    PortForward,
    /// Set a Deployment's desired replicas.
    Scale,
    RolloutRestart,
    Edit,
    Delete,
}

/// The generic detail's actions: what every kind without its own set gets.
pub const DEFAULT_ACTIONS: &[Action] = &[Action::Edit, Action::Delete];

/// One kind's registration.
pub struct Registration {
    /// Navigation entry and list tab name.
    pub label: &'static str,
    pub resource: fn() -> ApiResource,
    pub list: fn(&[&DynamicObject], Timestamp) -> ResourceTable,
    pub detail: fn(&DynamicObject, Timestamp) -> Option<DetailModel>,
    pub actions: &'static [Action],
    /// What else deleting this object takes with it, for the confirmation.
    pub cascade: fn(&DynamicObject) -> &'static str,
    pub related: Option<Related>,
}

/// A detail's related section: `kind`'s objects in the same namespace that
/// `select` ties to the shown object, as `kind`'s list projects them.
pub struct Related {
    pub title: &'static str,
    pub kind: BuiltinKind,
    pub select: for<'a> fn(&DynamicObject, &[&'a DynamicObject]) -> Vec<&'a DynamicObject>,
}

impl BuiltinKind {
    /// Navigation sections, in order, each with its kinds in order.
    pub const NAVIGATION: [(&'static str, &'static [BuiltinKind]); 3] = [
        ("Workloads", &[BuiltinKind::Pods, BuiltinKind::Deployments]),
        ("Network", &[BuiltinKind::Services, BuiltinKind::Ingresses]),
        ("Config", &[BuiltinKind::ConfigMaps, BuiltinKind::Secrets]),
    ];

    pub fn registration(self) -> &'static Registration {
        match self {
            BuiltinKind::Pods => &PODS,
            BuiltinKind::Deployments => &DEPLOYMENTS,
            BuiltinKind::Events => &EVENTS,
            BuiltinKind::Services => &SERVICES,
            BuiltinKind::Ingresses => &INGRESSES,
            BuiltinKind::ConfigMaps => &CONFIG_MAPS,
            BuiltinKind::Secrets => &SECRETS,
        }
    }

    /// The singular kind, as titles name it.
    pub fn kind(self) -> String {
        (self.registration().resource)().kind
    }
}

static PODS: Registration = Registration {
    label: "Pods",
    resource: || ApiResource::erase::<Pod>(&()),
    list: |objects, now| typed_list(objects, now, project_pods),
    detail: |object, now| Some(pod_detail(&parse::<Pod>(object)?, now)),
    actions: &[
        Action::Logs,
        Action::Exec,
        Action::PortForward,
        Action::Edit,
        Action::Delete,
    ],
    cascade: pod_cascade,
    related: Some(Related {
        title: "Events",
        kind: BuiltinKind::Events,
        select: pod_events,
    }),
};

static DEPLOYMENTS: Registration = Registration {
    label: "Deployments",
    resource: || ApiResource::erase::<Deployment>(&()),
    list: |objects, now| typed_list(objects, now, project_deployments),
    detail: |object, now| Some(deployment_detail(&parse::<Deployment>(object)?, now)),
    actions: &[
        Action::Scale,
        Action::RolloutRestart,
        Action::Edit,
        Action::Delete,
    ],
    cascade: |_| "Its ReplicaSets and their Pods will be deleted too.",
    related: Some(Related {
        title: "Pods",
        kind: BuiltinKind::Pods,
        select: deployment_pods,
    }),
};

static EVENTS: Registration = Registration {
    label: "Events",
    resource: || ApiResource::erase::<Event>(&()),
    list: |objects, now| typed_list(objects, now, project_events),
    detail: |object, now| Some(event_detail(&parse::<Event>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
};

static SERVICES: Registration = Registration {
    label: "Services",
    resource: || ApiResource::erase::<Service>(&()),
    list: |objects, now| typed_list(objects, now, project_services),
    detail: |object, now| Some(service_detail(&parse::<Service>(object)?, now)),
    actions: &[Action::PortForward, Action::Edit, Action::Delete],
    cascade: service_cascade,
    related: None,
};

static INGRESSES: Registration = Registration {
    label: "Ingresses",
    resource: || ApiResource::erase::<Ingress>(&()),
    list: |objects, now| typed_list(objects, now, project_ingresses),
    detail: |object, now| Some(ingress_detail(&parse::<Ingress>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
};

static CONFIG_MAPS: Registration = Registration {
    label: "ConfigMaps",
    resource: || ApiResource::erase::<ConfigMap>(&()),
    list: |objects, now| typed_list(objects, now, project_config_maps),
    detail: |object, now| Some(config_map_detail(&parse::<ConfigMap>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
};

static SECRETS: Registration = Registration {
    label: "Secrets",
    resource: || ApiResource::erase::<Secret>(&()),
    list: |objects, now| typed_list(objects, now, project_secrets),
    detail: |object, now| Some(secret_detail(&parse::<Secret>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
};

/// The EndpointSlice controller owns the slices of a Service with a
/// selector, so they go with it; a selector-less Service's slices are
/// someone else's and stay.
fn service_cascade(service: &DynamicObject) -> &'static str {
    let selects = parse::<Service>(service)
        .and_then(|service| service.spec)
        .and_then(|spec| spec.selector)
        .is_some_and(|selector| !selector.is_empty());
    if selects {
        "Its EndpointSlices will be deleted too."
    } else {
        "Nothing else is deleted."
    }
}

/// Events whose involvedObject is the Pod.
fn pod_events<'a>(pod: &DynamicObject, events: &[&'a DynamicObject]) -> Vec<&'a DynamicObject> {
    events
        .iter()
        .copied()
        .filter(|event| {
            parse::<Event>(event).is_some_and(|e| event_involves(&e, "Pod", &pod.metadata))
        })
        .collect()
}

/// A Pod with an owner (ReplicaSet, Job, StatefulSet…) is replaced by it;
/// a bare Pod is simply gone.
fn pod_cascade(pod: &DynamicObject) -> &'static str {
    if pod
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| !owners.is_empty())
    {
        "A controller that owns this Pod will create a replacement."
    } else {
        "Nothing else is deleted."
    }
}

/// Pods the Deployment's selector matches. Owners are not checked, so a
/// bare Pod carrying the labels shows too.
fn deployment_pods<'a>(
    deployment: &DynamicObject,
    pods: &[&'a DynamicObject],
) -> Vec<&'a DynamicObject> {
    let Some(selector) = parse::<Deployment>(deployment)
        .and_then(|deployment| deployment.spec)
        .map(|spec| spec.selector)
    else {
        return Vec::new();
    };
    pods.iter()
        .copied()
        .filter(|pod| selector_matches(&selector, pod.metadata.labels.as_ref()))
        .collect()
}

fn parse<K: Resource + DeserializeOwned>(object: &DynamicObject) -> Option<K> {
    object.clone().try_parse().ok()
}

/// Objects that fail to parse as `K` are left out of the list.
fn typed_list<K: Resource + DeserializeOwned>(
    objects: &[&DynamicObject],
    now: Timestamp,
    project: fn(&[&K], Timestamp) -> ResourceTable,
) -> ResourceTable {
    let typed: Vec<K> = objects.iter().filter_map(|object| parse(object)).collect();
    project(&typed.iter().collect::<Vec<_>>(), now)
}
