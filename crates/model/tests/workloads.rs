//! Pods and Deployments on the generic list/detail pipeline (#17).

#![expect(
    clippy::assert_is_empty,
    reason = "moved from the app with its assertions unchanged (#153)"
)]

use astrolabe_kube::k8s_openapi::api::core::v1::Pod;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::object::ObjectKey;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::workloads::{
    ForwardPort, pod_ports, running_containers, selector_matches,
};
use astrolabe_model::projection::{Containers, Forward, ForwardTo, ReplicaCounts, SummaryTable};
use std::collections::BTreeMap;
use support::{detail, list};

mod support;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

/// Synthetic Pod (plausible shape, not captured from a server): one init
/// container done, one app container crash-looping after 3 restarts.
fn crashing_pod() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": "default", "uid": "uid-pod",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {
            "nodeName": "node-a",
            "initContainers": [{"name": "migrate", "image": "busybox"}],
            "containers": [
                {"name": "app", "image": "nginx", "ports": [{"containerPort": 80, "name": "http"}]},
                {"name": "sidecar", "image": "envoy"}
            ]
        },
        "status": {
            "phase": "Running", "podIP": "10.0.0.7", "qosClass": "BestEffort",
            "conditions": [{"type": "Ready", "status": "False", "reason": "ContainersNotReady"}],
            "initContainerStatuses": [{
                "name": "migrate", "image": "busybox", "imageID": "", "ready": true, "restartCount": 0,
                "state": {"terminated": {"exitCode": 0, "reason": "Completed"}}
            }],
            "containerStatuses": [
                {
                    "name": "app", "image": "nginx", "imageID": "", "ready": false, "restartCount": 3,
                    "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                    "lastState": {"terminated": {"exitCode": 1, "reason": "Error",
                                                 "finishedAt": "2026-10-22T11:55:00Z"}}
                },
                {
                    "name": "sidecar", "image": "envoy", "imageID": "", "ready": true, "restartCount": 0,
                    "state": {"running": {"startedAt": "2026-10-22T11:01:00Z"}}
                }
            ]
        }
    })
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "callers pass a json! literal"
)]
fn pod(spec: serde_json::Value, status: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "metadata": {"name": "p", "namespace": "default", "creationTimestamp": "2026-10-22T11:59:00Z"},
        "spec": spec,
        "status": status
    })
}

fn typed_pod(spec: serde_json::Value, status: serde_json::Value) -> Pod {
    serde_json::from_value(pod(spec, status)).unwrap()
}

fn status_of(pod: serde_json::Value) -> String {
    list(BuiltinKind::Pods, vec![pod], now()).rows[0].cells[2].clone()
}

#[test]
fn pods_list_kubectl_columns() {
    let pod = crashing_pod();

    let table = list(BuiltinKind::Pods, vec![pod], now());

    assert_eq!(
        table.headers(),
        ["Name", "Ready", "Status", "Restarts", "IP", "Node", "Age"]
    );
    assert_eq!(
        table.rows[0].key,
        ObjectKey::new(Some("default".into()), "web-1")
    );
    assert_eq!(
        table.rows[0].cells,
        [
            "web-1",
            "1/2",
            "CrashLoopBackOff",
            "3 (5m ago)",
            "10.0.0.7",
            "node-a",
            "60m"
        ]
    );
}

