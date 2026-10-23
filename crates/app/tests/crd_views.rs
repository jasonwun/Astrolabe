use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use astrolabe::session::{CustomWatchMessage, ResourceKind, ScriptedCluster, WatchError, WatchKey};
use astrolabe::workspace::WorkspaceView;
use astrolabe_kube::discovery::{ServedResource, ServedResources};
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{TestAppContext, VisualTestContext};

type Sender<K> = mpsc::Sender<Result<Event<K>, WatchError>>;

/// Synthetic CRD (plausible shape, not captured from a server).
fn crontab_crd(served_flag: bool) -> CustomResourceDefinition {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": "crontabs.stable.example.com", "creationTimestamp": "2026-01-01T00:00:00Z"},
        "spec": {
            "group": "stable.example.com", "scope": "Namespaced",
            "names": {"kind": "CronTab", "plural": "crontabs", "shortNames": ["ct"]},
            "versions": [{
                "name": "v1", "served": served_flag, "storage": true,
                "additionalPrinterColumns": [
                    {"name": "Schedule", "type": "string", "jsonPath": ".spec.cronSpec"}
                ]
            }]
        }
    }))
    .unwrap()
}

fn crontab_resource() -> ApiResource {
    ApiResource {
        group: "stable.example.com".to_string(),
        version: "v1".to_string(),
        api_version: "stable.example.com/v1".to_string(),
        kind: "CronTab".to_string(),
        plural: "crontabs".to_string(),
    }
}

/// Synthetic custom resource (plausible shape, not captured from a server).
fn crontab(name: &str, schedule: &str) -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "stable.example.com/v1", "kind": "CronTab",
        "metadata": {"name": name, "namespace": "default", "uid": "uid-1",
                     "creationTimestamp": "2026-01-01T00:00:00Z"},
        "spec": {"cronSpec": schedule}
    }))
    .unwrap()
}

fn default_key() -> WatchKey {
    WatchKey {
        cluster: "kind-a".to_string(),
        kind: ResourceKind::Custom {
            group: "stable.example.com".to_string(),
            version: "v1".to_string(),
            kind: "CronTab".to_string(),
        },
        namespace: Some("default".to_string()),
    }
}

fn snapshot<K>(sender: &mut Sender<K>, objects: Vec<K>) {
    sender.try_send(Ok(Event::Init)).unwrap();
    for object in objects {
        sender.try_send(Ok(Event::InitApply(object))).unwrap();
    }
    sender.try_send(Ok(Event::InitDone)).unwrap();
}

type Deleted = Rc<RefCell<Vec<(String, Option<String>, String)>>>;

struct Ui<'a> {
    cx: &'a mut VisualTestContext,
    namespaces: Sender<astrolabe_kube::k8s_openapi::api::core::v1::Namespace>,
    crds: Sender<CustomResourceDefinition>,
    deleted: Deleted,
}

fn open(
    cx: &mut TestAppContext,
    custom: Vec<(WatchKey, mpsc::Receiver<CustomWatchMessage>)>,
) -> Ui<'_> {
    cx.update(gpui_kit::init);
    let (namespaces, namespace_events) = mpsc::channel(32);
    let (crds, crd_events) = mpsc::channel(32);
    let deleted: Deleted = Rc::default();
    let mut cluster = ScriptedCluster::new(namespace_events)
        .crds(crd_events)
        .discover(|| {
            Ok(ServedResources::new([ServedResource {
                resource: crontab_resource(),
                namespaced: true,
            }]))
        })
        .delete({
            let deleted = deleted.clone();
            move |resource, namespace, name| {
                deleted.borrow_mut().push((
                    resource.kind.clone(),
                    namespace.map(str::to_string),
                    name.to_string(),
                ));
                Ok(())
            }
        });
    for (key, events) in custom {
        cluster = cluster.custom(key, events);
    }
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        WorkspaceView::from_clusters(vec![("kind-a".to_string(), cluster)], window, cx)
    });
    Ui {
        cx,
        namespaces,
        crds,
        deleted,
    }
}

impl Ui<'_> {
    fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.executor().advance_clock(Duration::from_millis(400));
        self.cx.run_until_parked();
    }

    fn frame<R>(&mut self, f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R) -> R {
        self.cx.update(|window, cx| {
            window.render_frame(cx);
            f(window, cx)
        })
    }
}

