//! HorizontalPodAutoscalers on the generic pipeline (#20), with Freelens's
//! list columns and detail fields (Freelens 14a491f,
//! `config-horizontal-pod-autoscalers/` and the `HorizontalPodAutoscaler`
//! kube object). Freelens's Namespace column is left out, as for every
//! built-in kind.
//!
//! Watched as `autoscaling/v2`, as Freelens does, so only its v2 metric
//! parser applies: the server converts v1 objects, and v1's
//! `targetCPUUtilizationPercentage` never reaches a v2 response.
//!
//! Deviations agreed for #20: the detail adds Desired Replicas after
//! Replicas (#12 asks for current / desired), and an External metric
//! without a selector is named alone instead of Freelens's
//! `name on undefined`.

use astrolabe_kube::k8s_openapi::api::autoscaling::v2::{
    HorizontalPodAutoscaler, HorizontalPodAutoscalerCondition, MetricSpec, MetricStatus,
    MetricTarget, MetricValueStatus,
};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::{DetailModel, DetailSection, ResourceRow, ResourceTable, SummaryTable, age_of, detail};
use crate::store::ObjectKey;

/// Name / Metrics / Min Pods / Max Pods / Replicas / Age / Conditions, as
/// Freelens's HPA list shows them.
pub fn project_hpas(hpas: &[&HorizontalPodAutoscaler], now: Timestamp) -> ResourceTable {
    let rows = hpas
        .iter()
        .map(|hpa| ResourceRow {
            key: ObjectKey::of(*hpa),
            cells: vec![
                hpa.metadata.name.clone().unwrap_or_default(),
                list_metrics(hpa),
                min_pods(hpa).to_string(),
                hpa.spec.max_replicas.to_string(),
                replicas(hpa).to_string(),
                age_of(&hpa.metadata, now),
                condition_list(hpa),
            ],
        })
        .collect();
    ResourceTable {
        columns: [
            "Name",
            "Metrics",
            "Min Pods",
            "Max Pods",
            "Replicas",
            "Age",
            "Conditions",
        ]
        .map(String::from)
        .to_vec(),
        rows,
    }
}

fn min_pods(hpa: &HorizontalPodAutoscaler) -> i32 {
    hpa.spec.min_replicas.unwrap_or_default()
}

fn replicas(hpa: &HorizontalPodAutoscaler) -> i32 {
    hpa.status
        .as_ref()
        .and_then(|status| status.current_replicas)
        .unwrap_or_default()
}

fn metrics(hpa: &HorizontalPodAutoscaler) -> &[MetricSpec] {
    hpa.spec.metrics.as_deref().unwrap_or_default()
}

/// The list's Metrics: the first metric's `current / target`, `+N more...`
/// for the rest, `--` without any.
fn list_metrics(hpa: &HorizontalPodAutoscaler) -> String {
    let values = metric_values(hpa);
    let Some(first) = values.first() else {
        return "--".to_string();
    };
    match values.len() {
        1 => first.clone(),
        count => format!("{first} +{} more...", count - 1),
    }
}

/// Freelens's HPA details: Reference / Min Pods / Max Pods / Replicas, then
/// Desired Replicas (#12); the conditions fill the generic conditions
/// block, and a Metrics section follows when the spec has metrics.
pub fn hpa_detail(hpa: &HorizontalPodAutoscaler, now: Timestamp) -> DetailModel {
    let target = &hpa.spec.scale_target_ref;
    let desired = hpa
        .status
        .as_ref()
        .map(|status| status.desired_replicas)
        .unwrap_or_default();
    let summary = [
        ("Reference", format!("{}/{}", target.kind, target.name)),
        ("Min Pods", min_pods(hpa).to_string()),
        ("Max Pods", hpa.spec.max_replicas.to_string()),
        ("Replicas", replicas(hpa).to_string()),
        ("Desired Replicas", desired.to_string()),
    ]
    .map(|(label, value)| (label.to_string(), value))
    .to_vec();
    let value = serde_json::to_value(hpa).unwrap_or_default();
    let mut model = detail("HorizontalPodAutoscaler", hpa, &value, summary, now);
    if !metrics(hpa).is_empty() {
        let rows = metrics(hpa)
            .iter()
            .zip(metric_values(hpa))
            .map(|(metric, values)| vec![metric_title(metric), values])
            .collect();
        model.sections.push(DetailSection {
            title: "Metrics".to_string(),
            items: vec![
                SummaryTable {
                    title: String::new(),
                    columns: ["Name", "Current / Target"].map(String::from).to_vec(),
                    rows,
                }
                .into(),
            ],
        });
    }
    model
}

