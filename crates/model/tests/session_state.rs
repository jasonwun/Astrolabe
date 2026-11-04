use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_kube::object::ObjectKey;
use astrolabe_kube::pod::{ForwardEvent, ForwardRequest, ForwardTarget};
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::catalog::KindResource;
use astrolabe_model::session::{
    ConnectionHealth, CustomKindId, ForwardState, ResourceKind, SnapshotChange, WatchKey,
};
use astrolabe_model::session_state::{ActiveForward, Command, SessionState, custom_kind};
use support::{object, served};

mod support;

fn state() -> SessionState {
    SessionState::new(ClusterId::named("kind-test"), "default")
}

fn namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

/// The keys of the watches `commands` start.
fn started(commands: &[Command]) -> Vec<WatchKey> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::StartWatch { key, .. } => Some(key.clone()),
            Command::StartNamespacesWatch
            | Command::StartCrdsWatch
            | Command::ScheduleDiscovery { .. }
            | Command::RunDiscovery { .. }
            | Command::StartForward { .. }
            | Command::StopForward { .. } => None,
        })
        .collect()
}

fn pod_key(namespace: &str) -> WatchKey {
    WatchKey {
        cluster: ClusterId::named("kind-test"),
        kind: ResourceKind::Builtin(BuiltinKind::Pods),
        namespace: Some(namespace.to_string()),
    }
}

fn crontab_crd(scope: &str) -> CustomResourceDefinition {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": "crontabs.stable.example.com"},
        "spec": {
            "group": "stable.example.com",
            "scope": scope,
            "names": {"kind": "CronTab", "plural": "crontabs"},
            "versions": [{"name": "v1", "served": true, "storage": true}]
        }
    }))
    .unwrap()
}

fn crontab_id() -> CustomKindId {
    CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "CronTab".to_string(),
    }
}

/// The run a `RunDiscovery` among `commands` asks for.
fn run_of(commands: &[Command]) -> u64 {
    commands
        .iter()
        .find_map(|command| match command {
            Command::RunDiscovery { run } => Some(*run),
            _ => None,
        })
        .expect("a RunDiscovery command")
}

/// The generation a `ScheduleDiscovery` among `commands` asks for.
fn scheduled(commands: &[Command]) -> Option<u64> {
    commands.iter().find_map(|command| match command {
        Command::ScheduleDiscovery { generation } => Some(*generation),
        _ => None,
    })
}

/// A Discovery result that serves `CronTab`.
fn crontab_served(scope: &str) -> ServedResources {
    ServedResources::new([served(
        "stable.example.com",
        "v1",
        "CronTab",
        scope == "Namespaced",
    )])
}

/// Feeds `state` the first Definition snapshot, a `CronTab` Definition.
fn list_crontab_crd(state: &mut SessionState, scope: &str) -> Vec<Command> {
    let mut commands = state.crds_message(Ok(Event::Init));
    commands.extend(state.crds_message(Ok(Event::InitApply(crontab_crd(scope)))));
    commands.extend(state.crds_message(Ok(Event::InitDone)));
    commands
}

/// A state connected to a cluster whose Definitions list `CronTab`; its
/// first Discovery answered with the kind served, unless `discovered` is
/// false.
fn crontab_state(scope: &str, discovered: bool) -> SessionState {
    let mut state = state();
    let run = run_of(&state.connected());
    list_crontab_crd(&mut state, scope);
    if discovered {
        state.discovery_finished(run, Ok(crontab_served(scope)));
    }
    state
}

fn pod_event(name: &str) -> Event<DynamicObject> {
    Event::Apply(object(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": name, "namespace": "default"},
    })))
}

#[test]
fn opening_a_builtin_list_starts_its_watch_for_the_selected_namespace() {
    let mut state = state();

    let ensured = state.open_builtin_list(BuiltinKind::Pods);

    assert_eq!(ensured.key, pod_key("default"));
    assert_eq!(started(&ensured.commands), [pod_key("default")]);
    let Command::StartWatch { resource, .. } = &ensured.commands[0] else {
        panic!("a StartWatch command");
    };
    assert_eq!(resource.kind, "Pod");
    assert!(state.object_watch(&ensured.key).is_some());
}

#[test]
fn reopening_a_list_starts_no_second_watch() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);

    let ensured = state.open_builtin_list(BuiltinKind::Pods);

    assert!(ensured.commands.is_empty());
}

