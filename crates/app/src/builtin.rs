//! Built-in kind views (#17): each kind registers its list columns, its
//! status summary and its actions on the generic list/detail pipeline
//! (#11/#16) instead of shipping a hand-written view.

use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::k8s_openapi::api::core::v1::Pod;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::k8s_openapi::serde::de::DeserializeOwned;
use astrolabe_kube::kube::Resource;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;

use crate::projection::workloads::{
    deployment_detail, pod_detail, project_deployments, project_pods,
};
use crate::projection::{DetailModel, ResourceTable};

/// A built-in kind with a registered view. Watched untyped like custom
/// resources; the registration parses objects into their typed form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinKind {
    Pods,
    Deployments,
}

/// A detail-tab toolbar action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
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
    /// What else a delete takes with it, for the confirmation.
    pub cascade: &'static str,
}

impl BuiltinKind {
    /// Navigation order under Workloads.
    pub const ALL: [BuiltinKind; 2] = [BuiltinKind::Pods, BuiltinKind::Deployments];

    pub fn registration(self) -> &'static Registration {
        match self {
            BuiltinKind::Pods => &PODS,
            BuiltinKind::Deployments => &DEPLOYMENTS,
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
    actions: DEFAULT_ACTIONS,
    cascade: "A controller that owns this Pod will create a replacement.",
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
    cascade: "Its ReplicaSets and their Pods will be deleted too.",
};

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
