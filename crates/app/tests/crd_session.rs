use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use astrolabe::session::{ConnectRecovery, DEFAULT_NAMESPACE, ScriptedCluster, Session};
use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::{ServedResource, ServedResources};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_kube::watch::ObjectWatchMessage;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::session::{ConnectionHealth, CustomKindId, ResourceKind, WatchKey};
use astrolabe_model::session_state::RESIDENT_WATCH_CAP;
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
        cluster: ClusterId::named("kind-a"),
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

fn snapshot<K>(sender: &mut mpsc::Sender<Result<Event<K>, ClusterError>>, objects: Vec<K>) {
    sender.try_send(Ok(Event::Init)).unwrap();
    for object in objects {
        sender.try_send(Ok(Event::InitApply(object))).unwrap();
    }
    sender.try_send(Ok(Event::InitDone)).unwrap();
}

struct Harness {
    session: Entity<Session>,
    namespaces: mpsc::Sender<Result<Event<Namespace>, ClusterError>>,
    crds: mpsc::Sender<Result<Event<CustomResourceDefinition>, ClusterError>>,
    discoveries: Rc<Cell<usize>>,
    discovery: Rc<RefCell<Result<ServedResources, ClusterError>>>,
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
    let session = cx.update(|cx| {
        cx.new(|cx| Session::scripted(ClusterId::named("kind-a"), DEFAULT_NAMESPACE, cluster, cx))
    });
    Harness {
        session,
        namespaces,
        crds,
        discoveries,
        discovery,
    }
}

/// The rules (when a change schedules Discovery, which run's answer wins)
/// are `SessionState`'s, tested in `astrolabe-model`; this pins that Session
/// carries them out with the 400 ms timer.
#[gpui_kit::test]
fn a_definition_change_runs_discovery_after_the_400ms_debounce(cx: &mut TestAppContext) {
    let mut h = harness(cx, Vec::new());
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 1, "discovery runs on connect");
    snapshot(&mut h.crds, Vec::new());
    cx.run_until_parked();

    h.crds
        .try_send(Ok(Event::Apply(crd("acme.io", "Backup", "Cluster"))))
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(399));
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 1);

    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(h.discoveries.get(), 2);
    drop(h.namespaces);
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
        assert_eq!(session.state().namespace(), "default");
        assert!(
            session
                .state()
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
            .state()
            .object_watch(&custom_key(Some("default"), "CronTab"))
            .unwrap();
        assert_eq!(watch.status().health(), ConnectionHealth::Healthy);
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
            .state()
            .object_watch(&custom_key(Some("team-a"), "CronTab"))
            .unwrap();
        assert_eq!(team.objects().len(), 2);
        let default = session
            .state()
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
        assert_eq!(session.state().namespace(), "default");
        let watch = session
            .state()
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
                .state()
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

fn pod_key(namespace: &str) -> WatchKey {
    WatchKey {
        cluster: ClusterId::named("kind-a"),
        kind: ResourceKind::Builtin(BuiltinKind::Pods),
        namespace: Some(namespace.to_string()),
    }
}

/// The policy (which watch goes) is `SessionState`'s, tested in
/// `astrolabe-model`; this pins that Session carries out the stop: the
/// drain ends, so its channel closes, and asking again starts a new drain.
#[gpui_kit::test]
fn an_evicted_watch_is_stopped_and_starts_again_when_asked(cx: &mut TestAppContext) {
    let count = RESIDENT_WATCH_CAP + 1;
    let (senders, scripted): (Vec<_>, Vec<_>) = (0..count)
        .map(|index| {
            let (sender, receiver) = mpsc::channel(32);
            (sender, (pod_key(&format!("ns{index}")), receiver))
        })
        .unzip();
    let mut senders: Vec<mpsc::Sender<ObjectWatchMessage>> = senders;
    let h = harness(cx, scripted);
    for index in 0..count {
        h.session.update(cx, |session, cx| {
            session.watch_builtin(BuiltinKind::Pods, Some(&format!("ns{index}")), cx)
        });
    }
    cx.run_until_parked();

    assert!(senders[0].is_closed(), "the evicted drain ended");
    assert!(!senders[1].is_closed(), "a kept drain still runs");

    // Back to the evicted namespace: its watch starts, on a script that has
    // run out, so only the state shows it.
    h.session.update(cx, |session, cx| {
        session.watch_builtin(BuiltinKind::Pods, Some("ns0"), cx)
    });
    cx.read(|cx| {
        let state = h.session.read(cx).state();
        assert!(state.object_watch(&pod_key("ns0")).is_some());
    });
    senders.clear();
}

/// A scripted Cluster whose connect fails until the returned handle recovers
/// it: every watch reports the error and stays open, Discovery answers with
/// it, and nothing calls `reconnect`.
struct Recovering {
    session: Entity<Session>,
    recovery: ConnectRecovery,
    namespaces: mpsc::Sender<Result<Event<Namespace>, ClusterError>>,
    crds: mpsc::Sender<Result<Event<CustomResourceDefinition>, ClusterError>>,
    pods: mpsc::Sender<ObjectWatchMessage>,
    crontabs: mpsc::Sender<ObjectWatchMessage>,
    discoveries: Rc<Cell<usize>>,
}

fn recovering(cx: &mut TestAppContext) -> Recovering {
    let (namespaces, namespace_events) = mpsc::channel(32);
    let (crds, crd_events) = mpsc::channel(32);
    let (pods, pod_events) = mpsc::channel(32);
    let (crontabs, crontab_events) = mpsc::channel(32);
    let discoveries = Rc::new(Cell::new(0));
    let (cluster, recovery) = ScriptedCluster::new(namespace_events)
        .crds(crd_events)
        .watch(
            WatchKey {
                cluster: ClusterId::named("kind-a"),
                kind: ResourceKind::Builtin(BuiltinKind::Pods),
                namespace: Some(DEFAULT_NAMESPACE.to_string()),
            },
            pod_events,
        )
        .watch(
            custom_key(Some(DEFAULT_NAMESPACE), "CronTab"),
            crontab_events,
        )
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
        })
        .connect_fails_until_recovered(ClusterError::connect("kubelogin is not ready"));
    let session = cx.update(|cx| {
        cx.new(|cx| Session::scripted(ClusterId::named("kind-a"), DEFAULT_NAMESPACE, cluster, cx))
    });
    Recovering {
        session,
        recovery,
        namespaces,
        crds,
        pods,
        crontabs,
        discoveries,
    }
}