#[test]
fn a_detail_watch_ignores_the_namespace_selection() {
    let mut state = state();

    let ensured = state.watch_builtin(BuiltinKind::Pods, Some("team-a"));

    assert_eq!(started(&ensured.commands), [pod_key("team-a")]);
    assert_eq!(state.namespace(), "default");
}

#[test]
fn changing_the_namespace_moves_the_viewed_list_to_it() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);

    let commands = state.set_namespace("team-a".to_string());

    assert_eq!(started(&commands), [pod_key("team-a")]);
    assert_eq!(state.namespace(), "team-a");
}

#[test]
fn changing_back_to_a_visited_namespace_starts_nothing() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.set_namespace("team-a".to_string());

    let commands = state.set_namespace("default".to_string());

    assert!(commands.is_empty());
}

#[test]
fn selecting_the_selected_namespace_starts_nothing() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.open_namespaces_list();

    assert!(state.set_namespace("default".to_string()).is_empty());
}

#[test]
fn changing_the_namespace_with_no_list_viewed_starts_nothing() {
    let mut state = state();

    let commands = state.set_namespace("team-a".to_string());

    assert!(commands.is_empty());
    assert_eq!(state.namespace(), "team-a");
}

#[test]
fn a_closed_list_no_longer_follows_the_namespace() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.open_namespaces_list();

    assert!(state.set_namespace("team-a".to_string()).is_empty());
}

#[test]
fn the_last_opened_list_is_the_one_that_follows() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.open_builtin_list(BuiltinKind::Deployments);

    let commands = state.set_namespace("team-a".to_string());

    let kinds: Vec<_> = started(&commands).into_iter().map(|key| key.kind).collect();
    assert_eq!(kinds, [ResourceKind::Builtin(BuiltinKind::Deployments)]);
}

#[test]
fn a_namespaced_custom_list_follows_the_namespace() {
    let mut state = crontab_state("Namespaced", true);
    let ensured = state.open_custom_list(&crontab_id()).unwrap();
    assert_eq!(ensured.key.namespace.as_deref(), Some("default"));
    assert_eq!(ensured.commands.len(), 1);

    let commands = state.set_namespace("team-a".to_string());

    let keys = started(&commands);
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].namespace.as_deref(), Some("team-a"));
    assert!(matches!(
        &keys[0].kind,
        ResourceKind::Custom { group, version, kind }
            if group == "stable.example.com" && version == "v1" && kind == "CronTab"
    ));
}

#[test]
fn a_cluster_scoped_custom_list_ignores_the_namespace() {
    let mut state = crontab_state("Cluster", true);
    let ensured = state.open_custom_list(&crontab_id()).unwrap();
    assert_eq!(ensured.key.namespace, None);

    let commands = state.set_namespace("team-a".to_string());

    assert!(commands.is_empty());
}

#[test]
fn a_custom_list_of_an_unknown_kind_starts_nothing() {
    let mut state = state();

    assert!(state.open_custom_list(&crontab_id()).is_none());
}

#[test]
fn a_custom_list_that_is_not_served_starts_nothing() {
    let mut state = crontab_state("Namespaced", false);

    assert!(state.open_custom_list(&crontab_id()).is_none());
    assert!(
        state
            .watch_custom_object(&crontab_id(), Some("default"))
            .is_none()
    );
}

#[test]
fn a_viewed_custom_list_starts_its_watch_once_the_kind_is_served() {
    let mut state = state();
    let run = run_of(&state.connected());
    list_crontab_crd(&mut state, "Namespaced");
    assert!(state.open_custom_list(&crontab_id()).is_none());

    let commands = state.discovery_finished(run, Ok(crontab_served("Namespaced")));

    assert_eq!(started(&commands).len(), 1);
}

#[test]
fn a_custom_list_already_watched_starts_nothing_when_the_catalog_changes() {
    let mut state = crontab_state("Namespaced", true);
    state.open_custom_list(&crontab_id()).unwrap();

    let commands = state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))));

    assert_eq!(started(&commands), []);
}

#[test]
fn a_catalog_change_leaves_a_builtin_list_alone() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.open_namespaces_list();
    state.open_builtin_list(BuiltinKind::Deployments);
    state.set_namespace("team-a".to_string());

    let commands = list_crontab_crd(&mut state, "Namespaced");

    assert_eq!(started(&commands), []);
}

