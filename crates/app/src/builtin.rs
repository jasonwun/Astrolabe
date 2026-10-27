//! Built-in kind views (#17-#19): each kind registers its list columns, its
//! status summary and its actions on the generic list/detail pipeline
//! (#11/#16) instead of shipping a hand-written view, plus an optional
//! related section listing another kind's objects tied to this one.

use astrolabe_kube::k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use astrolabe_kube::k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use astrolabe_kube::k8s_openapi::api::batch::v1::{CronJob, Job};
use astrolabe_kube::k8s_openapi::api::core::v1::{
    ConfigMap, Event, LimitRange, Pod, ResourceQuota, Secret, Service,
};
use astrolabe_kube::k8s_openapi::api::discovery::v1::EndpointSlice;
use astrolabe_kube::k8s_openapi::api::networking::v1::Ingress;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::k8s_openapi::serde::de::DeserializeOwned;
use astrolabe_kube::kube::Resource;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;

use crate::projection::autoscaling::{HPA_COLUMN_WEIGHTS, hpa_detail, project_hpas};
use crate::projection::config::{
    config_map_detail, project_config_maps, project_secrets, secret_detail,
};
use crate::projection::endpoint_slices::{endpoint_slice_detail, project_endpoint_slices};
use crate::projection::events::{event_detail, event_involves, project_events};
use crate::projection::network::{
    ingress_detail, project_ingresses, project_services, service_detail_with_raw,
};
use crate::projection::quotas::{
    limit_range_detail, project_limit_ranges, project_resource_quotas, resource_quota_detail,
};
use crate::projection::workloads::{
    cronjob_detail, daemonset_detail, deployment_detail, job_detail, pod_detail, project_cronjobs,
    project_daemonsets, project_deployments, project_jobs, project_pods, project_replicasets,
    project_statefulsets, replicaset_detail, selector_matches, statefulset_detail,
};
use crate::projection::{DetailModel, ResourceTable};

/// A built-in kind with a registered view. Watched untyped like custom
/// resources; the registration parses objects into their typed form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinKind {
    Pods,
    Deployments,
    StatefulSets,
    ReplicaSets,
    DaemonSets,
    Jobs,
    CronJobs,
    Events,
    Services,
    Ingresses,
    ConfigMaps,
    Secrets,
    ResourceQuotas,
    LimitRanges,
    HorizontalPodAutoscalers,
    /// Shown only as a Service's related section, so it has no
    /// navigation entry.
    EndpointSlices,
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
    /// Set a workload's desired replicas.
    Scale,
    RolloutRestart,
    Suspend,
    Trigger,
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
    /// The list's column weights, one per column; `None` takes
    /// [`default_column_weights`](crate::projection::default_column_weights).
    pub column_weights: Option<&'static [f32]>,
}

/// A detail's related section: `kind`'s objects in the same namespace that
/// `select` ties to the shown object, as `kind`'s list projects them.
pub struct Related {
    pub title: &'static str,
    pub kind: BuiltinKind,
    pub select: for<'a> fn(&DynamicObject, &[&'a DynamicObject]) -> Vec<&'a DynamicObject>,
    /// Leave the section out, title included, when nothing is selected,
    /// instead of saying "No …". A forbidden watch still says so.
    pub hide_when_empty: bool,
}

impl BuiltinKind {
    /// Navigation sections, in order, each with its kinds in order.
    pub const NAVIGATION: [(&'static str, &'static [BuiltinKind]); 4] = [
        (
            "Workloads",
            &[
                BuiltinKind::Pods,
                BuiltinKind::Deployments,
                BuiltinKind::StatefulSets,
                BuiltinKind::DaemonSets,
                BuiltinKind::ReplicaSets,
                BuiltinKind::Jobs,
                BuiltinKind::CronJobs,
            ],
        ),
        ("Network", &[BuiltinKind::Services, BuiltinKind::Ingresses]),
        // Freelens's Config order.
        (
            "Config",
            &[
                BuiltinKind::ConfigMaps,
                BuiltinKind::Secrets,
                BuiltinKind::ResourceQuotas,
                BuiltinKind::LimitRanges,
                BuiltinKind::HorizontalPodAutoscalers,
            ],
        ),
        ("Events", &[BuiltinKind::Events]),
    ];

