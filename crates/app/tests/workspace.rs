use astrolabe::session::NamespaceWatchMessage;
use astrolabe::workspace::{STALE_MESSAGE, WorkspaceView};
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, NamespaceStatus};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::TestAppContext;
use gpui_kit::test::{TestQueryExt, TestWindowExt};

fn namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        status: Some(NamespaceStatus {
            phase: Some("Active".to_string()),
            ..NamespaceStatus::default()
        }),
        ..Namespace::default()
    }
}

fn initial_snapshot(sender: &mut mpsc::Sender<NamespaceWatchMessage>, name: &str) {
    sender.try_send(Ok(Event::Init)).unwrap();
    sender
        .try_send(Ok(Event::InitApply(namespace(name))))
        .unwrap();
    sender.try_send(Ok(Event::InitDone)).unwrap();
}

#[gpui_kit::test]
fn workspace_replaces_namespace_rows_keeps_stale_data_and_retains_sessions(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let (mut sender_a, receiver_a) = mpsc::channel(32);
    let (mut sender_b, receiver_b) = mpsc::channel(32);
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        WorkspaceView::from_events(
            vec![
                ("kind-a".to_string(), receiver_a),
                ("kind-b".to_string(), receiver_b),
            ],
            window,
            cx,
        )
    });

    initial_snapshot(&mut sender_a, "default");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("default").is_some());
        assert!(window.try_find_by_label("kind-a: Healthy").is_some());
        assert!(window.try_find_by_label("kind-b: Not open").is_some());
        assert!(window.try_find("namespace-stale").is_none());
    });

    sender_a
        .try_send(Err("connection lost".to_string()))
        .unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("default").is_some());
        assert!(window.try_find_by_label("kind-a: Stale").is_some());
        assert!(window.try_find_by_label(STALE_MESSAGE).is_some());
    });

    initial_snapshot(&mut sender_a, "default");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("namespace-stale").is_none());
        assert!(window.try_find_by_label("kind-a: Healthy").is_some());
        window.click("cluster-kind-b", cx);
        assert!(window.try_find_by_label("kind-b: Connecting").is_some());
    });

    initial_snapshot(&mut sender_b, "kube-system");
    sender_a
        .try_send(Ok(Event::Apply(namespace("other"))))
        .unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("kube-system").is_some());
        assert!(window.try_find_by_label("kind-b: Healthy").is_some());
        window.click("cluster-kind-a", cx);
        assert!(window.try_find_by_label("default").is_some());
        assert!(window.try_find_by_label("other").is_some());
        assert!(window.try_find_by_label("kube-system").is_none());
    });
}