#[test]
fn a_custom_object_watch_ignores_the_namespace_selection() {
    let mut state = crontab_state("Namespaced", true);

    let ensured = state
        .watch_custom_object(&crontab_id(), Some("team-b"))
        .unwrap();

    assert_eq!(ensured.key.namespace.as_deref(), Some("team-b"));
    assert_eq!(state.namespace(), "default");
}

#[test]
fn custom_kind_finds_a_kind_by_group_and_name() {
    let state = crontab_state("Namespaced", true);
    let catalog = state.catalog();

    assert_eq!(
        custom_kind(catalog, &crontab_id()).unwrap().plural,
        "crontabs"
    );
    let other_group = CustomKindId {
        group: "other.io".to_string(),
        kind: "CronTab".to_string(),
    };
    assert!(custom_kind(catalog, &other_group).is_none());
    let other_kind = CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "Backup".to_string(),
    };
    assert!(custom_kind(catalog, &other_kind).is_none());
}

#[test]
fn object_messages_fill_the_watch_they_are_keyed_to() {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;

    let change = state.object_message(&key, Ok(pod_event("web")));

    assert_eq!(change, SnapshotChange::Changed);
    assert!(
        state
            .builtin_object(BuiltinKind::Pods, Some("default"), "web")
            .is_some()
    );
    assert!(
        state
            .builtin_object(BuiltinKind::Pods, Some("default"), "db")
            .is_none()
    );
}

#[test]
fn a_message_for_a_watch_nobody_started_changes_nothing() {
    let mut state = state();

    let change = state.object_message(&pod_key("default"), Ok(pod_event("web")));

    assert_eq!(change, SnapshotChange::Unchanged);
    assert!(
        state
            .builtin_watch(BuiltinKind::Pods, Some("default"))
            .is_none()
    );
}

#[test]
fn namespaces_arrive_with_the_first_snapshot() {
    let mut state = state();
    assert_eq!(state.health(), ConnectionHealth::Connecting);

    state.namespaces_message(Ok(Event::Init));
    state.namespaces_message(Ok(Event::InitApply(namespace("default"))));
    let change = state.namespaces_message(Ok(Event::InitDone));

    assert_eq!(change, SnapshotChange::FirstSnapshot);
    assert_eq!(state.health(), ConnectionHealth::Healthy);
    assert_eq!(state.namespaces(), [&namespace("default")]);
    assert!(state.namespace_object("default").is_some());
    assert!(state.namespace_object("other").is_none());
}

#[test]
fn the_namespaces_watch_ending_marks_a_loaded_snapshot_stale() {
    let mut state = state();
    state.namespaces_message(Ok(Event::InitDone));
    assert!(!state.namespaces_stale());

    state.namespaces_ended();

    assert!(state.namespaces_stale());
    assert_eq!(state.health(), ConnectionHealth::Stale);
}

#[test]
fn the_namespaces_watch_ending_before_any_snapshot_is_not_stale() {
    let mut state = state();

    state.namespaces_ended();

    assert!(!state.namespaces_stale());
    assert_eq!(state.health(), ConnectionHealth::Stale);
}

#[test]
fn a_namespaces_error_marks_the_snapshot_stale_and_the_next_one_clears_it() {
    let mut state = state();
    state.namespaces_message(Ok(Event::InitDone));

    state.namespaces_message(Err(ClusterError::new("connection reset")));
    assert!(state.namespaces_stale());

    state.namespaces_message(Ok(Event::Init));
    assert!(state.namespaces_stale());
    state.namespaces_message(Ok(Event::InitDone));
    assert!(!state.namespaces_stale());
}

#[test]
fn an_object_watch_ending_marks_only_that_watch_stale() {
    let mut state = state();
    let pods = state.open_builtin_list(BuiltinKind::Pods).key;
    let deployments = state
        .watch_builtin(BuiltinKind::Deployments, Some("default"))
        .key;
    state.object_message(&pods, Ok(pod_event("web")));
    state.object_message(&deployments, Ok(pod_event("web")));

    state.object_ended(&pods);

    assert!(state.object_watch(&pods).unwrap().status().is_stale());
    assert!(
        !state
            .object_watch(&deployments)
            .unwrap()
            .status()
            .is_stale()
    );
    assert!(!state.namespaces_stale());
}

#[test]
fn an_ended_watch_nobody_started_is_ignored() {
    let mut state = state();

    state.object_ended(&pod_key("default"));

    assert!(state.object_watch(&pod_key("default")).is_none());
}