#[test]
fn unscheduled_pods_show_none_for_ip_and_node() {
    let pending = pod(
        serde_json::json!({"containers": [{"name": "app"}]}),
        serde_json::json!({"phase": "Pending"}),
    );

    let cells = &list(BuiltinKind::Pods, vec![pending], now()).rows[0].cells;

    assert_eq!(cells[1..6], ["0/1", "Pending", "0", "<none>", "<none>"]);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one case per kubectl printPod branch"
)]
fn status_column_follows_kubectl_print_pod() {
    let containers = serde_json::json!({"initContainers": [{"name": "a"}, {"name": "b"}],
                                        "containers": [{"name": "app"}]});
    let init_status = |a: serde_json::Value, b: serde_json::Value| {
        serde_json::json!({"phase": "Pending", "initContainerStatuses": [
            {"name": "a", "image": "", "imageID": "", "ready": false, "restartCount": 0, "state": a},
            {"name": "b", "image": "", "imageID": "", "ready": false, "restartCount": 0, "state": b}
        ]})
    };
    let done = serde_json::json!({"terminated": {"exitCode": 0}});
    let running = serde_json::json!({"running": {}});
    let initializing = serde_json::json!({"waiting": {"reason": "PodInitializing"}});

    // Init:i/n counts finished init containers; the running one stops the scan.
    assert_eq!(
        status_of(pod(containers.clone(), init_status(done.clone(), running))),
        "Init:1/2"
    );
    assert_eq!(
        status_of(pod(
            containers.clone(),
            init_status(done.clone(), initializing)
        )),
        "Init:1/2"
    );
    assert_eq!(
        status_of(pod(
            containers.clone(),
            init_status(
                serde_json::json!({"waiting": {"reason": "ImagePullBackOff"}}),
                done.clone()
            )
        )),
        "Init:ImagePullBackOff"
    );
    assert_eq!(
        status_of(pod(
            containers.clone(),
            init_status(
                serde_json::json!({"terminated": {"exitCode": 2}}),
                done.clone()
            )
        )),
        "Init:ExitCode:2"
    );
    assert_eq!(
        status_of(pod(
            containers.clone(),
            init_status(
                serde_json::json!({"terminated": {"exitCode": 1, "reason": "OOMKilled"}}),
                done.clone()
            )
        )),
        "Init:OOMKilled"
    );
    assert_eq!(
        status_of(pod(
            containers.clone(),
            init_status(
                serde_json::json!({"terminated": {"exitCode": 137, "signal": 9}}),
                done.clone()
            )
        )),
        "Init:Signal:9"
    );

    let app = |state: serde_json::Value, ready: bool| {
        serde_json::json!({"phase": "Running", "containerStatuses": [
            {"name": "app", "image": "", "imageID": "", "ready": ready, "restartCount": 0, "state": state}
        ]})
    };
    let single = serde_json::json!({"containers": [{"name": "app"}]});
    assert_eq!(
        status_of(pod(
            single.clone(),
            app(serde_json::json!({"running": {}}), true)
        )),
        "Running"
    );
    assert_eq!(
        status_of(pod(
            single.clone(),
            app(
                serde_json::json!({"terminated": {"exitCode": 0, "reason": "Completed"}}),
                false
            )
        )),
        "Completed"
    );
    assert_eq!(
        status_of(pod(
            single.clone(),
            app(serde_json::json!({"terminated": {"exitCode": 3}}), false)
        )),
        "ExitCode:3"
    );
    assert_eq!(
        status_of(pod(
            single.clone(),
            app(
                serde_json::json!({"terminated": {"exitCode": 143, "signal": 15}}),
                false
            )
        )),
        "Signal:15"
    );

    // A pod-level reason (eviction) wins over the phase.
    assert_eq!(
        status_of(pod(
            single.clone(),
            serde_json::json!({"phase": "Failed", "reason": "Evicted"})
        )),
        "Evicted"
    );

    let mut terminating = pod(
        single.clone(),
        app(serde_json::json!({"running": {}}), true),
    );
    terminating["metadata"]["deletionTimestamp"] = now().to_string().into();
    assert_eq!(status_of(terminating), "Terminating");
}

#[test]
fn completed_sidecar_with_running_app_reads_running_or_not_ready() {
    let spec = serde_json::json!({"containers": [{"name": "app"}, {"name": "job"}]});
    let status = |ready: &str| {
        serde_json::json!({"phase": "Running",
        "conditions": [{"type": "Ready", "status": ready}],
        "containerStatuses": [
            {"name": "app", "image": "", "imageID": "", "ready": true, "restartCount": 0,
             "state": {"running": {}}},
            {"name": "job", "image": "", "imageID": "", "ready": false, "restartCount": 0,
             "state": {"terminated": {"exitCode": 0, "reason": "Completed"}}}
        ]})
    };
    assert_eq!(status_of(pod(spec.clone(), status("True"))), "Running");
    assert_eq!(status_of(pod(spec, status("False"))), "NotReady");
}

