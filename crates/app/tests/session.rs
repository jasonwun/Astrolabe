use astrolabe::session::{ScriptedCluster, Session};
use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::session::{ConnectionHealth, ResourceKind};
use futures::channel::mpsc;
use gpui_kit::{AppContext, TestAppContext};
use support::LogCapture;

mod support;

fn namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

fn names(session: &Session) -> Vec<String> {
    session
        .state()
        .namespaces()
        .objects()
        .map(|namespace| namespace.metadata.name.clone().unwrap())
        .collect()
}

#[gpui_kit::test]
fn session_drains_namespace_events_and_tracks_stale_recovery(cx: &mut TestAppContext) {
    let logs = LogCapture::default();
    logs.install();
    let (mut sender, receiver) = mpsc::channel(32);
    let session =
        cx.update(|cx| cx.new(|cx| Session::from_events(ClusterId::named("kind-a"), receiver, cx)));

    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(session.state().watch_key().cluster.name(), "kind-a");
        assert_eq!(session.state().watch_key().kind, ResourceKind::Namespaces);
        assert_eq!(session.state().watch_key().namespace, None);
        assert_eq!(
            session.state().status().health,
            ConnectionHealth::Connecting
        );
        assert!(!session.state().status().show_stale_warning());
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    sender
        .try_send(Ok(Event::InitApply(namespace("default"))))
        .unwrap();
    sender.try_send(Ok(Event::InitDone)).unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.state().status().health, ConnectionHealth::Healthy);
        assert!(!session.state().status().show_stale_warning());
    });

    sender
        .try_send(Err(ClusterError::new("connection lost")))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.state().status().health, ConnectionHealth::Stale);
        assert!(session.state().status().show_stale_warning());
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    sender
        .try_send(Ok(Event::InitApply(namespace("kube-system"))))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(
            session.state().status().health,
            ConnectionHealth::Connecting
        );
        assert!(session.state().status().show_stale_warning());
    });

    sender.try_send(Ok(Event::InitDone)).unwrap();
    sender
        .try_send(Ok(Event::Apply(namespace("default"))))
        .unwrap();
    sender
        .try_send(Ok(Event::Delete(namespace("kube-system"))))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.state().status().health, ConnectionHealth::Healthy);
        assert!(!session.state().status().show_stale_warning());
    });

    drop(sender);
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.state().status().health, ConnectionHealth::Stale);
        assert!(session.state().status().show_stale_warning());
    });

    // The RUST_LOG-driven event trail covers the health transitions and the
    // snapshot replacement; the cluster layer's watch tests cover its part.
    let text = logs.text();
    assert!(
        text.contains("namespace session health changed"),
        "missing health transition log:\n{text}"
    );
    assert!(
        text.contains("namespace snapshot replaced"),
        "missing snapshot replacement log:\n{text}"
    );
}

#[gpui_kit::test]
fn session_coalesces_queued_events_into_one_notification(cx: &mut TestAppContext) {
    let (mut sender, receiver) = mpsc::channel(32);
    let session =
        cx.update(|cx| cx.new(|cx| Session::from_events(ClusterId::named("kind-a"), receiver, cx)));
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
    cx.update(|cx| {
        cx.observe(&session, {
            let notifications = notifications.clone();
            move |_, _| notifications.set(notifications.get() + 1)
        })
        .detach();
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    for index in 0..10 {
        sender
            .try_send(Ok(Event::InitApply(namespace(&format!("ns-{index}")))))
            .unwrap();
    }
    sender.try_send(Ok(Event::InitDone)).unwrap();
    cx.run_until_parked();

    cx.read(|cx| {
        assert_eq!(names(session.read(cx)).len(), 10);
        assert_eq!(notifications.get(), 1);
    });
}

#[gpui_kit::test]
fn session_reconnects_after_a_failed_connect(cx: &mut TestAppContext) {
    let (mut namespaces, namespace_events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let scripted = ScriptedCluster::new(namespace_events)
        .watch(support::key(BuiltinKind::Pods, "default"), pod_events)
        .connect_fails(ClusterError::new("kubelogin is not ready"));
    let session = cx.update(|cx| {
        cx.new(|cx| Session::scripted(ClusterId::named("kind-a"), "default", scripted, cx))
    });
    cx.update(|cx| {
        session.update(cx, |session, cx| {
            session.open_builtin_list(BuiltinKind::Pods, cx);
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let state = session.read(cx).state();
        assert_eq!(state.status().health, ConnectionHealth::Failed);
        assert!(state.discovery_failure().is_some());
        assert!(
            state
                .builtin_watch(BuiltinKind::Pods, Some("default"))
                .is_some_and(|watch| watch.status().health() == ConnectionHealth::Failed)
        );
    });

    cx.update(|cx| session.update(cx, |session, cx| session.reconnect(cx)));
    support::snapshot(&mut namespaces, vec![namespace("default")]);
    support::snapshot(
        &mut pods,
        vec![support::object(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "web", "namespace": "default"},
        }))],
    );
    cx.run_until_parked();

    cx.read(|cx| {
        let state = session.read(cx).state();
        assert_eq!(names(session.read(cx)), ["default"]);
        assert_eq!(state.status().health, ConnectionHealth::Healthy);
        assert!(state.discovery_failure().is_none());
        let watch = state
            .builtin_watch(BuiltinKind::Pods, Some("default"))
            .unwrap();
        assert_eq!(watch.status().health(), ConnectionHealth::Healthy);
        assert!(watch.get(Some("default"), "web").is_some());
    });
}

/// The Cluster layer stops a watch once its reader is dropped (#274); this
/// pins that a reconnect drops the reader of the watch it replaces.
#[gpui_kit::test]
fn a_reconnect_drops_the_watch_it_replaces(cx: &mut TestAppContext) {
    let (_namespaces, namespace_events) = mpsc::channel(32);
    let (pods, pod_events) = mpsc::channel(32);
    let scripted = ScriptedCluster::new(namespace_events)
        .watch(support::key(BuiltinKind::Pods, "default"), pod_events);
    let session = cx.update(|cx| {
        cx.new(|cx| Session::scripted(ClusterId::named("kind-a"), "default", scripted, cx))
    });
    cx.update(|cx| {
        session.update(cx, |session, cx| {
            session.open_builtin_list(BuiltinKind::Pods, cx);
        });
    });
    cx.run_until_parked();
    assert!(!pods.is_closed(), "the watch is being read");

    cx.update(|cx| session.update(cx, |session, cx| session.reconnect(cx)));
    cx.run_until_parked();

    assert!(pods.is_closed(), "the replaced watch's reader was dropped");
}
