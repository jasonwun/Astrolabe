//! What a detail tab's toolbar shows for each action and each operation it
//! sends (#96), with no view. Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::{ObjectKey, ObjectRef, Operation};
use astrolabe_model::builtin::{Action, BuiltinKind};
use astrolabe_model::operation_text;

fn web() -> ObjectRef {
    ObjectRef {
        resource: ApiResource::erase::<Deployment>(&()),
        key: ObjectKey::new(Some("default".to_string()), "web"),
    }
}

#[test]
fn each_operation_names_its_pending_done_and_failure_text() {
    let cases = [
        (
            Operation::Scale {
                object: web(),
                replicas: 3,
            },
            ("Scaling…", "Scaled to 3", "Scale"),
        ),
        (
            Operation::RolloutRestart { object: web() },
            ("Restarting…", "Restart requested", "Restart"),
        ),
        (
            Operation::SetSuspend {
                object: web(),
                suspend: true,
            },
            ("Suspending…", "Suspended", "Suspend"),
        ),
        (
            Operation::SetSuspend {
                object: web(),
                suspend: false,
            },
            ("Resuming…", "Resumed", "Resume"),
        ),
        (
            Operation::TriggerCronJob { object: web() },
            ("Triggering…", "Job creation requested", "Trigger"),
        ),
        (
            Operation::Delete { object: web() },
            ("Deleting…", "Deletion requested", "Delete"),
        ),
    ];
    for (operation, (pending, done, verb)) in cases {
        assert_eq!(
            (
                operation_text::pending(&operation),
                operation_text::done(&operation).as_str(),
                operation_text::verb(&operation)
            ),
            (pending, done, verb),
            "{operation:?}"
        );
    }
}

fn cronjob(suspend: Option<bool>) -> serde_json::Value {
    let mut spec = serde_json::json!({
        "schedule": "*/5 * * * *",
        "jobTemplate": {"spec": {"template": {"spec": {
            "restartPolicy": "OnFailure",
            "containers": [{"name": "app", "image": "busybox"}]
        }}}}
    });
    if let Some(suspend) = suspend {
        spec["suspend"] = suspend.into();
    }
    serde_json::json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": "nightly", "namespace": "default", "uid": "uid-cron"},
        "spec": spec
    })
}

#[test]
fn suspend_reads_resume_only_while_the_cronjob_is_suspended() {
    let now: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();
    let label = |suspend| {
        Action::Suspend.label(&support::detail(
            BuiltinKind::CronJobs,
            cronjob(suspend),
            now,
        ))
    };
    assert_eq!(label(Some(true)), "Resume");
    assert_eq!(label(Some(false)), "Suspend");
    assert_eq!(label(None), "Suspend");
}

#[test]
fn every_other_action_keeps_its_label() {
    let now: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();
    let model = support::detail(BuiltinKind::CronJobs, cronjob(Some(true)), now);
    let labels: Vec<_> = [
        Action::Logs,
        Action::Exec,
        Action::PortForward,
        Action::Scale,
        Action::RolloutRestart,
        Action::Trigger,
        Action::Edit,
        Action::Delete,
    ]
    .into_iter()
    .map(|action| action.label(&model))
    .collect();
    assert_eq!(
        labels,
        [
            "Logs",
            "Exec",
            "Port Forward",
            "Scale",
            "Rollout Restart",
            "Trigger",
            "Edit YAML",
            "Delete"
        ]
    );
}