#[test]
fn pod_detail_summarizes_containers_including_init_and_last_state() {
    let pod = crashing_pod();

    let detail = detail(BuiltinKind::Pods, pod, now());

    assert_eq!(detail.title, "Pod default/web-1");
    assert_eq!(
        detail.status,
        [
            ("Status", "CrashLoopBackOff"),
            ("Node", "node-a"),
            ("Pod IP", "10.0.0.7"),
            ("QoS class", "BestEffort"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    assert_eq!(
        detail.tables,
        [SummaryTable {
            title: "Containers".to_string(),
            columns: ["Name", "Image", "Ready", "Restarts", "State", "Last state"]
                .map(String::from)
                .to_vec(),
            rows: vec![
                [
                    "migrate (init)",
                    "busybox",
                    "true",
                    "0",
                    "Terminated: Completed (exit 0)",
                    ""
                ]
                .map(String::from)
                .to_vec(),
                [
                    "app",
                    "nginx",
                    "false",
                    "3",
                    "Waiting: CrashLoopBackOff",
                    "Terminated: Error (exit 1)"
                ]
                .map(String::from)
                .to_vec(),
                ["sidecar", "envoy", "true", "0", "Running since 59m", ""]
                    .map(String::from)
                    .to_vec(),
            ],
        }]
    );
    assert_eq!(detail.conditions.len(), 1);
    assert_eq!(detail.conditions[0].kind, "Ready");
    assert_eq!(detail.conditions[0].reason, "ContainersNotReady");
}

#[test]
fn containers_without_a_status_yet_still_list_from_the_spec() {
    let pending = pod(
        serde_json::json!({"containers": [{"name": "app", "image": "nginx"}]}),
        serde_json::json!({"phase": "Pending"}),
    );

    let detail = detail(BuiltinKind::Pods, pending, now());

    assert_eq!(
        detail.tables[0].rows,
        [["app", "nginx", "false", "0", "", ""]
            .map(String::from)
            .to_vec()]
    );
}

/// Synthetic Deployment (plausible shape, not captured from a server): a
/// rollout half done — 3 desired, 2 on the new template, 1 unavailable.
fn rolling_deployment() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-deploy",
                     "creationTimestamp": "2026-10-20T12:00:00Z"},
        "spec": {
            "replicas": 3,
            "selector": {"matchLabels": {"app": "web"}},
            "strategy": {"type": "RollingUpdate",
                         "rollingUpdate": {"maxSurge": "25%", "maxUnavailable": 1}},
            "template": {"metadata": {"labels": {"app": "web"}},
                         "spec": {"containers": [{"name": "app", "image": "nginx"}]}}
        },
        "status": {
            "replicas": 4, "updatedReplicas": 2, "readyReplicas": 2,
            "availableReplicas": 2, "unavailableReplicas": 1,
            "conditions": [
                {"type": "Available", "status": "True", "reason": "MinimumReplicasAvailable"},
                {"type": "Progressing", "status": "True", "reason": "ReplicaSetUpdated"}
            ]
        }
    })
}

#[test]
fn deployments_list_kubectl_columns() {
    let deployment = rolling_deployment();

    let table = list(BuiltinKind::Deployments, vec![deployment], now());

    assert_eq!(
        table.headers(),
        ["Name", "Ready", "Up-to-date", "Available", "Age"]
    );
    assert_eq!(
        table.rows[0].key,
        ObjectKey::new(Some("default".into()), "web")
    );
    assert_eq!(table.rows[0].cells, ["web", "2/3", "2", "2", "2d"]);
}

#[test]
fn a_deployment_with_no_status_yet_reads_zero() {
    let fresh = serde_json::json!({
        "metadata": {"name": "new", "namespace": "default", "creationTimestamp": "2026-10-22T11:59:00Z"},
        "spec": {"selector": {}, "template": {}}
    });

    let table = list(BuiltinKind::Deployments, vec![fresh], now());

    // spec.replicas defaults to 1 server-side; an unset field reads that way.
    assert_eq!(table.rows[0].cells[1..4], ["0/1", "0", "0"]);
}