#[test]
fn a_forbidden_crd_watch_denies_access_and_a_snapshot_lifts_it() {
    let mut state = state();
    assert!(!state.crd_access_denied());

    state.crds_message(Err(ClusterError::forbidden("crds forbidden")));
    assert!(state.crd_access_denied());

    state.crds_message(Ok(Event::InitDone));
    assert!(!state.crd_access_denied());
}

#[test]
fn the_crd_watch_ending_marks_its_snapshot_stale() {
    let mut state = state();
    state.crds_message(Ok(Event::InitDone));

    state.crds_ended();

    assert!(state.crds().status().is_stale());
    assert!(!state.namespaces_stale());
}

#[test]
fn the_watch_key_names_the_cluster() {
    let state = state();

    assert_eq!(state.watch_key().cluster, ClusterId::named("kind-test"));
    assert_eq!(state.watch_key().kind, ResourceKind::Namespaces);
    assert_eq!(state.watch_key().namespace, None);
}

#[test]
fn the_cluster_is_the_one_the_state_was_built_for() {
    assert_eq!(state().cluster(), &ClusterId::named("kind-test"));
}

// Discovery

#[test]
fn connecting_starts_the_cluster_watches_and_runs_the_first_discovery() {
    let mut state = state();

    let commands = state.connected();

    assert!(matches!(
        commands[..],
        [
            Command::StartNamespacesWatch,
            Command::StartCrdsWatch,
            Command::RunDiscovery { .. }
        ]
    ));
}

#[test]
fn a_changed_definition_schedules_discovery() {
    let mut state = crontab_state("Namespaced", true);

    let commands = state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))));

    assert!(scheduled(&commands).is_some());
}

#[test]
fn a_deleted_definition_schedules_discovery() {
    let mut state = crontab_state("Namespaced", true);

    let commands = state.crds_message(Ok(Event::Delete(crontab_crd("Namespaced"))));

    assert!(scheduled(&commands).is_some());
}

#[test]
fn a_first_snapshot_with_an_unserved_kind_schedules_discovery() {
    // Connect-time Discovery raced the Definition list and missed the kind.
    let mut state = state();
    let run = run_of(&state.connected());
    state.discovery_finished(run, Ok(ServedResources::default()));

    let commands = list_crontab_crd(&mut state, "Namespaced");

    assert!(scheduled(&commands).is_some());
}

#[test]
fn a_first_snapshot_already_discovered_schedules_nothing() {
    let mut state = state();
    let run = run_of(&state.connected());
    state.discovery_finished(run, Ok(crontab_served("Namespaced")));

    let commands = list_crontab_crd(&mut state, "Namespaced");

    assert_eq!(scheduled(&commands), None);
}

#[test]
fn a_first_snapshot_before_any_discovery_schedules_nothing() {
    // Undiscovered is not "not served": the first run is still in flight.
    let mut state = state();
    state.connected();

    let commands = list_crontab_crd(&mut state, "Namespaced");

    assert_eq!(scheduled(&commands), None);
}

#[test]
fn the_debounce_runs_discovery_when_no_later_change_superseded_it() {
    let mut state = crontab_state("Namespaced", true);
    let generation =
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap();

    let commands = state.debounce_elapsed(generation);

    assert!(matches!(commands[..], [Command::RunDiscovery { .. }]));
}

#[test]
fn a_later_change_supersedes_the_earlier_debounce() {
    let mut state = crontab_state("Namespaced", true);
    let first =
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap();
    let second =
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap();

    assert!(state.debounce_elapsed(first).is_empty());
    assert!(!state.debounce_elapsed(second).is_empty());
}

#[test]
fn each_discovery_run_has_its_own_number() {
    let mut state = state();
    let first = run_of(&state.connected());
    let generation = scheduled(&list_crontab_crd(&mut state, "Namespaced")).unwrap_or_else(|| {
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap()
    });

    let second = run_of(&state.debounce_elapsed(generation));

    assert!(second > first);
}

#[test]
fn a_newer_runs_answer_wins_over_an_older_one() {
    let mut state = state();
    let older = run_of(&state.connected());
    list_crontab_crd(&mut state, "Namespaced");
    let generation =
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap();
    let newer = run_of(&state.debounce_elapsed(generation));

    state.discovery_finished(newer, Ok(crontab_served("Namespaced")));
    // The older run answers late, with a failure.
    state.discovery_finished(older, Err(ClusterError::new("late")));

    assert!(state.discovery_failure().is_none());
    assert!(matches!(
        state.catalog()[0].kinds[0].resource,
        KindResource::Served(_)
    ));
}

