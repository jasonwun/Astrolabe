//! Built-in workload kinds on the generic pipeline (#17, #19).
//! P3 projections follow Freelens 14a491f, `workloads-*` components.

use std::collections::BTreeMap;

use astrolabe_kube::k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use astrolabe_kube::k8s_openapi::api::batch::v1::{CronJob, Job, JobSpec};
use astrolabe_kube::k8s_openapi::api::core::v1::{
    Container, ContainerState, ContainerStatus, Pod, PodTemplateSpec,
};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::{
    Column, Containers, DetailField, DetailItem, DetailModel, DetailSection, Forward, ForwardTo,
    ReplicaCounts, ResourceTable, SummaryTable, detail, format_age, list_table,
};

pub(crate) const NONE: &str = "<none>";

/// Name / Ready / Status / Restarts / IP / Node / Age, as `kubectl get pods
/// -o wide` prints them.
pub fn project_pods(pods: &[&Pod], now: Timestamp) -> ResourceTable {
    list_table(POD_COLUMNS, pods, now)
}

const POD_COLUMNS: &[Column<Pod>] = &[
    Column::NAME,
    Column::new("Ready", |pod, _| pod_state(pod).0),
    Column::new("Status", |pod, _| pod_state(pod).1),
    Column::new("Restarts", |pod, now| {
        let containers = pod
            .status
            .as_ref()
            .and_then(|status| status.container_statuses.as_deref())
            .unwrap_or_default();
        restarts(containers, now)
    }),
    Column::new("IP", |pod, _| {
        pod.status
            .as_ref()
            .and_then(|status| status.pod_ip.clone())
            .unwrap_or_else(|| NONE.to_string())
    }),
    Column::new("Node", |pod, _| {
        pod.spec
            .as_ref()
            .and_then(|spec| spec.node_name.clone())
            .unwrap_or_else(|| NONE.to_string())
    }),
    Column::AGE,
];

/// Restart count summed over the app containers, with the most recent
/// restart's age (kubectl >= 1.24).
fn restarts(containers: &[ContainerStatus], now: Timestamp) -> String {
    let count: i32 = containers.iter().map(|c| c.restart_count).sum();
    let last = containers
        .iter()
        .filter_map(|c| {
            c.last_state
                .as_ref()?
                .terminated
                .as_ref()?
                .finished_at
                .as_ref()
        })
        .map(|time| time.0)
        .max();
    match last {
        Some(last) if count > 0 => format!("{count} ({} ago)", format_age(last, now)),
        _ => count.to_string(),
    }
}

/// kubectl's pod READY and STATUS columns (`printPod` in
/// k8s.io/kubernetes pkg/printers/internalversion/printers.go): ready app
/// containers (plus started sidecars) over their total, and the reason.
fn pod_state(pod: &Pod) -> (String, String) {
    let status = pod.status.clone().unwrap_or_default();
    let spec = pod.spec.clone().unwrap_or_default();
    let init_specs = spec.init_containers.unwrap_or_default();
    let sidecar = |name: &str| {
        init_specs
            .iter()
            .any(|c| c.name == name && c.restart_policy.as_deref() == Some("Always"))
    };
    let total = spec.containers.len() + init_specs.iter().filter(|c| sidecar(&c.name)).count();
    let mut ready = 0;
    let mut reason = status
        .reason
        .clone()
        .filter(|reason| !reason.is_empty())
        .or(status.phase.clone())
        .unwrap_or_default();
    let condition_true = |kind: &str| {
        status
            .conditions
            .iter()
            .flatten()
            .any(|c| c.type_ == kind && c.status == "True")
    };

    let mut initializing = false;
    let init_statuses = status.init_container_statuses.clone().unwrap_or_default();
    for (index, container) in init_statuses.iter().enumerate() {
        if sidecar(&container.name) && container.started == Some(true) {
            if container.ready {
                ready += 1;
            }
            continue;
        }
        let state = container.state.clone().unwrap_or_default();
        match (&state.terminated, &state.waiting) {
            (Some(done), _) if done.exit_code == 0 => continue,
            (Some(done), _) => {
                reason = match done.reason.as_deref().filter(|r| !r.is_empty()) {
                    Some(why) => format!("Init:{why}"),
                    None => match done.signal.filter(|signal| *signal != 0) {
                        Some(signal) => format!("Init:Signal:{signal}"),
                        None => format!("Init:ExitCode:{}", done.exit_code),
                    },
                };
            }
            (None, Some(waiting))
                if waiting
                    .reason
                    .as_deref()
                    .is_some_and(|why| !why.is_empty() && why != "PodInitializing") =>
            {
                reason = format!("Init:{}", waiting.reason.clone().unwrap_or_default());
            }
            _ => reason = format!("Init:{index}/{}", init_specs.len()),
        }
        initializing = true;
        break;
    }

    if !initializing || condition_true("Initialized") {
        let mut has_running = false;
        for container in status.container_statuses.iter().flatten().rev() {
            let state = container.state.clone().unwrap_or_default();
            let waiting = state
                .waiting
                .and_then(|w| w.reason)
                .filter(|r| !r.is_empty());
            if let Some(why) = waiting {
                reason = why;
            } else if let Some(done) = &state.terminated {
                reason = match done.reason.clone().filter(|r| !r.is_empty()) {
                    Some(why) => why,
                    None => match done.signal.filter(|signal| *signal != 0) {
                        Some(signal) => format!("Signal:{signal}"),
                        None => format!("ExitCode:{}", done.exit_code),
                    },
                };
            } else if container.ready && state.running.is_some() {
                has_running = true;
                ready += 1;
            }
        }
        if reason == "Completed" && has_running {
            reason = if condition_true("Ready") {
                "Running".to_string()
            } else {
                "NotReady".to_string()
            };
        }
    }

    let terminal = matches!(status.phase.as_deref(), Some("Succeeded" | "Failed"));
    if pod.metadata.deletion_timestamp.is_some() {
        if status.reason.as_deref() == Some("NodeLost") {
            reason = "Unknown".to_string();
        } else if !terminal {
            reason = "Terminating".to_string();
        }
    }
    (format!("{ready}/{total}"), reason)
}