#[test]
fn deployment_detail_shows_replica_bar_and_strategy() {
    let deployment = rolling_deployment();

    let detail = detail(BuiltinKind::Deployments, deployment, now());

    assert_eq!(detail.title, "Deployment default/web");
    assert_eq!(
        detail.replicas,
        Some(ReplicaCounts {
            desired: 3,
            updated: 2,
            available: 2,
            unavailable: 1
        })
    );
    assert_eq!(
        detail.status,
        [
            ("Strategy", "RollingUpdate"),
            ("Max surge", "25%"),
            ("Max unavailable", "1"),
            ("Selector", "app=web"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    assert_eq!(
        detail
            .conditions
            .iter()
            .map(|c| c.kind.as_str())
            .collect::<Vec<_>>(),
        ["Available", "Progressing"]
    );
}

#[test]
fn recreate_strategy_has_no_rolling_parameters() {
    let mut deployment = rolling_deployment();
    deployment["spec"]["strategy"] = serde_json::json!({"type": "Recreate"});

    let detail = detail(BuiltinKind::Deployments, deployment, now());

    assert_eq!(
        detail.status,
        [("Strategy", "Recreate"), ("Selector", "app=web")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
    );
}

fn selector(value: serde_json::Value) -> LabelSelector {
    serde_json::from_value(value).unwrap()
}

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn match_labels_must_all_match() {
    let web = selector(serde_json::json!({"matchLabels": {"app": "web", "tier": "front"}}));
    assert!(selector_matches(
        &web,
        Some(&labels(&[
            ("app", "web"),
            ("tier", "front"),
            ("extra", "x")
        ]))
    ));
    assert!(!selector_matches(&web, Some(&labels(&[("app", "web")]))));
    assert!(!selector_matches(
        &web,
        Some(&labels(&[("app", "api"), ("tier", "front")]))
    ));
    assert!(!selector_matches(&web, None));
}

#[test]
fn match_expressions_follow_label_selector_semantics() {
    let expressions = selector(serde_json::json!({"matchExpressions": [
        {"key": "tier", "operator": "In", "values": ["front", "edge"]},
        {"key": "env", "operator": "NotIn", "values": ["dev"]},
        {"key": "app", "operator": "Exists"},
        {"key": "canary", "operator": "DoesNotExist"},
    ]}));
    let matching = labels(&[("tier", "edge"), ("app", "web")]);
    assert!(selector_matches(&expressions, Some(&matching)));
    for (key, value) in [("tier", "back"), ("env", "dev"), ("canary", "yes")] {
        let mut changed = matching.clone();
        changed.insert(key.to_string(), value.to_string());
        assert!(
            !selector_matches(&expressions, Some(&changed)),
            "{key}={value}"
        );
    }
    let mut no_app = matching.clone();
    no_app.remove("app");
    assert!(!selector_matches(&expressions, Some(&no_app)));
    let unknown = selector(serde_json::json!({"matchExpressions": [
        {"key": "app", "operator": "Gt", "values": ["1"]},
    ]}));
    assert!(!selector_matches(&unknown, Some(&matching)));
}

#[test]
fn an_empty_selector_matches_everything() {
    assert!(selector_matches(&LabelSelector::default(), None));
}

#[test]
fn pod_ports_are_the_running_containers_declared_tcp_ports_in_order() {
    let pod = typed_pod(
        serde_json::json!({
            "initContainers": [
                {"name": "migrate", "ports": [{"containerPort": 9000}]},
                {"name": "proxy", "restartPolicy": "Always", "ports": [{"containerPort": 15001}]}
            ],
            "containers": [
                {"name": "app", "ports": [
                    {"name": "http", "containerPort": 8080},
                    {"name": "dns", "containerPort": 53, "protocol": "UDP"}
                ]},
                {"name": "sidecar", "ports": [
                    {"name": "admin", "containerPort": 9901, "protocol": "TCP"},
                    {"name": "dup", "containerPort": 8080}
                ]},
                {"name": "plain"}
            ]
        }),
        all_running(&["proxy", "app", "sidecar", "plain"], &["proxy"]),
    );
    assert_eq!(
        pod_ports(&pod),
        [
            ForwardPort {
                port: 15001,
                label: "15001 (proxy)".to_string()
            },
            ForwardPort {
                port: 8080,
                label: "8080/http (app)".to_string()
            },
            ForwardPort {
                port: 9901,
                label: "9901/admin (sidecar)".to_string()
            },
        ]
    );
}

#[test]
fn exec_targets_sidecars_and_app_containers_but_not_run_once_init_containers() {
    let pod = typed_pod(
        serde_json::json!({
            "initContainers": [
                {"name": "migrate"},
                {"name": "proxy", "restartPolicy": "Always"}
            ],
            "containers": [{"name": "app"}, {"name": "log-shipper"}]
        }),
        all_running(&["proxy", "app", "log-shipper"], &["proxy"]),
    );
    assert_eq!(running_containers(&pod), ["proxy", "app", "log-shipper"]);
}

/// A status where every container in `names` is running; the names in
/// `init` are reported under `initContainerStatuses`, the rest under
/// `containerStatuses`.
fn all_running(names: &[&str], init: &[&str]) -> serde_json::Value {
    let statuses = |init_wanted: bool| -> Vec<serde_json::Value> {
        names
            .iter()
            .filter(|name| init.contains(name) == init_wanted)
            .map(|name| container_status(name, true, &serde_json::json!({"running": {}})))
            .collect()
    };
    serde_json::json!({
        "initContainerStatuses": statuses(true),
        "containerStatuses": statuses(false)
    })
}

/// A Pod with ports on `app` (8080) and `db` (5432), whose `app` status is
/// `app_state` and whose `db` status is `db_state` (None: not reported).
fn two_container_pod(
    app_state: Option<serde_json::Value>,
    db_state: Option<serde_json::Value>,
) -> Pod {
    let statuses: Vec<_> = [("app", app_state), ("db", db_state)]
        .into_iter()
        .filter_map(|(name, state)| Some(container_status(name, false, &state?)))
        .collect();
    typed_pod(
        serde_json::json!({"containers": [
            {"name": "app", "ports": [{"containerPort": 8080}]},
            {"name": "db", "ports": [{"containerPort": 5432}]}
        ]}),
        serde_json::json!({"containerStatuses": statuses}),
    )
}

#[test]
fn only_a_container_whose_status_is_running_counts_as_running() {
    let running = Some(serde_json::json!({"running": {}}));
    let waiting = Some(serde_json::json!({"waiting": {"reason": "CrashLoopBackOff"}}));
    let terminated = Some(serde_json::json!({"terminated": {"exitCode": 1}}));
    for (state, counts) in [
        (running, true),
        (waiting, false),
        (terminated, false),
        (None, false),
    ] {
        let pod = two_container_pod(state.clone(), state);
        let expected: &[&str] = if counts { &["app", "db"] } else { &[] };
        assert_eq!(running_containers(&pod), expected, "{pod:?}");
        assert_eq!(pod_ports(&pod).is_empty(), !counts, "{pod:?}");
    }
}

#[test]
fn a_status_with_no_state_is_not_running() {
    let mut pod = two_container_pod(Some(serde_json::json!({"running": {}})), None);
    pod.status
        .as_mut()
        .unwrap()
        .container_statuses
        .as_mut()
        .unwrap()[0]
        .state = None;
    assert!(running_containers(&pod).is_empty());
}

#[test]
fn a_crash_looping_container_leaves_its_running_sibling_and_ports() {
    let pod = two_container_pod(
        Some(serde_json::json!({"waiting": {"reason": "CrashLoopBackOff"}})),
        Some(serde_json::json!({"running": {}})),
    );
    assert_eq!(running_containers(&pod), ["db"]);
    assert_eq!(
        pod_ports(&pod),
        [ForwardPort {
            port: 5432,
            label: "5432 (db)".to_string()
        }]
    );
}

#[test]
fn statuses_match_by_name_and_sidecars_read_only_the_init_statuses() {
    let spec = serde_json::json!({
        "initContainers": [{"name": "proxy", "restartPolicy": "Always"}],
        "containers": [{"name": "app"}]
    });
    // A running `proxy` under containerStatuses (and `app` under
    // initContainerStatuses) is the wrong list: neither counts.
    let swapped = typed_pod(
        spec.clone(),
        serde_json::json!({
            "initContainerStatuses": [container_status("app", true, &serde_json::json!({"running": {}}))],
            "containerStatuses": [container_status("proxy", true, &serde_json::json!({"running": {}}))]
        }),
    );
    assert!(running_containers(&swapped).is_empty());
    assert!(pod_ports(&swapped).is_empty());
    // A status under another name does not count either.
    let renamed = typed_pod(
        spec.clone(),
        serde_json::json!({
            "containerStatuses": [container_status("other", true, &serde_json::json!({"running": {}}))]
        }),
    );
    assert!(running_containers(&renamed).is_empty());
    // A terminated sidecar drops out while the app stays.
    let sidecar_down = typed_pod(
        spec,
        serde_json::json!({
            "initContainerStatuses": [container_status("proxy", false, &serde_json::json!({"terminated": {"exitCode": 1}}))],
            "containerStatuses": [container_status("app", true, &serde_json::json!({"running": {}}))]
        }),
    );
    assert_eq!(running_containers(&sidecar_down), ["app"]);
}

#[test]
fn a_pod_without_a_spec_has_no_ports() {
    let pod: Pod = serde_json::from_value(serde_json::json!({"metadata": {"name": "p"}})).unwrap();
    assert!(pod_ports(&pod).is_empty());
}

#[test]
fn a_pod_detail_offers_its_containers_and_ports_to_the_actions() {
    let model = detail(BuiltinKind::Pods, crashing_pod(), now());

    assert_eq!(
        model.containers,
        Some(Containers {
            all: ["migrate", "app", "sidecar"].map(String::from).to_vec(),
            // `app` is in CrashLoopBackOff: only `sidecar` is running.
            running: ["sidecar"].map(String::from).to_vec(),
        })
    );
    assert_eq!(
        model.forward,
        Some(Forward {
            to: ForwardTo::Pod,
            ports: Vec::new(),
            // `app` declares port 80 but is not running.
            empty: "No running container with a declared port",
        })
    );
    assert_eq!(model.suspended, None);
}

/// A container status named `name`, in `state`.
fn container_status(name: &str, ready: bool, state: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"name": name, "image": "", "imageID": "", "ready": ready,
                       "restartCount": 0, "state": state})
}

/// The Ready and Status cells of a Pod with a `setup` init container that
/// finished, a `proxy` sidecar and an `app` container.
fn sidecar_pod_cells(proxy_started: bool, proxy_ready: bool) -> Vec<String> {
    let spec = serde_json::json!({
        "initContainers": [{"name": "setup"}, {"name": "proxy", "restartPolicy": "Always"}],
        "containers": [{"name": "app"}]
    });
    let mut proxy = container_status("proxy", proxy_ready, &serde_json::json!({"running": {}}));
    proxy["started"] = proxy_started.into();
    let status = serde_json::json!({
        "phase": "Running",
        "initContainerStatuses": [
            container_status("setup", false, &serde_json::json!({"terminated": {"exitCode": 0}})),
            proxy
        ],
        "containerStatuses": [
            container_status("app", true, &serde_json::json!({"running": {}}))
        ]
    });
    let row = &list(BuiltinKind::Pods, vec![pod(spec, status)], now()).rows[0];
    row.cells[1..3].to_vec()
}

#[test]
fn a_started_sidecar_counts_toward_ready_and_total() {
    assert_eq!(sidecar_pod_cells(true, true), ["2/2", "Running"]);
    assert_eq!(sidecar_pod_cells(true, false), ["1/2", "Running"]);
}

#[test]
fn a_sidecar_that_has_not_started_holds_the_pod_initializing() {
    assert_eq!(sidecar_pod_cells(false, false), ["0/2", "Init:1/2"]);
}

#[test]
fn a_running_container_counts_as_ready_only_when_ready() {
    let spec = serde_json::json!({"containers": [{"name": "app"}]});
    let status = |ready: bool| {
        serde_json::json!({"phase": "Running", "containerStatuses": [
            container_status("app", ready, &serde_json::json!({"running": {}}))
        ]})
    };
    let ready = |ready: bool| {
        list(
            BuiltinKind::Pods,
            vec![pod(spec.clone(), status(ready))],
            now(),
        )
        .rows[0]
            .cells[1]
            .clone()
    };
    assert_eq!(ready(true), "1/1");
    assert_eq!(ready(false), "0/1");
}

#[test]
fn a_last_termination_without_restarts_shows_no_age() {
    let spec = serde_json::json!({"containers": [{"name": "app"}]});
    let mut app = container_status("app", true, &serde_json::json!({"running": {}}));
    app["lastState"] = serde_json::json!({"terminated": {"exitCode": 0,
                                          "finishedAt": "2026-10-22T11:55:00Z"}});
    let status = serde_json::json!({"phase": "Running", "containerStatuses": [app]});
    assert_eq!(
        list(BuiltinKind::Pods, vec![pod(spec, status)], now()).rows[0].cells[3],
        "0"
    );
}
