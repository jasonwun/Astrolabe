use std::cell::Cell;
use std::collections::BTreeSet;
use std::time::Duration;

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
use crate::store::ObjectKey;

pub mod autoscaling;
pub mod config;
pub mod endpoint_slices;
pub mod events;
pub mod network;
pub mod quotas;
pub mod workloads;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRow {
    pub key: ObjectKey,
    pub cells: Vec<String>,
}

/// One list column: its header, its share of the list's width, and how a
/// row's object fills its cell.
pub struct Column<K> {
    pub header: &'static str,
    pub weight: f32,
    pub cell: fn(&K, Timestamp) -> String,
}

impl<K: Resource> Column<K> {
    /// The object's name, weighing 2 as a list's first column does.
    pub const NAME: Self = Self {
        header: "Name",
        weight: 2.,
        cell: |object, _| object.meta().name.clone().unwrap_or_default(),
    };

    pub const AGE: Self = Self {
        header: "Age",
        weight: 1.,
        cell: |object, now| age_of(object.meta(), now),
    };
}

/// A list column as the list view draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ListColumn {
    pub header: String,
    pub weight: f32,
}

/// A list view's columns and rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceTable {
    pub columns: Vec<ListColumn>,
    pub rows: Vec<ResourceRow>,
}

impl ResourceTable {
    /// A table whose columns take [`default_column_weights`].
    pub fn with_default_weights(headers: Vec<String>, rows: Vec<ResourceRow>) -> Self {
        let weights = default_column_weights(headers.len());
        let columns = headers
            .into_iter()
            .zip(weights)
            .map(|(header, weight)| ListColumn { header, weight })
            .collect();
        Self { columns, rows }
    }

    pub fn headers(&self) -> Vec<&str> {
        self.columns
            .iter()
            .map(|column| column.header.as_str())
            .collect()
    }

    pub fn weights(&self) -> Vec<f32> {
        self.columns.iter().map(|column| column.weight).collect()
    }
}

/// The table of `objects` under `columns`: one row per object, its cells in
/// column order.
pub fn list_table<K: Resource>(
    columns: &[Column<K>],
    objects: &[&K],
    now: Timestamp,
) -> ResourceTable {
    ResourceTable {
        columns: columns
            .iter()
            .map(|column| ListColumn {
                header: column.header.to_string(),
                weight: column.weight,
            })
            .collect(),
        rows: objects
            .iter()
            .map(|object| ResourceRow {
                key: ObjectKey::of(*object),
                cells: columns
                    .iter()
                    .map(|column| (column.cell)(object, now))
                    .collect(),
            })
            .collect(),
    }
}

/// The narrowest a list column gets, so short headers (`Age`, `Min Pods`)
/// stay readable; below the sum of minimums the list scrolls sideways.
pub const MIN_COLUMN_WIDTH: f32 = 80.;

/// Column weights for a list whose columns are known only at runtime: the
/// first (Name) column weighs 2 and every other column 1.
pub fn default_column_weights(count: usize) -> Vec<f32> {
    (0..count).map(|ix| if ix == 0 { 2. } else { 1. }).collect()
}

/// Whole-pixel widths sharing `available` by `weights`, as Freelens's lists
/// do. A column whose share falls under [`MIN_COLUMN_WIDTH`] takes the
/// minimum and the rest share what is left; the last unclamped column takes
/// the rounding remainder, so the widths fill `available` exactly.
pub fn column_widths(available: f32, weights: &[f32]) -> Vec<f32> {
    let mut clamped = vec![false; weights.len()];
    let (free, unclamped_weight) = loop {
        let clamped_count = clamped.iter().filter(|clamped| **clamped).count();
        let free = available - MIN_COLUMN_WIDTH * clamped_count as f32;
        let unclamped_weight: f32 = weights
            .iter()
            .zip(&clamped)
            .filter(|(_, clamped)| !**clamped)
            .map(|(weight, _)| weight)
            .sum();
        let mut changed = false;
        for (column_weight, clamped) in weights.iter().zip(clamped.iter_mut()) {
            if !*clamped && free * column_weight / unclamped_weight < MIN_COLUMN_WIDTH {
                *clamped = true;
                changed = true;
            }
        }
        if !changed {
            break (free, unclamped_weight);
        }
    };
    let mut widths: Vec<f32> = weights
        .iter()
        .zip(&clamped)
        .map(|(column_weight, clamped)| {
            if *clamped {
                MIN_COLUMN_WIDTH
            } else {
                (free * column_weight / unclamped_weight).floor()
            }
        })
        .collect();
    if let Some(last) = clamped.iter().rposition(|clamped| !clamped) {
        let others: f32 = widths
            .iter()
            .enumerate()
            .filter(|(ix, _)| *ix != last)
            .map(|(_, width)| width)
            .sum();
        widths[last] = available - others;
    }
    widths
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
                key: ObjectKey::of(*namespace),
                cells: vec![name, phase],
            }
        })
        .collect()
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
                key: ObjectKey::of(*object),
                cells,
            }
        })
        .collect();
    ResourceTable::with_default_weights(columns, rows)
}

