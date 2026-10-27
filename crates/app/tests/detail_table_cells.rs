//! Detail tables end an overlong cell in an ellipsis inside its column,
//! clear of the gutter before the next one (#75). Synthetic objects
//! (plausible shapes, never captured from a real apiserver).

mod support;

use astrolabe::builtin::BuiltinKind;
use astrolabe_kube::kube::core::DynamicObject;
use futures::channel::mpsc;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, Window, px};
use support::{key, open, snapshot};

/// A cell's right padding (`pr_2`): the gap its text keeps from the next.
const GUTTER: f32 = 8.;

const LONG: &str = "Container image \"busybox:1.36.1-musl-with-a-much-longer-tag\" \
                    already present on machine and more besides";

fn object(value: serde_json::Value) -> DynamicObject {
    serde_json::from_value(value).unwrap()
}

/// Within `scope`, the text element `text_id` ends one gutter (to 1px)
/// before cell `next_id` starts. Its bounds are the box the ellipsis is
/// drawn in; that the ellipsis shows is the manual check's to see.
fn assert_clear_of(window: &mut Window, scope: &'static str, text_id: String, next_id: String) {
    let scope = window.within(scope);
    let text = scope.try_find(text_id.clone()).expect(&text_id).bounds();
    let next = scope.try_find(next_id.clone()).expect(&next_id).bounds();
    let gap = next.left() - text.right();
    assert!(
        (gap - px(GUTTER)).abs() <= px(1.),
        "text ends at {:?}, next cell starts at {:?}",
        text.right(),
        next.left()
    );
}

#[gpui_kit::test]
fn a_summary_table_cell_stops_short_of_the_next_column(cx: &mut TestAppContext) {
    let (mut configs, config_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::ConfigMaps, "default"), config_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click("nav-configmaps", cx));
    let config = object(serde_json::json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": "app", "namespace": "default", "uid": "uid-cm"},
        "data": {LONG: "production"}
    }));
    snapshot(&mut configs, vec![config]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/app", cx));
    ui.settle();

    ui.frame(|window, _| {
        let data = window.within("detail-table-Data");
        assert!(data.try_find_by_label(LONG).is_some());
        assert_clear_of(
            window,
            "table-Data-row-0",
            "table-Data-cell-0-text".into(),
            "table-Data-cell-1".into(),
        );
    });
}

#[gpui_kit::test]
fn a_pod_event_message_stops_short_of_the_count_column(cx: &mut TestAppContext) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let (mut events, event_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::Pods, "default"), pod_events),
            (key(BuiltinKind::Events, "default"), event_events),
        ],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click("nav-pods", cx));
    let pod = object(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "agent-1", "namespace": "default", "uid": "uid-pod"},
        "spec": {"containers": [{"name": "app", "image": "busybox"}]},
        "status": {"phase": "Running"}
    }));
    snapshot(&mut pods, vec![pod]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/agent-1", cx));
    let event = object(serde_json::json!({
        "apiVersion": "v1", "kind": "Event",
        "metadata": {"name": "agent-1.pulled", "namespace": "default"},
        "involvedObject": {"kind": "Pod", "name": "agent-1", "namespace": "default",
                           "uid": "uid-pod"},
        "type": "Normal", "reason": "Pulled", "message": LONG, "count": 1,
        "lastTimestamp": "2026-10-22T11:58:00Z",
    }));
    snapshot(&mut events, vec![event]);
    ui.settle();

    ui.frame(|window, _| {
        let related = window.within("detail-related");
        assert!(related.try_find_by_label(LONG).is_some());
        // Message is column 3, Count column 4.
        assert_clear_of(
            window,
            "detail-related",
            "related-cell-0-3-text".into(),
            "related-cell-0-4".into(),
        );
    });
}