/// Status summary = status / node / IP / QoS, the container table (init
/// containers first, as they run) and `status.conditions`.
pub fn pod_detail(pod: &Pod, now: Timestamp) -> DetailModel {
    let status = pod.status.clone().unwrap_or_default();
    let spec = pod.spec.clone().unwrap_or_default();
    let (_, reason) = pod_state(pod);
    let summary = vec![
        ("Status".to_string(), reason),
        (
            "Node".to_string(),
            spec.node_name.clone().unwrap_or_default(),
        ),
        (
            "Pod IP".to_string(),
            status.pod_ip.clone().unwrap_or_default(),
        ),
        (
            "QoS class".to_string(),
            status.qos_class.clone().unwrap_or_default(),
        ),
    ];
    let rows = |containers: &[Container], statuses: &[ContainerStatus], suffix: &str| {
        containers
            .iter()
            .map(|container| {
                let status = statuses.iter().find(|s| s.name == container.name);
                vec![
                    format!("{}{suffix}", container.name),
                    container.image.clone().unwrap_or_default(),
                    status.is_some_and(|s| s.ready).to_string(),
                    status
                        .map(|s| s.restart_count)
                        .unwrap_or_default()
                        .to_string(),
                    status
                        .and_then(|s| s.state.as_ref())
                        .map(|state| describe_state(state, now))
                        .unwrap_or_default(),
                    status
                        .and_then(|s| s.last_state.as_ref())
                        .map(|state| describe_state(state, now))
                        .unwrap_or_default(),
                ]
            })
            .collect::<Vec<_>>()
    };
    let mut containers = rows(
        spec.init_containers.as_deref().unwrap_or_default(),
        status
            .init_container_statuses
            .as_deref()
            .unwrap_or_default(),
        " (init)",
    );
    containers.extend(rows(
        &spec.containers,
        status.container_statuses.as_deref().unwrap_or_default(),
        "",
    ));
    let value = serde_json::to_value(pod).unwrap_or_default();
    let mut model = detail("Pod", pod, &value, summary, now);
    model.tables = vec![SummaryTable {
        title: "Containers".to_string(),
        columns: ["Name", "Image", "Ready", "Restarts", "State", "Last state"]
            .map(String::from)
            .to_vec(),
        rows: containers,
    }];
    model.containers = Some(Containers {
        all: pod_containers(pod),
        running: running_containers(pod),
    });
    model.forward = Some(Forward {
        to: ForwardTo::Pod,
        ports: pod_ports(pod),
        empty: "No declared ports",
    });
    model
}