#[test]
fn an_older_runs_answer_is_dropped_while_a_newer_run_is_in_flight() {
    let mut state = state();
    let older = run_of(&state.connected());
    list_crontab_crd(&mut state, "Namespaced");
    let generation =
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap();
    state.debounce_elapsed(generation);

    state.discovery_finished(older, Ok(crontab_served("Namespaced")));

    assert!(matches!(
        state.catalog()[0].kinds[0].resource,
        KindResource::Undiscovered
    ));
}

#[test]
fn the_catalog_follows_the_definitions_before_discovery_catches_up() {
    let mut state = state();
    let run = run_of(&state.connected());
    state.discovery_finished(run, Ok(ServedResources::default()));

    list_crontab_crd(&mut state, "Namespaced");

    assert_eq!(state.catalog().len(), 1);
    assert_eq!(
        state.catalog()[0].kinds[0].resource,
        KindResource::NotServed
    );
}

#[test]
fn a_failed_discovery_keeps_the_kinds_served_behind_the_stale_warning() {
    let mut state = crontab_state("Namespaced", true);
    state.namespaces_message(Ok(Event::InitDone));
    assert!(!state.show_stale_warning());
    let generation =
        scheduled(&state.crds_message(Ok(Event::Apply(crontab_crd("Namespaced"))))).unwrap();
    let run = run_of(&state.debounce_elapsed(generation));

    state.discovery_finished(run, Err(ClusterError::new("connection refused")));

    assert!(state.show_stale_warning());
    assert!(state.discovery_failure().is_some());
    assert_eq!(state.health(), ConnectionHealth::Healthy);
    assert!(matches!(
        state.catalog()[0].kinds[0].resource,
        KindResource::Served(_)
    ));
}

#[test]
fn a_forbidden_definition_list_leaves_the_catalog_empty() {
    let mut state = state();

    let commands = state.crds_message(Err(ClusterError::forbidden("crds forbidden")));

    assert!(state.catalog().is_empty());
    assert!(state.crd_access_denied());
    assert_eq!(scheduled(&commands), None);
    assert!(!state.show_stale_warning());
}

#[test]
fn the_state_resolves_a_kind_in_its_own_catalog() {
    let state = crontab_state("Namespaced", true);

    assert_eq!(state.custom_kind(&crontab_id()).unwrap().plural, "crontabs");
    assert!(
        state
            .custom_kind(&CustomKindId {
                group: "other.io".to_string(),
                kind: "CronTab".to_string(),
            })
            .is_none()
    );
}

fn pod_target(name: &str) -> ForwardTarget {
    ForwardTarget::Pod(ObjectKey::new(Some("default".to_string()), name))
}

fn forward_request(name: &str, port: u16) -> ForwardRequest {
    ForwardRequest {
        target: pod_target(name),
        port,
    }
}

/// The `index`th forward to the Pod `name`.
fn nth_forward_to_pod(state: &SessionState, name: &str, index: usize) -> ActiveForward {
    state
        .forwards(&pod_target(name))
        .nth(index)
        .unwrap()
        .clone()
}

/// The requests of the forwards to `target`, in list order.
fn requests_to(state: &SessionState, target: &ForwardTarget) -> Vec<ForwardRequest> {
    state
        .forwards(target)
        .map(|forward| forward.request.clone())
        .collect()
}

fn is_start(command: &Command, request: &ForwardRequest) -> bool {
    matches!(command, Command::StartForward { request: r } if r == request)
}

fn is_stop(command: &Command, request: &ForwardRequest) -> bool {
    matches!(command, Command::StopForward { request: r } if r == request)
}

#[test]
fn a_new_forward_starts_and_is_listed_as_starting() {
    let mut state = state();
    let request = forward_request("web", 80);

    let commands = state.forward_port(request.clone());

    assert_eq!(commands.len(), 1);
    assert!(is_start(&commands[0], &request));
    let forward = nth_forward_to_pod(&state, "web", 0);
    assert_eq!(forward.state, ForwardState::Starting);
    assert_eq!(forward.last_error, None);
}