    pub fn registration(self) -> &'static Registration {
        match self {
            BuiltinKind::Pods => &PODS,
            BuiltinKind::Deployments => &DEPLOYMENTS,
            BuiltinKind::StatefulSets => &STATEFULSETS,
            BuiltinKind::ReplicaSets => &REPLICASETS,
            BuiltinKind::DaemonSets => &DAEMONSETS,
            BuiltinKind::Jobs => &JOBS,
            BuiltinKind::CronJobs => &CRONJOBS,
            BuiltinKind::Events => &EVENTS,
            BuiltinKind::Services => &SERVICES,
            BuiltinKind::Ingresses => &INGRESSES,
            BuiltinKind::ConfigMaps => &CONFIG_MAPS,
            BuiltinKind::Secrets => &SECRETS,
            BuiltinKind::ResourceQuotas => &RESOURCE_QUOTAS,
            BuiltinKind::LimitRanges => &LIMIT_RANGES,
            BuiltinKind::HorizontalPodAutoscalers => &HORIZONTAL_POD_AUTOSCALERS,
            BuiltinKind::EndpointSlices => &ENDPOINT_SLICES,
        }
    }

    pub const ALL: [Self; 16] = [
        Self::Pods,
        Self::Deployments,
        Self::StatefulSets,
        Self::ReplicaSets,
        Self::DaemonSets,
        Self::Jobs,
        Self::CronJobs,
        Self::Events,
        Self::Services,
        Self::Ingresses,
        Self::ConfigMaps,
        Self::Secrets,
        Self::ResourceQuotas,
        Self::LimitRanges,
        Self::HorizontalPodAutoscalers,
        Self::EndpointSlices,
    ];

    /// The singular kind, as titles name it.
    pub fn kind(self) -> String {
        (self.registration().resource)().kind
    }

    pub fn from_kind(kind: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.kind() == kind)
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
        hide_when_empty: false,
    }),
    column_weights: None,
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
        hide_when_empty: false,
    }),
    column_weights: None,
};

static STATEFULSETS: Registration = Registration {
    label: "StatefulSets",
    resource: || ApiResource::erase::<StatefulSet>(&()),
    list: |objects, now| typed_list(objects, now, project_statefulsets),
    detail: |object, now| Some(statefulset_detail(&parse::<StatefulSet>(object)?, now)),
    actions: &[
        Action::Scale,
        Action::RolloutRestart,
        Action::Edit,
        Action::Delete,
    ],
    cascade: |_| "Its Pods will be deleted too.",
    related: Some(Related {
        title: "Pods",
        kind: BuiltinKind::Pods,
        select: |object, pods| owned_by(object, pods, "StatefulSet"),
        hide_when_empty: false,
    }),
    column_weights: None,
};

static REPLICASETS: Registration = Registration {
    label: "ReplicaSets",
    resource: || ApiResource::erase::<ReplicaSet>(&()),
    list: |objects, now| typed_list(objects, now, project_replicasets),
    detail: |object, now| Some(replicaset_detail(&parse::<ReplicaSet>(object)?, now)),
    actions: &[Action::Scale, Action::Edit, Action::Delete],
    cascade: |_| "Its Pods will be deleted too.",
    related: Some(Related {
        title: "Pods",
        kind: BuiltinKind::Pods,
        select: |object, pods| owned_by(object, pods, "ReplicaSet"),
        hide_when_empty: false,
    }),
    column_weights: None,
};

static DAEMONSETS: Registration = Registration {
    label: "DaemonSets",
    resource: || ApiResource::erase::<DaemonSet>(&()),
    list: |objects, now| typed_list(objects, now, project_daemonsets),
    detail: |object, now| Some(daemonset_detail(&parse::<DaemonSet>(object)?, now)),
    actions: &[Action::RolloutRestart, Action::Edit, Action::Delete],
    cascade: |_| "Its Pods will be deleted too.",
    related: Some(Related {
        title: "Pods",
        kind: BuiltinKind::Pods,
        select: |object, pods| owned_by(object, pods, "DaemonSet"),
        hide_when_empty: false,
    }),
    column_weights: None,
};