/// One container state as `kubectl describe` words it; empty when the
/// state carries nothing (e.g. an empty `lastState`).
fn describe_state(state: &ContainerState, now: Timestamp) -> String {
    if let Some(running) = &state.running {
        return match &running.started_at {
            Some(started) => format!("Running since {}", format_age(started.0, now)),
            None => "Running".to_string(),
        };
    }
    if let Some(waiting) = &state.waiting {
        return match &waiting.reason {
            Some(reason) => format!("Waiting: {reason}"),
            None => "Waiting".to_string(),
        };
    }
    if let Some(done) = &state.terminated {
        let reason = done
            .reason
            .clone()
            .unwrap_or_else(|| "Terminated".to_string());
        return format!("Terminated: {reason} (exit {})", done.exit_code);
    }
    String::new()
}

/// `kubectl get deployments` columns.
pub fn project_deployments(deployments: &[&Deployment], now: Timestamp) -> ResourceTable {
    list_table(DEPLOYMENT_COLUMNS, deployments, now)
}

const DEPLOYMENT_COLUMNS: &[Column<Deployment>] = &[
    Column::NAME,
    Column::new("Ready", |deployment, _| {
        let ready = deployment
            .status
            .as_ref()
            .and_then(|s| s.ready_replicas)
            .unwrap_or_default();
        format!("{ready}/{}", replica_counts(deployment).desired)
    }),
    Column::new("Up-to-date", |deployment, _| {
        replica_counts(deployment).updated.to_string()
    }),
    Column::new("Available", |deployment, _| {
        replica_counts(deployment).available.to_string()
    }),
    Column::AGE,
];

/// Status summary = the replica bar, the rollout strategy and selector, and
/// `status.conditions`.
pub fn deployment_detail(deployment: &Deployment, now: Timestamp) -> DetailModel {
    let spec = deployment.spec.clone().unwrap_or_default();
    let strategy = spec.strategy.unwrap_or_default();
    let mut summary = vec![(
        "Strategy".to_string(),
        strategy
            .type_
            .unwrap_or_else(|| "RollingUpdate".to_string()),
    )];
    if let Some(rolling) = strategy.rolling_update {
        for (label, value) in [
            ("Max surge", rolling.max_surge),
            ("Max unavailable", rolling.max_unavailable),
        ] {
            if let Some(value) = value {
                summary.push((label.to_string(), int_or_string(&value)));
            }
        }
    }
    summary.push(("Selector".to_string(), selector_text(&spec.selector)));
    let value = serde_json::to_value(deployment).unwrap_or_default();
    let mut model = detail("Deployment", deployment, &value, summary, now);
    model.replicas = Some(replica_counts(deployment));
    model
}

fn replica_counts(deployment: &Deployment) -> ReplicaCounts {
    let status = deployment.status.clone().unwrap_or_default();
    ReplicaCounts {
        desired: deployment
            .spec
            .as_ref()
            .and_then(|s| s.replicas)
            .unwrap_or(1),
        updated: status.updated_replicas.unwrap_or_default(),
        available: status.available_replicas.unwrap_or_default(),
        unavailable: status.unavailable_replicas.unwrap_or_default(),
    }
}

pub(crate) fn int_or_string(value: &IntOrString) -> String {
    match value {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s.clone(),
    }
}

/// Every container a Pod runs, init containers first, in spec order.
pub fn pod_containers(pod: &Pod) -> Vec<String> {
    let Some(spec) = &pod.spec else {
        return Vec::new();
    };
    spec.init_containers
        .iter()
        .flatten()
        .chain(&spec.containers)
        .map(|container| container.name.clone())
        .collect()
}

/// A port Port Forward offers: a Pod's container port or a Service port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardPort {
    pub port: u16,
    /// What the picker shows, e.g. `8080/http (app)` for a container port.
    pub label: String,
}

/// The containers that keep running, so Exec and Port Forward can reach
/// them: native sidecars (init containers with `restartPolicy: Always`)
/// first, then the app containers. Run-once init containers have exited.
fn long_running(pod: &Pod) -> impl Iterator<Item = &Container> {
    pod.spec.iter().flat_map(|spec| {
        spec.init_containers
            .iter()
            .flatten()
            .filter(|init| init.restart_policy.as_deref() == Some("Always"))
            .chain(&spec.containers)
    })
}

/// The names of the containers Exec can run in; see [`long_running`].
pub fn running_containers(pod: &Pod) -> Vec<String> {
    long_running(pod)
        .map(|container| container.name.clone())
        .collect()
}

