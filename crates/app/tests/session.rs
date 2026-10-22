use astrolabe::session::{ConnectionHealth, ResourceKind, Session};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::{AppContext, TestAppContext};

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
        .namespaces()
        .into_iter()
        .map(|namespace| namespace.metadata.name.clone().unwrap())
        .collect()
}

#[gpui_kit::test]
fn session_drains_namespace_events_and_tracks_stale_recovery(cx: &mut TestAppContext) {
    let (mut sender, receiver) = mpsc::channel(32);
    let session = cx.update(|cx| cx.new(|cx| Session::from_events("kind-a", receiver, cx)));

    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(session.watch_key().cluster, "kind-a");
        assert_eq!(session.watch_key().kind, ResourceKind::Namespaces);
        assert_eq!(session.watch_key().namespace, None);
        assert_eq!(session.health(), ConnectionHealth::Connecting);
        assert!(!session.show_stale_warning());
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
        assert_eq!(session.health(), ConnectionHealth::Healthy);
        assert!(!session.show_stale_warning());
    });

    sender.try_send(Err("connection lost".to_string())).unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Stale);
        assert!(session.show_stale_warning());
    });

    sender.try_send(Ok(Event::Init)).unwrap();
    sender
        .try_send(Ok(Event::InitApply(namespace("kube-system"))))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Connecting);
        assert!(session.show_stale_warning());
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
        assert_eq!(session.health(), ConnectionHealth::Healthy);
        assert!(!session.show_stale_warning());
    });

    drop(sender);
    cx.run_until_parked();
    cx.read(|cx| {
        let session = session.read(cx);
        assert_eq!(names(session), ["default"]);
        assert_eq!(session.health(), ConnectionHealth::Stale);
        assert!(session.show_stale_warning());
    });
}

#[gpui_kit::test]
fn session_coalesces_queued_events_into_one_notification(cx: &mut TestAppContext) {
    let (mut sender, receiver) = mpsc::channel(32);
    let session = cx.update(|cx| cx.new(|cx| Session::from_events("kind-a", receiver, cx)));
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
