//! Built-in kind views (#17-#19): each kind registers its list columns, its
//! status summary and its actions on the generic list/detail pipeline
//! (#11/#16) instead of shipping a hand-written view, plus an optional
//! related section listing another kind's objects tied to this one.

use astrolabe_kube::k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use astrolabe_kube::k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use astrolabe_kube::k8s_openapi::api::batch::v1::{CronJob, Job};
use astrolabe_kube::k8s_openapi::api::core::v1::{
    ConfigMap, Event, LimitRange, ObjectReference, Pod, ResourceQuota, Secret, Service,
};
use astrolabe_kube::k8s_openapi::api::discovery::v1::EndpointSlice;
use astrolabe_kube::k8s_openapi::api::networking::v1::Ingress;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::k8s_openapi::serde::de::DeserializeOwned;
use astrolabe_kube::kube::Resource;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use gpui_kit::SharedString;

use crate::projection::autoscaling::{hpa_detail, project_hpas};
use crate::projection::config::{
    config_map_detail, project_config_maps, project_secrets, secret_detail,
};
use crate::projection::endpoint_slices::{endpoint_slice_detail, project_endpoint_slices};
use crate::projection::events::{event_detail, event_involves, project_events};
use crate::projection::network::{
    ingress_detail, project_ingresses, project_services, service_detail,
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

/// Declares the built-in kinds once: each variant with its registration
/// static, generating [`BuiltinKind`], [`BuiltinKind::ALL`] and
/// [`BuiltinKind::registration`]. A variant without a static fails to
/// compile.
macro_rules! builtin_kinds {
    ($($(#[$meta:meta])* $variant:ident => $registration:ident,)*) => {
        /// A built-in kind with a registered view. Watched untyped like
        /// custom resources; the registration parses objects into their
        /// typed form.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum BuiltinKind {
            $($(#[$meta])* $variant,)*
        }

        const KIND_COUNT: usize = [$(stringify!($variant)),*].len();

        impl BuiltinKind {
            pub const ALL: [Self; KIND_COUNT] = [$(Self::$variant,)*];

            pub fn registration(self) -> &'static Registration {
                match self {
                    $(Self::$variant => &$registration,)*
                }
            }
        }
    };
}

builtin_kinds! {
    Pods => PODS,
    Deployments => DEPLOYMENTS,
    StatefulSets => STATEFULSETS,
    ReplicaSets => REPLICASETS,
    DaemonSets => DAEMONSETS,
    Jobs => JOBS,
    CronJobs => CRONJOBS,
    Events => EVENTS,
    Services => SERVICES,
    Ingresses => INGRESSES,
    ConfigMaps => CONFIG_MAPS,
    Secrets => SECRETS,
    ResourceQuotas => RESOURCE_QUOTAS,
    LimitRanges => LIMIT_RANGES,
    HorizontalPodAutoscalers => HORIZONTAL_POD_AUTOSCALERS,
    /// Shown only as a Service's related section, so it has no
    /// navigation entry.
    EndpointSlices => ENDPOINT_SLICES,
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
    /// The object a list row opens, when not the row's own object: an
    /// Event opens what it is about.
    pub row_target: Option<fn(&DynamicObject) -> Option<ObjectReference>>,
    /// Every cell of a row opens it, not only the first.
    pub open_all_cells: bool,
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

    /// The navigation item's element id, from the label.
    pub fn nav_id(self) -> SharedString {
        format!("nav-{}", self.registration().label.to_lowercase()).into()
    }

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
    row_target: None,
    open_all_cells: false,
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
    row_target: None,
    open_all_cells: false,
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
    row_target: None,
    open_all_cells: false,
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
    row_target: None,
    open_all_cells: false,
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
    row_target: None,
    open_all_cells: false,
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
    row_target: None,
    open_all_cells: false,
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
    row_target: None,
    open_all_cells: false,
};

static EVENTS: Registration = Registration {
    label: "Events",
    resource: || ApiResource::erase::<Event>(&()),
    list: |objects, now| typed_list(objects, now, project_events),
    detail: |object, now| Some(event_detail(&parse::<Event>(object)?, now)),
    actions: &[],
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: Some(|object| Some(parse::<Event>(object)?.involved_object)),
    open_all_cells: true,
};

static SERVICES: Registration = Registration {
    label: "Services",
    resource: || ApiResource::erase::<Service>(&()),
    list: |objects, now| typed_list(objects, now, project_services),
    detail: |object, now| {
        Some(service_detail(
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
    row_target: None,
    open_all_cells: false,
};

static INGRESSES: Registration = Registration {
    label: "Ingresses",
    resource: || ApiResource::erase::<Ingress>(&()),
    list: |objects, now| typed_list(objects, now, project_ingresses),
    detail: |object, now| Some(ingress_detail(&parse::<Ingress>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: None,
    open_all_cells: false,
};

static CONFIG_MAPS: Registration = Registration {
    label: "ConfigMaps",
    resource: || ApiResource::erase::<ConfigMap>(&()),
    list: |objects, now| typed_list(objects, now, project_config_maps),
    detail: |object, now| Some(config_map_detail(&parse::<ConfigMap>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: None,
    open_all_cells: false,
};

static SECRETS: Registration = Registration {
    label: "Secrets",
    resource: || ApiResource::erase::<Secret>(&()),
    list: |objects, now| typed_list(objects, now, project_secrets),
    detail: |object, now| Some(secret_detail(&parse::<Secret>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: None,
    open_all_cells: false,
};

static RESOURCE_QUOTAS: Registration = Registration {
    label: "ResourceQuotas",
    resource: || ApiResource::erase::<ResourceQuota>(&()),
    list: |objects, now| typed_list(objects, now, project_resource_quotas),
    detail: |object, now| Some(resource_quota_detail(&parse::<ResourceQuota>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: None,
    open_all_cells: false,
};

static LIMIT_RANGES: Registration = Registration {
    label: "LimitRanges",
    resource: || ApiResource::erase::<LimitRange>(&()),
    list: |objects, now| typed_list(objects, now, project_limit_ranges),
    detail: |object, now| Some(limit_range_detail(&parse::<LimitRange>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: None,
    open_all_cells: false,
};

static HORIZONTAL_POD_AUTOSCALERS: Registration = Registration {
    label: "HorizontalPodAutoscalers",
    resource: || ApiResource::erase::<HorizontalPodAutoscaler>(&()),
    list: |objects, now| typed_list(objects, now, project_hpas),
    detail: |object, now| Some(hpa_detail(&parse::<HorizontalPodAutoscaler>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: |_| "Nothing else is deleted.",
    related: None,
    row_target: None,
    open_all_cells: false,
};

static ENDPOINT_SLICES: Registration = Registration {
    label: "EndpointSlices",
    resource: || ApiResource::erase::<EndpointSlice>(&()),
    list: |objects, now| typed_list(objects, now, project_endpoint_slices),
    detail: |object, now| Some(endpoint_slice_detail(&parse::<EndpointSlice>(object)?, now)),
    actions: DEFAULT_ACTIONS,
    cascade: endpoint_slice_cascade,
    related: None,
    row_target: None,
    open_all_cells: false,
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

/// The EndpointSlice controller rebuilds a slice it manages, which names
/// its Service as controller; any other slice is simply gone.
fn endpoint_slice_cascade(slice: &DynamicObject) -> &'static str {
    let controlled = slice
        .metadata
        .owner_references
        .iter()
        .flatten()
        .any(|owner| owner.controller == Some(true));
    if controlled {
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