#[test]
fn forwarding_the_same_request_again_changes_nothing() {
    let mut state = state();
    let request = forward_request("web", 80);
    state.forward_port(request.clone());
    state.forward_reported(&request, ForwardEvent::Listening(8080));

    let commands = state.forward_port(request.clone());

    assert!(commands.is_empty());
    let forward = nth_forward_to_pod(&state, "web", 0);
    assert_eq!(forward.state, ForwardState::Listening(8080));
}

#[test]
fn a_listening_report_sets_the_local_port() {
    let mut state = state();
    let request = forward_request("web", 80);
    state.forward_port(request.clone());

    state.forward_reported(&request, ForwardEvent::Listening(8080));

    let forward = nth_forward_to_pod(&state, "web", 0);
    assert_eq!(forward.state, ForwardState::Listening(8080));
}

#[test]
fn a_connection_failure_keeps_listening_and_records_the_error() {
    let mut state = state();
    let request = forward_request("web", 80);
    state.forward_port(request.clone());
    state.forward_reported(&request, ForwardEvent::Listening(8080));

    state.forward_reported(
        &request,
        ForwardEvent::ConnectionFailed(ClusterError::new("refused")),
    );

    let forward = nth_forward_to_pod(&state, "web", 0);
    assert_eq!(forward.state, ForwardState::Listening(8080));
    assert_eq!(forward.last_error.as_deref(), Some("refused"));
}

#[test]
fn a_failed_report_fails_the_forward() {
    let mut state = state();
    let request = forward_request("web", 80);
    state.forward_port(request.clone());

    state.forward_reported(&request, ForwardEvent::Failed(ClusterError::new("gone")));

    let forward = nth_forward_to_pod(&state, "web", 0);
    assert_eq!(forward.state, ForwardState::Failed("gone".to_string()));
}

#[test]
fn a_failed_forward_starts_over_at_the_end_of_the_list() {
    let mut state = state();
    let first = forward_request("web", 80);
    let second = forward_request("web", 443);
    state.forward_port(first.clone());
    state.forward_port(second.clone());
    state.forward_reported(&first, ForwardEvent::Failed(ClusterError::new("gone")));

    let commands = state.forward_port(first.clone());

    assert_eq!(commands.len(), 2);
    assert!(is_stop(&commands[0], &first));
    assert!(is_start(&commands[1], &first));
    assert_eq!(requests_to(&state, &pod_target("web")), [second, first]);
    let restarted = nth_forward_to_pod(&state, "web", 1);
    assert_eq!(restarted.state, ForwardState::Starting);
    assert_eq!(restarted.last_error, None);
}

#[test]
fn stopping_a_forward_removes_it_and_drops_its_io() {
    let mut state = state();
    let kept = forward_request("web", 443);
    let stopped = forward_request("web", 80);
    state.forward_port(stopped.clone());
    state.forward_port(kept.clone());

    let commands = state.stop_forward(&stopped);

    assert_eq!(commands.len(), 1);
    assert!(is_stop(&commands[0], &stopped));
    assert_eq!(requests_to(&state, &pod_target("web")), [kept]);
}

#[test]
fn stopping_an_unknown_forward_changes_nothing() {
    let mut state = state();
    state.forward_port(forward_request("web", 80));

    let commands = state.stop_forward(&forward_request("web", 81));

    assert!(commands.is_empty());
    assert_eq!(requests_to(&state, &pod_target("web")).len(), 1);
}

#[test]
fn a_report_of_a_stopped_forward_changes_nothing() {
    let mut state = state();
    let request = forward_request("web", 80);
    state.forward_port(request.clone());
    state.stop_forward(&request);

    state.forward_reported(&request, ForwardEvent::Listening(8080));

    assert_eq!(requests_to(&state, &pod_target("web")), []);
}

#[test]
fn a_report_reaches_only_its_own_forward() {
    let mut state = state();
    let web = forward_request("web", 80);
    let db = forward_request("db", 80);
    state.forward_port(web.clone());
    state.forward_port(db.clone());

    state.forward_reported(&web, ForwardEvent::Listening(8080));

    let other = nth_forward_to_pod(&state, "db", 0);
    assert_eq!(other.state, ForwardState::Starting);
}