/// The rules live in `SessionState` and the retry in `Cluster`; this pins
/// that a Session whose first connect failed brings back its built-in list,
/// its custom-kind list and the stale-discovery banner without a call to
/// `reconnect` (#277).
#[gpui_kit::test]
fn a_session_recovers_by_itself_from_a_failed_first_connect(cx: &mut TestAppContext) {
    let mut h = recovering(cx);
    cx.update(|cx| {
        h.session.update(cx, |session, cx| {
            session.open_builtin_list(BuiltinKind::Pods, cx);
        });
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let state = h.session.read(cx).state();
        assert_eq!(state.status().health, ConnectionHealth::Stale);
        assert!(state.status().show_stale_warning());
        assert!(state.discovery_failure().is_some());
    });

    // The connect works from now on; the watches' next events arrive.
    h.recovery.recover();
    snapshot(&mut h.namespaces, vec![Namespace::default()]);
    snapshot(
        &mut h.crds,
        vec![crd("stable.example.com", "CronTab", "Namespaced")],
    );
    snapshot(&mut h.pods, Vec::new());
    cx.run_until_parked();

    let key = cx
        .update(|cx| {
            h.session.update(cx, |session, cx| {
                session.open_custom_list(&crontab_id(), cx)
            })
        })
        .expect("the CronTab kind is served once Discovery ran again");
    snapshot(&mut h.crontabs, vec![object(Some("default"), "nightly")]);
    cx.run_until_parked();

    cx.read(|cx| {
        let state = h.session.read(cx).state();
        assert!(!state.status().show_stale_warning());
        assert!(state.discovery_failure().is_none());
        assert_eq!(state.status().health, ConnectionHealth::Healthy);
        let pods = state
            .builtin_watch(BuiltinKind::Pods, Some(DEFAULT_NAMESPACE))
            .unwrap();
        assert_eq!(pods.status().health(), ConnectionHealth::Healthy);
        let crontabs = state.object_watch(&key).unwrap();
        assert!(crontabs.get(Some("default"), "nightly").is_some());
    });
}

/// With no Definition event to trigger it, the failed run retries on its
/// own timer.
#[gpui_kit::test]
fn a_failed_discovery_retries_on_a_timer_with_no_user_action(cx: &mut TestAppContext) {
    let h = recovering(cx);
    cx.run_until_parked();
    let failed = |cx: &TestAppContext| {
        cx.read(|cx| h.session.read(cx).state().discovery_failure().is_some())
    };
    assert!(failed(cx));

    h.recovery.recover();
    cx.executor().advance_clock(Duration::from_millis(999));
    cx.run_until_parked();
    assert!(failed(cx), "the first retry waits a second");
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();

    assert!(!failed(cx));
    assert_eq!(h.discoveries.get(), 1);
}
