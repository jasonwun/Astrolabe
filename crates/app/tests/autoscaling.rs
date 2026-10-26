//! HorizontalPodAutoscalers on the generic list/detail pipeline (#20), with
//! Freelens 14a491f's columns and detail fields.

use astrolabe::projection::autoscaling::{hpa_detail, project_hpas};
use astrolabe::projection::{DetailItem, DetailSection, SummaryTable};
use astrolabe_kube::k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn hpa(value: serde_json::Value) -> HorizontalPodAutoscaler {
    serde_json::from_value(value).unwrap()
}

/// Synthetic autoscaling/v2 HPA (plausible shape, not captured from a
/// server): one metric of each source type, all but the External one
/// reported in `currentMetrics`.
fn web_hpa() -> HorizontalPodAutoscaler {
    hpa(serde_json::json!({
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
    }))
}

#[test]
fn hpa_list_follows_freelens_columns() {
    let table = project_hpas(&[&web_hpa()], now());

    assert_eq!(
        table.columns,
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
    let bare = hpa(serde_json::json!({
        "metadata": {"name": "bare", "namespace": "default"},
        "spec": {
            "scaleTargetRef": {"kind": "Deployment", "name": "web"},
            "maxReplicas": 5
        }
    }));

    let table = project_hpas(&[&bare], now());

    assert_eq!(table.rows[0].cells, ["bare", "--", "0", "5", "0", "", ""]);
}

#[test]
fn a_single_metric_has_no_more_suffix_and_a_false_ready_condition_shows_negated() {
    let mut single = web_hpa();
    single.spec.metrics.as_mut().unwrap().truncate(1);
    let status = single.status.as_mut().unwrap();
    status.current_metrics = None;
    status.conditions = Some(
        serde_json::from_value(serde_json::json!([
            {"type": "AbleToScale", "status": "True", "lastTransitionTime": "2026-10-22T11:00:00Z"},
            {"type": "Ready", "status": "False", "lastTransitionTime": "2026-10-22T11:00:00Z"}
        ]))
        .unwrap(),
    );

    let table = project_hpas(&[&single], now());

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
    let model = hpa_detail(&web_hpa(), now());

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
    let bare = hpa(serde_json::json!({
        "metadata": {"name": "bare", "namespace": "default"},
        "spec": {
            "scaleTargetRef": {"kind": "Deployment", "name": "web"},
            "maxReplicas": 5
        }
    }));

    let model = hpa_detail(&bare, now());

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

#[test]
fn an_external_metric_names_its_selector_only_when_it_has_one() {
    let mut external = web_hpa();
    let metric = &mut external.spec.metrics.as_mut().unwrap()[3];
    metric.external.as_mut().unwrap().metric.selector = None;
    external.spec.metrics.as_mut().unwrap().drain(..3);
    external.spec.metrics.as_mut().unwrap().truncate(1);
    external.status = None;

    let model = hpa_detail(&external, now());

    assert_eq!(
        model.sections,
        [metrics_section(&[["queue_messages", "unknown / 30 (avg)"]])]
    );
}

#[test]
fn an_external_selector_names_match_labels_before_match_expressions() {
    let mut external = web_hpa();
    external.spec.metrics.as_mut().unwrap().drain(..3);
    external.spec.metrics.as_mut().unwrap().truncate(1);
    external.spec.metrics.as_mut().unwrap()[0]
        .external
        .as_mut()
        .unwrap()
        .metric
        .selector = Some(
        serde_json::from_value(serde_json::json!({
            "matchExpressions": [{"key": "tier", "operator": "In", "values": ["a", "b"]}],
            "matchLabels": {"queue": "jobs", "app": "web"}
        }))
        .unwrap(),
    );

    let model = hpa_detail(&external, now());

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
    let mut external = web_hpa();
    external.spec.metrics.as_mut().unwrap().drain(..3);
    external.spec.metrics.as_mut().unwrap().truncate(1);
    external.spec.metrics.as_mut().unwrap()[0]
        .external
        .as_mut()
        .unwrap()
        .target = serde_json::from_value(serde_json::json!({
        "type": "AverageValue", "averageValue": ""
    }))
    .unwrap();
    external.status = None;

    let table = project_hpas(&[&external], now());

    assert_eq!(table.rows[0].cells[1], "unknown / unknown");
}
