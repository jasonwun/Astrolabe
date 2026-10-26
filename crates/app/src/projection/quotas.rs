//! ResourceQuotas and LimitRanges on the generic pipeline (#20), with
//! Freelens's list columns and detail fields (Freelens 14a491f,
//! `config-resource-quotas/` and `config-limit-ranges/`). Freelens's
//! Namespace column is left out, as for every built-in kind.
//!
//! Deviation agreed for #20: a quota's usage parses every Kubernetes
//! quantity form, where Freelens's unit parsers reject decimal memory
//! suffixes (`1G`) and read a count's `m` as mega. A value that still does
//! not parse leaves Usage blank, as Freelens leaves out its progress bar.

use astrolabe_kube::k8s_openapi::api::core::v1::{LimitRange, LimitRangeItem, ResourceQuota};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::Resource;

use super::{
    DetailField, DetailModel, DetailSection, DetailValue, ResourceRow, ResourceTable, SummaryTable,
    age_of, detail,
};
use crate::store::ObjectKey;

/// Name / Age, as Freelens's ResourceQuotas list shows them.
pub fn project_resource_quotas(quotas: &[&ResourceQuota], now: Timestamp) -> ResourceTable {
    name_and_age(quotas, now)
}

/// Name / Age, as Freelens's LimitRanges list shows them.
pub fn project_limit_ranges(limits: &[&LimitRange], now: Timestamp) -> ResourceTable {
    name_and_age(limits, now)
}

fn name_and_age<K: Resource>(objects: &[&K], now: Timestamp) -> ResourceTable {
    let rows = objects
        .iter()
        .map(|object| ResourceRow {
            key: ObjectKey::of(*object),
            cells: vec![
                object.meta().name.clone().unwrap_or_default(),
                age_of(object.meta(), now),
            ],
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Age"].map(String::from).to_vec(),
        rows,
    }
}

/// Freelens's ResourceQuota details: a Quotas row per hard limit, its usage
/// beside it, then the Scope Selector when the spec has one.
pub fn resource_quota_detail(quota: &ResourceQuota, now: Timestamp) -> DetailModel {
    let status = quota.status.as_ref();
    let used = status.and_then(|status| status.used.as_ref());
    let rows = status
        .and_then(|status| status.hard.as_ref())
        .into_iter()
        .flatten()
        .map(|(name, hard)| {
            let used = used
                .and_then(|used| used.get(name))
                .map_or("0", |quantity| quantity.0.as_str());
            vec![
                name.clone(),
                used.to_string(),
                hard.0.clone(),
                usage(used, &hard.0),
            ]
        })
        .collect();
    let value = serde_json::to_value(quota).unwrap_or_default();
    let mut model = detail("ResourceQuota", quota, &value, Vec::new(), now);
    // Freelens shows the Quotas item even with no limits.
    model.tables.push(SummaryTable {
        title: "Quotas".to_string(),
        columns: ["Resource", "Used", "Hard", "Usage"]
            .map(String::from)
            .to_vec(),
        rows,
    });
    let selector: Vec<Vec<String>> = quota
        .spec
        .as_ref()
        .and_then(|spec| spec.scope_selector.as_ref())
        .and_then(|selector| selector.match_expressions.as_ref())
        .into_iter()
        .flatten()
        .map(|expression| {
            vec![
                expression.operator.clone(),
                expression.scope_name.clone(),
                expression
                    .values
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ]
        })
        .collect();
    if !selector.is_empty() {
        model.sections.push(DetailSection {
            title: "Scope Selector".to_string(),
            items: vec![
                SummaryTable {
                    title: String::new(),
                    columns: ["Operator", "Scope name", "Values"]
                        .map(String::from)
                        .to_vec(),
                    rows: selector,
                }
                .into(),
            ],
        });
    }
    model
}

/// `used / hard` as a percentage to two decimals (Freelens's
/// `+usage.toFixed(2)`); a zero hard limit counts as fully used, as in
/// Freelens.
fn usage(used: &str, hard: &str) -> String {
    let (Some(used), Some(hard)) = (quantity_value(used), quantity_value(hard)) else {
        return String::new();
    };
    let percent = if hard == 0.0 {
        100.0
    } else {
        used / hard * 100.0
    };
    let fixed = format!("{percent:.2}");
    let trimmed = fixed.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}%")
}

/// A Kubernetes quantity (`<signed number><suffix>`, the suffix binary SI,
/// decimal SI or a decimal exponent) as a number.
fn quantity_value(quantity: &str) -> Option<f64> {
    let digits_end = quantity
        .char_indices()
        .find(|&(index, c)| !(c.is_ascii_digit() || c == '.' || (index == 0 && "+-".contains(c))))
        .map_or(quantity.len(), |(index, _)| index);
    let (number, suffix) = quantity.split_at(digits_end);
    if !number.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let number: f64 = number.parse().ok()?;
    let scale = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024f64,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        exponent => 10f64.powi(exponent.strip_prefix(['e', 'E'])?.parse().ok()?),
    };
    Some(number * scale)
}

/// Freelens's LimitRange details: a section per limit type that has items,
/// a field per resource that type limits, each item's parts as badges.
pub fn limit_range_detail(limits: &LimitRange, now: Timestamp) -> DetailModel {
    let items = limits
        .spec
        .as_ref()
        .map(|spec| spec.limits.as_slice())
        .unwrap_or_default();
    let compute = ["cpu", "memory", "ephemeral-storage"].as_slice();
    let value = serde_json::to_value(limits).unwrap_or_default();
    let mut model = detail("LimitRange", limits, &value, Vec::new(), now);
    for (kind, title, resources) in [
        ("Container", "Container Limits", compute),
        ("Pod", "Pod Limits", compute),
        (
            "PersistentVolumeClaim",
            "Persistent Volume Claim Limits",
            &["storage"],
        ),
    ] {
        let of_kind: Vec<&LimitRangeItem> =
            items.iter().filter(|item| item.type_ == kind).collect();
        if of_kind.is_empty() {
            continue;
        }
        model.sections.push(DetailSection {
            title: title.to_string(),
            items: resources
                .iter()
                .map(|resource| {
                    DetailField {
                        label: resource.to_string(),
                        values: of_kind
                            .iter()
                            .flat_map(|item| limit_badges(item, resource))
                            .collect(),
                    }
                    .into()
                })
                .collect(),
        });
    }
    model
}

/// One item's `part:value` badges for a resource, in Freelens's part order.
fn limit_badges(item: &LimitRangeItem, resource: &str) -> Vec<DetailValue> {
    [
        ("min", &item.min),
        ("max", &item.max),
        ("default", &item.default),
        ("defaultRequest", &item.default_request),
        ("maxLimitRequestRatio", &item.max_limit_request_ratio),
    ]
    .into_iter()
    .filter_map(|(part, values)| {
        let value = values.as_ref()?.get(resource)?;
        (!value.0.is_empty()).then(|| DetailValue {
            text: format!("{part}:{}", value.0),
            href: None,
        })
    })
    .collect()
}
