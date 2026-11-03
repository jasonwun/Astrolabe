//! A Registration's navigation id, delete cascade and related selection,
//! which the app's views use and the model tests drive without GPUI (#153).

use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_model::builtin::BuiltinKind;
use support::object;

mod support;

const NOTHING: &str = "Nothing else is deleted.";

fn cascade(kind: BuiltinKind, raw: serde_json::Value) -> &'static str {
    (kind.registration().cascade)(&object(raw))
}

/// Names of the objects `kind`'s related section selects for `raw`.
fn related(
    kind: BuiltinKind,
    raw: serde_json::Value,
    candidates: Vec<serde_json::Value>,
) -> Vec<String> {
    let shown = object(raw);
    let candidates: Vec<DynamicObject> = candidates.into_iter().map(object).collect();
    let related = kind
        .registration()
        .related
        .as_ref()
        .expect("a related section");
    (related.select)(&shown, &candidates.iter().collect::<Vec<_>>())
        .into_iter()
        .map(|object| object.metadata.name.clone().unwrap())
        .collect()
}

#[test]
fn a_navigation_id_is_the_lowercased_label() {
    assert_eq!(BuiltinKind::Pods.nav_id(), "nav-pods");
    assert_eq!(BuiltinKind::ConfigMaps.nav_id(), "nav-configmaps");
}

/// Synthetic Service (plausible shape, not captured from a server).
fn service(spec: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": "web", "namespace": "default"},
        "spec": spec,
    })
}

#[test]
fn only_a_service_with_a_selector_takes_its_endpoint_slices() {
    let services = BuiltinKind::Services;
    assert_eq!(
        cascade(
            services,
            service(&serde_json::json!({"selector": {"app": "web"}}))
        ),
        "Its EndpointSlices will be deleted too."
    );
    assert_eq!(
        cascade(services, service(&serde_json::json!({"selector": {}}))),
        NOTHING
    );
    assert_eq!(cascade(services, service(&serde_json::json!({}))), NOTHING);
}

/// Synthetic Pod (plausible shape, not captured from a server).
fn pod(name: &str, metadata: &serde_json::Value) -> serde_json::Value {
    let mut value = serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": "default", "uid": format!("uid-{name}")},
    });
    value["metadata"]
        .as_object_mut()
        .unwrap()
        .extend(metadata.as_object().unwrap().clone());
    value
}

fn owner(kind: &str, name: &str, uid: &str) -> serde_json::Value {
    serde_json::json!({"apiVersion": "apps/v1", "kind": kind, "name": name, "uid": uid})
}

#[test]
fn only_an_owned_pod_is_replaced() {
    let pods = BuiltinKind::Pods;
    let owners = serde_json::json!({"ownerReferences": [owner("ReplicaSet", "web", "uid-rs")]});
    assert_eq!(
        cascade(pods, pod("web-1", &owners)),
        "A controller that owns this Pod will create a replacement."
    );
    let no_owners = serde_json::json!({"ownerReferences": []});
    assert_eq!(cascade(pods, pod("web-1", &no_owners)), NOTHING);
    assert_eq!(cascade(pods, pod("web-1", &serde_json::json!({}))), NOTHING);
}

/// Synthetic EndpointSlice (plausible shape, not captured from a server).
fn slice(owners: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "discovery.k8s.io/v1", "kind": "EndpointSlice",
        "metadata": {"name": "web-abc", "namespace": "default", "ownerReferences": owners},
        "addressType": "IPv4", "endpoints": [],
    })
}

#[test]
fn only_a_controlled_endpoint_slice_is_recreated() {
    let slices = BuiltinKind::EndpointSlices;
    let mut controller = owner("Service", "web", "uid-svc");
    controller["controller"] = true.into();
    assert_eq!(
        cascade(slices, slice(&serde_json::json!([controller]))),
        "A controller that owns this EndpointSlice will recreate it."
    );
    assert_eq!(
        cascade(
            slices,
            slice(&serde_json::json!([owner("Service", "web", "uid-svc")]))
        ),
        NOTHING
    );
}

/// Synthetic Event (plausible shape, not captured from a server).
fn event(name: &str, about: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1", "kind": "Event",
        "metadata": {"name": name, "namespace": "default"},
        "involvedObject": {"kind": "Pod", "name": about, "namespace": "default",
                           "uid": format!("uid-{about}")},
    })
}

#[test]
fn a_pods_related_events_are_those_about_it() {
    let selected = related(
        BuiltinKind::Pods,
        pod("web-1", &serde_json::json!({})),
        vec![
            event("one", "web-1"),
            event("other", "web-2"),
            event("two", "web-1"),
        ],
    );
    assert_eq!(selected, ["one", "two"]);
}

/// Synthetic Deployment (plausible shape, not captured from a server).
fn deployment(spec: Option<serde_json::Value>) -> serde_json::Value {
    let mut value = serde_json::json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-web"},
    });
    if let Some(spec) = spec {
        value["spec"] = spec;
    }
    value
}

#[test]
fn a_deployments_related_pods_are_those_its_selector_matches() {
    let labelled = |name: &str, app: &str| pod(name, &serde_json::json!({"labels": {"app": app}}));
    let pods = || vec![labelled("web-1", "web"), labelled("db-1", "db")];
    let spec = serde_json::json!({
        "selector": {"matchLabels": {"app": "web"}},
        "template": {"metadata": {"labels": {"app": "web"}}},
    });

    assert_eq!(
        related(BuiltinKind::Deployments, deployment(Some(spec)), pods()),
        ["web-1"]
    );
    assert_eq!(
        related(BuiltinKind::Deployments, deployment(None), pods()),
        [] as [String; 0]
    );
}

#[test]
fn related_owned_objects_match_owner_kind_name_and_uid() {
    let stateful_set = serde_json::json!({
        "apiVersion": "apps/v1", "kind": "StatefulSet",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-sts"},
    });
    let owned_by = |name: &str, reference: serde_json::Value| {
        pod(name, &serde_json::json!({"ownerReferences": [reference]}))
    };
    let pods = vec![
        owned_by("mine", owner("StatefulSet", "web", "uid-sts")),
        // Same name, recreated owner: someone else's.
        owned_by("stale", owner("StatefulSet", "web", "uid-old")),
        owned_by("other-name", owner("StatefulSet", "db", "uid-sts")),
        owned_by("other-kind", owner("ReplicaSet", "web", "uid-sts")),
        pod("bare", &serde_json::json!({})),
    ];

    assert_eq!(
        related(
            BuiltinKind::StatefulSets,
            stateful_set.clone(),
            pods.clone()
        ),
        ["mine"]
    );

    // An owner without a uid matches on kind and name alone.
    let mut no_uid = stateful_set;
    no_uid["metadata"].as_object_mut().unwrap().remove("uid");
    assert_eq!(
        related(BuiltinKind::StatefulSets, no_uid, pods),
        ["mine", "stale"]
    );
}