static JOBS: Registration = Registration {
    label: "Jobs",
    resource: || ApiResource::erase::<Job>(&()),
    list: |objects, now| typed_list(objects, now, project_jobs),
    detail: |object, now| Some(job_detail(&parse::<Job>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Its Pods will be deleted too.",
    related: Some(Related {
        title: "Pods",
        kind: BuiltinKind::Pods,
        select: |object, pods| owned_by(object, pods, "Job"),
        hide_when_empty: false,
    }),
    column_weights: None,
};

static CRONJOBS: Registration = Registration {
    label: "CronJobs",
    resource: || ApiResource::erase::<CronJob>(&()),
    list: |objects, now| typed_list(objects, now, project_cronjobs),
    detail: |object, now| Some(cronjob_detail(&parse::<CronJob>(object)?, now)),
    actions: &[Action::Suspend, Action::Trigger, Action::Delete],
    cascade: |_| "Its Jobs and their Pods will be deleted too.",
    related: Some(Related {
        title: "Jobs",
        kind: BuiltinKind::Jobs,
        select: |object, jobs| owned_by(object, jobs, "CronJob"),
        hide_when_empty: false,
    }),
    column_weights: None,
};

static EVENTS: Registration = Registration {
    label: "Events",
    resource: || ApiResource::erase::<Event>(&()),
    list: |objects, now| typed_list(objects, now, project_events),
    detail: |object, now| Some(event_detail(&parse::<Event>(object)?, now)),
    actions: &[],
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: None,
};

static SERVICES: Registration = Registration {
    label: "Services",
    resource: || ApiResource::erase::<Service>(&()),
    list: |objects, now| typed_list(objects, now, project_services),
    detail: |object, now| {
        Some(service_detail_with_raw(
            &parse::<Service>(object)?,
            &object.data,
            now,
        ))
    },
    actions: &[Action::PortForward, Action::Edit, Action::Delete],
    cascade: service_cascade,
    related: Some(Related {
        title: "Endpoint Slices",
        kind: BuiltinKind::EndpointSlices,
        select: |object, slices| owned_by(object, slices, "Service"),
        hide_when_empty: true,
    }),
    column_weights: None,
};

static INGRESSES: Registration = Registration {
    label: "Ingresses",
    resource: || ApiResource::erase::<Ingress>(&()),
    list: |objects, now| typed_list(objects, now, project_ingresses),
    detail: |object, now| Some(ingress_detail(&parse::<Ingress>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: None,
};

static CONFIG_MAPS: Registration = Registration {
    label: "ConfigMaps",
    resource: || ApiResource::erase::<ConfigMap>(&()),
    list: |objects, now| typed_list(objects, now, project_config_maps),
    detail: |object, now| Some(config_map_detail(&parse::<ConfigMap>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: None,
};

static SECRETS: Registration = Registration {
    label: "Secrets",
    resource: || ApiResource::erase::<Secret>(&()),
    list: |objects, now| typed_list(objects, now, project_secrets),
    detail: |object, now| Some(secret_detail(&parse::<Secret>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: None,
};

static RESOURCE_QUOTAS: Registration = Registration {
    label: "ResourceQuotas",
    resource: || ApiResource::erase::<ResourceQuota>(&()),
    list: |objects, now| typed_list(objects, now, project_resource_quotas),
    detail: |object, now| Some(resource_quota_detail(&parse::<ResourceQuota>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: None,
};

static LIMIT_RANGES: Registration = Registration {
    label: "LimitRanges",
    resource: || ApiResource::erase::<LimitRange>(&()),
    list: |objects, now| typed_list(objects, now, project_limit_ranges),
    detail: |object, now| Some(limit_range_detail(&parse::<LimitRange>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: None,
};

static HORIZONTAL_POD_AUTOSCALERS: Registration = Registration {
    label: "HorizontalPodAutoscalers",
    resource: || ApiResource::erase::<HorizontalPodAutoscaler>(&()),
    list: |objects, now| typed_list(objects, now, project_hpas),
    detail: |object, now| Some(hpa_detail(&parse::<HorizontalPodAutoscaler>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    column_weights: Some(HPA_COLUMN_WEIGHTS),
};

static ENDPOINT_SLICES: Registration = Registration {
    label: "EndpointSlices",
    resource: || ApiResource::erase::<EndpointSlice>(&()),
    list: |objects, now| typed_list(objects, now, project_endpoint_slices),
    detail: |object, now| Some(endpoint_slice_detail(&parse::<EndpointSlice>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: endpoint_slice_cascade,
    related: None,
    column_weights: None,
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

/// The EndpointSlice controller rebuilds a slice its Service owns; a
/// slice nobody owns is simply gone.
fn endpoint_slice_cascade(slice: &DynamicObject) -> &'static str {
    if slice
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| !owners.is_empty())
    {
        "A controller that owns this EndpointSlice will recreate it."
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

fn owned_by<'a>(
    owner: &DynamicObject,
    objects: &[&'a DynamicObject],
    kind: &str,
) -> Vec<&'a DynamicObject> {
    objects
        .iter()
        .copied()
        .filter(|object| {
            object
                .metadata
                .owner_references
                .iter()
                .flatten()
                .any(|reference| {
                    reference.kind == kind
                        && Some(&reference.name) == owner.metadata.name.as_ref()
                        && owner
                            .metadata
                            .uid
                            .as_ref()
                            .is_none_or(|uid| &reference.uid == uid)
                })
        })
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
