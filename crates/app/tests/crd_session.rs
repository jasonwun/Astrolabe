use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use astrolabe::session::{
    ConnectionHealth, CustomKindId, ObjectWatchMessage, ResourceKind, ScriptedCluster, Session,
    WatchError, WatchKey,
};
use astrolabe_kube::crd::KindResource;
use astrolabe_kube::discovery::{ServedResource, ServedResources};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::{AppContext, Entity, TestAppContext};

/// Synthetic CRD (plausible shape, not captured from a server).
fn crd(group: &str, kind: &str, scope: &str) -> CustomResourceDefinition {
    let plural = format!("{}s", kind.to_lowercase());
    serde_json::from_value(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": format!("{plural}.{group}")},
        "spec": {
            "group": group, "scope": scope,
            "names": {"kind": kind, "plural": plural},
            "versions": [{"name": "v1", "served": true, "storage": true}]
        }
    }))
    .unwrap()
}

fn served(group: &str, kind: &str, namespaced: bool) -> ServedResource {
    ServedResource {
        resource: ApiResource {
            group: group.to_string(),
            version: "v1".to_string(),
            api_version: format!("{group}/v1"),
            kind: kind.to_string(),
            plural: format!("{}s", kind.to_lowercase()),
        },
        namespaced,
    }
}

/// Synthetic custom resource (plausible shape, not captured from a server).
fn object(namespace: Option<&str>, name: &str) -> DynamicObject {
    let mut metadata = serde_json::json!({"name": name});
    if let Some(namespace) = namespace {
        metadata["namespace"] = namespace.into();
    }
    serde_json::from_value(serde_json::json!({
        "apiVersion": "stable.example.com/v1", "kind": "CronTab", "metadata": metadata
    }))
    .unwrap()
}

fn custom_key(namespace: Option<&str>, kind: &str) -> WatchKey {
    WatchKey {
        cluster: "kind-a".to_string(),
        kind: ResourceKind::Custom {
            group: "stable.example.com".to_string(),
            version: "v1".to_string(),
            kind: kind.to_string(),
        },
        namespace: namespace.map(str::to_string),
    }
}

fn crontab_id() -> CustomKindId {
    CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "CronTab".to_string(),
    }
}

fn snapshot<K>(sender: &mut mpsc::Sender<Result<Event<K>, WatchError>>, objects: Vec<K>) {
    sender.try_send(Ok(Event::Init)).unwrap();
    for object in objects {
        sender.try_send(Ok(Event::InitApply(object))).unwrap();
    }
    sender.try_send(Ok(Event::InitDone)).unwrap();
}

struct Harness {
    session: Entity<Session>,
    namespaces: mpsc::Sender<Result<Event<Namespace>, WatchError>>,
    crds: mpsc::Sender<Result<Event<CustomResourceDefinition>, WatchError>>,
    discoveries: Rc<Cell<usize>>,
    discovery: Rc<RefCell<Result<ServedResources, String>>>,
}

fn harness(
    cx: &mut TestAppContext,
    custom: Vec<(WatchKey, mpsc::Receiver<ObjectWatchMessage>)>,
) -> Harness {
    let (namespaces, namespace_events) = mpsc::channel(32);
    let (crds, crd_events) = mpsc::channel(32);
    let discoveries = Rc::new(Cell::new(0));
    let discovery = Rc::new(RefCell::new(Ok(ServedResources::default())));
    let mut cluster = ScriptedCluster::new(namespace_events)
        .crds(crd_events)
        .discover({
            let discoveries = discoveries.clone();
            let discovery = discovery.clone();
            move || {
                discoveries.set(discoveries.get() + 1);
                discovery.borrow().clone()
            }
        });
    for (key, receiver) in custom {
        cluster = cluster.watch(key, receiver);
    }
    let session = cx.update(|cx| cx.new(|cx| Session::scripted("kind-a", cluster, cx)));
    Harness {
        session,
        namespaces,
        crds,
        discoveries,
        discovery,
    }
}

fn kind_names(session: &Session) -> Vec<(String, Vec<String>)> {
    session
        .catalog()
        .iter()
        .map(|group| {
            (
                group.group.clone(),
                group.kinds.iter().map(|kind| kind.kind.clone()).collect(),
            )
        })
        .collect()
}

