use std::collections::BTreeSet;

use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::list::{CRD_DENIED, ListOutcome, Resolved, resolve};
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

#[test]
fn namespaces_resolve_to_rows() {
    let state = state();

    let resolved = resolved(&state);

    assert_eq!(rows(&resolved), 0);
    assert!(!resolved.stale);
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
fn a_builtin_list_before_its_first_snapshot_has_no_rows() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);

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
