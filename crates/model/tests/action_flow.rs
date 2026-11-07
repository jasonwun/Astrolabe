//! The detail panel's action flow (#300), with no view: what each step
//! asks for and which effect it answers with. Synthetic objects.

use astrolabe_kube::cluster::ClusterError;
use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::object::{ObjectKey, ObjectRef, Operation};
use astrolabe_kube::pod::{ForwardRequest, ForwardTarget};
use astrolabe_model::builtin::Action;
use astrolabe_model::detail_actions::{
    DetailActions, Effect, ForwardTone, PodAction, Subject, forward_row,
};
use astrolabe_model::projection::Containers;
use astrolabe_model::session::ForwardState;

fn key() -> ObjectKey {
    ObjectKey::new(Some("default".to_string()), "web-0")
}

fn object() -> ObjectRef {
    ObjectRef {
        resource: ApiResource::erase::<Deployment>(&()),
        key: key(),
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn pod(all: &[&str], running: &[&str]) -> Subject {
    Subject {
        pod: Some(key()),
        containers: Containers {
            all: names(all),
            running: names(running),
        },
        forward: Some(ForwardTarget::Pod(key())),
        object: Some(object()),
        ..Subject::default()
    }
}

#[test]
fn one_container_skips_the_picker() {
    let mut actions = DetailActions::default();
    let effect = actions.press(Action::Logs, &pod(&["app"], &["app"]));
    let Effect::OpenLogs { title, request } = effect else {
        panic!("expected a Logs tab, got {effect:?}");
    };
    assert_eq!(title, "Logs default/web-0 (app)");
    assert_eq!(request.container, "app");
    assert_eq!(actions.picking(), None);
}

#[test]
fn several_containers_ask_which() {
    let mut actions = DetailActions::default();
    let effect = actions.press(Action::Logs, &pod(&["a", "b"], &["a", "b"]));
    assert_eq!(effect, Effect::None);
    assert_eq!(
        actions.picking().unwrap().prompt(),
        "Logs: choose a container"
    );
}

#[test]
fn exec_then_asks_for_a_shell() {
    let subject = pod(&["a", "b"], &["a", "b"]);
    let mut actions = DetailActions::default();
    actions.press(Action::Exec, &subject);
    assert_eq!(
        actions.picking().unwrap().prompt(),
        "Exec: choose a container"
    );
    let effect = actions.choose_container(PodAction::Exec, "b".to_string(), &subject);
    assert_eq!(effect, Effect::None);
    assert_eq!(
        actions.picking().unwrap().prompt(),
        "Exec in b: choose a shell"
    );
    let effect = actions.choose_shell("b".to_string(), "bash", &subject);
    let Effect::OpenExec { title, request } = effect else {
        panic!("expected an Exec tab, got {effect:?}");
    };
    assert_eq!(title, "Exec default/web-0 (b)");
    assert_eq!(
        (request.container.as_str(), request.shell.as_str()),
        ("b", "bash")
    );
    assert_eq!(actions.picking(), None);
}

#[test]
fn a_single_running_container_still_asks_for_a_shell() {
    let mut actions = DetailActions::default();
    actions.press(Action::Exec, &pod(&["a"], &["a"]));
    assert_eq!(
        actions.picking().unwrap().prompt(),
        "Exec in a: choose a shell"
    );
}

#[test]
fn logs_offer_init_containers_but_exec_only_running_ones() {
    let subject = pod(&["init", "app"], &["app"]);
    assert_eq!(subject.containers.all, names(&["init", "app"]));
    let mut logs = DetailActions::default();
    assert_eq!(logs.press(Action::Logs, &subject), Effect::None);
    assert_eq!(logs.picking().unwrap().action, PodAction::Logs);
    // Exec sees one running container, so it skips to the shell.
    let mut exec = DetailActions::default();
    exec.press(Action::Exec, &subject);
    assert_eq!(
        exec.picking().unwrap().prompt(),
        "Exec in app: choose a shell"
    );
}

#[test]
fn with_no_running_container_exec_asks_with_nothing_to_pick() {
    let mut actions = DetailActions::default();
    let effect = actions.press(Action::Exec, &pod(&["done"], &[]));
    assert_eq!(effect, Effect::None);
    assert_eq!(
        actions.picking().unwrap().prompt(),
        "Exec: choose a container"
    );
}

#[test]
fn forward_asks_for_a_port_and_then_forwards_it() {
    let subject = pod(&["a"], &["a"]);
    let mut actions = DetailActions::default();
    assert_eq!(actions.press(Action::PortForward, &subject), Effect::None);
    assert_eq!(
        actions.picking().unwrap().prompt(),
        "Port forward: choose a port"
    );
    assert_eq!(
        actions.choose_port(8080, &subject),
        Effect::Forward(ForwardRequest {
            target: ForwardTarget::Pod(key()),
            port: 8080,
        })
    );
    assert_eq!(actions.picking(), None);
}

#[test]
fn cancelling_the_picker_closes_it() {
    let mut actions = DetailActions::default();
    actions.press(Action::PortForward, &pod(&[], &[]));
    actions.cancel_picker();
    assert_eq!(actions.picking(), None);
}

#[test]
fn scale_rejects_what_is_not_a_whole_number_of_zero_or_more() {
    for text in ["-1", "1.5", "abc", ""] {
        let mut actions = DetailActions::default();
        actions.press(Action::Scale, &pod(&[], &[]));
        assert_eq!(actions.submit_scale(text, &pod(&[], &[])), Effect::None);
        assert_eq!(
            actions.notice(),
            Some("Replicas must be a whole number, 0 or more"),
            "{text:?}"
        );
        assert!(actions.scale_open(), "{text:?} keeps the form open");
    }
}

#[test]
fn scale_accepts_zero_and_closes_the_form() {
    let subject = pod(&[], &[]);
    let mut actions = DetailActions::default();
    actions.press(Action::Scale, &subject);
    assert!(actions.scale_open());
    let effect = actions.submit_scale(" 0 ", &subject);
    assert_eq!(
        effect,
        Effect::Run(Operation::Scale {
            object: object(),
            replicas: 0,
        })
    );
    assert!(!actions.scale_open());
    assert_eq!(actions.notice(), Some("Scaling…"));
}

#[test]
fn scale_keeps_the_form_open_once_the_object_vanished() {
    let gone = Subject::default();
    let mut actions = DetailActions::default();
    actions.press(Action::Scale, &gone);
    assert_eq!(actions.submit_scale("2", &gone), Effect::None);
    assert!(actions.scale_open());
    assert_eq!(actions.notice(), None);
}

#[test]
fn delete_runs_only_after_confirmation() {
    let subject = pod(&[], &[]);
    let mut actions = DetailActions::default();
    assert_eq!(actions.press(Action::Delete, &subject), Effect::None);
    assert!(actions.confirming_delete());
    assert_eq!(actions.notice(), None);
    assert_eq!(
        actions.confirm_delete(&subject),
        Effect::Run(Operation::Delete { object: object() })
    );
    assert!(!actions.confirming_delete());
    assert_eq!(actions.notice(), Some("Deleting…"));
}

#[test]
fn cancelling_delete_runs_nothing() {
    let mut actions = DetailActions::default();
    actions.press(Action::Delete, &pod(&[], &[]));
    actions.cancel_delete();
    assert!(!actions.confirming_delete());
    assert_eq!(actions.notice(), None);
}

#[test]
fn a_vanished_object_runs_nothing() {
    let gone = Subject::default();
    let mut actions = DetailActions::default();
    for action in [Action::RolloutRestart, Action::Suspend, Action::Trigger] {
        assert_eq!(actions.press(action, &gone), Effect::None);
    }
    actions.press(Action::Delete, &gone);
    assert_eq!(actions.confirm_delete(&gone), Effect::None);
    assert_eq!(actions.notice(), None);
}

#[test]
fn suspend_flips_the_current_state() {
    let mut subject = pod(&[], &[]);
    let mut actions = DetailActions::default();
    let effect = actions.press(Action::Suspend, &subject);
    assert_eq!(
        effect,
        Effect::Run(Operation::SetSuspend {
            object: object(),
            suspend: true,
        })
    );
    subject.suspended = true;
    let effect = actions.press(Action::Suspend, &subject);
    assert_eq!(
        effect,
        Effect::Run(Operation::SetSuspend {
            object: object(),
            suspend: false,
        })
    );
}

#[test]
fn restart_and_trigger_run_their_operations() {
    let subject = pod(&[], &[]);
    let mut actions = DetailActions::default();
    assert_eq!(
        actions.press(Action::RolloutRestart, &subject),
        Effect::Run(Operation::RolloutRestart { object: object() })
    );
    assert_eq!(
        actions.press(Action::Trigger, &subject),
        Effect::Run(Operation::TriggerCronJob { object: object() })
    );
}

#[test]
fn edit_opens_no_flow() {
    let mut actions = DetailActions::default();
    assert_eq!(actions.press(Action::Edit, &pod(&[], &[])), Effect::None);
    assert_eq!(actions, DetailActions::default());
}

#[test]
fn a_finished_operation_shows_its_done_or_failed_notice() {
    let operation = Operation::Scale {
        object: object(),
        replicas: 3,
    };
    let mut actions = DetailActions::default();
    actions.finished(&operation, Ok::<(), ClusterError>(()));
    assert_eq!(actions.notice(), Some("Scaled to 3"));
    actions.finished(&operation, Err("boom"));
    assert_eq!(actions.notice(), Some("Scale failed: boom"));
}

#[test]
fn a_forward_row_reads_by_state() {
    assert_eq!(
        forward_row(&ForwardState::Starting, 80),
        ("Starting → 80…".to_string(), ForwardTone::Muted)
    );
    assert_eq!(
        forward_row(&ForwardState::Listening(5000), 80),
        ("localhost:5000 → 80".to_string(), ForwardTone::Normal)
    );
    let failed = forward_row(
        &ForwardState::Failed(ClusterError::unreachable("refused")),
        80,
    );
    assert_eq!(failed.1, ForwardTone::Danger);
    assert!(failed.0.starts_with("Forward to 80 failed: "), "{failed:?}");
}
