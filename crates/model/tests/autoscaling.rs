//! HorizontalPodAutoscalers on the generic list/detail pipeline (#20), with
//! Freelens 14a491f's columns and detail fields.

#![expect(
    clippy::assert_is_empty,
    reason = "moved from the app with its assertions unchanged (#153)"
)]

use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::{DetailItem, DetailSection, SummaryTable};
use support::{detail, list};

mod support;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

/// Synthetic autoscaling/v2 HPA (plausible shape, not captured from a
/// server): one metric of each source type, all but the External one
/// reported in `currentMetrics`.
fn web_hpa() -> serde_json::Value {
    serde_json::json!({
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-hpa",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {
            "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
            "minReplicas": 2,
            "maxReplicas": 10,
            "metrics": [
                {"type": "Resource", "resource": {"name": "cpu",
                    "target": {"type": "Utilization", "averageUtilization": 50}}},
                {"type": "Pods", "pods": {"metric": {"name": "packets-per-second"},
                    "target": {"type": "AverageValue", "averageValue": "1k"}}},
                {"type": "Object", "object": {"metric": {"name": "requests-per-second"},
                    "describedObject": {"apiVersion": "networking.k8s.io/v1", "kind": "Ingress", "name": "main"},
                    "target": {"type": "Value", "value": "10k"}}},
                {"type": "External", "external": {
                    "metric": {"name": "queue_messages", "selector": {"matchLabels": {"queue": "jobs"}}},
                    "target": {"type": "AverageValue", "averageValue": "30"}}},
                {"type": "ContainerResource", "containerResource": {"name": "memory", "container": "app",
                    "target": {"type": "AverageValue", "averageValue": "500Mi"}}}
            ]
        },
        "status": {
            "currentReplicas": 3,
            "desiredReplicas": 4,
            "currentMetrics": [
                {"type": "Resource", "resource": {"name": "cpu",
                    "current": {"averageUtilization": 30, "averageValue": "150m"}}},
                {"type": "Pods", "pods": {"metric": {"name": "packets-per-second"},
                    "current": {"averageValue": "900"}}},
                {"type": "Object", "object": {"metric": {"name": "requests-per-second"},
                    "describedObject": {"apiVersion": "networking.k8s.io/v1", "kind": "Ingress", "name": "main"},
                    "current": {"value": "12k"}}},
                {"type": "ContainerResource", "containerResource": {"name": "memory", "container": "app",
                    "current": {"averageValue": "300Mi"}}}
            ],
            "conditions": [
                {"type": "ScalingLimited", "status": "False", "reason": "DesiredWithinRange",
                 "lastTransitionTime": "2026-10-22T11:00:00Z"},
                {"type": "AbleToScale", "status": "True", "reason": "ReadyForNewScale",
                 "lastTransitionTime": "2026-10-22T11:10:00Z"},
                {"type": "ScalingActive", "status": "True", "reason": "ValidMetricFound",
                 "lastTransitionTime": "2026-10-22T11:20:00Z"}
            ]
        }
    })
}

#[test]
fn hpa_list_follows_freelens_columns() {
    let table = list(
        BuiltinKind::HorizontalPodAutoscalers,
        vec![web_hpa()],
        now(),
    );

    assert_eq!(
        table.headers(),
        [
            "Name",
            "Metrics",
            "Min Pods",
            "Max Pods",
            "Replicas",
            "Age",
            "Conditions"
        ]
    );
    assert_eq!(
        table.rows[0].cells,
        [
            "web",
            "30% / 50% +4 more...",
            "2",
            "10",
            "3",
            "60m",
            // Newest transition first; only True conditions (or Ready) show.
            "ScalingActive, AbleToScale"
        ]
    );
}

#[test]
fn an_hpa_without_metrics_min_replicas_or_status_lists_defaults() {
    let bare = serde_json::json!({
        "metadata": {"name": "bare", "namespace": "default"},
        "spec": {
            "scaleTargetRef": {"kind": "Deployment", "name": "web"},
            "maxReplicas": 5
        }
    });

    let table = list(BuiltinKind::HorizontalPodAutoscalers, vec![bare], now());

    assert_eq!(table.rows[0].cells, ["bare", "--", "0", "5", "0", "", ""]);
}

#[test]
fn a_single_metric_has_no_more_suffix_and_a_false_ready_condition_shows_negated() {
    let mut single = web_hpa();
    single["spec"]["metrics"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    let status = &mut single["status"];
    status.as_object_mut().unwrap().remove("currentMetrics");
    status["conditions"] = serde_json::json!([
        {"type": "AbleToScale", "status": "True", "lastTransitionTime": "2026-10-22T11:00:00Z"},
        {"type": "Ready", "status": "False", "lastTransitionTime": "2026-10-22T11:00:00Z"}
    ]);

    let table = list(BuiltinKind::HorizontalPodAutoscalers, vec![single], now());

    assert_eq!(table.rows[0].cells[1], "unknown / 50%");
    assert_eq!(table.rows[0].cells[6], "NotReady, AbleToScale");
}

fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(label, value)| (label.to_string(), value.to_string()))
        .collect()
}