#[gpui_kit::test]
fn crd_changes_rerun_discovery_after_a_400ms_debounce(cx: &mut TestAppContext) {
    let mut h = harness(cx, Vec::new());
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 1, "discovery runs on connect");

    snapshot(
        &mut h.crds,
        vec![crd("stable.example.com", "CronTab", "Namespaced")],
    );
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(
        h.discoveries.get(),
        2,
        "a kind unresolved by the racing connect-time discovery rediscovers"
    );
    cx.read(|cx| {
        let session = h.session.read(cx);
        assert_eq!(
            kind_names(session),
            [(
                "stable.example.com".to_string(),
                vec!["CronTab".to_string()]
            )]
        );
        // Discovery still does not list the CRD: not served (yet).
        assert_eq!(
            session.catalog()[0].kinds[0].resource,
            KindResource::NotServed
        );
    });

    *h.discovery.borrow_mut() = Ok(ServedResources::new([
        served("stable.example.com", "CronTab", true),
        served("acme.io", "Backup", false),
    ]));
    h.crds
        .try_send(Ok(Event::Apply(crd("acme.io", "Backup", "Cluster"))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    h.crds
        .try_send(Ok(Event::Apply(crd("acme.io", "Backup", "Cluster"))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(399));
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 2, "debounce restarts on every change");
    cx.read(|cx| {
        // Navigation follows the CRD watch before discovery catches up.
        assert_eq!(kind_names(h.session.read(cx)).len(), 2);
    });

    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 3);
    cx.read(|cx| {
        let session = h.session.read(cx);
        for group in session.catalog() {
            assert!(matches!(group.kinds[0].resource, KindResource::Served(_)));
        }
    });

    h.crds
        .try_send(Ok(Event::Delete(crd("acme.io", "Backup", "Cluster"))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 4);
    cx.read(|cx| assert_eq!(kind_names(h.session.read(cx)).len(), 1));
    drop(h.namespaces);
}

#[gpui_kit::test]
fn a_first_crd_snapshot_already_discovered_does_not_rediscover(cx: &mut TestAppContext) {
    let (namespaces, namespace_events) = mpsc::channel::<Result<Event<Namespace>, WatchError>>(32);
    let (mut crds, crd_events) = mpsc::channel(32);
    let discoveries = Rc::new(Cell::new(0));
    let cluster = ScriptedCluster::new(namespace_events)
        .crds(crd_events)
        .discover({
            let discoveries = discoveries.clone();
            move || {
                discoveries.set(discoveries.get() + 1);
                Ok(ServedResources::new([served(
                    "stable.example.com",
                    "CronTab",
                    true,
                )]))
            }
        });
    let _session = cx.update(|cx| cx.new(|cx| Session::scripted("kind-a", cluster, cx)));
    snapshot(
        &mut crds,
        vec![crd("stable.example.com", "CronTab", "Namespaced")],
    );
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(discoveries.get(), 1);
    drop(namespaces);
}

#[gpui_kit::test]
fn failed_rediscovery_keeps_kinds_served_behind_the_stale_banner(cx: &mut TestAppContext) {
    let mut h = harness(cx, Vec::new());
    *h.discovery.borrow_mut() = Ok(ServedResources::new([served(
        "stable.example.com",
        "CronTab",
        true,
    )]));
    snapshot(&mut h.namespaces, Vec::new());
    snapshot(
        &mut h.crds,
        vec![crd("stable.example.com", "CronTab", "Namespaced")],
    );
    cx.run_until_parked();
    h.crds
        .try_send(Ok(Event::Apply(crd(
            "stable.example.com",
            "CronTab",
            "Namespaced",
        ))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    cx.read(|cx| assert!(!h.session.read(cx).show_stale_warning()));

    *h.discovery.borrow_mut() = Err("connection refused".to_string());
    h.crds
        .try_send(Ok(Event::Apply(crd(
            "stable.example.com",
            "CronTab",
            "Namespaced",
        ))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    cx.read(|cx| {
        let session = h.session.read(cx);
        assert!(session.show_stale_warning());
        assert_eq!(session.health(), ConnectionHealth::Healthy);
        assert!(matches!(
            session.catalog()[0].kinds[0].resource,
            KindResource::Served(_)
        ));
    });
}

#[gpui_kit::test]
fn rbac_denied_crd_list_leaves_custom_resources_empty(cx: &mut TestAppContext) {
    let mut h = harness(cx, Vec::new());
    snapshot(&mut h.namespaces, Vec::new());
    h.crds
        .try_send(Err(WatchError::forbidden(
            "customresourcedefinitions is forbidden",
        )))
        .unwrap();
    cx.run_until_parked();

    cx.read(|cx| {
        let session = h.session.read(cx);
        assert!(session.crd_access_denied());
        assert!(session.catalog().is_empty());
        assert_eq!(session.health(), ConnectionHealth::Healthy);
        assert!(!session.show_stale_warning());
    });
}

#[gpui_kit::test]
fn custom_resource_watches_start_lazily_per_namespace_and_stay_resident(cx: &mut TestAppContext) {
    let (mut default_ns, default_events) = mpsc::channel(32);
    let (mut team_a, team_a_events) = mpsc::channel(32);
    let (_cluster_scoped, cluster_events) = mpsc::channel(32);
    let mut h = harness(
        cx,
        vec![
            (custom_key(Some("default"), "CronTab"), default_events),
            (custom_key(Some("team-a"), "CronTab"), team_a_events),
            (custom_key(None, "Backup"), cluster_events),
        ],
    );
    *h.discovery.borrow_mut() = Ok(ServedResources::new([
        served("stable.example.com", "CronTab", true),
        served("stable.example.com", "Backup", false),
    ]));
    snapshot(
        &mut h.crds,
        vec![
            crd("stable.example.com", "CronTab", "Namespaced"),
            crd("stable.example.com", "Backup", "Cluster"),
        ],
    );
    cx.run_until_parked();
    h.crds
        .try_send(Ok(Event::Apply(crd(
            "stable.example.com",
            "CronTab",
            "Namespaced",
        ))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    cx.read(|cx| {
        let session = h.session.read(cx);
        assert_eq!(session.namespace(), "default");
        assert!(
            session
                .object_watch(&custom_key(Some("default"), "CronTab"))
                .is_none()
        );
    });

    let key = h.session.update(cx, |session, cx| {
        session.open_custom_list(&crontab_id(), cx)
    });
    assert_eq!(key, Some(custom_key(Some("default"), "CronTab")));
    snapshot(&mut default_ns, vec![object(Some("default"), "nightly")]);
    cx.run_until_parked();
    cx.read(|cx| {
        let watch = h
            .session
            .read(cx)
            .object_watch(&custom_key(Some("default"), "CronTab"))
            .unwrap();
        assert_eq!(watch.health(), ConnectionHealth::Healthy);
        assert_eq!(watch.objects().len(), 1);
    });

    // Switching namespace starts that namespace's watch; the old one stays.
    h.session.update(cx, |session, cx| {
        session.set_namespace("team-a".to_string(), cx)
    });
    snapshot(
        &mut team_a,
        vec![object(Some("team-a"), "a"), object(Some("team-a"), "b")],
    );
    cx.run_until_parked();
    cx.read(|cx| {
        let session = h.session.read(cx);
        let team = session
            .object_watch(&custom_key(Some("team-a"), "CronTab"))
            .unwrap();
        assert_eq!(team.objects().len(), 2);
        let default = session
            .object_watch(&custom_key(Some("default"), "CronTab"))
            .unwrap();
        assert_eq!(default.objects().len(), 1);
    });

    // A cluster-scoped kind ignores the namespace selection.
    let backup = CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "Backup".to_string(),
    };
    let key = h
        .session
        .update(cx, |session, cx| session.open_custom_list(&backup, cx));
    assert_eq!(key, Some(custom_key(None, "Backup")));
}

#[gpui_kit::test]
fn unserved_kinds_start_no_watch(cx: &mut TestAppContext) {
    let mut h = harness(cx, Vec::new());
    snapshot(
        &mut h.crds,
        vec![crd("stable.example.com", "CronTab", "Namespaced")],
    );
    cx.run_until_parked();

    let key = h.session.update(cx, |session, cx| {
        session.open_custom_list(&crontab_id(), cx)
    });
    assert_eq!(key, None);
    cx.read(|cx| {
        assert!(
            h.session
                .read(cx)
                .object_watch(&custom_key(Some("default"), "CronTab"))
                .is_none()
        )
    });
}

#[gpui_kit::test]
fn an_object_watch_ignores_the_namespace_selection_and_list(cx: &mut TestAppContext) {
    let (mut team_a, team_a_events) = mpsc::channel(32);
    let mut h = harness(
        cx,
        vec![(custom_key(Some("team-a"), "CronTab"), team_a_events)],
    );
    *h.discovery.borrow_mut() = Ok(ServedResources::new([
        served("stable.example.com", "CronTab", true),
        served("stable.example.com", "Backup", false),
    ]));
    snapshot(
        &mut h.crds,
        vec![
            crd("stable.example.com", "CronTab", "Namespaced"),
            crd("stable.example.com", "Backup", "Cluster"),
        ],
    );
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();

    let key = h.session.update(cx, |session, cx| {
        session.watch_custom_object(&crontab_id(), Some("team-a".to_string()), cx)
    });
    assert_eq!(key, Some(custom_key(Some("team-a"), "CronTab")));
    snapshot(&mut team_a, vec![object(Some("team-a"), "nightly")]);
    cx.run_until_parked();
    cx.read(|cx| {
        let session = h.session.read(cx);
        assert_eq!(session.namespace(), "default");
        let watch = session
            .object_watch(&custom_key(Some("team-a"), "CronTab"))
            .unwrap();
        assert!(watch.get(Some("team-a"), "nightly").is_some());
    });

    // Not a list visit: a namespace switch starts no watch.
    h.session.update(cx, |session, cx| {
        session.set_namespace("team-b".to_string(), cx)
    });
    cx.read(|cx| {
        assert!(
            h.session
                .read(cx)
                .object_watch(&custom_key(Some("team-b"), "CronTab"))
                .is_none()
        )
    });

    let backup = CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "Backup".to_string(),
    };
    let key = h.session.update(cx, |session, cx| {
        session.watch_custom_object(&backup, None, cx)
    });
    assert_eq!(key, Some(custom_key(None, "Backup")));
}
