//! The Events list component (#17, #19): kubectl's event columns, newest first.
//! A related section reuses it pre-filtered by involvedObject; the global
//! Events view reuses it unfiltered. Events use generic three-section details.

use astrolabe_kube::k8s_openapi::api::core::v1::Event;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::{Column, DetailModel, ResourceTable, detail, format_age, list_table};

/// Type / Reason / Involved Object / Message / Count / Last Seen, read the
/// way kubectl's `printEvent` reads them; newest first.
pub fn project_events(events: &[&Event], now: Timestamp) -> ResourceTable {
    let mut events = events.to_vec();
    events.sort_by_key(|event| std::cmp::Reverse(last_seen(event)));
    list_table(EVENT_COLUMNS, &events, now)
}

const EVENT_COLUMNS: &[Column<Event>] = &[
    Column {
        header: "Type",
        weight: 2.,
        cell: |event, _| event.type_.clone().unwrap_or_default(),
    },
    Column {
        header: "Reason",
        weight: 1.,
        cell: |event, _| event.reason.clone().unwrap_or_default(),
    },
    Column {
        header: "Involved Object",
        weight: 1.,
        cell: |event, _| target(event),
    },
    Column {
        header: "Message",
        weight: 1.,
        cell: |event, _| message(event),
    },
    Column {
        header: "Count",
        weight: 1.,
        cell: |event, _| count(event).to_string(),
    },
    Column {
        header: "Last Seen",
        weight: 1.,
        cell: |event, now| since(last_seen(event), now),
    },
];

pub fn event_detail(event: &Event, now: Timestamp) -> DetailModel {
    let value = serde_json::to_value(event).unwrap_or_default();
    detail("Event", event, &value, Vec::new(), now)
}

/// Whether `event` is about the object of `kind` with `meta`: by uid when
/// both carry one (a recreated object with the same name is another
/// object), else by kind, namespace and name.
pub fn event_involves(event: &Event, kind: &str, meta: &ObjectMeta) -> bool {
    let involved = &event.involved_object;
    if let (Some(event_uid), Some(uid)) = (&involved.uid, &meta.uid) {
        return event_uid == uid;
    }
    involved.kind.as_deref() == Some(kind)
        && involved.name == meta.name
        && involved.namespace == meta.namespace
}

/// `kind/name` with the kind lowercased, or the bare kind when unnamed.
fn target(event: &Event) -> String {
    let involved = &event.involved_object;
    let kind = involved.kind.clone().unwrap_or_default().to_lowercase();
    match involved.name.as_deref() {
        Some(name) if !name.is_empty() => format!("{kind}/{name}"),
        _ => kind,
    }
}

fn message(event: &Event) -> String {
    event
        .message
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// A series' count; a singleton (count unset or 0) counts once.
fn count(event: &Event) -> i32 {
    match &event.series {
        Some(series) => series.count.unwrap_or_default(),
        None => event.count.filter(|count| *count != 0).unwrap_or(1),
    }
}

fn first_seen(event: &Event) -> Option<Timestamp> {
    event
        .first_timestamp
        .as_ref()
        .map(|time| time.0)
        .or_else(|| event.event_time.as_ref().map(|time| time.0))
}

fn last_seen(event: &Event) -> Option<Timestamp> {
    match &event.series {
        Some(series) => series.last_observed_time.as_ref().map(|time| time.0),
        None => event
            .last_timestamp
            .as_ref()
            .map(|time| time.0)
            .or_else(|| first_seen(event)),
    }
}

fn since(time: Option<Timestamp>, now: Timestamp) -> String {
    time.map(|time| format_age(time, now))
        .unwrap_or_else(|| "<unknown>".to_string())
}