/// The running containers' declared TCP ports (port-forward carries TCP
/// only), in container order, each port once.
pub fn pod_ports(pod: &Pod) -> Vec<ForwardPort> {
    let mut ports: Vec<ForwardPort> = Vec::new();
    for container in long_running(pod) {
        for declared in container.ports.iter().flatten() {
            let tcp = declared
                .protocol
                .as_deref()
                .is_none_or(|protocol| protocol == "TCP");
            let Ok(port) = u16::try_from(declared.container_port) else {
                continue;
            };
            if !tcp || ports.iter().any(|known| known.port == port) {
                continue;
            }
            let name = declared
                .name
                .as_ref()
                .map(|name| format!("/{name}"))
                .unwrap_or_default();
            ports.push(ForwardPort {
                port,
                label: format!("{port}{name} ({})", container.name),
            });
        }
    }
    ports
}

/// Whether `labels` satisfy `selector`, with `LabelSelectorAsSelector`'s
/// semantics: every term must hold, an empty selector matches everything,
/// and an unknown operator matches nothing.
pub fn selector_matches(
    selector: &LabelSelector,
    labels: Option<&BTreeMap<String, String>>,
) -> bool {
    let value = |key: &str| labels.and_then(|labels| labels.get(key));
    let by_labels = selector
        .match_labels
        .iter()
        .flatten()
        .all(|(key, expected)| value(key) == Some(expected));
    let by_expressions = selector.match_expressions.iter().flatten().all(|e| {
        let values = e.values.as_deref().unwrap_or_default();
        let found = value(&e.key);
        match e.operator.as_str() {
            "In" => found.is_some_and(|found| values.contains(found)),
            "NotIn" => found.is_none_or(|found| !values.contains(found)),
            "Exists" => found.is_some(),
            "DoesNotExist" => found.is_none(),
            _ => false,
        }
    });
    by_labels && by_expressions
}

/// A label selector in `kubectl` selector syntax (`app=web,tier in (a,b)`).
pub fn selector_text(selector: &LabelSelector) -> String {
    let labels = selector
        .match_labels
        .iter()
        .flatten()
        .map(|(k, v)| format!("{k}={v}"));
    let expressions = selector.match_expressions.iter().flatten().map(|e| {
        let values = e.values.clone().unwrap_or_default().join(",");
        match e.operator.as_str() {
            "In" => format!("{} in ({values})", e.key),
            "NotIn" => format!("{} notin ({values})", e.key),
            "Exists" => e.key.clone(),
            "DoesNotExist" => format!("!{}", e.key),
            other => format!("{} {other} ({values})", e.key),
        }
    });
    labels.chain(expressions).collect::<Vec<_>>().join(",")
}

/// Name / Ready / Desired / Age (Freelens 14a491f, workloads-statefulsets).
pub fn project_statefulsets(statefulsets: &[&StatefulSet], now: Timestamp) -> ResourceTable {
    list_table(STATEFULSET_COLUMNS, statefulsets, now)
}

const STATEFULSET_COLUMNS: &[Column<StatefulSet>] = &[
    Column::NAME,
    Column::new("Ready", |statefulset, _| {
        count_text(
            statefulset
                .status
                .as_ref()
                .and_then(|status| status.ready_replicas),
        )
    }),
    Column::new("Desired", |statefulset, _| {
        count_text(statefulset.spec.as_ref().and_then(|spec| spec.replicas))
    }),
    Column::AGE,
];

pub fn statefulset_detail(statefulset: &StatefulSet, now: Timestamp) -> DetailModel {
    let spec = statefulset.spec.clone().unwrap_or_default();
    let template = spec.template.spec.clone().unwrap_or_default();
    let mut summary = Vec::new();
    let selector = selector_text(&spec.selector);
    if !selector.is_empty() {
        summary.push(("Selector".to_string(), selector));
    }
    push_template_fields(
        &mut summary,
        &template.node_selector,
        template
            .containers
            .iter()
            .map(|container| container.image.clone().unwrap_or_default())
            .collect(),
        &template,
    );
    let status = statefulset.status.clone().unwrap_or_default();
    let value = serde_json::to_value(statefulset).unwrap_or_default();
    let mut model = detail("StatefulSet", statefulset, &value, summary, now);
    model.replicas = Some(ReplicaCounts {
        desired: spec.replicas.unwrap_or_default(),
        updated: status.updated_replicas.unwrap_or_default(),
        available: status.available_replicas.unwrap_or_default(),
        unavailable: status.replicas - status.available_replicas.unwrap_or_default(),
    });
    model
}