#[test]
fn the_list_holds_only_the_forwards_of_its_target() {
    let mut state = state();
    state.forward_port(forward_request("web", 80));
    let service = ForwardTarget::Service(ObjectKey::new(Some("default".to_string()), "web"));
    let on_service = ForwardRequest {
        target: service.clone(),
        port: 80,
    };
    state.forward_port(on_service.clone());

    assert_eq!(requests_to(&state, &service), [on_service]);
    assert_eq!(requests_to(&state, &pod_target("web")).len(), 1);
    assert_eq!(requests_to(&state, &pod_target("db")), []);
}

#[test]
fn the_served_resources_are_what_discovery_last_found() {
    let discovered = crontab_state("Namespaced", true);
    let undiscovered = crontab_state("Namespaced", false);

    let found = discovered.served_resources();
    assert!(
        found
            .resolve("stable.example.com", "v1", "CronTab")
            .is_some()
    );
    assert!(
        undiscovered
            .served_resources()
            .resolve("stable.example.com", "v1", "CronTab")
            .is_none()
    );
}

#[test]
fn reconnecting_starts_every_resident_watch_and_discovery_again() {
    let mut state = crontab_state("Namespaced", true);
    state.open_builtin_list(BuiltinKind::Pods);
    state.open_custom_list(&crontab_id()).unwrap();

    let commands = state.reconnect();

    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Command::StartNamespacesWatch))
    );
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Command::StartCrdsWatch))
    );
    let keys = started(&commands);
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&pod_key("default")));
    assert!(
        keys.iter()
            .any(|key| matches!(key.kind, ResourceKind::Custom { .. }))
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, Command::RunDiscovery { .. }))
            .count(),
        1
    );
}

#[test]
fn a_reconnect_discovery_supersedes_the_one_in_flight() {
    let mut state = state();
    let first = run_of(&state.connected());

    let second = run_of(&state.reconnect());

    assert_ne!(first, second);
    assert!(
        state
            .discovery_finished(first, Ok(ServedResources::default()))
            .is_empty()
    );
    assert!(state.discovery_failure().is_none());
}

#[test]
fn a_reconnect_restarts_a_watch_with_the_resource_it_started_with() {
    let mut state = state();
    let started_with = state
        .watch_builtin(BuiltinKind::Pods, Some("default"))
        .commands;
    let Command::StartWatch { resource, .. } = &started_with[0] else {
        panic!("expected a StartWatch");
    };

    let commands = state.reconnect();

    let restarted = commands.iter().find_map(|command| match command {
        Command::StartWatch { resource, .. } => Some(resource),
        _ => None,
    });
    assert_eq!(restarted, Some(resource));
}

fn crontab(namespace: &str, name: &str) -> Event<DynamicObject> {
    Event::Apply(object(serde_json::json!({
        "apiVersion": "stable.example.com/v1",
        "kind": "CronTab",
        "metadata": {"name": name, "namespace": namespace},
    })))
}

#[test]
fn the_viewed_custom_list_reads_the_watch_of_the_selected_namespace() {
    let mut state = crontab_state("Namespaced", true);
    let key = state.open_custom_list(&crontab_id()).unwrap().key;
    state.object_message(&key, Ok(crontab("default", "nightly")));
    let kind = state.custom_kind(&crontab_id()).unwrap().clone();

    let watch = state.custom_watch(&kind).unwrap();
    assert!(watch.get(Some("default"), "nightly").is_some());

    state.set_namespace("team-a".to_string());
    let watch = state.custom_watch(&kind).unwrap();
    assert!(watch.get(Some("default"), "nightly").is_none());
}

#[test]
fn a_custom_detail_resolves_from_its_namespaces_watch() {
    use astrolabe_model::detail::{DetailTarget, resolve};
    let mut state = crontab_state("Namespaced", true);
    let key = state.open_custom_list(&crontab_id()).unwrap().key;
    state.object_message(&key, Ok(crontab("default", "nightly")));
    let target = |namespace: &str| DetailTarget::Custom {
        id: crontab_id(),
        key: ObjectKey::new(Some(namespace.to_string()), "nightly"),
    };
    let now = "2026-10-22T12:00:00Z".parse().unwrap();

    assert!(resolve(&target("default"), &state, now).is_some());
    assert!(resolve(&target("team-b"), &state, now).is_none());
}

