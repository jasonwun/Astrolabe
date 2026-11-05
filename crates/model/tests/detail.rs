//! Detail resolution over a `SessionState` built by hand (#97, #167): the
//! watches a detail tab needs, the Related section's rule and the object a
//! tab's operations address, with no Session entity, window or clicks.
//! Synthetic objects (plausible shapes, never captured from a real
//! apiserver).

use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::k8s_openapi::api::core::v1::ObjectReference;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_kube::object::{ObjectKey, ObjectRef};
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::detail::{
    DetailOutcome, DetailTarget, NeededWatch, ReferenceError, RelatedOutcome, Resolved,
    failed_message, resolve, watches,
};
use astrolabe_model::session_state::SessionState;
use support::object;

mod support;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn target(kind: BuiltinKind, name: &str) -> DetailTarget {
    DetailTarget::Builtin {
        kind,
        key: ObjectKey::new(Some("default".to_string()), name),
    }
}

/// A state with `target`'s needed watches started, as the detail panel
/// starts them. `target`'s kind must have a Related section.
fn state(target: &DetailTarget) -> SessionState {
    let mut state = SessionState::new(ClusterId::named("kind-a"), "default");
    for watch in watches(target) {
        state.watch_builtin(watch.kind, watch.namespace.as_deref());
    }
    state
}

/// Feeds `objects` to the watch of `kind` in `default` as its snapshot.
fn snapshot(state: &mut SessionState, kind: BuiltinKind, objects: Vec<DynamicObject>) {
    let key = state.watch_builtin(kind, Some("default")).key;
    state.object_message(&key, Ok(Event::Init));
    for object in objects {
        state.object_message(&key, Ok(Event::InitApply(object)));
    }
    state.object_message(&key, Ok(Event::InitDone));
}

fn forbid(state: &mut SessionState, kind: BuiltinKind, message: &str) {
    let key = state.watch_builtin(kind, Some("default")).key;
    state.object_message(&key, Err(ClusterError::forbidden(message)));
}

fn resolved(target: &DetailTarget, state: &SessionState) -> Resolved {
    resolve(target, state, now()).shown().unwrap()
}

fn web_service() -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-svc"},
        "spec": {"selector": {"app": "web"},
                 "ports": [{"name": "http", "port": 80, "protocol": "TCP"}]}
    }))
}

fn endpoint_slice(name: &str, owner_uid: &str) -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "discovery.k8s.io/v1", "kind": "EndpointSlice",
        "metadata": {"name": name, "namespace": "default", "uid": format!("uid-{name}"),
                     "ownerReferences": [{"apiVersion": "v1", "kind": "Service", "name": "web",
                                          "uid": owner_uid, "controller": true}]},
        "addressType": "IPv4",
        "endpoints": [{"addresses": ["10.0.0.1"]}]
    }))
}

fn web_deployment() -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-deploy"},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "web"}},
                 "template": {"metadata": {"labels": {"app": "web"}},
                              "spec": {"containers": [{"name": "app", "image": "nginx"}]}}}
    }))
}

fn pod(name: &str, app: &str) -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": "default", "uid": format!("uid-{name}"),
                     "labels": {"app": app}},
        "spec": {"containers": [{"name": "app", "image": "nginx"}]}
    }))
}

#[test]
fn a_service_detail_needs_its_own_and_its_endpoint_slices_watch() {
    let target = DetailTarget::Builtin {
        kind: BuiltinKind::Services,
        key: ObjectKey::new(Some("team-a".to_string()), "web"),
    };
    assert_eq!(
        watches(&target),
        [
            NeededWatch {
                kind: BuiltinKind::Services,
                namespace: Some("team-a".to_string()),
            },
            NeededWatch {
                kind: BuiltinKind::EndpointSlices,
                namespace: Some("team-a".to_string()),
            },
        ]
    );
}

#[test]
fn a_forbidden_related_watch_says_so() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![web_deployment()]);
    forbid(&mut state, BuiltinKind::Pods, "pods is forbidden");

    let related = resolved(&target, &state).related.unwrap();
    assert_eq!(related.title, "Pods");
    assert_eq!(
        related.outcome,
        RelatedOutcome::Forbidden("No permission to list pods here".to_string())
    );
}

#[test]
fn a_hiding_section_with_nothing_selected_is_hidden() {
    let target = target(BuiltinKind::Services, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Services, vec![web_service()]);
    snapshot(
        &mut state,
        BuiltinKind::EndpointSlices,
        vec![endpoint_slice("api-xyz", "uid-api")],
    );

    assert!(resolved(&target, &state).related.is_none());
}

#[test]
fn a_hiding_section_still_says_its_watch_is_forbidden() {
    let target = target(BuiltinKind::Services, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Services, vec![web_service()]);
    forbid(
        &mut state,
        BuiltinKind::EndpointSlices,
        "endpointslices is forbidden",
    );

    let related = resolved(&target, &state).related.unwrap();
    assert_eq!(related.title, "Endpoint Slices");
    assert_eq!(
        related.outcome,
        RelatedOutcome::Forbidden("No permission to list endpoint slices here".to_string())
    );
}

