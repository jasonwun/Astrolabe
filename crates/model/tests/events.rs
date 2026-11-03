//! The Events list component (#17): kubectl's columns, newest first, and
//! the involvedObject filter a related section applies.

use astrolabe_kube::k8s_openapi::api::core::v1::{Event, Pod};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::object::ObjectKey;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::events::event_involves;
use support::list;

mod support;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn typed_event(name: &str, extra: serde_json::Value) -> Event {
    serde_json::from_value(event_json(name, extra)).unwrap()
}

/// Synthetic Event (plausible shape, not captured from a server).
#[expect(
    clippy::needless_pass_by_value,
    reason = "callers pass a json! literal"
)]
fn event_json(name: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut value = serde_json::json!({
        "apiVersion": "v1", "kind": "Event",
        "metadata": {"name": name, "namespace": "default",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "involvedObject": {"kind": "Pod", "name": "web-1", "namespace": "default",
                           "uid": "uid-pod"},
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    value
}

/// Synthetic Pod (plausible shape, not captured from a server).
fn pod(name: &str, uid: Option<&str>) -> Pod {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": name, "namespace": "default", "uid": uid},
    }))
    .unwrap()
}

#[test]
fn events_list_kubectl_columns_newest_first() {
    let backoff = event_json(
        "web-1.backoff",
        serde_json::json!({
            "type": "Warning", "reason": "BackOff",
            "message": "Back-off restarting failed container app\n",
            "count": 7,
            "firstTimestamp": "2026-10-22T11:00:00Z",
            "lastTimestamp": "2026-10-22T11:58:00Z",
        }),
    );
    let pulled = event_json(
        "web-1.pulled",
        serde_json::json!({
            "type": "Normal", "reason": "Pulled", "message": "Pulled image nginx",
            "firstTimestamp": "2026-10-22T11:30:00Z",
        }),
    );
    let table = list(BuiltinKind::Events, vec![pulled, backoff], now());
    assert_eq!(
        table.headers(),
        [
            "Type",
            "Reason",
            "Involved Object",
            "Message",
            "Count",
            "Last Seen"
        ]
    );
    let rows: Vec<_> = table.rows.iter().map(|row| row.cells.clone()).collect();
    assert_eq!(
        rows,
        [
            [
                "Warning",
                "BackOff",
                "pod/web-1",
                "Back-off restarting failed container app",
                "7",
                "2m"
            ],
            // No count: a singleton; no lastTimestamp: first seen stands in.
            [
                "Normal",
                "Pulled",
                "pod/web-1",
                "Pulled image nginx",
                "1",
                "30m"
            ],
        ]
    );
    assert_eq!(
        table.rows[0].key,
        ObjectKey::new(Some("default".into()), "web-1.backoff")
    );
}

#[test]
fn a_series_event_reads_its_series() {
    // events.k8s.io-style: eventTime instead of firstTimestamp, a series.
    let probe = event_json(
        "web-1.unhealthy",
        serde_json::json!({
            "type": "Warning", "reason": "Unhealthy", "count": 1,
            "eventTime": "2026-10-22T10:00:00.000000Z",
            "series": {"count": 40, "lastObservedTime": "2026-10-22T11:59:30.000000Z"},
        }),
    );
    let table = list(BuiltinKind::Events, vec![probe], now());
    assert_eq!(table.rows[0].cells[4], "40");
    assert_eq!(table.rows[0].cells[5], "30s");
}

#[test]
fn an_event_without_any_time_is_unknown() {
    let bare = event_json("bare", serde_json::json!({}));
    let table = list(BuiltinKind::Events, vec![bare], now());
    assert_eq!(table.rows[0].cells[5], "<unknown>");
}

#[test]
fn an_unnamed_involved_object_shows_its_kind() {
    let node = event_json(
        "node",
        serde_json::json!({"involvedObject": {"kind": "Node"}}),
    );
    assert_eq!(
        list(BuiltinKind::Events, vec![node], now()).rows[0].cells[2],
        "node"
    );
    let unnamed = event_json(
        "unnamed",
        serde_json::json!({"involvedObject": {"kind": "Node", "name": ""}}),
    );
    assert_eq!(
        list(BuiltinKind::Events, vec![unnamed], now()).rows[0].cells[2],
        "node"
    );
}

#[test]
fn events_involve_the_object_with_their_uid() {
    let about_web = typed_event("e", serde_json::json!({}));
    assert!(event_involves(
        &about_web,
        "Pod",
        &pod("web-1", Some("uid-pod")).metadata
    ));
    // Same name, recreated: a different object.
    assert!(!event_involves(
        &about_web,
        "Pod",
        &pod("web-1", Some("uid-new")).metadata
    ));
    assert!(!event_involves(
        &about_web,
        "Pod",
        &pod("web-2", Some("uid-pod2")).metadata
    ));
}

#[test]
fn without_a_uid_kind_and_name_decide() {
    let no_uid = typed_event(
        "e",
        serde_json::json!({"involvedObject": {"kind": "Pod", "name": "web-1",
                                              "namespace": "default"}}),
    );
    assert!(event_involves(
        &no_uid,
        "Pod",
        &pod("web-1", Some("uid-pod")).metadata
    ));
    assert!(!event_involves(
        &no_uid,
        "Deployment",
        &pod("web-1", None).metadata
    ));
    assert!(!event_involves(
        &no_uid,
        "Pod",
        &pod("web-2", None).metadata
    ));
}
