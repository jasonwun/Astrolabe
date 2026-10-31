//! P3 built-in views through the workspace UI, on a scripted cluster.

mod support;

use astrolabe::builtin::BuiltinKind;
use astrolabe::session::Operation;
use astrolabe_kube::kube::core::DynamicObject;
use futures::channel::mpsc;
use gpui_kit::TestAppContext;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use support::{key, open, open_detail, snapshot};

/// Synthetic objects (plausible shapes, not captured from a server).
fn pod() -> DynamicObject {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": "default", "uid": "pod-1"},
        "spec": {"containers": [{"name": "web", "image": "nginx"}]}
    }))
    .unwrap()
}

fn event(name: &str, kind: &str, involved: &str) -> DynamicObject {
    let api_version = match kind {
        "Pod" | "Namespace" | "Node" => "v1",
        "Job" | "CronJob" => "batch/v1",
        _ => "apps/v1",
    };
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Event",
        "metadata": {"name": name, "namespace": "default"},
        "involvedObject": {"apiVersion": api_version, "kind": kind, "name": involved, "namespace": "default"},
        "type": "Normal", "reason": "Started", "message": format!("Started {involved}")
    }))
    .unwrap()
}

#[gpui_kit::test]
fn global_event_rows_open_the_involved_object(cx: &mut TestAppContext) {
    let (mut events, event_events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::Events, "default"), event_events),
            (key(BuiltinKind::Pods, "default"), pod_events),
        ],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::Events.nav_id(), cx));
    snapshot(&mut events, vec![event("web.started", "Pod", "web-1")]);
    snapshot(&mut pods, vec![pod()]);
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Involved Object").is_some());
        window.click("event-row-default/web.started-3", cx);
    });
    ui.settle();
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("Pod default/web-1").is_some());
        assert!(
            window
                .try_find_by_label("Event default/web.started")
                .is_none()
        );
    });
}

#[gpui_kit::test]
fn events_open_cluster_scoped_namespaces_without_a_namespace_key(cx: &mut TestAppContext) {
    let (mut events, event_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Events, "default"), event_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::Events.nav_id(), cx));
    snapshot(
        &mut events,
        vec![event("namespace.active", "Namespace", "default")],
    );
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/namespace.active", cx));
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find_by_label("Namespace default").is_some()));
}

#[gpui_kit::test]
fn an_unopenable_involved_object_reports_why(cx: &mut TestAppContext) {
    let (mut events, event_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Events, "default"), event_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::Events.nav_id(), cx));
    snapshot(&mut events, vec![event("node.started", "Node", "node-a")]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/node.started", cx));
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label(
                    "Cannot open involved object Node/node-a: kind is not registered"
                )
                .is_some()
        );
        assert!(
            window
                .try_find_by_label("Event default/node.started")
                .is_none()
        );
    });
}

#[gpui_kit::test]
fn a_custom_kind_with_a_builtin_name_is_not_opened_as_builtin(cx: &mut TestAppContext) {
    let (mut events, event_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Events, "default"), event_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::Events.nav_id(), cx));
    let mut custom = event("custom.job", "Job", "custom");
    custom.data["involvedObject"]["apiVersion"] = serde_json::json!("example.com/v1");
    snapshot(&mut events, vec![custom]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/custom.job", cx));
    ui.settle();
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Cannot open involved object Job/custom: kind is not registered")
                .is_some()
        );
        assert!(window.try_find_by_label("Job default/custom").is_none());
    });
}

