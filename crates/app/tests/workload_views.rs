//! Built-in kind views (#17) through the workspace UI, on a scripted cluster.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use astrolabe::builtin::BuiltinKind;
use astrolabe::session::{
    ObjectRef, ObjectWatchMessage, Operation, ResourceKind, ScriptedCluster, WatchError, WatchKey,
};
use astrolabe::store::ObjectKey;
use astrolabe::workspace::WorkspaceView;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{TestAppContext, VisualTestContext};

type Sender<K> = mpsc::Sender<Result<Event<K>, WatchError>>;

/// Synthetic Pod (plausible shape, not captured from a server).
fn crashing_pod(namespace: &str) -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": namespace, "uid": "uid-pod",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {"nodeName": "node-a", "containers": [{"name": "app", "image": "nginx"}]},
        "status": {
            "phase": "Running", "podIP": "10.0.0.7",
            "containerStatuses": [{
                "name": "app", "image": "nginx", "imageID": "", "ready": false, "restartCount": 3,
                "state": {"waiting": {"reason": "CrashLoopBackOff"}}
            }]
        }
    }))
    .unwrap()
}

/// Synthetic Deployment (plausible shape, not captured from a server).
fn web_deployment() -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-deploy",
                     "creationTimestamp": "2026-10-20T12:00:00Z"},
        "spec": {
            "replicas": 3,
            "selector": {"matchLabels": {"app": "web"}},
            "strategy": {"type": "Recreate"},
            "template": {"metadata": {"labels": {"app": "web"}}}
        },
        "status": {"updatedReplicas": 2, "readyReplicas": 2, "availableReplicas": 2,
                   "unavailableReplicas": 1}
    }))
    .unwrap()
}

fn key(kind: BuiltinKind, namespace: &str) -> WatchKey {
    WatchKey {
        cluster: "kind-a".to_string(),
        kind: ResourceKind::Builtin(kind),
        namespace: Some(namespace.to_string()),
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
type Operated = Rc<RefCell<Vec<Operation>>>;

struct Ui<'a> {
    cx: &'a mut VisualTestContext,
    namespaces: Sender<Namespace>,
    deleted: Deleted,
    operated: Operated,
}

fn open(
    cx: &mut TestAppContext,
    watches: Vec<(WatchKey, mpsc::Receiver<ObjectWatchMessage>)>,
) -> Ui<'_> {
    cx.update(gpui_kit::init);
    let (namespaces, namespace_events) = mpsc::channel(32);
    let deleted: Deleted = Rc::default();
    let operated: Operated = Rc::default();
    let mut cluster = ScriptedCluster::new(namespace_events)
        .delete({
            let deleted = deleted.clone();
            move |object: &ObjectRef| {
                let resource = &object.resource;
                deleted.borrow_mut().push((
                    format!("{}/{}", resource.api_version, resource.kind),
                    object.key.namespace.clone(),
                    object.key.name.clone(),
                ));
                Ok(())
            }
        })
        .operate({
            let operated = operated.clone();
            move |operation: &Operation| {
                operated.borrow_mut().push(operation.clone());
                match operation {
                    Operation::Scale { replicas: 0, .. } => Err("scale refused".to_string()),
                    _ => Ok(()),
                }
            }
        });
    for (key, events) in watches {
        cluster = cluster.watch(key, events);
    }
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        WorkspaceView::from_clusters(vec![("kind-a".to_string(), cluster)], window, cx)
    });
    Ui {
        cx,
        namespaces,
        deleted,
        operated,
    }
}

impl Ui<'_> {
    fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.executor().advance_clock(Duration::from_millis(400));
        self.cx.run_until_parked();
    }

    fn namespaces(&mut self, names: &[&str]) {
        let namespaces = names
            .iter()
            .map(|name| {
                serde_json::from_value(serde_json::json!({"metadata": {"name": name}})).unwrap()
            })
            .collect();
        snapshot(&mut self.namespaces, namespaces);
    }

    fn frame<R>(&mut self, f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R) -> R {
        self.cx.update(|window, cx| {
            window.render_frame(cx);
            f(window, cx)
        })
    }
}