fn push_template_fields(
    summary: &mut Vec<(String, String)>,
    node_selector: &Option<BTreeMap<String, String>>,
    images: Vec<String>,
    spec: &astrolabe_kube::k8s_openapi::api::core::v1::PodSpec,
) {
    push_node_selector(summary, node_selector);
    if !images.is_empty() {
        summary.push(("Images".to_string(), images.join(",")));
    }
    push_pod_policies(summary, spec);
}

fn push_node_selector(
    summary: &mut Vec<(String, String)>,
    node_selector: &Option<BTreeMap<String, String>>,
) {
    let nodes = node_selector
        .iter()
        .flatten()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    if !nodes.is_empty() {
        summary.push(("Node Selector".to_string(), nodes));
    }
}

fn push_pod_policies(
    summary: &mut Vec<(String, String)>,
    spec: &astrolabe_kube::k8s_openapi::api::core::v1::PodSpec,
) {
    let tolerations = spec
        .tolerations
        .iter()
        .flatten()
        .map(|toleration| {
            let mut text = toleration.key.clone().unwrap_or_default();
            if toleration.operator.as_deref() == Some("Equal") {
                text.push('=');
                text.push_str(toleration.value.as_deref().unwrap_or_default());
            }
            if let Some(effect) = &toleration.effect {
                text.push(':');
                text.push_str(effect);
            }
            if let Some(seconds) = toleration.toleration_seconds {
                text.push_str(&format!(" for {seconds}s"));
            }
            text
        })
        .collect::<Vec<_>>()
        .join(",");
    if !tolerations.is_empty() {
        summary.push(("Tolerations".to_string(), tolerations));
    }
    let affinities = spec.affinity.as_ref().map_or(0, |affinity| {
        [
            affinity.node_affinity.is_some(),
            affinity.pod_affinity.is_some(),
            affinity.pod_anti_affinity.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    });
    if affinities != 0 {
        summary.push(("Affinities".to_string(), affinities.to_string()));
    }
}

pub fn project_replicasets(replicasets: &[&ReplicaSet], now: Timestamp) -> ResourceTable {
    list_table(REPLICASET_COLUMNS, replicasets, now)
}

const REPLICASET_COLUMNS: &[Column<ReplicaSet>] = &[
    Column::NAME,
    Column::new("Desired", |replicaset, _| {
        count_text(replicaset.spec.as_ref().and_then(|spec| spec.replicas))
    }),
    Column::new("Current", |replicaset, _| {
        count_text(
            replicaset
                .status
                .as_ref()
                .and_then(|status| status.available_replicas),
        )
    }),
    Column::new("Ready", |replicaset, _| {
        count_text(
            replicaset
                .status
                .as_ref()
                .and_then(|status| status.ready_replicas),
        )
    }),
    Column::AGE,
];

pub fn replicaset_detail(replicaset: &ReplicaSet, now: Timestamp) -> DetailModel {
    let spec = replicaset.spec.clone().unwrap_or_default();
    let mut summary = vec![("Selector".to_string(), selector_text(&spec.selector))];
    push_pod_template_fields(&mut summary, spec.template.as_ref(), false);
    let status = replicaset.status.clone().unwrap_or_default();
    summary.push((
        "Replicas".to_string(),
        format!(
            "{} current / {} desired",
            status.available_replicas.unwrap_or_default(),
            status.replicas
        ),
    ));
    let value = serde_json::to_value(replicaset).unwrap_or_default();
    let mut model = detail("ReplicaSet", replicaset, &value, summary, now);
    model.replicas = Some(ReplicaCounts {
        desired: spec.replicas.unwrap_or_default(),
        updated: status.fully_labeled_replicas.unwrap_or_default(),
        available: status.available_replicas.unwrap_or_default(),
        unavailable: status.replicas - status.available_replicas.unwrap_or_default(),
    });
    model
}

pub fn project_daemonsets(daemonsets: &[&DaemonSet], now: Timestamp) -> ResourceTable {
    list_table(DAEMONSET_COLUMNS, daemonsets, now)
}

const DAEMONSET_COLUMNS: &[Column<DaemonSet>] = &[
    Column::NAME,
    Column::new("Desired", |daemonset, _| {
        count_text(
            daemonset
                .status
                .as_ref()
                .map(|status| status.desired_number_scheduled),
        )
    }),
    Column::new("Current", |daemonset, _| {
        count_text(
            daemonset
                .status
                .as_ref()
                .map(|status| status.current_number_scheduled),
        )
    }),
    Column::new("Ready", |daemonset, _| {
        count_text(daemonset.status.as_ref().map(|status| status.number_ready))
    }),
    Column::new("Updated", |daemonset, _| {
        count_text(
            daemonset
                .status
                .as_ref()
                .and_then(|status| status.updated_number_scheduled),
        )
    }),
    Column::new("Available", |daemonset, _| {
        count_text(
            daemonset
                .status
                .as_ref()
                .and_then(|status| status.number_available),
        )
    }),
    Column::new("Node Selector", |daemonset, _| {
        node_selectors(daemonset.spec.as_ref().map(|spec| &spec.template))
    }),
    Column::AGE,
];

/// A count the object may not report yet, shown as 0 until it does.
fn count_text(count: Option<i32>) -> String {
    count.unwrap_or_default().to_string()
}

pub fn daemonset_detail(daemonset: &DaemonSet, now: Timestamp) -> DetailModel {
    let spec = daemonset.spec.clone().unwrap_or_default();
    let mut summary = vec![("Selector".to_string(), selector_text(&spec.selector))];
    push_pod_template_fields(&mut summary, Some(&spec.template), true);
    if let Some(strategy) = spec.update_strategy.and_then(|strategy| strategy.type_) {
        summary.push(("Strategy Type".to_string(), strategy));
    }
    let status = daemonset.status.clone().unwrap_or_default();
    let value = serde_json::to_value(daemonset).unwrap_or_default();
    let mut model = detail("DaemonSet", daemonset, &value, summary, now);
    model.replicas = Some(ReplicaCounts {
        desired: status.desired_number_scheduled,
        updated: status.updated_number_scheduled.unwrap_or_default(),
        available: status.number_available.unwrap_or_default(),
        unavailable: status.number_unavailable.unwrap_or_default(),
    });
    model
}

fn node_selectors(template: Option<&PodTemplateSpec>) -> String {
    template
        .and_then(|template| template.spec.as_ref())
        .and_then(|spec| spec.node_selector.as_ref())
        .into_iter()
        .flatten()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn push_pod_template_fields(
    summary: &mut Vec<(String, String)>,
    template: Option<&PodTemplateSpec>,
    include_init: bool,
) {
    let Some(spec) = template.and_then(|template| template.spec.as_ref()) else {
        return;
    };
    let images = spec
        .containers
        .iter()
        .chain(
            spec.init_containers
                .iter()
                .flatten()
                .filter(move |_| include_init),
        )
        .map(|container| container.image.clone().unwrap_or_default())
        .collect();
    push_template_fields(summary, &spec.node_selector, images, spec);
}

pub fn project_jobs(jobs: &[&Job], now: Timestamp) -> ResourceTable {
    list_table(JOB_COLUMNS, jobs, now)
}

const JOB_COLUMNS: &[Column<Job>] = &[
    Column::NAME,
    Column::new("Resumed", |job, _| {
        (!job
            .spec
            .as_ref()
            .and_then(|spec| spec.suspend)
            .unwrap_or(false))
        .to_string()
    }),
    Column::new("Status", |job, _| job_status(job).to_string()),
    Column::new("Succeeded", |job, _| {
        count_text(job.status.as_ref().and_then(|status| status.succeeded))
    }),
    Column::new("Completions", |job, _| {
        count_text(job.spec.as_ref().and_then(|spec| spec.completions))
    }),
    Column::new("Parallelism", |job, _| {
        job.spec
            .as_ref()
            .and_then(|spec| spec.parallelism)
            .map(|value| value.to_string())
            .unwrap_or_default()
    }),
    Column::new("Duration", job_duration),
    Column::AGE,
];

fn job_status(job: &Job) -> &'static str {
    let Some(status) = &job.status else {
        return "Unknown";
    };
    let Some(conditions) = status
        .conditions
        .as_ref()
        .filter(|conditions| !conditions.is_empty())
    else {
        return "Unknown";
    };
    let has = |kind| conditions.iter().any(|condition| condition.type_ == kind);
    if has("Complete") {
        "Complete"
    } else if has("Failed") {
        "Failed"
    } else if job.metadata.deletion_timestamp.is_some() {
        if job
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|items| !items.is_empty())
        {
            "Finalizing"
        } else {
            "Terminating"
        }
    } else if has("Suspended") {
        "Suspended"
    } else if has("FailureTarget") {
        "FailureTarget"
    } else {
        "Running"
    }
}

