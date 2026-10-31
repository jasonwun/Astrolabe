//! The complete built-in Registration contract. New kinds join this matrix
//! with their list columns, full action set and optional related kind.

use astrolabe::builtin::{Action, BuiltinKind};
use astrolabe_kube::k8s_openapi::api::core::v1::ObjectReference;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;

struct Contract {
    kind: BuiltinKind,
    columns: &'static [&'static str],
    actions: &'static [Action],
    related: Option<BuiltinKind>,
}

const EDIT_DELETE: &[Action] = &[Action::Edit, Action::Delete];

#[test]
fn every_builtin_registration_matches_its_contract() {
    let contracts = [
        Contract {
            kind: BuiltinKind::Pods,
            columns: &["Name", "Ready", "Status", "Restarts", "IP", "Node", "Age"],
            actions: &[
                Action::Logs,
                Action::Exec,
                Action::PortForward,
                Action::Edit,
                Action::Delete,
            ],
            related: Some(BuiltinKind::Events),
        },
        Contract {
            kind: BuiltinKind::Deployments,
            columns: &["Name", "Ready", "Up-to-date", "Available", "Age"],
            actions: &[
                Action::Scale,
                Action::RolloutRestart,
                Action::Edit,
                Action::Delete,
            ],
            related: Some(BuiltinKind::Pods),
        },
        Contract {
            kind: BuiltinKind::StatefulSets,
            columns: &["Name", "Ready", "Desired", "Age"],
            actions: &[
                Action::Scale,
                Action::RolloutRestart,
                Action::Edit,
                Action::Delete,
            ],
            related: Some(BuiltinKind::Pods),
        },
        Contract {
            kind: BuiltinKind::DaemonSets,
            columns: &[
                "Name",
                "Desired",
                "Current",
                "Ready",
                "Updated",
                "Available",
                "Node Selector",
                "Age",
            ],
            actions: &[Action::RolloutRestart, Action::Edit, Action::Delete],
            related: Some(BuiltinKind::Pods),
        },
        Contract {
            kind: BuiltinKind::ReplicaSets,
            columns: &["Name", "Desired", "Current", "Ready", "Age"],
            actions: &[Action::Scale, Action::Edit, Action::Delete],
            related: Some(BuiltinKind::Pods),
        },
        Contract {
            kind: BuiltinKind::Jobs,
            columns: &[
                "Name",
                "Resumed",
                "Status",
                "Succeeded",
                "Completions",
                "Parallelism",
                "Duration",
                "Age",
            ],
            actions: EDIT_DELETE,
            related: Some(BuiltinKind::Pods),
        },
        Contract {
            kind: BuiltinKind::CronJobs,
            columns: &[
                "Name",
                "Schedule",
                "Timezone",
                "Resumed",
                "Active",
                "Last schedule",
                "Age",
            ],
            actions: &[Action::Suspend, Action::Trigger, Action::Delete],
            related: Some(BuiltinKind::Jobs),
        },
        Contract {
            kind: BuiltinKind::Events,
            columns: &[
                "Type",
                "Reason",
                "Involved Object",
                "Message",
                "Count",
                "Last Seen",
            ],
            actions: &[],
            related: None,
        },
        Contract {
            kind: BuiltinKind::Services,
            columns: &[
                "Name",
                "Type",
                "Cluster IP",
                "External IP",
                "Ports",
                "Age",
                "Status",
            ],
            actions: &[Action::PortForward, Action::Edit, Action::Delete],
            related: Some(BuiltinKind::EndpointSlices),
        },
        Contract {
            kind: BuiltinKind::Ingresses,
            columns: &["Name", "LoadBalancers", "Rules", "Age"],
            actions: EDIT_DELETE,
            related: None,
        },
        Contract {
            kind: BuiltinKind::ConfigMaps,
            columns: &["Name", "Keys", "Age"],
            actions: EDIT_DELETE,
            related: None,
        },
        Contract {
            kind: BuiltinKind::Secrets,
            columns: &["Name", "Keys", "Type", "Age"],
            actions: EDIT_DELETE,
            related: None,
        },
        Contract {
            kind: BuiltinKind::ResourceQuotas,
            columns: &["Name", "Age"],
            actions: EDIT_DELETE,
            related: None,
        },
        Contract {
            kind: BuiltinKind::LimitRanges,
            columns: &["Name", "Age"],
            actions: EDIT_DELETE,
            related: None,
        },
        Contract {
            kind: BuiltinKind::HorizontalPodAutoscalers,
            columns: &[
                "Name",
                "Metrics",
                "Min Pods",
                "Max Pods",
                "Replicas",
                "Age",
                "Conditions",
            ],
            actions: EDIT_DELETE,
            related: None,
        },
        Contract {
            kind: BuiltinKind::EndpointSlices,
            columns: &["Name", "Type", "Ports", "Endpoints"],
            actions: EDIT_DELETE,
            related: None,
        },
    ];
    let now = Timestamp::UNIX_EPOCH;
    assert_eq!(contracts.len(), BuiltinKind::ALL.len());
    for kind in BuiltinKind::ALL {
        assert!(
            contracts.iter().any(|contract| contract.kind == kind),
            "{kind:?} has no contract"
        );
    }
    for contract in contracts {
        let kind = contract.kind;
        let registration = kind.registration();
        assert_eq!(registration.actions, contract.actions, "{kind:?} actions");
        assert_eq!(
            registration.related.as_ref().map(|related| related.kind),
            contract.related,
            "{kind:?} related kind"
        );
        assert_eq!(
            (registration.list)(&[], now).headers(),
            contract.columns,
            "{kind:?} list columns"
        );
    }
}