fn metrics_section(rows: &[[&str; 2]]) -> DetailSection {
    DetailSection {
        title: "Metrics".to_string(),
        items: vec![DetailItem::Table(SummaryTable {
            title: String::new(),
            columns: ["Name", "Current / Target"].map(String::from).to_vec(),
            rows: rows
                .iter()
                .map(|row| row.map(String::from).to_vec())
                .collect(),
        })],
    }
}

#[test]
fn an_hpa_detail_shows_freelens_fields_desired_replicas_and_metrics() {
    let model = detail(BuiltinKind::HorizontalPodAutoscalers, web_hpa(), now());

    assert_eq!(model.title, "HorizontalPodAutoscaler default/web");
    assert_eq!(
        model.status,
        pairs(&[
            ("Reference", "Deployment/web"),
            ("Min Pods", "2"),
            ("Max Pods", "10"),
            ("Replicas", "3"),
            ("Desired Replicas", "4"),
        ])
    );
    assert_eq!(
        model
            .conditions
            .iter()
            .map(|condition| condition.kind.as_str())
            .collect::<Vec<_>>(),
        ["ScalingLimited", "AbleToScale", "ScalingActive"]
    );
    assert_eq!(
        model.sections,
        [metrics_section(&[
            ["Resource cpu on Pods", "30% / 50%"],
            ["packets-per-second on Pods", "900 / 1k"],
            ["requests-per-second on Ingress/main", "12k / 10k"],
            [
                r#"queue_messages on {"matchLabels":{"queue":"jobs"}}"#,
                "unknown / 30 (avg)"
            ],
            ["Resource memory on Pods", "300Mi / 500Mi"],
        ])]
    );
}

#[test]
fn an_hpa_detail_without_metrics_or_status_has_no_metrics_section() {
    let bare = serde_json::json!({
        "metadata": {"name": "bare", "namespace": "default"},
        "spec": {
            "scaleTargetRef": {"kind": "Deployment", "name": "web"},
            "maxReplicas": 5
        }
    });

    let model = detail(BuiltinKind::HorizontalPodAutoscalers, bare, now());

    assert_eq!(
        model.status,
        pairs(&[
            ("Reference", "Deployment/web"),
            ("Min Pods", "0"),
            ("Max Pods", "5"),
            ("Replicas", "0"),
            ("Desired Replicas", "0"),
        ])
    );
    assert!(model.sections.is_empty());
    assert!(model.conditions.is_empty());
}

/// `hpa` with only its External metric left.
fn external_only(mut hpa: serde_json::Value) -> serde_json::Value {
    let metrics = hpa["spec"]["metrics"].as_array_mut().unwrap();
    metrics.drain(..3);
    metrics.truncate(1);
    hpa
}

#[test]
fn an_external_metric_names_its_selector_only_when_it_has_one() {
    let mut external = external_only(web_hpa());
    external["spec"]["metrics"][0]["external"]["metric"]
        .as_object_mut()
        .unwrap()
        .remove("selector");
    external.as_object_mut().unwrap().remove("status");

    let model = detail(BuiltinKind::HorizontalPodAutoscalers, external, now());

    assert_eq!(
        model.sections,
        [metrics_section(&[["queue_messages", "unknown / 30 (avg)"]])]
    );
}

#[test]
fn an_external_selector_names_match_labels_before_match_expressions() {
    let mut external = external_only(web_hpa());
    external["spec"]["metrics"][0]["external"]["metric"]["selector"] = serde_json::json!({
        "matchExpressions": [{"key": "tier", "operator": "In", "values": ["a", "b"]}],
        "matchLabels": {"queue": "jobs", "app": "web"}
    });

    let model = detail(BuiltinKind::HorizontalPodAutoscalers, external, now());

    let DetailItem::Table(table) = &model.sections[0].items[0] else {
        panic!("{:?}", model.sections);
    };
    // Server (Go) field order, as Freelens's JSON.stringify sees it.
    assert_eq!(
        table.rows[0][0],
        r#"queue_messages on {"matchLabels":{"app":"web","queue":"jobs"},"matchExpressions":[{"key":"tier","operator":"In","values":["a","b"]}]}"#
    );
}

#[test]
fn an_empty_external_average_reads_as_unknown_like_a_missing_one() {
    let mut external = external_only(web_hpa());
    external["spec"]["metrics"][0]["external"]["target"] = serde_json::json!({
        "type": "AverageValue", "averageValue": ""
    });
    external.as_object_mut().unwrap().remove("status");

    let table = list(BuiltinKind::HorizontalPodAutoscalers, vec![external], now());

    assert_eq!(table.rows[0].cells[1], "unknown / unknown");
}