#[test]
fn a_section_with_nothing_selected_says_none() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![web_deployment()]);
    snapshot(&mut state, BuiltinKind::Pods, vec![pod("api-1", "api")]);

    let related = resolved(&target, &state).related.unwrap();
    assert_eq!(related.title, "Pods");
    assert_eq!(
        related.outcome,
        RelatedOutcome::Empty("No pods".to_string())
    );
}

#[test]
fn a_section_lists_only_the_selected_objects() {
    let target = target(BuiltinKind::Services, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Services, vec![web_service()]);
    snapshot(
        &mut state,
        BuiltinKind::EndpointSlices,
        vec![
            endpoint_slice("web-abc", "uid-svc"),
            endpoint_slice("api-xyz", "uid-api"),
        ],
    );

    let related = resolved(&target, &state).related.unwrap();
    assert_eq!(related.title, "Endpoint Slices");
    let RelatedOutcome::Rows(table) = related.outcome else {
        panic!("expected rows, got {:?}", related.outcome);
    };
    let keys: Vec<String> = table.rows.iter().map(|row| row.key.to_string()).collect();
    assert_eq!(keys, ["default/web-abc"]);
}

#[test]
fn an_involved_definition_opens_as_a_definition() {
    let state = SessionState::new(ClusterId::named("kind-a"), "default");
    let reference = ObjectReference {
        api_version: Some("apiextensions.k8s.io/v1".to_string()),
        kind: Some("CustomResourceDefinition".to_string()),
        name: Some("widgets.example.com".to_string()),
        ..ObjectReference::default()
    };
    assert_eq!(
        DetailTarget::by_reference(reference, None, &state),
        Ok(DetailTarget::Definition {
            name: "widgets.example.com".to_string()
        })
    );
}

#[test]
fn an_involved_object_without_a_name_is_a_missing_name_error() {
    let state = SessionState::new(ClusterId::named("kind-a"), "default");
    let reference = ObjectReference {
        kind: Some("Pod".to_string()),
        ..ObjectReference::default()
    };
    assert_eq!(
        DetailTarget::by_reference(reference, None, &state),
        Err(ReferenceError::MissingName {
            kind: "Pod".to_string()
        })
    );
}

#[test]
fn an_involved_object_of_an_unregistered_kind_is_an_unregistered_kind_error() {
    let state = SessionState::new(ClusterId::named("kind-a"), "default");
    let reference = ObjectReference {
        api_version: Some("example.com/v1".to_string()),
        kind: Some("Widget".to_string()),
        name: Some("w".to_string()),
        ..ObjectReference::default()
    };
    assert_eq!(
        DetailTarget::by_reference(reference, None, &state),
        Err(ReferenceError::UnregisteredKind {
            kind: "Widget".to_string(),
            name: "w".to_string()
        })
    );
}

#[test]
fn reference_errors_are_send_sync_errors() {
    fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
    assert_error::<ReferenceError>();
}

#[test]
fn an_operation_addresses_the_shown_object_until_it_leaves_its_watch() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![web_deployment()]);
    assert_eq!(
        target.object(&state),
        Some(ObjectRef {
            resource: ApiResource::erase::<Deployment>(&()),
            key: ObjectKey::new(Some("default".to_string()), "web"),
        })
    );

    // Gone from the watch: Delete, Scale and the rest send nothing.
    snapshot(&mut state, BuiltinKind::Deployments, vec![]);
    assert_eq!(target.object(&state), None);
}

#[test]
fn reference_errors_display_the_involved_object() {
    assert_eq!(
        ReferenceError::MissingName {
            kind: "Pod".to_string()
        }
        .to_string(),
        "Cannot open involved object Pod/: name is missing"
    );
    assert_eq!(
        ReferenceError::UnregisteredKind {
            kind: "Widget".to_string(),
            name: "w".to_string()
        }
        .to_string(),
        "Cannot open involved object Widget/w: kind is not registered"
    );
}

#[test]
fn a_cluster_scoped_builtin_is_watched_and_found_without_a_namespace() {
    // No cluster-scoped built-in kind is registered yet: Pods stands in as a
    // kind without a Related watch, fed an object that has no namespace.
    let target = DetailTarget::Builtin {
        kind: BuiltinKind::Pods,
        key: ObjectKey::cluster_scoped("node-a"),
    };
    let needed = watches(&target);
    assert_eq!(needed[0].kind, BuiltinKind::Pods);
    assert_eq!(needed[0].namespace, None);
    let mut state = SessionState::new(ClusterId::named("kind-a"), "default");
    for watch in &needed {
        state.watch_builtin(watch.kind, watch.namespace.as_deref());
    }
    let key = state.watch_builtin(BuiltinKind::Pods, None).key;
    state.object_message(&key, Ok(Event::Init));
    state.object_message(
        &key,
        Ok(Event::InitApply(object(serde_json::json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "node-a", "uid": "uid-node-a"},
            "spec": {"containers": [{"name": "app", "image": "nginx"}]}
        })))),
    );
    state.object_message(&key, Ok(Event::InitDone));
    assert!(state.builtin_watch(BuiltinKind::Pods, None).is_some());
    assert!(
        state
            .builtin_object(BuiltinKind::Pods, None, "node-a")
            .is_some()
    );
    assert_eq!(target.object(&state).unwrap().key.name, "node-a");
    assert_eq!(resolved(&target, &state).object.key.namespace, None);
}