fn job_duration(job: &Job, now: Timestamp) -> String {
    let Some(start) = job
        .status
        .as_ref()
        .and_then(|status| status.start_time.as_ref())
    else {
        return "0s".to_string();
    };
    let end = job
        .status
        .as_ref()
        .and_then(|status| status.completion_time.as_ref())
        .map(|time| time.0)
        .unwrap_or(now);
    format_age(start.0, end)
}

pub fn job_detail(job: &Job, now: Timestamp) -> DetailModel {
    let spec = job.spec.clone().unwrap_or_default();
    let status = job.status.clone().unwrap_or_default();
    let mut summary = Vec::new();
    if let Some(selector) = spec.selector.as_ref() {
        let selector = selector_text(selector);
        if !selector.is_empty() {
            summary.push(("Selector".to_string(), selector));
        }
    }
    if let Some(template) = &spec.template.spec {
        push_node_selector(&mut summary, &template.node_selector);
    }
    summary.push(("Status".to_string(), job_status(job).to_string()));
    summary.extend(job_spec_fields(&spec));
    if let Some(start) = &status.start_time {
        summary.push(("Start Time".to_string(), timestamp(start.0, now)));
    }
    if let Some(completed) = &status.completion_time {
        summary.push(("Completed At".to_string(), timestamp(completed.0, now)));
    }
    if status.start_time.is_some() && status.completion_time.is_some() {
        summary.push(("Duration".to_string(), job_duration(job, now)));
    }
    summary.extend(active_deadline_field(&spec));
    let active = status.active.unwrap_or_default();
    let active = match status.ready {
        Some(ready) => format!("{active} Active ({ready} Ready)"),
        None => format!("{active} Active"),
    };
    summary.push((
        "Pods Statuses".to_string(),
        format!(
            "{} / {} Succeeded / {} Failed",
            active,
            status.succeeded.unwrap_or_default(),
            status.failed.unwrap_or_default()
        ),
    ));
    if let Some(indexes) = status.completed_indexes {
        summary.push(("Completed Indexes".to_string(), indexes));
    }
    if let Some(template) = &spec.template.spec {
        push_pod_policies(&mut summary, template);
    }
    let value = serde_json::to_value(job).unwrap_or_default();
    detail("Job", job, &value, summary, now)
}