#[gpui_kit::test]
fn pods_list_and_detail_use_the_generic_pipeline(cx: &mut TestAppContext) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(key(BuiltinKind::Pods, "default"), pod_events)]);
    ui.namespaces(&["default"]);
    ui.settle();

    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Workloads").is_some());
        assert!(window.try_find("nav-deployments").is_some());
        window.click("nav-pods", cx);
    });
    snapshot(&mut pods, vec![crashing_pod("default")]);
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Restarts").is_some());
        assert!(window.try_find_by_label("CrashLoopBackOff").is_some());
        assert!(window.try_find_by_label("node-a").is_some());
        window.click("row-default/web-1", cx);
    });
    ui.settle();

    // The detail tab: the generic three sections plus the container table.
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("Pod default/web-1").is_some());
        assert!(
            window
                .within("detail-metadata")
                .try_find_by_label("UID: uid-pod")
                .is_some()
        );
        let status = window.within("detail-status");
        assert!(
            status
                .try_find_by_label("Status: CrashLoopBackOff")
                .is_some()
        );
        let containers = window.within("detail-table-Containers");
        assert!(containers.try_find_by_label("Last state").is_some());
        assert!(
            containers
                .try_find_by_label("Waiting: CrashLoopBackOff")
                .is_some()
        );
        assert!(window.try_find("detail-yaml").is_some());
    });

    // Delete names what replaces the Pod, then deletes it.
    ui.frame(|window, cx| window.click("detail-delete", cx));
    ui.frame(|window, cx| {
        assert!(
            window
                .try_find_by_label("A controller that owns this Pod will create a replacement.")
                .is_some()
        );
        window.click("delete-confirm-yes", cx);
    });
    ui.settle();
    assert_eq!(
        *ui.deleted.borrow(),
        [(
            "v1/Pod".to_string(),
            Some("default".to_string()),
            "web-1".to_string()
        )]
    );
}

#[gpui_kit::test]
fn deployments_show_a_replica_bar(cx: &mut TestAppContext) {
    let (mut deployments, deployment_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Deployments, "default"), deployment_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();

    ui.frame(|window, cx| window.click("nav-deployments", cx));
    snapshot(&mut deployments, vec![web_deployment()]);
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Up-to-date").is_some());
        assert!(window.try_find_by_label("2/3").is_some());
        window.click("row-default/web", cx);
    });
    ui.settle();

    ui.frame(|window, cx| {
        let status = window.within("detail-status");
        assert!(
            status
                .try_find_by_label("Replicas: 3 desired, 2 updated, 2 available, 1 unavailable")
                .is_some()
        );
        assert!(status.try_find_by_label("Strategy: Recreate").is_some());
        window.click("detail-delete", cx);
    });
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Its ReplicaSets and their Pods will be deleted too.")
                .is_some()
        );
    });
}

#[gpui_kit::test]
fn the_pods_list_follows_the_namespace_selection(cx: &mut TestAppContext) {
    let (mut default_pods, default_events) = mpsc::channel(32);
    let (mut web_pods, web_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::Pods, "default"), default_events),
            (key(BuiltinKind::Pods, "web"), web_events),
        ],
    );
    ui.namespaces(&["default", "web"]);
    ui.settle();

    ui.frame(|window, cx| window.click("nav-pods", cx));
    snapshot(&mut default_pods, Vec::new());
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find("row-web/web-1").is_none());
        window.click("namespace-select", cx);
    });
    ui.frame(|window, cx| window.click("namespace-option-web", cx));
    snapshot(&mut web_pods, vec![crashing_pod("web")]);
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find("row-web/web-1").is_some()));
}

#[gpui_kit::test]
fn deployments_scale_and_rollout_restart_from_the_toolbar(cx: &mut TestAppContext) {
    let (mut deployments, deployment_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Deployments, "default"), deployment_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click("nav-deployments", cx));
    snapshot(&mut deployments, vec![web_deployment()]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/web", cx));
    ui.settle();
    let web = ObjectKey::new(Some("default".into()), "web");

    // Scale: a numeric field with the current desired count as its hint.
    ui.frame(|window, cx| window.click("detail-scale", cx));
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Replicas (now 3)").is_some());
        window.click("scale-input", cx);
        window.input("5", cx);
        window.click("scale-apply", cx);
    });
    ui.settle();
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("Scaled to 5").is_some());
        assert!(window.try_find("scale-input").is_none());
    });

    // Not a number: nothing is sent.
    ui.frame(|window, cx| window.click("detail-scale", cx));
    ui.frame(|window, cx| {
        window.click("scale-input", cx);
        window.input("-x", cx);
        window.click("scale-apply", cx);
    });
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Replicas must be a whole number, 0 or more")
                .is_some()
        );
    });

    // A refusal is reported.
    ui.frame(|window, cx| {
        window.click("scale-input", cx);
        for _ in 0..2 {
            window.press("backspace", cx);
        }
        window.input("0", cx);
        window.press("enter", cx);
    });
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Scale failed: scale refused")
                .is_some()
        );
    });

    ui.frame(|window, cx| window.click("detail-restart", cx));
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find_by_label("Restart requested").is_some()));

    assert_eq!(
        *ui.operated.borrow(),
        [
            Operation::Scale {
                key: web.clone(),
                replicas: 5
            },
            Operation::Scale {
                key: web.clone(),
                replicas: 0
            },
            Operation::RolloutRestart { key: web },
        ]
    );
}

#[gpui_kit::test]
fn pods_have_no_deployment_actions(cx: &mut TestAppContext) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(key(BuiltinKind::Pods, "default"), pod_events)]);
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click("nav-pods", cx));
    snapshot(&mut pods, vec![crashing_pod("default")]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/web-1", cx));
    ui.settle();

    ui.frame(|window, _| {
        assert!(window.try_find("detail-edit").is_some());
        assert!(window.try_find("detail-scale").is_none());
        assert!(window.try_find("detail-restart").is_none());
    });
}
