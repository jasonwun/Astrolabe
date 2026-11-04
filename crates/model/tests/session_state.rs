use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::catalog::{CustomGroup, catalog};
use astrolabe_model::session::{
    ConnectionHealth, CustomKindId, ResourceKind, SnapshotChange, WatchKey,
};
use astrolabe_model::session_state::{Command, SessionState, custom_kind};
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
        .map(|Command::StartWatch { key, .. }| key.clone())
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

/// `CronTab` in the catalog, served unless `discovered` is false.
fn crontab_catalog(scope: &str, discovered: bool) -> Vec<CustomGroup> {
    let crd = crontab_crd(scope);
    let resources = ServedResources::new([served(
        "stable.example.com",
        "v1",
        "CronTab",
        scope == "Namespaced",
    )]);
    catalog(&[&crd], discovered.then_some(&resources))
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
    let Command::StartWatch { resource, .. } = &ensured.commands[0];
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

    let ensured = state.watch_builtin(BuiltinKind::Pods, "team-a");

    assert_eq!(started(&ensured.commands), [pod_key("team-a")]);
    assert_eq!(state.namespace(), "default");
}

#[test]
fn changing_the_namespace_moves_the_viewed_list_to_it() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);

    let commands = state.set_namespace("team-a".to_string(), &[]);

    assert_eq!(started(&commands), [pod_key("team-a")]);
    assert_eq!(state.namespace(), "team-a");
}

#[test]
fn changing_back_to_a_visited_namespace_starts_nothing() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.set_namespace("team-a".to_string(), &[]);

    let commands = state.set_namespace("default".to_string(), &[]);

    assert!(commands.is_empty());
}

#[test]
fn selecting_the_selected_namespace_starts_nothing() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.close_list();

    assert!(state.set_namespace("default".to_string(), &[]).is_empty());
}

#[test]
fn changing_the_namespace_with_no_list_viewed_starts_nothing() {
    let mut state = state();

    let commands = state.set_namespace("team-a".to_string(), &[]);

    assert!(commands.is_empty());
    assert_eq!(state.namespace(), "team-a");
}

#[test]
fn a_closed_list_no_longer_follows_the_namespace() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.close_list();

    assert!(state.set_namespace("team-a".to_string(), &[]).is_empty());
}

#[test]
fn the_last_opened_list_is_the_one_that_follows() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.open_builtin_list(BuiltinKind::Deployments);

    let commands = state.set_namespace("team-a".to_string(), &[]);

    let kinds: Vec<_> = started(&commands).into_iter().map(|key| key.kind).collect();
    assert_eq!(kinds, [ResourceKind::Builtin(BuiltinKind::Deployments)]);
}

#[test]
fn a_namespaced_custom_list_follows_the_namespace() {
    let mut state = state();
    let catalog = crontab_catalog("Namespaced", true);
    let ensured = state.open_custom_list(&crontab_id(), &catalog).unwrap();
    assert_eq!(ensured.key.namespace.as_deref(), Some("default"));
    assert_eq!(ensured.commands.len(), 1);

    let commands = state.set_namespace("team-a".to_string(), &catalog);

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
    let mut state = state();
    let catalog = crontab_catalog("Cluster", true);
    let ensured = state.open_custom_list(&crontab_id(), &catalog).unwrap();
    assert_eq!(ensured.key.namespace, None);

    let commands = state.set_namespace("team-a".to_string(), &catalog);

    assert!(commands.is_empty());
}

#[test]
fn a_custom_list_of_an_unknown_kind_starts_nothing() {
    let mut state = state();

    assert!(state.open_custom_list(&crontab_id(), &[]).is_none());
}

#[test]
fn a_custom_list_that_is_not_served_starts_nothing() {
    let mut state = state();
    let catalog = crontab_catalog("Namespaced", false);

    assert!(state.open_custom_list(&crontab_id(), &catalog).is_none());
    assert!(
        state
            .watch_custom_object(&crontab_id(), Some("default"), &catalog)
            .is_none()
    );
}

#[test]
fn a_viewed_custom_list_starts_its_watch_once_the_kind_is_served() {
    let mut state = state();
    assert!(
        state
            .open_custom_list(&crontab_id(), &crontab_catalog("Namespaced", false))
            .is_none()
    );

    let commands = state.catalog_changed(&crontab_catalog("Namespaced", true));

    assert_eq!(commands.len(), 1);
    assert!(
        state
            .catalog_changed(&crontab_catalog("Namespaced", true))
            .is_empty()
    );
}

#[test]
fn a_catalog_change_leaves_a_builtin_list_alone() {
    let mut state = state();
    state.open_builtin_list(BuiltinKind::Pods);
    state.close_list();
    state.open_builtin_list(BuiltinKind::Deployments);
    state.set_namespace("team-a".to_string(), &[]);

    assert!(
        state
            .catalog_changed(&crontab_catalog("Namespaced", true))
            .is_empty()
    );
}

#[test]
fn a_custom_object_watch_ignores_the_namespace_selection() {
    let mut state = state();
    let catalog = crontab_catalog("Namespaced", true);

    let ensured = state
        .watch_custom_object(&crontab_id(), Some("team-b"), &catalog)
        .unwrap();

    assert_eq!(ensured.key.namespace.as_deref(), Some("team-b"));
    assert_eq!(state.namespace(), "default");
}

#[test]
fn custom_kind_finds_a_kind_by_group_and_name() {
    let catalog = crontab_catalog("Namespaced", true);

    assert_eq!(
        custom_kind(&catalog, &crontab_id()).unwrap().plural,
        "crontabs"
    );
    let other_group = CustomKindId {
        group: "other.io".to_string(),
        kind: "CronTab".to_string(),
    };
    assert!(custom_kind(&catalog, &other_group).is_none());
    let other_kind = CustomKindId {
        group: "stable.example.com".to_string(),
        kind: "Backup".to_string(),
    };
    assert!(custom_kind(&catalog, &other_kind).is_none());
}

#[test]
fn object_messages_fill_the_watch_they_are_keyed_to() {
    let mut state = state();
    let key = state.open_builtin_list(BuiltinKind::Pods).key;

    let change = state.object_message(&key, Ok(pod_event("web")));

    assert_eq!(change, SnapshotChange::Changed);
    assert!(
        state
            .builtin_object(BuiltinKind::Pods, "default", "web")
            .is_some()
    );
    assert!(
        state
            .builtin_object(BuiltinKind::Pods, "default", "db")
            .is_none()
    );
}

#[test]
fn a_message_for_a_watch_nobody_started_changes_nothing() {
    let mut state = state();

    let change = state.object_message(&pod_key("default"), Ok(pod_event("web")));

    assert_eq!(change, SnapshotChange::Unchanged);
    assert!(state.builtin_watch(BuiltinKind::Pods, "default").is_none());
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
    let deployments = state.watch_builtin(BuiltinKind::Deployments, "default").key;
    state.object_message(&pods, Ok(pod_event("web")));
    state.object_message(&deployments, Ok(pod_event("web")));

    state.object_ended(&pods);

    assert!(state.object_watch(&pods).unwrap().show_stale_warning());
    assert!(
        !state
            .object_watch(&deployments)
            .unwrap()
            .show_stale_warning()
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

    assert!(state.crds().show_stale_warning());
    assert!(!state.namespaces_stale());
}

#[test]
fn the_watch_key_names_the_cluster() {
    let state = state();

    assert_eq!(state.watch_key().cluster, ClusterId::named("kind-test"));
    assert_eq!(state.watch_key().kind, ResourceKind::Namespaces);
    assert_eq!(state.watch_key().namespace, None);
}
