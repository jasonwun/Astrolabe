//! Built-in workload kinds on the generic pipeline (#17): Pods and
//! Deployments project to list rows and detail models like any kind.

use std::collections::BTreeMap;

use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::k8s_openapi::api::core::v1::{Container, ContainerState, ContainerStatus, Pod};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::{
    DetailModel, ReplicaCounts, ResourceRow, ResourceTable, SummaryTable, age_of, detail,
    format_age,
};
use crate::store::ObjectKey;

const NONE: &str = "<none>";

/// Name / Ready / Status / Restarts / IP / Node / Age, as `kubectl get pods
/// -o wide` prints them.
pub fn project_pods(pods: &[&Pod], now: Timestamp) -> ResourceTable {
    let columns = ["Name", "Ready", "Status", "Restarts", "IP", "Node", "Age"]
        .map(String::from)
        .to_vec();
    let rows = pods
        .iter()
        .map(|pod| {
            let spec = pod.spec.as_ref();
            let status = pod.status.as_ref();
            let containers = status
                .and_then(|status| status.container_statuses.as_deref())
                .unwrap_or_default();
            let (ready, reason) = pod_state(pod);
            ResourceRow {
                key: ObjectKey::of(*pod),
                cells: vec![
                    pod.metadata.name.clone().unwrap_or_default(),
                    ready,
                    reason,
                    restarts(containers, now),
                    status
                        .and_then(|status| status.pod_ip.clone())
                        .unwrap_or_else(|| NONE.to_string()),
                    spec.and_then(|spec| spec.node_name.clone())
                        .unwrap_or_else(|| NONE.to_string()),
                    age_of(&pod.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable { columns, rows }
}

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
    let rows = deployments
        .iter()
        .map(|deployment| {
            let counts = replica_counts(deployment);
            let ready = deployment
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or_default();
            ResourceRow {
                key: ObjectKey::of(*deployment),
                cells: vec![
                    deployment.metadata.name.clone().unwrap_or_default(),
                    format!("{ready}/{}", counts.desired),
                    counts.updated.to_string(),
                    counts.available.to_string(),
                    age_of(&deployment.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Ready", "Up-to-date", "Available", "Age"]
            .map(String::from)
            .to_vec(),
        rows,
    }
}

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

fn int_or_string(value: &IntOrString) -> String {
    match value {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s.clone(),
    }
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
