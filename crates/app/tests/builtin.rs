//! The complete built-in Registration contract. New kinds join this matrix
//! with their list columns, full action set and optional related kind.

use astrolabe::builtin::{Action, BuiltinKind};
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
            related: None,
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
    ];
    let now = Timestamp::now();
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
            (registration.list)(&[], now).columns,
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

    let detail = (BuiltinKind::Events.registration().detail)(&event, Timestamp::now()).unwrap();

    assert!(detail.status.is_empty());
    assert!(detail.sections.is_empty());
    assert!(detail.tables.is_empty());
    assert!(detail.replicas.is_none());
    assert!(!detail.metadata.is_empty());
    assert!(!detail.yaml.is_empty());
}
