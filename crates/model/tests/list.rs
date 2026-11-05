use std::collections::BTreeSet;

use astrolabe_kube::cluster::{ClusterError, ClusterErrorKind, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::list::{CRD_DENIED, ListOutcome, Resolved, failed_message, resolve};
use astrolabe_model::session::CustomKindId;
use astrolabe_model::session_state::{Command, SessionState};
use support::{object, served};

mod support;

fn state() -> SessionState {
    SessionState::new(ClusterId::named("kind-test"), "default")
}

fn now() -> astrolabe_kube::k8s_openapi::jiff::Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn resolved(state: &SessionState) -> Resolved {
    resolve(state, &BTreeSet::new(), now())
}

fn rows(resolved: &Resolved) -> usize {
    match &resolved.outcome {
        ListOutcome::Rows(table) => table.rows.len(),
        other => panic!("expected rows, got {other:?}"),
    }
}

fn run_of(commands: &[Command]) -> u64 {
    commands
        .iter()
        .find_map(|command| match command {
            Command::RunDiscovery { run } => Some(*run),
            _ => None,
        })
        .unwrap()
}

fn crontab_id() -> CustomKindId {
    CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "CronTab".to_string(),
    }
}

fn crontab_crd() -> CustomResourceDefinition {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": "crontabs.stable.example.com"},
        "spec": {
            "group": "stable.example.com",
            "scope": "Namespaced",
            "names": {"kind": "CronTab", "plural": "crontabs"},
            "versions": [{"name": "v1", "served": true, "storage": true}]
        }
    }))
    .unwrap()
}

/// A connected state whose Definitions list `CronTab`; Discovery has not
/// answered. Returns the first Discovery run.
fn crontab_state() -> (SessionState, u64) {
    let mut state = state();
    let run = run_of(&state.connected());
    state.crds_message(Ok(Event::Init));
    state.crds_message(Ok(Event::InitApply(crontab_crd())));
    state.crds_message(Ok(Event::InitDone));
    (state, run)
}

fn crontab_served() -> ServedResources {
    ServedResources::new([served("stable.example.com", "v1", "CronTab", true)])
}

fn pod(name: &str) -> Event<astrolabe_kube::kube::core::DynamicObject> {
    Event::Apply(object(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": name, "namespace": "default"},
    })))
}

fn pod_init(name: &str) -> Event<astrolabe_kube::kube::core::DynamicObject> {
    let Event::Apply(object) = pod(name) else {
        unreachable!()
    };
    Event::InitApply(object)
}

fn denied<T>() -> Result<T, ClusterError> {
    Err(ClusterError::forbidden("forbidden"))
}

fn live_namespaces(state: &mut SessionState) {
    state.namespaces_message(Ok(Event::Init));
    state.namespaces_message(Ok(Event::InitDone));
}

#[test]
fn namespaces_resolve_to_rows() {
    let mut state = state();
    live_namespaces(&mut state);

    let resolved = resolved(&state);

    assert_eq!(rows(&resolved), 0);
    assert!(!resolved.stale);
}

#[test]
fn namespaces_are_loading_before_their_first_snapshot() {
    let state = state();

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Loading("Loading namespaces…".to_string())
    );
}

#[test]
fn namespaces_are_stale_with_their_watch() {
    let mut state = state();
    live_namespaces(&mut state);
    state.namespaces_ended();

    let resolved = resolved(&state);

    assert!(resolved.stale);
    assert_eq!(rows(&resolved), 0);
}

#[test]
fn namespaces_that_failed_before_a_snapshot_are_failed() {
    let mut state = state();
    state.namespaces_message(Err(ClusterError::unreachable("refused")));

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Failed(Some(ClusterError::unreachable("refused")))
    );
}

#[test]
fn definitions_are_loading_before_their_first_snapshot() {
    let mut state = state();
    state.open_definitions_list();

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Loading("Loading custom resource definitions…".to_string())
    );
}

#[test]
fn definitions_that_failed_before_a_snapshot_are_failed() {
    let mut state = state();
    state.open_definitions_list();
    state.crds_message(Err(ClusterError::connect("no kubeconfig")));

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Failed(Some(ClusterError::connect("no kubeconfig")))
    );
}

fn failed_pods(message: Option<Result<(), ClusterError>>) -> Resolved {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;
    match message {
        Some(Err(error)) => {
            state.object_message(&key, Err(error));
        }
        Some(Ok(())) => unreachable!(),
        None => state.object_ended(&key),
    }
    resolved(&state)
}

#[test]
fn a_builtin_list_that_failed_unreachable_carries_the_error() {
    let resolved = failed_pods(Some(Err(ClusterError::unreachable("refused"))));

    let ListOutcome::Failed(Some(error)) = &resolved.outcome else {
        panic!("expected failed, got {:?}", resolved.outcome);
    };
    assert_eq!(error.kind(), ClusterErrorKind::Unreachable);
    assert!(!resolved.stale);
}

#[test]
fn a_builtin_list_that_failed_to_connect_carries_the_error() {
    let resolved = failed_pods(Some(Err(ClusterError::connect("no kubeconfig"))));

    let ListOutcome::Failed(Some(error)) = &resolved.outcome else {
        panic!("expected failed, got {:?}", resolved.outcome);
    };
    assert_eq!(error.kind(), ClusterErrorKind::Connect);
}

#[test]
fn a_builtin_list_whose_watch_ended_with_no_error_is_failed_without_one() {
    assert_eq!(failed_pods(None).outcome, ListOutcome::Failed(None));
}