/// The details' metric name: Freelens's `renderName`.
fn metric_title(metric: &MetricSpec) -> String {
    let name = metric_name(metric).unwrap_or_default();
    match metric.type_.as_str() {
        "Resource" | "ContainerResource" => {
            let resource = metric
                .resource
                .as_ref()
                .map(|source| source.name.as_str())
                .or(metric
                    .container_resource
                    .as_ref()
                    .map(|source| source.name.as_str()))
                .unwrap_or_default();
            format!("Resource {resource} on Pods")
        }
        "Pods" => format!("{name} on Pods"),
        "Object" => match &metric.object {
            Some(source) => format!(
                "{name} on {}/{}",
                source.described_object.kind, source.described_object.name
            ),
            None => name.to_string(),
        },
        "External" => match metric
            .external
            .as_ref()
            .and_then(|source| source.metric.selector.as_ref())
        {
            Some(selector) => format!("{name} on {}", selector_json(selector)),
            None => name.to_string(),
        },
        _ => "unknown".to_string(),
    }
}

/// The selector as the server serializes it (Go field order, empty fields
/// omitted), which is what Freelens's `JSON.stringify` prints.
fn selector_json(selector: &LabelSelector) -> String {
    let mut fields = Vec::new();
    if let Some(labels) = selector.match_labels.as_ref().filter(|map| !map.is_empty()) {
        fields.push(format!(
            r#""matchLabels":{}"#,
            serde_json::to_string(labels).unwrap_or_default()
        ));
    }
    if let Some(expressions) = selector
        .match_expressions
        .as_ref()
        .filter(|list| !list.is_empty())
    {
        fields.push(format!(
            r#""matchExpressions":{}"#,
            serde_json::to_string(expressions).unwrap_or_default()
        ));
    }
    format!("{{{}}}", fields.join(","))
}

/// Freelens's `getHorizontalPodAutoscalerMetrics`: each metric's
/// `current / target`, the current value taken from the `currentMetrics`
/// entry of the same type and metric name.
fn metric_values(hpa: &HorizontalPodAutoscaler) -> Vec<String> {
    let current = hpa
        .status
        .as_ref()
        .and_then(|status| status.current_metrics.as_deref())
        .unwrap_or_default();
    metrics(hpa)
        .iter()
        .map(|metric| {
            let status = current.iter().find(|status| {
                status.type_ == metric.type_ && status_metric_name(status) == metric_name(metric)
            });
            let (current, target) = current_target(metric, status);
            // Freelens falls back to its v1 parser when neither is set,
            // which reads nothing from a v2 object.
            let known = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
            let (current, target) = if known(&current) || known(&target) {
                (current, target)
            } else {
                (None, None)
            };
            format!(
                "{} / {}",
                current.as_deref().unwrap_or("unknown"),
                target.as_deref().unwrap_or("unknown")
            )
        })
        .collect()
}

/// Freelens's `getMetricName`.
fn metric_name(metric: &MetricSpec) -> Option<&str> {
    match metric.type_.as_str() {
        "Resource" => metric.resource.as_ref().map(|source| source.name.as_str()),
        "Pods" => metric
            .pods
            .as_ref()
            .map(|source| source.metric.name.as_str()),
        "Object" => metric
            .object
            .as_ref()
            .map(|source| source.metric.name.as_str()),
        "External" => metric
            .external
            .as_ref()
            .map(|source| source.metric.name.as_str()),
        "ContainerResource" => metric
            .container_resource
            .as_ref()
            .map(|source| source.name.as_str()),
        _ => None,
    }
    .filter(|name| !name.is_empty())
}

fn status_metric_name(status: &MetricStatus) -> Option<&str> {
    match status.type_.as_str() {
        "Resource" => status.resource.as_ref().map(|source| source.name.as_str()),
        "Pods" => status
            .pods
            .as_ref()
            .map(|source| source.metric.name.as_str()),
        "Object" => status
            .object
            .as_ref()
            .map(|source| source.metric.name.as_str()),
        "External" => status
            .external
            .as_ref()
            .map(|source| source.metric.name.as_str()),
        "ContainerResource" => status
            .container_resource
            .as_ref()
            .map(|source| source.name.as_str()),
        _ => None,
    }
    .filter(|name| !name.is_empty())
}