fn workload(kind: &str, name: &str) -> DynamicObject {
    let api_version = match kind {
        "Pod" => "v1",
        "Job" | "CronJob" => "batch/v1",
        _ => "apps/v1",
    };
    let spec = match kind {
        "Pod" => serde_json::json!({"containers": [{"name": "run", "image": "busybox"}]}),
        "StatefulSet" => {
            serde_json::json!({"serviceName": "db", "replicas": 2, "selector": {"matchLabels": {"app": "db"}}, "template": {"spec": {"containers": [{"name": "db", "image": "postgres"}]}}})
        }
        "ReplicaSet" => {
            serde_json::json!({"replicas": 2, "selector": {"matchLabels": {"app": "web"}}, "template": {"spec": {"containers": [{"name": "web", "image": "nginx"}]}}})
        }
        "DaemonSet" => {
            serde_json::json!({"selector": {"matchLabels": {"app": "agent"}}, "template": {"spec": {"containers": [{"name": "agent", "image": "agent"}]}}})
        }
        "Job" => {
            serde_json::json!({"template": {"spec": {"containers": [{"name": "run", "image": "busybox"}], "restartPolicy": "Never"}}})
        }
        "CronJob" => {
            serde_json::json!({"schedule": "0 0 * * *", "jobTemplate": {"spec": {"template": {"spec": {"containers": [{"name": "run", "image": "busybox"}], "restartPolicy": "Never"}}}}})
        }
        _ => unreachable!(),
    };
    let status = match kind {
        "StatefulSet" => {
            serde_json::json!({"replicas": 2, "readyReplicas": 1, "availableReplicas": 1})
        }
        "ReplicaSet" => {
            serde_json::json!({"replicas": 2, "readyReplicas": 1, "availableReplicas": 1})
        }
        "DaemonSet" => {
            serde_json::json!({"desiredNumberScheduled": 2, "currentNumberScheduled": 2, "numberReady": 1, "numberMisscheduled": 0})
        }
        _ => serde_json::json!({}),
    };
    serde_json::from_value(serde_json::json!({
        "apiVersion": api_version, "kind": kind,
        "metadata": {"name": name, "namespace": "default", "uid": format!("{name}-uid")},
        "spec": spec, "status": status
    }))
    .unwrap()
}

fn owned(kind: &str, name: &str, owner_kind: &str, owner: &str, uid: &str) -> DynamicObject {
    let mut object = workload(kind, name);
    object.metadata.owner_references = serde_json::from_value(serde_json::json!([{
        "apiVersion": if owner_kind == "CronJob" { "batch/v1" } else { "apps/v1" },
        "kind": owner_kind, "name": owner, "uid": uid, "controller": true
    }]))
    .unwrap();
    object
}

#[gpui_kit::test]
fn statefulsets_scale_restart_and_list_owned_pods(cx: &mut TestAppContext) {
    let (mut statefulsets, events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::StatefulSets, "default"), events),
            (key(BuiltinKind::Pods, "default"), pod_events),
        ],
    );
    open_detail(
        &mut ui,
        BuiltinKind::StatefulSets,
        &mut statefulsets,
        vec![workload("StatefulSet", "db")],
        "db",
    );
    snapshot(
        &mut pods,
        vec![
            owned("Pod", "db-0", "StatefulSet", "db", "db-uid"),
            owned("Pod", "other-0", "StatefulSet", "other", "other-uid"),
        ],
    );
    ui.settle();
    ui.frame(|window, cx| {
        let related = window.within("detail-related");
        assert!(related.try_find("related-row-default/db-0").is_some());
        assert!(related.try_find("related-row-default/other-0").is_none());
        window.click("detail-scale", cx);
    });
    ui.frame(|window, cx| {
        window.click("scale-input", cx);
        window.input("5", cx);
        window.click("scale-apply", cx);
    });
    ui.settle();
    ui.frame(|window, cx| window.click("detail-restart", cx));
    ui.settle();
    let operations = ui.operated.borrow();
    assert!(
        matches!(&operations[0], Operation::Scale { object, replicas: 5 } if object.resource.kind == "StatefulSet" && object.key.name == "db")
    );
    assert!(
        matches!(&operations[1], Operation::RolloutRestart { object } if object.resource.kind == "StatefulSet")
    );
}

#[gpui_kit::test]
fn replicasets_scale_without_a_restart_action(cx: &mut TestAppContext) {
    let (mut replicasets, events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::ReplicaSets, "default"), events),
            (key(BuiltinKind::Pods, "default"), pod_events),
        ],
    );
    open_detail(
        &mut ui,
        BuiltinKind::ReplicaSets,
        &mut replicasets,
        vec![workload("ReplicaSet", "web-abc")],
        "web-abc",
    );
    snapshot(
        &mut pods,
        vec![owned(
            "Pod",
            "web-1",
            "ReplicaSet",
            "web-abc",
            "web-abc-uid",
        )],
    );
    ui.settle();
    ui.frame(|window, _| {
        assert!(window.try_find("detail-scale").is_some());
        assert!(window.try_find("detail-restart").is_none());
        assert!(window.try_find("detail-edit").is_some());
        assert!(window.try_find("detail-delete").is_some());
        assert!(
            window
                .within("detail-related")
                .try_find("related-row-default/web-1")
                .is_some()
        );
    });
}