pub fn project_cronjobs(cronjobs: &[&CronJob], now: Timestamp) -> ResourceTable {
    list_table(CRONJOB_COLUMNS, cronjobs, now)
}

const CRONJOB_COLUMNS: &[Column<CronJob>] = &[
    Column::NAME,
    Column::new("Schedule", |cronjob, _| cronjob_schedule(cronjob)),
    Column::new("Timezone", |cronjob, _| {
        cronjob.spec.time_zone.clone().unwrap_or_default()
    }),
    Column::new("Resumed", |cronjob, _| {
        (!cronjob.spec.suspend.unwrap_or(false)).to_string()
    }),
    Column::new("Active", |cronjob, _| {
        cronjob
            .status
            .as_ref()
            .and_then(|status| status.active.as_ref())
            .map(|jobs| jobs.len())
            .unwrap_or_default()
            .to_string()
    }),
    Column::new("Last schedule", |cronjob, now| {
        cronjob
            .status
            .as_ref()
            .and_then(|status| status.last_schedule_time.as_ref())
            .map(|time| format_age(time.0, now))
            .unwrap_or_else(|| "-".to_string())
    }),
    Column::AGE,
];

fn cronjob_schedule(cronjob: &CronJob) -> String {
    if cronjob_never_run(cronjob) {
        "never".to_string()
    } else {
        cronjob.spec.schedule.clone()
    }
}

fn cronjob_never_run(cronjob: &CronJob) -> bool {
    let spec = &cronjob.spec;
    let parts = spec.schedule.split(' ').collect::<Vec<_>>();
    !spec.schedule.starts_with('@')
        && parts.len() == 5
        && match (parts[2].parse::<usize>(), parts[3].parse::<usize>()) {
            (Ok(day), Ok(month)) if (1..=12).contains(&month) => {
                day > [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1]
            }
            _ => false,
        }
}

