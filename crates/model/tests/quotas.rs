//! ResourceQuotas and LimitRanges on the generic list/detail pipeline
//! (#20), with Freelens 14a491f's columns and detail fields.

#![expect(
    clippy::assert_is_empty,
    reason = "moved from the app with its assertions unchanged (#153)"
)]

use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::{
    DetailField, DetailItem, DetailSection, DetailValue, SummaryTable,
};
use support::{detail, list};

mod support;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn rows(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect()
}

/// Synthetic ResourceQuota (plausible shape, not captured from a server).
fn compute_quota() -> serde_json::Value {
    serde_json::json!({
        "metadata": {"name": "compute", "namespace": "default", "uid": "uid-quota",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {
            "hard": {"cpu": "2"},
            "scopeSelector": {"matchExpressions": [
                {"operator": "In", "scopeName": "PriorityClass", "values": ["high", "medium"]},
                {"operator": "Exists", "scopeName": "BestEffort"}
            ]}
        },
        "status": {
            "hard": {
                "count/configmaps": "0", "cpu": "2", "limits.cpu": "1",
                "memory": "1G", "pods": "10", "requests.storage": "10Gi"
            },
            "used": {
                "count/configmaps": "0", "cpu": "500m",
                "memory": "512M", "pods": "3", "requests.storage": "2.5Gi"
            }
        }
    })
}

#[test]
fn resource_quotas_list_names_and_ages() {
    let table = list(BuiltinKind::ResourceQuotas, vec![compute_quota()], now());

    assert_eq!(table.headers(), ["Name", "Age"]);
    assert_eq!(table.rows[0].cells, ["compute", "60m"]);
}

#[test]
fn a_resource_quota_detail_shows_usage_of_each_hard_limit() {
    let model = detail(BuiltinKind::ResourceQuotas, compute_quota(), now());

    assert_eq!(model.title, "ResourceQuota default/compute");
    assert!(model.status.is_empty());
    assert_eq!(
        model.tables,
        [SummaryTable {
            title: "Quotas".to_string(),
            columns: ["Resource", "Used", "Hard", "Usage"]
                .map(String::from)
                .to_vec(),
            rows: rows(&[
                // A zero hard limit counts as fully used, as in Freelens.
                &["count/configmaps", "0", "0", "100%"],
                &["cpu", "500m", "2", "25%"],
                // Nothing used yet reads as 0.
                &["limits.cpu", "0", "1", "0%"],
                // Decimal SI suffixes, which Freelens's parser rejects.
                &["memory", "512M", "1G", "51.2%"],
                &["pods", "3", "10", "30%"],
                &["requests.storage", "2.5Gi", "10Gi", "25%"],
            ]),
        }]
    );
    assert_eq!(
        model.sections,
        [DetailSection {
            title: "Scope Selector".to_string(),
            items: vec![DetailItem::Table(SummaryTable {
                title: String::new(),
                columns: ["Operator", "Scope name", "Values"]
                    .map(String::from)
                    .to_vec(),
                rows: rows(&[
                    &["In", "PriorityClass", "high, medium"],
                    &["Exists", "BestEffort", ""],
                ]),
            })],
        }]
    );
}

#[test]
fn quota_usage_parses_every_quantity_form_and_leaves_bad_values_blank() {
    let mut quota = compute_quota();
    quota.as_object_mut().unwrap().remove("spec");
    quota["status"] = serde_json::json!({
        "hard": {
            "a": "1Ki", "b": "3", "c": "1e3", "d": "1E", "e": "2", "f": "1Mi",
            "g": "4", "h": "2"
        },
        "used": {
            "a": "512", "b": "1", "c": "250", "d": "500P", "e": "1500m",
            "f": "1.5Mi", "g": "bogus", "h": "-1"
        }
    });

    let model = detail(BuiltinKind::ResourceQuotas, quota, now());

    let usage: Vec<&str> = model.tables[0]
        .rows
        .iter()
        .map(|row| row[3].as_str())
        .collect();
    assert_eq!(
        usage,
        // 1/3 rounds to two decimals; overuse is not capped.
        ["50%", "33.33%", "25%", "50%", "75%", "150%", "", "-50%"]
    );
    assert!(model.sections.is_empty(), "no scope selector");
}

#[test]
fn a_resource_quota_without_status_keeps_an_empty_quotas_table() {
    let mut quota = compute_quota();
    quota.as_object_mut().unwrap().remove("status");

    let model = detail(BuiltinKind::ResourceQuotas, quota, now());

    assert_eq!(model.tables.len(), 1);
    assert_eq!(model.tables[0].title, "Quotas");
    assert!(model.tables[0].rows.is_empty());
}

/// Synthetic LimitRange (plausible shape, not captured from a server).
fn defaults() -> serde_json::Value {
    serde_json::json!({
        "metadata": {"name": "defaults", "namespace": "default", "uid": "uid-limits",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {"limits": [
            {"type": "Container",
             "min": {"cpu": "100m"}, "max": {"cpu": "2", "memory": "1Gi"},
             "default": {"cpu": "500m", "memory": "512Mi"},
             "defaultRequest": {"cpu": "250m"},
             "maxLimitRequestRatio": {"cpu": "4"}},
            {"type": "PersistentVolumeClaim", "min": {"storage": "1Gi"}, "max": {"storage": "10Gi"}},
            {"type": "Container", "max": {"ephemeral-storage": "2Gi"}}
        ]}
    })
}

#[test]
fn limit_ranges_list_names_and_ages() {
    let table = list(BuiltinKind::LimitRanges, vec![defaults()], now());

    assert_eq!(table.headers(), ["Name", "Age"]);
    assert_eq!(table.rows[0].cells, ["defaults", "60m"]);
}

fn field(label: &str, values: &[&str]) -> DetailItem {
    DetailItem::Field(DetailField {
        label: label.to_string(),
        values: values
            .iter()
            .map(|text| DetailValue {
                text: text.to_string(),
                href: None,
            })
            .collect(),
    })
}

#[test]
fn a_limit_range_detail_groups_limits_by_type_and_resource() {
    let model = detail(BuiltinKind::LimitRanges, defaults(), now());

    assert_eq!(model.title, "LimitRange default/defaults");
    assert!(model.status.is_empty());
    assert_eq!(
        model.sections,
        [
            DetailSection {
                title: "Container Limits".to_string(),
                items: vec![
                    field(
                        "cpu",
                        &[
                            "min:100m",
                            "max:2",
                            "default:500m",
                            "defaultRequest:250m",
                            "maxLimitRequestRatio:4",
                        ],
                    ),
                    field("memory", &["max:1Gi", "default:512Mi"]),
                    // From the second Container item.
                    field("ephemeral-storage", &["max:2Gi"]),
                ],
            },
            // No Pod item, so no Pod Limits.
            DetailSection {
                title: "Persistent Volume Claim Limits".to_string(),
                items: vec![field("storage", &["min:1Gi", "max:10Gi"])],
            },
        ]
    );
}

#[test]
fn a_limit_type_lists_every_resource_even_without_limits() {
    let mut pods = defaults();
    pods["spec"] = serde_json::json!({"limits": [
        {"type": "Pod", "max": {"memory": "2Gi"}}
    ]});

    let model = detail(BuiltinKind::LimitRanges, pods, now());

    assert_eq!(
        model.sections,
        [DetailSection {
            title: "Pod Limits".to_string(),
            items: vec![
                field("cpu", &[]),
                field("memory", &["max:2Gi"]),
                field("ephemeral-storage", &[]),
            ],
        }]
    );
}

#[test]
fn an_empty_limit_value_shows_no_badge() {
    let mut empty = defaults();
    empty["spec"] = serde_json::json!({"limits": [
        {"type": "PersistentVolumeClaim", "min": {"storage": ""}, "max": {"storage": "10Gi"}}
    ]});

    let model = detail(BuiltinKind::LimitRanges, empty, now());

    assert_eq!(model.sections[0].items, [field("storage", &["max:10Gi"])]);
}

#[test]
fn empty_limits_and_hard_lists_behave_as_missing_ones() {
    let mut limits = defaults();
    limits["spec"] = serde_json::json!({"limits": []});
    let mut quota = compute_quota();
    quota["status"] = serde_json::json!({"hard": {}});

    assert!(
        detail(BuiltinKind::LimitRanges, limits, now())
            .sections
            .is_empty()
    );
    assert!(
        detail(BuiltinKind::ResourceQuotas, quota, now()).tables[0]
            .rows
            .is_empty()
    );
}