#[test]
fn events_use_generic_three_section_details_without_special_status() {
    let event: DynamicObject = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Event",
        "metadata": {"name": "web.started", "namespace": "default"},
        "involvedObject": {"kind": "Pod", "name": "web", "namespace": "default"},
        "reason": "Started"
    }))
    .unwrap();

    let detail =
        (BuiltinKind::Events.registration().detail)(&event, Timestamp::UNIX_EPOCH).unwrap();

    assert!(detail.status.is_empty());
    assert!(detail.sections.is_empty());
    assert!(detail.tables.is_empty());
    assert!(detail.replicas.is_none());
    assert!(!detail.metadata.is_empty());
    assert!(!detail.yaml.is_empty());
}

#[test]
fn an_events_row_opens_its_involved_object_from_any_cell() {
    let event: DynamicObject = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Event",
        "metadata": {"name": "web.started", "namespace": "default"},
        "involvedObject": {"apiVersion": "v1", "kind": "Pod", "name": "web", "namespace": "default", "uid": "uid-web"},
        "reason": "Started"
    }))
    .unwrap();
    let registration = BuiltinKind::Events.registration();

    let row_target = registration.row_target.expect("Events open another object");

    assert_eq!(
        row_target(&event),
        Some(ObjectReference {
            api_version: Some("v1".to_string()),
            kind: Some("Pod".to_string()),
            name: Some("web".to_string()),
            namespace: Some("default".to_string()),
            uid: Some("uid-web".to_string()),
            ..ObjectReference::default()
        })
    );
    assert!(registration.open_all_cells);
}

#[test]
fn other_rows_open_themselves_from_their_first_cell() {
    for kind in BuiltinKind::ALL {
        if kind == BuiltinKind::Events {
            continue;
        }
        let registration = kind.registration();
        assert!(registration.row_target.is_none(), "{kind:?} row target");
        assert!(!registration.open_all_cells, "{kind:?} open all cells");
    }
}

#[test]
fn navigation_lists_every_kind_but_endpoint_slices_once() {
    let mut listed: Vec<BuiltinKind> = BuiltinKind::NAVIGATION
        .iter()
        .flat_map(|(_, kinds)| kinds.iter().copied())
        .chain([BuiltinKind::EndpointSlices])
        .collect();
    let count = listed.len();
    listed.sort();
    listed.dedup();
    assert_eq!(listed.len(), count, "a kind is listed twice");
    let mut all = BuiltinKind::ALL.to_vec();
    all.sort();
    assert_eq!(listed, all);
}
