//! Ages keep counting while a list or detail is open, with no cluster
//! activity (#70). Synthetic objects (plausible shapes, never captured
//! from a real apiserver).

mod support;

use std::time::Duration;

use astrolabe::builtin::BuiltinKind;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use futures::channel::mpsc;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use support::{Sender, Ui, follow_test_clock, key, open, snapshot};

/// The HPA list's Age column.
const AGE: &str = "cell-0-5";

fn start() -> Timestamp {
    "2026-10-26T12:00:00Z".parse().unwrap()
}

fn hpa(created: &str) -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-hpa",
            "creationTimestamp": created},
        "spec": {
            "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
            "minReplicas": 1, "maxReplicas": 3
        }
    }))
    .unwrap()
}

/// A Pod created a quarter of an hour ago whose container restarted a
/// minute ago.
fn pod() -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": "default", "uid": "uid-pod",
            "creationTimestamp": "2026-10-26T11:45:00Z"},
        "spec": {"containers": [{"name": "app", "image": "nginx"}]},
        "status": {"phase": "Running", "containerStatuses": [{
            "name": "app", "image": "nginx", "imageID": "", "ready": true, "restartCount": 1,
            "state": {"running": {"startedAt": "2026-10-26T11:59:00Z"}}
        }]}
    }))
    .unwrap()
}

/// Open `kind`'s list in `default` holding `object`.
fn open_list(
    cx: &mut TestAppContext,
    kind: BuiltinKind,
    object: DynamicObject,
) -> (Ui<'_>, Sender<DynamicObject>) {
    follow_test_clock(cx, start());
    let (mut objects, events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(key(kind, "default"), events)]);
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(kind.nav_id(), cx));
    snapshot(&mut objects, vec![object]);
    ui.settle();
    (ui, objects)
}

fn open_hpas<'a>(cx: &'a mut TestAppContext, created: &str) -> (Ui<'a>, Sender<DynamicObject>) {
    open_list(cx, BuiltinKind::HorizontalPodAutoscalers, hpa(created))
}

fn label(ui: &mut Ui<'_>, id: &'static str) -> String {
    ui.frame(|window, _| {
        window
            .try_find(id)
            .unwrap_or_else(|| panic!("{id} not rendered"))
            .label()
            .unwrap_or_default()
            .to_string()
    })
}

fn advance(ui: &mut Ui<'_>, duration: Duration) {
    ui.cx.executor().advance_clock(duration);
    ui.cx.run_until_parked();
}

#[gpui_kit::test]
fn a_young_age_counts_every_second(cx: &mut TestAppContext) {
    // `settle` moved the clock 0.8s on; the age shows whole seconds.
    let (mut ui, _hpas) = open_hpas(cx, "2026-10-26T11:59:00Z");
    assert_eq!(label(&mut ui, AGE), "60s");

    advance(&mut ui, Duration::from_secs(3));
    assert_eq!(label(&mut ui, AGE), "63s");

    advance(&mut ui, Duration::from_secs(1));
    assert_eq!(label(&mut ui, AGE), "64s");
}

#[gpui_kit::test]
fn an_old_age_counts_every_minute(cx: &mut TestAppContext) {
    let (mut ui, _hpas) = open_hpas(cx, "2026-10-26T11:45:00Z");
    assert_eq!(label(&mut ui, AGE), "15m");

    advance(&mut ui, Duration::from_secs(30));
    assert_eq!(label(&mut ui, AGE), "15m");

    advance(&mut ui, Duration::from_secs(30));
    assert_eq!(label(&mut ui, AGE), "16m");
}

#[gpui_kit::test]
fn a_new_object_brings_back_per_second_ticks(cx: &mut TestAppContext) {
    let (mut ui, mut hpas) = open_hpas(cx, "2026-10-26T11:45:00Z");
    // A fresh HPA, created as the list opened, replaces the old one.
    snapshot(&mut hpas, vec![hpa("2026-10-26T12:00:00Z")]);
    ui.cx.run_until_parked();
    assert_eq!(label(&mut ui, AGE), "0s");

    advance(&mut ui, Duration::from_secs(2));
    assert_eq!(label(&mut ui, AGE), "2s");
}

#[gpui_kit::test]
fn a_detail_age_counts_while_it_is_open(cx: &mut TestAppContext) {
    // The list's youngest age is 15m, so only the detail's own tick moves
    // the container's minute-old start.
    let (mut ui, _pods) = open_list(cx, BuiltinKind::Pods, pod());
    ui.frame(|window, cx| window.click("row-default/web-1", cx));
    ui.cx.run_until_parked();
    // Read the last drawn frame: `Ui::frame` would render the detail
    // afresh and mask a missing tick.
    let state = |ui: &mut Ui<'_>, text: &str| {
        ui.cx.update(|window, _| {
            window
                .within("detail-table-Containers")
                .try_find_by_label(text)
                .is_some()
        })
    };
    assert!(state(&mut ui, "Running since 60s"));
    assert_eq!(
        label(&mut ui, "field-Created"),
        "Created: 2026-10-26T11:45:00Z (15m ago)"
    );

    advance(&mut ui, Duration::from_secs(3));
    assert!(state(&mut ui, "Running since 63s"));
}
