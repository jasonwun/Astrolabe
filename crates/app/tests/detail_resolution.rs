//! Detail resolution over a `Session` (#97): the watches a detail tab needs
//! and the Related section's rule, with no view and no clicks. Synthetic
//! objects (plausible shapes, never captured from a real apiserver).

mod support;

use astrolabe::builtin::BuiltinKind;
use astrolabe::detail::{DetailTarget, NeededWatch, RelatedOutcome, Resolved, resolve, watches};
use astrolabe::session::{ScriptedCluster, Session, WatchError};
use astrolabe::store::ObjectKey;
use astrolabe_kube::k8s_openapi::api::core::v1::ObjectReference;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use futures::channel::mpsc;
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use support::{Sender, key, object, snapshot};

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn target(kind: BuiltinKind, name: &str) -> DetailTarget {
    DetailTarget::Builtin {
        kind,
        key: ObjectKey::new(Some("default".to_string()), name),
    }
}

/// A session on a scripted cluster with `target`'s needed watches started,
/// as the detail panel starts them, and the senders of those watches: the
/// shown kind's, then the Related kind's. `target`'s kind must have a
/// Related section.
fn session(
    target: &DetailTarget,
    cx: &mut TestAppContext,
) -> (
    Entity<Session>,
    Sender<DynamicObject>,
    Sender<DynamicObject>,
) {
    let needed = watches(target);
    let [shown_watch, related_watch] = needed.as_slice() else {
        panic!("expected a shown and a related watch, got {needed:?}");
    };
    let (_namespaces, namespace_events) = mpsc::channel(32);
    let (shown, shown_events) = mpsc::channel(32);
    let (related, related_events) = mpsc::channel(32);
    let cluster = ScriptedCluster::new(namespace_events)
        .watch(key(shown_watch.kind, &shown_watch.namespace), shown_events)
        .watch(
            key(related_watch.kind, &related_watch.namespace),
            related_events,
        );
    let session = cx.update(|cx| cx.new(|cx| Session::scripted("kind-a", "default", cluster, cx)));
    session.update(cx, |session, cx| {
        for watch in &needed {
            session.watch_builtin(watch.kind, &watch.namespace, cx);
        }
    });
    (session, shown, related)
}

fn resolved(target: &DetailTarget, session: &Entity<Session>, cx: &mut TestAppContext) -> Resolved {
    cx.run_until_parked();
    cx.read(|cx| resolve(target, session.read(cx), now()))
        .unwrap()
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
                namespace: "team-a".to_string(),
            },
            NeededWatch {
                kind: BuiltinKind::EndpointSlices,
                namespace: "team-a".to_string(),
            },
        ]
    );
}

#[gpui_kit::test]
fn a_forbidden_related_watch_says_so(cx: &mut TestAppContext) {
    let target = target(BuiltinKind::Deployments, "web");
    let (session, mut deployments, mut pods) = session(&target, cx);
    snapshot(&mut deployments, vec![web_deployment()]);
    pods.try_send(Err(WatchError::forbidden("pods is forbidden")))
        .unwrap();

    let related = resolved(&target, &session, cx).related.unwrap();
    assert_eq!(related.title, "Pods");
    assert_eq!(
        related.outcome,
        RelatedOutcome::Forbidden("No permission to list pods here".to_string())
    );
}

#[gpui_kit::test]
fn a_hiding_section_with_nothing_selected_is_hidden(cx: &mut TestAppContext) {
    let target = target(BuiltinKind::Services, "web");
    let (session, mut services, mut slices) = session(&target, cx);
    snapshot(&mut services, vec![web_service()]);
    snapshot(&mut slices, vec![endpoint_slice("api-xyz", "uid-api")]);

    assert!(resolved(&target, &session, cx).related.is_none());
}

#[gpui_kit::test]
fn a_hiding_section_still_says_its_watch_is_forbidden(cx: &mut TestAppContext) {
    let target = target(BuiltinKind::Services, "web");
    let (session, mut services, mut slices) = session(&target, cx);
    snapshot(&mut services, vec![web_service()]);
    slices
        .try_send(Err(WatchError::forbidden("endpointslices is forbidden")))
        .unwrap();

    let related = resolved(&target, &session, cx).related.unwrap();
    assert_eq!(related.title, "Endpoint Slices");
    assert_eq!(
        related.outcome,
        RelatedOutcome::Forbidden("No permission to list endpoint slices here".to_string())
    );
}

#[gpui_kit::test]
fn a_section_with_nothing_selected_says_none(cx: &mut TestAppContext) {
    let target = target(BuiltinKind::Deployments, "web");
    let (session, mut deployments, mut pods) = session(&target, cx);
    snapshot(&mut deployments, vec![web_deployment()]);
    snapshot(&mut pods, vec![pod("api-1", "api")]);

    let related = resolved(&target, &session, cx).related.unwrap();
    assert_eq!(related.title, "Pods");
    assert_eq!(
        related.outcome,
        RelatedOutcome::Empty("No pods".to_string())
    );
}

#[gpui_kit::test]
fn a_section_lists_only_the_selected_objects(cx: &mut TestAppContext) {
    let target = target(BuiltinKind::Services, "web");
    let (session, mut services, mut slices) = session(&target, cx);
    snapshot(&mut services, vec![web_service()]);
    snapshot(
        &mut slices,
        vec![
            endpoint_slice("web-abc", "uid-svc"),
            endpoint_slice("api-xyz", "uid-api"),
        ],
    );

    let related = resolved(&target, &session, cx).related.unwrap();
    assert_eq!(related.title, "Endpoint Slices");
    let RelatedOutcome::Rows(table) = related.outcome else {
        panic!("expected rows, got {:?}", related.outcome);
    };
    let keys: Vec<String> = table.rows.iter().map(|row| row.key.to_string()).collect();
    assert_eq!(keys, ["default/web-abc"]);
}

#[gpui_kit::test]
fn an_involved_definition_opens_as_a_definition(cx: &mut TestAppContext) {
    let (_namespaces, namespace_events) = mpsc::channel(32);
    let session = cx.update(|cx| cx.new(|cx| Session::from_events("kind-a", namespace_events, cx)));
    let reference = ObjectReference {
        api_version: Some("apiextensions.k8s.io/v1".to_string()),
        kind: Some("CustomResourceDefinition".to_string()),
        name: Some("widgets.example.com".to_string()),
        ..ObjectReference::default()
    };
    let target = cx.read(|cx| DetailTarget::by_reference(reference, None, session.read(cx)));
    assert_eq!(
        target,
        Ok(DetailTarget::Definition {
            name: "widgets.example.com".to_string()
        })
    );
}