#[gpui_kit::test]
fn crd_kinds_navigate_to_live_lists_and_details_with_delete(cx: &mut TestAppContext) {
    let (mut instances, instance_events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(default_key(), instance_events)]);
    snapshot(&mut ui.namespaces, Vec::new());
    snapshot(&mut ui.crds, vec![crontab_crd(true)]);
    ui.settle();

    // Navigation: Custom Resources → Definitions → group → kind.
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("Custom Resources").is_some());
        assert!(window.try_find("nav-definitions").is_some());
        assert!(window.try_find_by_label("stable.example.com").is_some());
        assert!(
            window
                .try_find("nav-kind-stable.example.com-CronTab")
                .is_some()
        );
    });

    // Group headers collapse their kinds.
    ui.frame(|window, cx| window.click("nav-group-stable.example.com", cx));
    ui.frame(|window, cx| {
        assert!(
            window
                .try_find("nav-kind-stable.example.com-CronTab")
                .is_none()
        );
        window.click("nav-group-stable.example.com", cx);
    });

    ui.frame(|window, cx| window.click("nav-kind-stable.example.com-CronTab", cx));
    snapshot(&mut instances, vec![crontab("nightly", "*/5 * * * *")]);
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Schedule").is_some());
        assert!(window.try_find_by_label("*/5 * * * *").is_some());
        window.click("row-default/nightly", cx);
    });
    ui.settle();

    // Detail tab: metadata summary, YAML preview, live refresh.
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("UID: uid-1").is_some());
        assert!(window.try_find("detail-yaml").is_some());
        assert!(
            window
                .within("detail-metadata")
                .try_find_by_label("Namespace: default")
                .is_some()
        );
    });
    instances
        .try_send(Ok(Event::Apply(crontab("nightly", "0 0 * * *"))))
        .unwrap();
    ui.settle();
    ui.frame(|window, _| {
        assert!(!window.find_all_by_label("  cronSpec: 0 0 * * *").is_empty());
    });

    // Edit YAML opens the object's manifest in its own editor tab.
    ui.frame(|window, cx| window.click("detail-edit", cx));
    ui.settle();
    ui.frame(|window, cx| {
        assert!(
            window
                .try_find_by_label("Edit CronTab default/nightly")
                .is_some()
        );
        // Back to the detail tab: the list's row reopens (selects) it.
        window.click("nav-kind-stable.example.com-CronTab", cx);
    });
    ui.frame(|window, cx| window.click("row-default/nightly", cx));
    ui.settle();

    // Delete asks first, warns about cascading, then deletes.
    ui.frame(|window, cx| window.click("detail-delete", cx));
    ui.frame(|window, cx| {
        assert!(window.try_find("delete-confirm").is_some());
        assert!(
            window
                .try_find_by_label(
                    "Objects that list it in ownerReferences will be garbage-collected."
                )
                .is_some()
        );
        window.click("delete-confirm-no", cx);
    });
    assert!(ui.deleted.borrow().is_empty());
    ui.frame(|window, cx| window.click("detail-delete", cx));
    ui.frame(|window, cx| window.click("delete-confirm-yes", cx));
    ui.settle();
    assert_eq!(
        *ui.deleted.borrow(),
        [(
            "CronTab".to_string(),
            Some("default".to_string()),
            "nightly".to_string()
        )]
    );

    // The watch reports the deletion; the open detail says so.
    instances
        .try_send(Ok(Event::Delete(crontab("nightly", "0 0 * * *"))))
        .unwrap();
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find("detail-gone").is_some()));
}

#[gpui_kit::test]
fn definitions_list_filters_by_group_and_opens_crd_detail(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    snapshot(&mut ui.namespaces, Vec::new());
    snapshot(&mut ui.crds, vec![crontab_crd(true)]);
    ui.settle();

    ui.frame(|window, cx| window.click("nav-definitions", cx));
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("ShortNames").is_some());
        assert!(window.try_find("row-crontabs.stable.example.com").is_some());
        window.click("group-filter-stable.example.com", cx);
    });
    ui.frame(|window, cx| {
        assert!(window.try_find("row-crontabs.stable.example.com").is_some());
        window.click("row-crontabs.stable.example.com", cx);
    });
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Versions: v1 (served, storage)")
                .is_some()
        );
        assert!(window.try_find("detail-yaml").is_some());
    });
}

