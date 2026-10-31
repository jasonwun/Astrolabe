//! A Service's EndpointSlices (#58): the related table's Name / Type /
//! Ports / Endpoints columns and the minimal slice detail, after Freelens
//! 088d085's `service-details-endpoint-slices.tsx` and `EndpointSlice`.
//! Synthetic objects (plausible shapes, never captured from a real
//! apiserver).

use astrolabe::builtin::BuiltinKind;
use astrolabe::projection::endpoint_slices::{endpoint_slice_detail, project_endpoint_slices};
use astrolabe_kube::k8s_openapi::api::discovery::v1::EndpointSlice;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use support::object;

mod support;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn slice_json(name: &str, owner: &str, ports: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "discovery.k8s.io/v1", "kind": "EndpointSlice",
        "metadata": {"name": name, "namespace": "default", "uid": format!("uid-{name}"),
                     "ownerReferences": [{"apiVersion": "v1", "kind": "Service", "name": owner,
                                          "uid": format!("uid-{owner}"), "controller": true}]},
        "addressType": "IPv4",
        "ports": ports,
        "endpoints": [
            {"addresses": ["10.0.0.1", "10.0.0.2"]},
            {"addresses": ["10.0.0.3"]},
            {"addresses": ["10.0.0.4", "10.0.0.5"]}
        ]
    })
}

fn slice(ports: serde_json::Value) -> EndpointSlice {
    serde_json::from_value(slice_json("web-abc", "web", ports)).unwrap()
}

fn cells(slice: &EndpointSlice) -> Vec<String> {
    project_endpoint_slices(&[slice], now()).rows[0]
        .cells
        .clone()
}

#[test]
fn columns_are_name_type_ports_endpoints() {
    assert_eq!(
        project_endpoint_slices(&[], now()).headers(),
        ["Name", "Type", "Ports", "Endpoints"]
    );
}

#[test]
fn a_slice_projects_its_ports_and_flattened_endpoints() {
    let slice = slice(serde_json::json!([
        {"name": "http", "port": 80, "protocol": "TCP"},
        {"name": "dns", "port": 53, "protocol": "UDP"}
    ]));
    assert_eq!(
        cells(&slice),
        [
            "web-abc",
            "IPv4",
            "80/TCP, 53/UDP",
            "10.0.0.1, 10.0.0.2, 10.0.0.3, 10.0.0.4, 10.0.0.5",
        ]
    );
}

#[test]
fn a_port_without_protocol_is_tcp() {
    let slice = slice(serde_json::json!([{"port": 8080}]));
    assert_eq!(cells(&slice)[2], "8080/TCP");
}

/// Upstream prints `undefined/TCP` for a port entry without `port` (all
/// ports); Astrolabe shows the protocol alone.
#[test]
fn a_port_entry_without_a_port_shows_only_its_protocol() {
    let slice = slice(serde_json::json!([{"protocol": "UDP"}, {}]));
    assert_eq!(cells(&slice)[2], "UDP, TCP");
}

#[test]
fn a_slice_without_ports_or_endpoints_shows_empty_cells() {
    let mut value = slice_json("web-abc", "web", serde_json::Value::Null);
    value.as_object_mut().unwrap().remove("endpoints");
    let slice: EndpointSlice = serde_json::from_value(value).unwrap();
    assert_eq!(cells(&slice), ["web-abc", "IPv4", "", ""]);
}

#[test]
fn the_detail_shows_address_type_ports_and_endpoints() {
    let slice = slice(serde_json::json!([{"port": 80}, {"protocol": "SCTP"}]));
    let model = endpoint_slice_detail(&slice, now());
    assert_eq!(model.title, "EndpointSlice default/web-abc");
    assert_eq!(
        model.status,
        [
            ("Address Type", "IPv4"),
            ("Ports", "80/TCP, SCTP"),
            (
                "Endpoints",
                "10.0.0.1, 10.0.0.2, 10.0.0.3, 10.0.0.4, 10.0.0.5"
            ),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
}

#[test]
fn a_service_selects_only_the_slices_it_owns() {
    let service = object(serde_json::json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-web"},
        "spec": {}
    }));
    let owned = object(slice_json("web-abc", "web", serde_json::json!([])));
    let other = object(slice_json("api-xyz", "api", serde_json::json!([])));
    let mut stale_value = slice_json("web-old", "web", serde_json::json!([]));
    stale_value["metadata"]["ownerReferences"][0]["uid"] = "uid-gone".into();
    let stale = object(stale_value);

    let related = BuiltinKind::Services
        .registration()
        .related
        .as_ref()
        .unwrap();
    assert_eq!(related.kind, BuiltinKind::EndpointSlices);
    assert_eq!(related.title, "Endpoint Slices");
    assert!(related.hide_when_empty);
    let selected = (related.select)(&service, &[&owned, &other, &stale]);
    let names: Vec<_> = selected
        .iter()
        .map(|slice| slice.metadata.name.as_deref().unwrap())
        .collect();
    assert_eq!(names, ["web-abc"]);
}

#[test]
fn endpoint_slices_resolve_by_kind_but_have_no_navigation_entry() {
    assert_eq!(
        BuiltinKind::from_kind("EndpointSlice"),
        Some(BuiltinKind::EndpointSlices)
    );
    assert!(
        BuiltinKind::NAVIGATION
            .iter()
            .flat_map(|(_, kinds)| kinds.iter())
            .all(|kind| *kind != BuiltinKind::EndpointSlices)
    );
}

#[test]
fn only_the_service_hides_its_related_section_when_empty() {
    for kind in BuiltinKind::ALL {
        if let Some(related) = &kind.registration().related {
            assert_eq!(
                related.hide_when_empty,
                kind == BuiltinKind::Services,
                "{kind:?}"
            );
        }
    }
}

#[test]
fn deleting_an_owned_slice_warns_it_comes_back() {
    let cascade = BuiltinKind::EndpointSlices.registration().cascade;
    let owned = object(slice_json("web-abc", "web", serde_json::json!([])));
    let mut bare_value = slice_json("manual", "web", serde_json::json!([]));
    bare_value["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("ownerReferences");
    let bare = object(bare_value);
    // A hand-made slice pointing at its Service: no controller rebuilds it.
    let mut referencing_value = slice_json("manual", "web", serde_json::json!([]));
    referencing_value["metadata"]["ownerReferences"][0]["controller"] = false.into();
    let referencing = object(referencing_value);
    assert_eq!(
        cascade(&owned),
        "A controller that owns this EndpointSlice will recreate it."
    );
    assert_eq!(cascade(&bare), "Nothing else is deleted.");
    assert_eq!(cascade(&referencing), "Nothing else is deleted.");
}