#[test]
fn the_viewed_list_is_readable_for_every_list() {
    use astrolabe_model::session_state::Viewing;
    let mut state = crontab_state("Namespaced", true);
    assert_eq!(state.viewing(), &Viewing::Namespaces);

    state.open_builtin_list(BuiltinKind::Pods);
    assert_eq!(state.viewing(), &Viewing::Builtin(BuiltinKind::Pods));

    state.open_definitions_list();
    assert_eq!(state.viewing(), &Viewing::Definitions);

    state.open_custom_list(&crontab_id()).unwrap();
    assert_eq!(state.viewing(), &Viewing::Custom(crontab_id()));

    state.open_namespaces_list();
    assert_eq!(state.viewing(), &Viewing::Namespaces);
}

/// A `CronTab` Definition whose storage version is `storage` of v1 and v2.
fn crontab_crd_stored_at(storage: &str) -> CustomResourceDefinition {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": "crontabs.stable.example.com"},
        "spec": {
            "group": "stable.example.com",
            "scope": "Namespaced",
            "names": {"kind": "CronTab", "plural": "crontabs"},
            "versions": [
                {"name": "v1", "served": true, "storage": storage == "v1"},
                {"name": "v2", "served": true, "storage": storage == "v2"},
            ]
        }
    }))
    .unwrap()
}

fn versions_served() -> ServedResources {
    ServedResources::new([
        served("stable.example.com", "v1", "CronTab", true),
        served("stable.example.com", "v2", "CronTab", true),
    ])
}

fn crontab_detail(namespace: &str) -> astrolabe_model::detail::DetailTarget {
    astrolabe_model::detail::DetailTarget::Custom {
        id: crontab_id(),
        key: ObjectKey::new(Some(namespace.to_string()), "nightly"),
    }
}

/// A state whose `CronTab` is stored at v1, with Discovery serving both.
fn versioned_state() -> SessionState {
    let mut state = state();
    let run = run_of(&state.connected());
    state.crds_message(Ok(Event::Init));
    state.crds_message(Ok(Event::InitApply(crontab_crd_stored_at("v1"))));
    state.crds_message(Ok(Event::InitDone));
    state.discovery_finished(run, Ok(versions_served()));
    state
}

#[test]
fn a_preferred_version_change_moves_open_custom_details_to_the_new_version() {
    let mut state = versioned_state();
    let commands = state.open_detail(crontab_detail("team-a"));
    let old = started(&commands);
    assert_eq!(old.len(), 1);
    assert!(matches!(&old[0].kind, ResourceKind::Custom { version, .. } if version == "v1"));

    let commands = state.crds_message(Ok(Event::Apply(crontab_crd_stored_at("v2"))));

    let moved = started(&commands);
    assert_eq!(moved.len(), 1);
    assert!(matches!(&moved[0].kind, ResourceKind::Custom { version, .. } if version == "v2"));
    assert_eq!(moved[0].namespace.as_deref(), Some("team-a"));
}

#[test]
fn a_closed_custom_detail_is_not_followed() {
    let mut state = versioned_state();
    let target = crontab_detail("team-a");
    state.open_detail(target.clone());
    state.close_detail(&target);

    let commands = state.crds_message(Ok(Event::Apply(crontab_crd_stored_at("v2"))));

    assert_eq!(started(&commands), []);
}

#[test]
fn a_detail_open_twice_is_followed_until_both_close() {
    let mut state = versioned_state();
    let target = crontab_detail("team-a");
    state.open_detail(target.clone());
    state.open_detail(target.clone());
    state.close_detail(&target);

    let commands = state.crds_message(Ok(Event::Apply(crontab_crd_stored_at("v2"))));

    let moved = started(&commands);
    assert_eq!(moved.len(), 1);
    assert!(matches!(&moved[0].kind, ResourceKind::Custom { version, .. } if version == "v2"));
}

#[test]
fn a_custom_detail_of_a_kind_not_yet_served_starts_when_it_is() {
    let mut state = state();
    let run = run_of(&state.connected());
    list_crontab_crd(&mut state, "Namespaced");
    assert_eq!(started(&state.open_detail(crontab_detail("team-a"))), []);

    let commands = state.discovery_finished(run, Ok(crontab_served("Namespaced")));

    assert_eq!(started(&commands).len(), 1);
}

#[test]
fn a_builtin_detail_starts_nothing_when_the_catalog_changes() {
    use astrolabe_model::detail::DetailTarget;
    let mut state = state();
    let commands = state.open_detail(DetailTarget::Namespace {
        name: "default".to_string(),
    });
    assert_eq!(started(&commands), []);

    let commands = list_crontab_crd(&mut state, "Namespaced");

    assert_eq!(started(&commands), []);
}