fn cronjob_schedule_description(cronjob: &CronJob) -> String {
    let schedule = cronjob
        .spec
        .schedule
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let description = cronspeak::describe(&schedule)
        .unwrap_or_else(|_| "Unrecognized cron expression syntax".to_string());
    let never = if cronjob_never_run(cronjob) {
        ", never ran"
    } else {
        ""
    };
    format!("{schedule} ({description}{never})")
}

pub fn cronjob_detail(cronjob: &CronJob, now: Timestamp) -> DetailModel {
    let spec = cronjob.spec.clone();
    let status = cronjob.status.clone().unwrap_or_default();
    let mut summary = vec![
        (
            "Schedule".to_string(),
            cronjob_schedule_description(cronjob),
        ),
        (
            "Timezone".to_string(),
            spec.time_zone.clone().unwrap_or_default(),
        ),
    ];
    if spec
        .starting_deadline_seconds
        .is_some_and(|seconds| seconds != 0)
    {
        summary.push((
            "Starting Deadline Seconds".to_string(),
            human_seconds(spec.starting_deadline_seconds.unwrap_or_default()),
        ));
    }
    if let Some(policy) = spec.concurrency_policy {
        summary.push(("Concurrency Policy".to_string(), policy));
    }
    summary.push((
        "Resumed".to_string(),
        (!spec.suspend.unwrap_or(false)).to_string(),
    ));
    if spec
        .successful_jobs_history_limit
        .is_some_and(|limit| limit != 0)
    {
        summary.push((
            "Successful Jobs History Limit".to_string(),
            spec.successful_jobs_history_limit
                .unwrap_or_default()
                .to_string(),
        ));
    }
    if spec
        .failed_jobs_history_limit
        .is_some_and(|limit| limit != 0)
    {
        summary.push((
            "Failed Jobs History Limit".to_string(),
            spec.failed_jobs_history_limit
                .unwrap_or_default()
                .to_string(),
        ));
    }
    if let Some(last) = status.last_schedule_time {
        summary.push(("Last Schedule".to_string(), timestamp(last.0, now)));
    }
    if let Some(last) = status.last_successful_time {
        summary.push(("Last Successful Run".to_string(), timestamp(last.0, now)));
    }
    summary.push((
        "Active".to_string(),
        status
            .active
            .map(|jobs| jobs.len())
            .unwrap_or_default()
            .to_string(),
    ));
    let value = serde_json::to_value(cronjob).unwrap_or_default();
    let mut model = detail("CronJob", cronjob, &value, summary, now);
    model.suspended = Some(spec.suspend.unwrap_or(false));
    if let Some(template) = &spec.job_template.spec {
        model.sections.push(DetailSection {
            title: "Template".to_string(),
            items: job_template_fields(template)
                .into_iter()
                .map(DetailItem::Field)
                .collect(),
        });
    }
    model
}

fn job_template_fields(spec: &JobSpec) -> Vec<DetailField> {
    job_spec_fields(spec)
        .into_iter()
        .chain(active_deadline_field(spec))
        .map(|(label, value)| DetailField::from((label.as_str(), value.as_str())))
        .collect()
}

fn job_spec_fields(spec: &JobSpec) -> Vec<(String, String)> {
    let mut fields = vec![
        (
            "Parallelism".to_string(),
            spec.parallelism
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "Completions".to_string(),
            spec.completions.unwrap_or_default().to_string(),
        ),
    ];
    if let Some(mode) = &spec.completion_mode {
        fields.push(("Completion Mode".to_string(), mode.clone()));
    }
    fields.push((
        "Resumed".to_string(),
        (!spec.suspend.unwrap_or(false)).to_string(),
    ));
    if let Some(limit) = spec.backoff_limit {
        fields.push(("Backoff Limit".to_string(), limit.to_string()));
    }
    if let Some(seconds) = spec.ttl_seconds_after_finished {
        fields.push((
            "TTL Seconds After Finished".to_string(),
            human_seconds(i64::from(seconds)),
        ));
    }
    fields
}

fn active_deadline_field(spec: &JobSpec) -> Option<(String, String)> {
    spec.active_deadline_seconds
        .filter(|seconds| *seconds != 0)
        .map(|seconds| {
            (
                "Active Deadline Seconds".to_string(),
                human_seconds(seconds),
            )
        })
}

fn timestamp(time: Timestamp, now: Timestamp) -> String {
    format!("{time} ({} ago)", format_age(time, now))
}

fn human_seconds(seconds: i64) -> String {
    if seconds == 0 {
        "0s".to_string()
    } else if seconds % 3600 == 0 {
        format!("{}h", seconds / 3600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}
