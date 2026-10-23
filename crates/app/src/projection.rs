use std::collections::BTreeSet;

use astrolabe_kube::crd::{CustomKind, preferred_version};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceColumnDefinition, CustomResourceDefinition,
};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::k8s_openapi::serde::Serialize;
use astrolabe_kube::kube::Resource;
use astrolabe_kube::kube::core::DynamicObject;
use serde_json::Value;

use crate::jsonpath;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRow {
    pub key: String,
    pub cells: Vec<String>,
}

/// A list view's columns and rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceTable {
    pub columns: Vec<String>,
    pub rows: Vec<ResourceRow>,
}

pub fn project_namespaces(namespaces: &[&Namespace]) -> Vec<ResourceRow> {
    namespaces
        .iter()
        .map(|namespace| {
            let name = namespace.metadata.name.clone().unwrap_or_default();
            let phase = namespace
                .status
                .as_ref()
                .and_then(|status| status.phase.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            ResourceRow {
                key: name.clone(),
                cells: vec![name, phase],
            }
        })
        .collect()
}

/// Store key of an object, as rows and detail tabs address it.
pub fn object_key(meta: &ObjectMeta) -> String {
    let name = meta.name.clone().unwrap_or_default();
    match &meta.namespace {
        Some(namespace) => format!("{namespace}/{name}"),
        None => name,
    }
}

/// Name / Namespace (namespaced kinds only) / the preferred version's
/// priority-0 printer columns (minus a literal "age") / Age.
pub fn project_custom_resources(
    kind: &CustomKind,
    objects: &[&DynamicObject],
    now: Timestamp,
) -> ResourceTable {
    let printer: Vec<&CustomResourceColumnDefinition> = kind
        .columns
        .iter()
        .filter(|column| column.priority.unwrap_or(0) == 0)
        .filter(|column| !column.name.eq_ignore_ascii_case("age"))
        .collect();
    let mut columns = vec!["Name".to_string()];
    if kind.namespaced {
        columns.push("Namespace".to_string());
    }
    columns.extend(printer.iter().map(|column| column.name.clone()));
    columns.push("Age".to_string());

    let rows = objects
        .iter()
        .map(|object| {
            let value = serde_json::to_value(object).unwrap_or_default();
            let meta = &object.metadata;
            let mut cells = vec![meta.name.clone().unwrap_or_default()];
            if kind.namespaced {
                cells.push(meta.namespace.clone().unwrap_or_default());
            }
            cells.extend(
                printer
                    .iter()
                    .map(|column| printer_cell(column, &value, now)),
            );
            cells.push(age_of(meta, now));
            ResourceRow {
                key: object_key(meta),
                cells,
            }
        })
        .collect();
    ResourceTable { columns, rows }
}

/// The Definitions list; an empty `groups` filter shows every group.
pub fn project_definitions(
    crds: &[&CustomResourceDefinition],
    groups: &BTreeSet<String>,
    now: Timestamp,
) -> ResourceTable {
    let columns = ["Resource", "Group", "Version", "ShortNames", "Scope", "Age"]
        .map(String::from)
        .to_vec();
    let rows = crds
        .iter()
        .filter(|crd| groups.is_empty() || groups.contains(&crd.spec.group))
        .map(|crd| {
            let spec = &crd.spec;
            ResourceRow {
                key: object_key(&crd.metadata),
                cells: vec![
                    spec.names.kind.clone(),
                    spec.group.clone(),
                    preferred_version(crd)
                        .map(|version| version.name.clone())
                        .unwrap_or_default(),
                    spec.names
                        .short_names
                        .clone()
                        .unwrap_or_default()
                        .join(", "),
                    spec.scope.clone(),
                    age_of(&crd.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable { columns, rows }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConditionRow {
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub message: String,
}

/// The generic detail page (#11): metadata summary, status summary, YAML.
/// Built-in kinds reuse the same structure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailModel {
    pub title: String,
    pub metadata: Vec<(String, String)>,
    pub labels: Vec<String>,
    pub annotations: Vec<String>,
    pub owners: Vec<String>,
    pub status: Vec<(String, String)>,
    pub conditions: Vec<ConditionRow>,
    pub yaml: String,
}

/// Status summary = the priority > 0 printer columns + `status.conditions`.
pub fn custom_resource_detail(
    kind: &CustomKind,
    object: &DynamicObject,
    now: Timestamp,
) -> DetailModel {
    let value = serde_json::to_value(object).unwrap_or_default();
    let status = kind
        .columns
        .iter()
        .filter(|column| column.priority.unwrap_or(0) > 0)
        .map(|column| (column.name.clone(), printer_cell(column, &value, now)))
        .collect();
    detail(&kind.kind, object, &value, status, now)
}

/// Status summary = versions / scope / names / printer columns; the
/// validation schema stays in the YAML section.
pub fn definition_detail(crd: &CustomResourceDefinition, now: Timestamp) -> DetailModel {
    let spec = &crd.spec;
    let versions = spec
        .versions
        .iter()
        .map(|version| {
            let mut flags = vec![if version.served {
                "served"
            } else {
                "not served"
            }];
            if version.storage {
                flags.push("storage");
            }
            format!("{} ({})", version.name, flags.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let columns = preferred_version(crd)
        .and_then(|version| version.additional_printer_columns.as_ref())
        .map(|columns| {
            columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let names = &spec.names;
    let status = [
        ("Versions", versions),
        ("Scope", spec.scope.clone()),
        ("Kind", names.kind.clone()),
        ("Plural", names.plural.clone()),
        ("Singular", names.singular.clone().unwrap_or_default()),
        (
            "Short names",
            names.short_names.clone().unwrap_or_default().join(", "),
        ),
        ("Printer columns", columns),
    ]
    .map(|(label, value)| (label.to_string(), value))
    .to_vec();
    let value = serde_json::to_value(crd).unwrap_or_default();
    detail("CustomResourceDefinition", crd, &value, status, now)
}

/// YAML for preview and editing: `managedFields` stripped, as an SSA
/// payload must not carry them.
pub fn object_yaml<K: Resource + Clone + Serialize>(object: &K) -> String {
    let mut object = object.clone();
    object.meta_mut().managed_fields = None;
    serde_saphyr::to_string(&object).unwrap_or_else(|error| format!("# {error}"))
}

fn detail<K: Resource + Clone + Serialize>(
    kind: &str,
    object: &K,
    value: &Value,
    status: Vec<(String, String)>,
    now: Timestamp,
) -> DetailModel {
    let meta = object.meta();
    let mut metadata = vec![("Name".to_string(), meta.name.clone().unwrap_or_default())];
    if let Some(namespace) = &meta.namespace {
        metadata.push(("Namespace".to_string(), namespace.clone()));
    }
    metadata.push(("UID".to_string(), meta.uid.clone().unwrap_or_default()));
    if let Some(created) = &meta.creation_timestamp {
        metadata.push((
            "Created".to_string(),
            format!("{} ({} ago)", created.0, format_age(created.0, now)),
        ));
    }
    let pairs = |map: &Option<std::collections::BTreeMap<String, String>>| {
        map.iter()
            .flatten()
            .map(|(key, value)| format!("{key}={value}"))
            .collect()
    };
    DetailModel {
        title: format!("{kind} {}", object_key(meta)),
        metadata,
        labels: pairs(&meta.labels),
        annotations: pairs(&meta.annotations),
        owners: meta
            .owner_references
            .iter()
            .flatten()
            .map(|owner| format!("{} {}", owner.kind, owner.name))
            .collect(),
        status,
        conditions: conditions(value),
        yaml: object_yaml(object),
    }
}

fn conditions(value: &Value) -> Vec<ConditionRow> {
    let text =
        |condition: &Value, field: &str| condition.get(field).map(scalar_text).unwrap_or_default();
    jsonpath::evaluate(".status.conditions[*]", value)
        .into_iter()
        .map(|condition| ConditionRow {
            kind: text(condition, "type"),
            status: text(condition, "status"),
            reason: text(condition, "reason"),
            message: text(condition, "message"),
        })
        .collect()
}

fn printer_cell(column: &CustomResourceColumnDefinition, value: &Value, now: Timestamp) -> String {
    jsonpath::evaluate(&column.json_path, value)
        .into_iter()
        .map(
            |selected| match (column.type_.as_str(), selected.as_str()) {
                ("date", Some(text)) => text
                    .parse::<Timestamp>()
                    .map(|time| format_age(time, now))
                    .unwrap_or_else(|_| text.to_string()),
                _ => scalar_text(selected),
            },
        )
        .collect::<Vec<_>>()
        .join(",")
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn age_of(meta: &ObjectMeta, now: Timestamp) -> String {
    meta.creation_timestamp
        .as_ref()
        .map(|created| format_age(created.0, now))
        .unwrap_or_default()
}

/// kubectl's `HumanDuration` (k8s.io/apimachinery/pkg/util/duration).
pub fn format_age(since: Timestamp, now: Timestamp) -> String {
    let seconds = now.duration_since(since).as_secs();
    if seconds < -1 {
        return "<invalid>".to_string();
    }
    if seconds < 0 {
        return "0s".to_string();
    }
    if seconds < 120 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 10 {
        return match seconds % 60 {
            0 => format!("{minutes}m"),
            s => format!("{minutes}m{s}s"),
        };
    }
    if minutes < 180 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 8 {
        return match minutes % 60 {
            0 => format!("{hours}h"),
            m => format!("{hours}h{m}m"),
        };
    }
    if hours < 48 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if hours < 24 * 8 {
        return match hours % 24 {
            0 => format!("{days}d"),
            h => format!("{days}d{h}h"),
        };
    }
    if days < 365 * 2 {
        return format!("{days}d");
    }
    let years = days / 365;
    if days < 365 * 8 {
        return match days % 365 {
            0 => format!("{years}y"),
            d => format!("{years}y{d}d"),
        };
    }
    format!("{years}y")
}