#[gpui_kit::test]
fn unserved_preferred_version_shows_an_error_not_an_empty_table(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    snapshot(&mut ui.namespaces, Vec::new());
    snapshot(&mut ui.crds, vec![crontab_crd(false)]);
    ui.settle();

    ui.frame(|window, cx| window.click("nav-kind-stable.example.com-CronTab", cx));
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("CronTab v1 is not served by the API server")
                .is_some()
        );
    });
}

#[gpui_kit::test]
fn rbac_denied_crd_list_shows_an_empty_state_hint(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    snapshot(&mut ui.namespaces, Vec::new());
    ui.crds
        .try_send(Err(WatchError::forbidden("forbidden")))
        .unwrap();
    ui.settle();

    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("No permission to list CustomResourceDefinitions")
                .is_some()
        );
        assert!(window.try_find("namespace-stale").is_none());
    });
}

#[gpui_kit::test]
fn namespace_selection_switches_namespaced_lists(cx: &mut TestAppContext) {
    let (mut default_ns, default_events) = mpsc::channel(32);
    let (mut team_a, team_a_events) = mpsc::channel(32);
    let mut team_key = default_key();
    team_key.namespace = Some("team-a".to_string());
    let mut ui = open(
        cx,
        vec![(default_key(), default_events), (team_key, team_a_events)],
    );
    let namespace = |name: &str| {
        serde_json::from_value(serde_json::json!({"metadata": {"name": name}})).unwrap()
    };
    snapshot(
        &mut ui.namespaces,
        vec![namespace("default"), namespace("team-a")],
    );
    snapshot(&mut ui.crds, vec![crontab_crd(true)]);
    ui.settle();
    ui.frame(|window, cx| window.click("nav-kind-stable.example.com-CronTab", cx));
    snapshot(&mut default_ns, vec![crontab("nightly", "*/5 * * * *")]);
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find("row-default/nightly").is_some());
        window.click("namespace-select", cx);
    });
    ui.frame(|window, cx| window.click("namespace-option-team-a", cx));
    let mut hourly = crontab("hourly", "0 * * * *");
    hourly.metadata.namespace = Some("team-a".to_string());
    snapshot(&mut team_a, vec![hourly]);
    ui.settle();
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("Namespace: team-a").is_some());
        assert!(window.try_find("row-team-a/hourly").is_some());
        assert!(window.try_find("row-default/nightly").is_none());
    });
}

/// Synthetic Namespace (plausible shape, not captured from a server).
fn namespace_in_phase(
    name: &str,
    phase: &str,
) -> astrolabe_kube::k8s_openapi::api::core::v1::Namespace {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": name, "uid": "uid-ns"},
        "spec": {"finalizers": ["kubernetes"]},
        "status": {"phase": phase}
    }))
    .unwrap()
}

#[gpui_kit::test]
fn namespace_rows_open_a_live_detail_tab_with_delete(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    snapshot(
        &mut ui.namespaces,
        vec![namespace_in_phase("old", "Active")],
    );
    snapshot(&mut ui.crds, Vec::new());
    ui.settle();

    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .try_find("detail-tab")
                .is_some_and(|tab| tab.label() == Some("Namespace old"))
        );
        let status = window.within("detail-status");
        assert!(status.try_find_by_label("Phase: Active").is_some());
        assert!(status.try_find_by_label("Finalizers: kubernetes").is_some());
    });

    // The tab follows the session's namespace watch; no GET of its own.
    ui.namespaces
        .try_send(Ok(Event::Apply(namespace_in_phase("old", "Terminating"))))
        .unwrap();
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .within("detail-status")
                .try_find_by_label("Phase: Terminating")
                .is_some()
        );
    });

    // Edit YAML opens the manifest in its own tab; the row reselects the detail.
    ui.frame(|window, cx| window.click("detail-edit", cx));
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Edit Namespace old").is_some());
        window.click("nav-namespaces", cx);
    });
    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();

    ui.frame(|window, cx| window.click("detail-delete", cx));
    ui.frame(|window, cx| {
        assert!(
            window
                .try_find_by_label("Deleting a Namespace deletes every object in it.")
                .is_some()
        );
        window.click("delete-confirm-yes", cx);
    });
    ui.settle();
    assert_eq!(
        *ui.deleted.borrow(),
        [("Namespace".to_string(), None, "old".to_string())]
    );

    ui.namespaces
        .try_send(Ok(Event::Delete(namespace_in_phase("old", "Terminating"))))
        .unwrap();
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find("detail-gone").is_some()));
}