const DEFINITION_COLUMNS: &[Column<CustomResourceDefinition>] = &[
    Column {
        header: "Resource",
        weight: 2.,
        cell: |crd, _| crd.spec.names.kind.clone(),
    },
    Column {
        header: "Group",
        weight: 1.,
        cell: |crd, _| crd.spec.group.clone(),
    },
    Column {
        header: "Version",
        weight: 1.,
        cell: |crd, _| {
            preferred_version(crd)
                .map(|version| version.name.clone())
                .unwrap_or_default()
        },
    },
    Column {
        header: "ShortNames",
        weight: 1.,
        cell: |crd, _| {
            crd.spec
                .names
                .short_names
                .clone()
                .unwrap_or_default()
                .join(", ")
        },
    },
    Column {
        header: "Scope",
        weight: 1.,
        cell: |crd, _| crd.spec.scope.clone(),
    },
    Column::AGE,
];

/// The Definitions list; an empty `groups` filter shows every group.
pub fn project_definitions(
    crds: &[&CustomResourceDefinition],
    groups: &BTreeSet<String>,
    now: Timestamp,
) -> ResourceTable {
    let crds: Vec<_> = crds
        .iter()
        .copied()
        .filter(|crd| groups.is_empty() || groups.contains(&crd.spec.group))
        .collect();
    list_table(DEFINITION_COLUMNS, &crds, now)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConditionRow {
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub message: String,
}

/// A small table inside a detail's status summary (e.g. a Pod's
/// containers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryTable {
    pub title: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailValue {
    pub text: String,
    pub href: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailField {
    pub label: String,
    pub values: Vec<DetailValue>,
}

impl From<(&str, &str)> for DetailField {
    fn from((label, value): (&str, &str)) -> Self {
        Self {
            label: label.to_string(),
            values: vec![DetailValue {
                text: value.to_string(),
                href: None,
            }],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailItem {
    Field(DetailField),
    Table(SummaryTable),
}

impl From<DetailField> for DetailItem {
    fn from(field: DetailField) -> Self {
        Self::Field(field)
    }
}

impl From<SummaryTable> for DetailItem {
    fn from(table: SummaryTable) -> Self {
        Self::Table(table)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailSection {
    pub title: String,
    pub items: Vec<DetailItem>,
}

/// A workload's replica counts, drawn as the status summary's replica bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplicaCounts {
    pub desired: i32,
    pub updated: i32,
    pub available: i32,
    pub unavailable: i32,
}

/// The containers a Pod's actions run in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Containers {
    /// Every container, init containers first: what Logs reads.
    pub all: Vec<String>,
    /// The containers still running: what Exec reaches.
    pub running: Vec<String>,
}

/// What Port Forward reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardTo {
    Pod,
    Service,
}

/// What Port Forward offers from a detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forward {
    pub to: ForwardTo,
    pub ports: Vec<workloads::ForwardPort>,
    /// What the picker says when there are no `ports`.
    pub empty: &'static str,
}

/// The generic detail page (#11): metadata summary, status summary, YAML.
/// Built-in kinds reuse the same structure; their registration may add
/// summary tables and a replica bar to the status summary (#17).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailModel {
    pub title: String,
    pub metadata: Vec<(String, String)>,
    pub labels: Vec<String>,
    pub annotations: Vec<String>,
    pub owners: Vec<String>,
    pub status: Vec<(String, String)>,
    pub sections: Vec<DetailSection>,
    pub replicas: Option<ReplicaCounts>,
    pub tables: Vec<SummaryTable>,
    pub conditions: Vec<ConditionRow>,
    /// A Secret's keys, whose values the panel reveals only on request.
    pub secret_keys: Vec<config::SecretKey>,
    /// What Logs and Exec choose from.
    pub containers: Option<Containers>,
    /// What Port Forward reaches and offers.
    pub forward: Option<Forward>,
    /// Whether a CronJob is suspended, for Suspend/Resume.
    pub suspended: Option<bool>,
    /// The YAML preview, values masked for a Secret.
    pub yaml: String,
    /// What Edit opens when it differs from the preview: a Secret's real
    /// manifest.
    pub edit_yaml: Option<String>,
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

/// Status summary = phase + finalizers; `status.conditions` fill the
/// conditions block.
pub fn namespace_detail(namespace: &Namespace, now: Timestamp) -> DetailModel {
    let phase = namespace
        .status
        .as_ref()
        .and_then(|status| status.phase.clone())
        .unwrap_or_default();
    let finalizers = namespace
        .spec
        .as_ref()
        .and_then(|spec| spec.finalizers.clone())
        .unwrap_or_default()
        .join(", ");
    let status = vec![
        ("Phase".to_string(), phase),
        ("Finalizers".to_string(), finalizers),
    ];
    let value = serde_json::to_value(namespace).unwrap_or_default();
    detail("Namespace", namespace, &value, status, now)
}

/// YAML for preview and editing: `managedFields` stripped, as an SSA
/// payload must not carry them.
pub fn object_yaml<K: Resource + Clone + Serialize>(object: &K) -> String {
    let mut object = object.clone();
    object.meta_mut().managed_fields = None;
    serde_saphyr::to_string(&object).unwrap_or_else(|error| format!("# {error}"))
}

pub(crate) fn detail<K: Resource + Clone + Serialize>(
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
        title: format!("{kind} {}", ObjectKey::of(object)),
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
        sections: Vec::new(),
        replicas: None,
        tables: Vec::new(),
        conditions: conditions(value),
        secret_keys: Vec::new(),
        containers: None,
        forward: None,
        suspended: None,
        yaml: object_yaml(object),
        edit_yaml: None,
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

pub(crate) fn age_of(meta: &ObjectMeta, now: Timestamp) -> String {
    meta.creation_timestamp
        .as_ref()
        .map(|created| format_age(created.0, now))
        .unwrap_or_default()
}

const EVERY_SECOND: Duration = Duration::from_secs(1);
const EVERY_MINUTE: Duration = Duration::from_secs(60);

thread_local! {
    /// While [`track_ages`] runs: its `now`, and the latest `since` that
    /// [`format_age`] formatted against it.
    static TRACKED: Cell<Option<(Timestamp, Option<Timestamp>)>> = const { Cell::new(None) };
}

/// How often an age needs re-projecting to stay current (#70): every
/// second while [`format_age`] shows seconds (under 10 minutes), every
/// minute after. Freelens 113abe5 `computeUpdateInterval` with
/// `compact = true`.
pub fn age_cadence(since: Timestamp, now: Timestamp) -> Duration {
    let minutes = now.duration_since(since).as_secs().div_euclid(60);
    if minutes < 10 {
        EVERY_SECOND
    } else {
        EVERY_MINUTE
    }
}

/// How long until the age next crosses an [`age_cadence`] boundary. A
/// tick timed by the youngest age lands as that age's text changes; older
/// ages, on other phases, may show up to a cadence late, as in Freelens.
pub fn next_age_change(since: Timestamp, now: Timestamp) -> Duration {
    let cadence = age_cadence(since, now).as_nanos() as i128;
    let age = now.duration_since(since).as_nanos();
    Duration::from_nanos((cadence - age.rem_euclid(cadence)) as u64)
}

/// Runs `project` and returns its result with how long until the youngest
/// age it formatted against `now` next changes ([`next_age_change`]); a
/// minute when it formatted none. Ages formatted against another instant
/// (a finished Job's duration) are not live and do not count.
pub fn track_ages<T>(now: Timestamp, project: impl FnOnce() -> T) -> (T, Duration) {
    let outer = TRACKED.replace(Some((now, None)));
    let result = project();
    let youngest = TRACKED.replace(outer).and_then(|(_, youngest)| youngest);
    // A nested call against the same `now` reports to the outer one too.
    if let (Some((at, outer_youngest)), Some(since)) = (outer, youngest)
        && at == now
    {
        let merged = outer_youngest.map_or(since, |y| y.max(since));
        TRACKED.set(Some((at, Some(merged))));
    }
    let next = youngest.map_or(EVERY_MINUTE, |since| next_age_change(since, now));
    (result, next)
}

/// kubectl's `HumanDuration` (k8s.io/apimachinery/pkg/util/duration).
pub fn format_age(since: Timestamp, now: Timestamp) -> String {
    TRACKED.with(|tracked| {
        if let Some((at, youngest)) = tracked.get()
            && at == now
        {
            tracked.set(Some((at, Some(youngest.map_or(since, |y| y.max(since))))));
        }
    });
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
