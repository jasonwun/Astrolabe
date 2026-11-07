//! The stale banner of a list says why its rows are old (#282): a lost
//! connection or a denied watch.

mod support;

use astrolabe_kube::cluster::ClusterError;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::session::{DENIED_STALE_MESSAGE, STALE_MESSAGE};
use futures::channel::mpsc;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use support::{key, object, open, open_list};

fn pod() -> astrolabe_kube::kube::core::DynamicObject {
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-pod"},
        "spec": {"containers": [{"name": "app", "image": "nginx"}]}
    }))
}

#[gpui_kit::test]
fn a_list_denied_after_a_snapshot_keeps_its_rows_under_the_denial_banner(cx: &mut TestAppContext) {
    let (mut pods, events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(key(BuiltinKind::Pods, "default"), events)]);
    open_list(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()]);

    pods.try_send(Err(ClusterError::forbidden("pods is forbidden")))
        .unwrap();
    ui.settle();

    ui.frame(|window, _| {
        assert!(window.try_find("row-default/web").is_some());
        assert!(window.try_find("list-denied").is_some());
        assert!(window.try_find("list-stale").is_none());
        assert!(window.try_find("list-message").is_none());
    });
    assert_eq!(ui.label("list-denied"), DENIED_STALE_MESSAGE);
}

#[gpui_kit::test]
fn a_list_that_lost_its_connection_keeps_the_connection_banner(cx: &mut TestAppContext) {
    let (mut pods, events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(key(BuiltinKind::Pods, "default"), events)]);
    open_list(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()]);

    pods.try_send(Err(ClusterError::unreachable("refused")))
        .unwrap();
    ui.settle();

    ui.frame(|window, _| {
        assert!(window.try_find("row-default/web").is_some());
        assert!(window.try_find("list-stale").is_some());
        assert!(window.try_find("list-denied").is_none());
    });
    assert_eq!(ui.label("list-stale"), STALE_MESSAGE);
}
