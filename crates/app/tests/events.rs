//! The Events list component (#17): kubectl's columns, newest first, and
//! the involvedObject filter a related section applies.

use astrolabe::projection::events::{event_involves, project_events};
use astrolabe::store::ObjectKey;
use astrolabe_kube::k8s_openapi::api::core::v1::{Event, Pod};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

/// Synthetic Event (plausible shape, not captured from a server).
fn event(name: &str, extra: serde_json::Value) -> Event {
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
    serde_json::from_value(value).unwrap()
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
    let backoff = event(
        "web-1.backoff",
        serde_json::json!({
            "type": "Warning", "reason": "BackOff",
            "message": "Back-off restarting failed container app\n",
            "count": 7,
            "firstTimestamp": "2026-10-22T11:00:00Z",
            "lastTimestamp": "2026-10-22T11:58:00Z",
        }),
    );
    let pulled = event(
        "web-1.pulled",
        serde_json::json!({
            "type": "Normal", "reason": "Pulled", "message": "Pulled image nginx",
            "firstTimestamp": "2026-10-22T11:30:00Z",
        }),
    );
    let table = project_events(&[&pulled, &backoff], now());
    assert_eq!(
        table.columns,
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
    let probe = event(
        "web-1.unhealthy",
        serde_json::json!({
            "type": "Warning", "reason": "Unhealthy", "count": 1,
            "eventTime": "2026-10-22T10:00:00.000000Z",
            "series": {"count": 40, "lastObservedTime": "2026-10-22T11:59:30.000000Z"},
        }),
    );
    let table = project_events(&[&probe], now());
    assert_eq!(table.rows[0].cells[4], "40");
    assert_eq!(table.rows[0].cells[5], "30s");
}

#[test]
fn an_event_without_any_time_is_unknown() {
    let bare = event("bare", serde_json::json!({}));
    let table = project_events(&[&bare], now());
    assert_eq!(table.rows[0].cells[5], "<unknown>");
}

#[test]
fn an_unnamed_involved_object_shows_its_kind() {
    let node = event(
        "node",
        serde_json::json!({"involvedObject": {"kind": "Node"}}),
    );
    assert_eq!(project_events(&[&node], now()).rows[0].cells[2], "node");
}

#[test]
fn events_involve_the_object_with_their_uid() {
    let about_web = event("e", serde_json::json!({}));
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
    let no_uid = event(
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