#[test]
fn a_custom_list_that_failed_before_a_snapshot_is_failed() {
    let (mut state, run) = crontab_state();
    state.discovery_finished(run, Ok(crontab_served()));
    let key = state.open_custom_list(&crontab_id()).unwrap().key;
    state.object_message(&key, Err(ClusterError::unreachable("refused")));

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Failed(Some(ClusterError::unreachable("refused")))
    );
}

#[test]
fn a_custom_list_before_its_first_snapshot_is_loading() {
    let (mut state, run) = crontab_state();
    state.discovery_finished(run, Ok(crontab_served()));
    state.open_custom_list(&crontab_id());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Loading("Loading crontabs…".to_string())
    );
}

#[test]
fn failed_messages_name_the_kind_of_failure() {
    assert_eq!(
        failed_message(Some(&ClusterError::unreachable("refused"))),
        "Cannot reach the cluster: refused"
    );
    assert_eq!(
        failed_message(Some(&ClusterError::connect("no kubeconfig"))),
        "Cannot connect to the cluster: no kubeconfig"
    );
    assert_eq!(
        failed_message(Some(&ClusterError::new("boom"))),
        "Cannot load the list: boom"
    );
    assert_eq!(
        failed_message(None),
        "The connection ended before the list loaded"
    );
}

#[test]
fn a_builtin_list_resolves_to_its_rows() {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;
    state.object_message(&key, Ok(Event::Init));
    state.object_message(&key, Ok(pod_init("web")));
    state.object_message(&key, Ok(Event::InitDone));

    assert_eq!(rows(&resolved(&state)), 1);
}

#[test]
fn a_builtin_list_before_its_first_snapshot_is_loading() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Loading("Loading pods…".to_string())
    );
}

#[test]
fn a_live_builtin_list_with_no_objects_is_empty_rows() {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;
    state.object_message(&key, Ok(Event::Init));
    state.object_message(&key, Ok(Event::InitDone));

    assert_eq!(rows(&resolved(&state)), 0);
}

#[test]
fn a_builtin_list_whose_watch_ended_is_stale_with_its_old_rows() {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;
    state.object_message(&key, Ok(Event::Init));
    state.object_message(&key, Ok(pod_init("web")));
    state.object_message(&key, Ok(Event::InitDone));
    state.object_ended(&key);

    let resolved = resolved(&state);

    assert!(resolved.stale);
    assert_eq!(rows(&resolved), 1);
}

#[test]
fn a_denied_builtin_list_is_forbidden() {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;
    state.object_message(&key, denied());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Forbidden("No permission to list pods here".to_string())
    );
}

#[test]
fn definitions_resolve_to_rows_narrowed_by_group() {
    let (mut state, _) = crontab_state();
    state.open_definitions_list();

    assert_eq!(rows(&resolved(&state)), 1);
    let other = BTreeSet::from(["other.example.com".to_string()]);
    let narrowed = resolve(&state, &other, now());
    assert_eq!(rows(&narrowed), 0);
}

#[test]
fn denied_definitions_are_forbidden() {
    let mut state = state();
    state.open_definitions_list();
    state.crds_message(denied());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Forbidden(CRD_DENIED.to_string())
    );
}

#[test]
fn a_custom_kind_missing_from_the_catalog_is_undefined() {
    let mut state = state();
    state.open_custom_list(&crontab_id());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Undefined("CronTab is no longer defined".to_string())
    );
}

#[test]
fn a_custom_kind_before_discovery_is_loading() {
    let (mut state, _) = crontab_state();
    state.open_custom_list(&crontab_id());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Loading("Discovering API resources…".to_string())
    );
}

#[test]
fn a_custom_kind_after_failed_discovery_says_so() {
    let (mut state, run) = crontab_state();
    state.discovery_finished(run, Err(ClusterError::new("boom")));
    state.open_custom_list(&crontab_id());

    let ListOutcome::Loading(text) = resolved(&state).outcome else {
        panic!("expected loading");
    };
    assert!(text.starts_with("API discovery failed:"), "{text}");
}

#[test]
fn a_custom_kind_the_server_does_not_serve_is_not_served() {
    let (mut state, run) = crontab_state();
    state.discovery_finished(run, Ok(ServedResources::default()));
    state.open_custom_list(&crontab_id());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::NotServed("CronTab v1 is not served by the API server".to_string())
    );
}

#[test]
fn a_served_custom_kind_resolves_to_rows_and_goes_stale_with_its_watch() {
    let (mut state, run) = crontab_state();
    state.discovery_finished(run, Ok(crontab_served()));
    let key = state.open_custom_list(&crontab_id()).unwrap().key;
    state.object_message(&key, Ok(Event::Init));
    state.object_message(
        &key,
        Ok(Event::InitApply(object(serde_json::json!({
            "apiVersion": "stable.example.com/v1",
            "kind": "CronTab",
            "metadata": {"name": "nightly", "namespace": "default"},
        })))),
    );
    state.object_message(&key, Ok(Event::InitDone));
    assert_eq!(rows(&resolved(&state)), 1);
    assert!(!resolved(&state).stale);

    state.object_ended(&key);

    assert!(resolved(&state).stale);
}

#[test]
fn a_denied_custom_list_is_forbidden() {
    let (mut state, run) = crontab_state();
    state.discovery_finished(run, Ok(crontab_served()));
    let key = state.open_custom_list(&crontab_id()).unwrap().key;
    state.object_message(&key, denied());

    assert_eq!(
        resolved(&state).outcome,
        ListOutcome::Forbidden("No permission to list crontabs here".to_string())
    );
}

#[test]
fn definitions_whose_watch_ended_are_stale() {
    let (mut state, _) = crontab_state();
    state.open_definitions_list();
    assert!(!resolved(&state).stale);

    state.crds_ended();

    assert!(resolved(&state).stale);
}