fn outcome(target: &DetailTarget, state: &SessionState) -> DetailOutcome {
    resolve(target, state, now())
}

#[test]
fn a_builtin_detail_is_loading_before_its_watchs_first_snapshot() {
    let target = target(BuiltinKind::Deployments, "web");
    let state = state(&target);

    assert!(matches!(outcome(&target, &state), DetailOutcome::Loading));
}

#[test]
fn a_builtin_detail_with_no_watch_is_loading() {
    let target = target(BuiltinKind::Deployments, "web");
    let state = SessionState::new(ClusterId::named("kind-a"), "default");

    assert!(matches!(outcome(&target, &state), DetailOutcome::Loading));
}

#[test]
fn a_builtin_detail_whose_watch_failed_before_a_snapshot_carries_the_error() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    let key = state
        .watch_builtin(BuiltinKind::Deployments, Some("default"))
        .key;
    state.object_message(&key, Err(ClusterError::unreachable("refused")));

    let DetailOutcome::Failed(Some(error)) = outcome(&target, &state) else {
        panic!("expected Failed with an error");
    };
    assert!(error.to_string().contains("refused"));
}

#[test]
fn a_builtin_detail_whose_watch_is_denied_is_forbidden() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    forbid(
        &mut state,
        BuiltinKind::Deployments,
        "deployments is forbidden",
    );

    assert!(matches!(outcome(&target, &state), DetailOutcome::Forbidden));
}

#[test]
fn a_builtin_detail_absent_from_its_snapshot_is_gone() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![]);

    assert!(matches!(outcome(&target, &state), DetailOutcome::Gone));
}

#[test]
fn a_builtin_detail_absent_from_a_stale_snapshot_is_gone() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![]);
    let key = state
        .watch_builtin(BuiltinKind::Deployments, Some("default"))
        .key;
    state.object_ended(&key);

    assert!(matches!(outcome(&target, &state), DetailOutcome::Gone));
}

#[test]
fn a_projection_that_cannot_parse_the_object_is_unreadable_not_gone() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    let broken = object(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-deploy"},
        "spec": "not an object"
    }));
    snapshot(&mut state, BuiltinKind::Deployments, vec![broken]);

    assert!(matches!(
        outcome(&target, &state),
        DetailOutcome::Unreadable
    ));
}

#[test]
fn a_related_section_is_loading_while_its_watch_has_no_snapshot() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![web_deployment()]);

    let related = resolved(&target, &state).related.unwrap();
    assert_eq!(
        related.outcome,
        RelatedOutcome::Loading("Loading pods…".to_string())
    );
}

#[test]
fn a_related_section_whose_watch_failed_before_a_snapshot_is_failed() {
    let target = target(BuiltinKind::Deployments, "web");
    let mut state = state(&target);
    snapshot(&mut state, BuiltinKind::Deployments, vec![web_deployment()]);
    let key = state.watch_builtin(BuiltinKind::Pods, Some("default")).key;
    state.object_message(&key, Err(ClusterError::unreachable("refused")));

    let related = resolved(&target, &state).related.unwrap();
    let RelatedOutcome::Failed(Some(error)) = related.outcome else {
        panic!("expected Failed with an error");
    };
    assert!(error.to_string().contains("refused"));
}

#[test]
fn failed_messages_name_the_kind_of_failure() {
    assert_eq!(
        failed_message(Some(&ClusterError::unreachable("refused"))),
        format!(
            "Cannot reach the cluster: {}",
            ClusterError::unreachable("refused")
        )
    );
    assert!(failed_message(Some(&ClusterError::connect("x"))).starts_with("Cannot connect"));
    assert!(failed_message(Some(&ClusterError::new("boom"))).starts_with("Cannot load the object"));
    assert_eq!(
        failed_message(None),
        "The watch ended before the object loaded"
    );
}

#[test]
fn only_gone_says_no_longer_exists() {
    let texts = [
        DetailOutcome::Loading,
        DetailOutcome::Failed(None),
        DetailOutcome::Forbidden,
        DetailOutcome::Undefined,
        DetailOutcome::NotServed,
        DetailOutcome::Unreadable,
    ]
    .map(|outcome| outcome.notice("Pod web").unwrap().1);
    assert!(texts.iter().all(|text| !text.contains("no longer exists")));
    assert_eq!(
        DetailOutcome::Gone.notice("Pod web"),
        Some(("detail-gone", "Pod web no longer exists".to_string()))
    );
}