/// Freelens's `HorizontalPodAutoscalerV2MetricParser`, by metric type.
fn current_target(
    metric: &MetricSpec,
    status: Option<&MetricStatus>,
) -> (Option<String>, Option<String>) {
    fn utilization(average_utilization: Option<i32>, average: Option<&Quantity>) -> Option<String> {
        match average_utilization {
            Some(percent) => Some(format!("{percent}%")),
            None => average.map(|quantity| quantity.0.clone()),
        }
    }
    fn exact(value: Option<&Quantity>, average: Option<&Quantity>) -> Option<String> {
        value.or(average).map(|quantity| quantity.0.clone())
    }
    fn external(value: Option<&Quantity>, average: Option<&Quantity>) -> Option<String> {
        value.map(|quantity| quantity.0.clone()).or_else(|| {
            average
                .filter(|quantity| !quantity.0.is_empty())
                .map(|quantity| format!("{} (avg)", quantity.0))
        })
    }
    type Pick = fn(Option<&Quantity>, Option<&Quantity>) -> Option<String>;
    let current_of = |value: Option<&MetricValueStatus>, pick: Option<Pick>| {
        value.and_then(|value| match pick {
            Some(pick) => pick(value.value.as_ref(), value.average_value.as_ref()),
            None => utilization(value.average_utilization, value.average_value.as_ref()),
        })
    };
    let target_of = |target: Option<&MetricTarget>, pick: Option<Pick>| {
        target.and_then(|target| match pick {
            Some(pick) => pick(target.value.as_ref(), target.average_value.as_ref()),
            None => utilization(target.average_utilization, target.average_value.as_ref()),
        })
    };
    let averaged: Pick = |_, average| average.map(|quantity| quantity.0.clone());
    match metric.type_.as_str() {
        "Resource" => (
            current_of(
                status.and_then(|s| s.resource.as_ref()).map(|s| &s.current),
                None,
            ),
            target_of(metric.resource.as_ref().map(|m| &m.target), None),
        ),
        "ContainerResource" => (
            current_of(
                status
                    .and_then(|s| s.container_resource.as_ref())
                    .map(|s| &s.current),
                None,
            ),
            target_of(metric.container_resource.as_ref().map(|m| &m.target), None),
        ),
        "Pods" => (
            current_of(
                status.and_then(|s| s.pods.as_ref()).map(|s| &s.current),
                Some(averaged),
            ),
            target_of(metric.pods.as_ref().map(|m| &m.target), Some(averaged)),
        ),
        "Object" => (
            current_of(
                status.and_then(|s| s.object.as_ref()).map(|s| &s.current),
                Some(exact),
            ),
            target_of(metric.object.as_ref().map(|m| &m.target), Some(exact)),
        ),
        "External" => (
            current_of(
                status.and_then(|s| s.external.as_ref()).map(|s| &s.current),
                Some(external),
            ),
            target_of(metric.external.as_ref().map(|m| &m.target), Some(external)),
        ),
        _ => (None, None),
    }
}

/// Freelens's `sortConditions` priorities: higher sorts first among
/// conditions that changed at the same time.
fn condition_priority(kind: &str) -> i32 {
    match kind {
        "PodScheduled" => 1,
        "PodReadyToStartContainers" => 2,
        "ContainersReady" | "ArtifactInStorage" | "Released" => 3,
        "Initialized" => 4,
        "Ready" => 5,
        "Synced" => 6,
        _ => 0,
    }
}

/// Freelens's `sortConditions`: newest transition first, then priority.
fn sorted_conditions(hpa: &HorizontalPodAutoscaler) -> Vec<&HorizontalPodAutoscalerCondition> {
    let mut conditions: Vec<_> = hpa
        .status
        .iter()
        .flat_map(|status| status.conditions.iter().flatten())
        .collect();
    let seconds = |condition: &HorizontalPodAutoscalerCondition| {
        condition
            .last_transition_time
            .as_ref()
            .map(|time| time.0.as_second())
            .unwrap_or_default()
    };
    conditions.sort_by(|a, b| {
        seconds(b)
            .cmp(&seconds(a))
            .then(condition_priority(&b.type_).cmp(&condition_priority(&a.type_)))
    });
    conditions
}

/// Freelens's `KubeObjectConditionsList`: the True conditions plus Ready,
/// Ready first, a Ready that is not True shown as `NotReady`.
fn condition_list(hpa: &HorizontalPodAutoscaler) -> String {
    let mut shown: Vec<_> = sorted_conditions(hpa)
        .into_iter()
        .filter(|condition| condition.status == "True" || condition.type_ == "Ready")
        .collect();
    shown.sort_by_key(|condition| condition.type_ != "Ready");
    shown
        .iter()
        .map(|condition| match condition.status.as_str() {
            "False" | "Unknown" => format!("Not{}", condition.type_),
            _ => condition.type_.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