#[gpui_kit::test]
fn daemonsets_restart_without_a_scale_action(cx: &mut TestAppContext) {
    let (mut daemonsets, events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::DaemonSets, "default"), events),
            (key(BuiltinKind::Pods, "default"), pod_events),
        ],
    );
    open_detail(
        &mut ui,
        BuiltinKind::DaemonSets,
        &mut daemonsets,
        vec![workload("DaemonSet", "agent")],
        "agent",
    );
    snapshot(
        &mut pods,
        vec![owned("Pod", "agent-1", "DaemonSet", "agent", "agent-uid")],
    );
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find("detail-scale").is_none());
        assert!(
            window
                .within("detail-related")
                .try_find("related-row-default/agent-1")
                .is_some()
        );
        window.click("detail-restart", cx);
    });
    ui.settle();
    assert!(
        matches!(&ui.operated.borrow()[0], Operation::RolloutRestart { object } if object.resource.kind == "DaemonSet")
    );
}

#[gpui_kit::test]
fn jobs_list_owned_pods_and_warn_about_cascade(cx: &mut TestAppContext) {
    let (mut jobs, events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::Jobs, "default"), events),
            (key(BuiltinKind::Pods, "default"), pod_events),
        ],
    );
    open_detail(
        &mut ui,
        BuiltinKind::Jobs,
        &mut jobs,
        vec![workload("Job", "migrate")],
        "migrate",
    );
    snapshot(
        &mut pods,
        vec![owned("Pod", "migrate-1", "Job", "migrate", "migrate-uid")],
    );
    ui.settle();
    ui.frame(|window, cx| {
        assert!(window.try_find("detail-scale").is_none());
        assert!(window.try_find("detail-restart").is_none());
        assert!(
            window
                .within("detail-related")
                .try_find("related-row-default/migrate-1")
                .is_some()
        );
        window.click("detail-delete", cx);
    });
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Its Pods will be deleted too.")
                .is_some()
        )
    });
}

#[gpui_kit::test]
fn cronjobs_suspend_trigger_and_list_owned_jobs(cx: &mut TestAppContext) {
    let (mut cronjobs, events) = mpsc::channel(32);
    let (mut jobs, job_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::CronJobs, "default"), events),
            (key(BuiltinKind::Jobs, "default"), job_events),
        ],
    );
    open_detail(
        &mut ui,
        BuiltinKind::CronJobs,
        &mut cronjobs,
        vec![workload("CronJob", "nightly")],
        "nightly",
    );
    snapshot(
        &mut jobs,
        vec![
            owned(
                "Job",
                "nightly-manual-1",
                "CronJob",
                "nightly",
                "nightly-uid",
            ),
            owned("Job", "other-1", "CronJob", "other", "other-uid"),
        ],
    );
    ui.settle();
    ui.frame(|window, cx| {
        let related = window.within("detail-related");
        assert!(
            related
                .try_find("related-row-default/nightly-manual-1")
                .is_some()
        );
        assert!(related.try_find("related-row-default/other-1").is_none());
        assert!(window.try_find("detail-scale").is_none());
        assert!(window.try_find("detail-restart").is_none());
        assert!(window.try_find("detail-edit").is_none());
        window.click("detail-suspend", cx);
    });
    ui.settle();
    ui.frame(|window, cx| window.click("detail-trigger", cx));
    ui.settle();
    let operations = ui.operated.borrow();
    assert!(
        matches!(&operations[0], Operation::SetSuspend { object, suspend: true } if object.resource.kind == "CronJob")
    );
    assert!(
        matches!(&operations[1], Operation::TriggerCronJob { object } if object.resource.kind == "CronJob")
    );
}
