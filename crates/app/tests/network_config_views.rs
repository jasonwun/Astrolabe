//! Services, Ingresses, ConfigMaps and Secrets (#18) through the workspace
//! UI, on a scripted cluster. Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use astrolabe::builtin::BuiltinKind;
use astrolabe::session::{ForwardRequest, ForwardTarget, WatchError};
use astrolabe::store::ObjectKey;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::TestAppContext;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use support::{Sender, Ui, key, open, snapshot};

fn object(value: serde_json::Value) -> DynamicObject {
    serde_json::from_value(value).unwrap()
}

fn web_service(selector: Option<serde_json::Value>) -> DynamicObject {
    let mut spec = serde_json::json!({
        "type": "ClusterIP", "clusterIP": "10.96.0.10", "clusterIPs": ["10.96.0.10"],
        "ports": [
            {"name": "http", "port": 80, "targetPort": 8080, "protocol": "TCP"},
            {"name": "dns", "port": 53, "protocol": "UDP"}
        ]
    });
    if let Some(selector) = selector {
        spec["selector"] = selector;
    }
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-svc"},
        "spec": spec
    }))
}

fn db_secret() -> DynamicObject {
    // `password` is "hunter2".
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Secret",
        "metadata": {"name": "db", "namespace": "default", "uid": "uid-secret"},
        "type": "Opaque",
        "data": {"password": "aHVudGVyMg=="}
    }))
}

/// Open `kind`'s list from the navigation with `objects` in it, then the
/// detail of `name`.
fn open_detail(
    ui: &mut Ui<'_>,
    kind: &str,
    watch: &mut Sender<DynamicObject>,
    objects: Vec<DynamicObject>,
    name: &str,
) {
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(format!("nav-{kind}"), cx));
    snapshot(watch, objects);
    ui.settle();
    ui.frame(|window, cx| window.click(format!("row-default/{name}"), cx));
    ui.settle();
}

#[gpui_kit::test]
fn navigation_groups_the_built_in_kinds(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    ui.namespaces(&["default"]);
    ui.settle();

    ui.frame(|window, _| {
        let nav = window.within("session-nav");
        let labels: Vec<String> = nav
            .elements()
            .iter()
            .filter_map(|element| element.label().map(|label| label.to_string()))
            .collect();
        let position = |label: &str| {
            labels
                .iter()
                .position(|known| known == label)
                .unwrap_or_else(|| panic!("{label} missing from {labels:?}"))
        };
        let order = [
            "Workloads",
            "Pods",
            "Deployments",
            "Network",
            "Services",
            "Ingresses",
            "Config",
            "ConfigMaps",
            "Secrets",
            "ResourceQuotas",
            "LimitRanges",
            "HorizontalPodAutoscalers",
            "Custom Resources",
        ]
        .map(position);
        assert!(order.is_sorted(), "{labels:?}");
        assert!(
            !labels
                .iter()
                .any(|label| label == "EndpointSlices" || label == "Endpoint Slices"),
            "{labels:?}"
        );
    });
}

#[gpui_kit::test]
fn a_service_detail_renders_freelens_sections_tables_and_external_links(cx: &mut TestAppContext) {
    let (mut services, service_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Services, "default"), service_events)],
    );
    let service = object(serde_json::json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": "web-lb", "namespace": "default", "uid": "uid-lb"},
        "spec": {
            "type": "LoadBalancer", "sessionAffinity": "ClientIP",
            "sessionAffinityConfig": {"clientIP": {"timeoutSeconds": 150}},
            "topologyKeys": ["kubernetes.io/hostname"],
            "ports": [{"port": 443, "targetPort": 8443}]
        },
        "status": {"loadBalancer": {
            "ingress": [
                {"ip": "9.9.9.9", "ports": [{"port": 443}]},
                {"hostname": "second.example", "ports": [{"port": 80, "protocol": "UDP"}]}
            ],
            "conditions": [{
                "lastTransitionTime": "2026-10-22T11:30:00Z",
                "reason": "Ready", "status": "True", "type": "Ready",
                "message": "load balancer is ready"
            }]
        }}
    }));
    open_detail(&mut ui, "services", &mut services, vec![service], "web-lb");

    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Topology Keys: kubernetes.io/hostname")
                .is_some()
        );
        let affinity = window.within("detail-section-Session Affinity Config");
        assert!(
            affinity
                .try_find_by_label("Client IP Timeout: 2m 30s")
                .is_some()
        );
        let balancer = window.within("detail-section-Load Balancer");
        assert!(balancer.try_find_by_label("IP: 9.9.9.9").is_some());
        let labels = balancer
            .elements()
            .iter()
            .filter_map(|element| element.label().map(|label| label.to_string()))
            .collect::<Vec<_>>();
        let position = |label: &str| labels.iter().position(|known| known == label).unwrap();
        assert!(position("TCP") < position("Hostname: second.example"));
        assert!(position("Hostname: second.example") < position("UDP"));
        let connection = window.within("detail-section-Connection");
        assert!(connection.try_find("link-External IPs-0").is_some());
        assert!(connection.try_find_by_label("9.9.9.9").is_some());
        let conditions = window.within("detail-section-Conditions");
        assert!(conditions.try_find_by_label("Reason: Ready").is_some());
        assert!(
            conditions
                .try_find_by_label("Message: load balancer is ready")
                .is_some()
        );
    });
}

#[gpui_kit::test]
fn a_service_forwards_one_of_its_tcp_ports(cx: &mut TestAppContext) {
    let (mut services, service_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Services, "default"), service_events)],
    );
    open_detail(
        &mut ui,
        "services",
        &mut services,
        vec![web_service(Some(serde_json::json!({"app": "web"})))],
        "web",
    );

    ui.frame(|window, cx| {
        assert!(window.try_find_by_label("Service default/web").is_some());
        assert!(
            window
                .try_find_by_label("Ports: 80:8080/TCP, 53/UDP")
                .is_some()
        );
        assert!(window.try_find("detail-logs").is_none());
        window.click("detail-forward", cx);
    });
    ui.frame(|window, cx| {
        let picker = window.within("container-picker");
        assert!(picker.try_find_by_label("80/http → 8080").is_some());
        assert!(
            picker.try_find("pick-port-53").is_none(),
            "UDP is not forwarded"
        );
        window.click("pick-port-80", cx);
    });
    ui.settle();

    ui.frame(|window, _| {
        assert!(
            window
                .within("detail-forwards")
                .try_find_by_label("localhost:10080 → 80")
                .is_some()
        );
    });
    assert_eq!(
        *ui.forwarded.borrow(),
        [ForwardRequest {
            target: ForwardTarget::Service(ObjectKey::new(Some("default".into()), "web")),
            port: 80
        }]
    );
}

#[gpui_kit::test]
fn a_service_with_only_udp_ports_has_none_to_forward(cx: &mut TestAppContext) {
    let (mut services, service_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Services, "default"), service_events)],
    );
    let mut dns = web_service(Some(serde_json::json!({"app": "web"})));
    dns.data["spec"]["ports"] = serde_json::json!([{"name": "dns", "port": 53, "protocol": "UDP"}]);
    open_detail(&mut ui, "services", &mut services, vec![dns], "web");

    ui.frame(|window, cx| window.click("detail-forward", cx));
    ui.frame(|window, _| {
        let picker = window.within("container-picker");
        assert!(picker.try_find_by_label("No TCP ports").is_some());
    });
}

fn delete_warning(selector: Option<serde_json::Value>, cx: &mut TestAppContext) -> bool {
    let (mut services, service_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Services, "default"), service_events)],
    );
    open_detail(
        &mut ui,
        "services",
        &mut services,
        vec![web_service(selector)],
        "web",
    );
    ui.frame(|window, cx| window.click("detail-delete", cx));
    ui.frame(|window, _| {
        let slices = window
            .try_find_by_label("Its EndpointSlices will be deleted too.")
            .is_some();
        let nothing = window
            .try_find_by_label("Nothing else is deleted.")
            .is_some();
        assert_ne!(slices, nothing);
        slices
    })
}

#[gpui_kit::test]
fn deleting_a_selecting_service_takes_its_endpoint_slices(cx: &mut TestAppContext) {
    assert!(delete_warning(Some(serde_json::json!({"app": "web"})), cx));
}

#[gpui_kit::test]
fn deleting_a_selectorless_service_takes_nothing_else(cx: &mut TestAppContext) {
    assert!(!delete_warning(None, cx));
}

/// An EndpointSlice owned by the Service `owner` with uid `owner_uid`.
fn endpoint_slice(
    name: &str,
    (owner, owner_uid): (&str, &str),
    addresses: &[&str],
) -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "discovery.k8s.io/v1", "kind": "EndpointSlice",
        "metadata": {"name": name, "namespace": "default", "uid": format!("uid-{name}"),
                     "ownerReferences": [{"apiVersion": "v1", "kind": "Service", "name": owner,
                                          "uid": owner_uid, "controller": true}]},
        "addressType": "IPv4",
        "ports": [{"name": "http", "port": 8080, "protocol": "TCP"}],
        "endpoints": [{"addresses": addresses}]
    }))
}

/// `web_service`'s name and uid, as its slices' owner.
const WEB: (&str, &str) = ("web", "uid-svc");
const API: (&str, &str) = ("api", "uid-api");

/// A `web` Service detail with the EndpointSlices watch scripted.
fn open_service_with_slices(cx: &mut TestAppContext) -> (Ui<'_>, Sender<DynamicObject>) {
    let (mut services, service_events) = mpsc::channel(32);
    let (slices, slice_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::Services, "default"), service_events),
            (key(BuiltinKind::EndpointSlices, "default"), slice_events),
        ],
    );
    open_detail(
        &mut ui,
        "services",
        &mut services,
        vec![web_service(Some(serde_json::json!({"app": "web"})))],
        "web",
    );
    (ui, slices)
}

#[gpui_kit::test]
fn a_service_without_owned_slices_has_no_endpoint_slices_section(cx: &mut TestAppContext) {
    let (mut ui, mut slices) = open_service_with_slices(cx);
    snapshot(
        &mut slices,
        vec![endpoint_slice("api-xyz", API, &["10.0.0.9"])],
    );
    ui.settle();

    ui.frame(|window, _| {
        assert!(window.try_find("detail-metadata").is_some());
        assert!(window.try_find("detail-related").is_none());
        assert!(window.try_find_by_label("Endpoint Slices").is_none());
        assert!(window.try_find_by_label("No endpoint slices").is_none());
    });
}

#[gpui_kit::test]
fn a_service_lists_its_endpoint_slices_live_and_opens_one(cx: &mut TestAppContext) {
    let (mut ui, mut slices) = open_service_with_slices(cx);
    snapshot(
        &mut slices,
        vec![
            endpoint_slice("web-abc", WEB, &["10.0.0.1"]),
            endpoint_slice("api-xyz", API, &["10.0.0.9"]),
        ],
    );
    ui.settle();

    ui.frame(|window, _| {
        let related = window.within("detail-related");
        assert!(related.try_find_by_label("Endpoint Slices").is_some());
        let columns: Vec<String> = (0..4)
            .map(|index| {
                related
                    .try_find(format!("related-column-{index}"))
                    .and_then(|column| column.label().map(|label| label.to_string()))
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(columns, ["Name", "Type", "Ports", "Endpoints"]);
        assert!(related.try_find("related-row-default/web-abc").is_some());
        assert!(related.try_find("related-row-default/api-xyz").is_none());
        assert!(related.try_find_by_label("8080/TCP").is_some());
        assert!(related.try_find_by_label("10.0.0.1").is_some());
    });

    slices
        .try_send(Ok(Event::Apply(endpoint_slice(
            "web-abc",
            WEB,
            &["10.0.0.1", "10.0.0.2"],
        ))))
        .unwrap();
    ui.settle();
    ui.frame(|window, cx| {
        let related = window.within("detail-related");
        assert!(related.try_find_by_label("10.0.0.1, 10.0.0.2").is_some());
        window.click("related-row-default/web-abc", cx);
    });
    ui.settle();

    ui.frame(|window, cx| {
        assert!(
            window
                .try_find_by_label("EndpointSlice default/web-abc")
                .is_some()
        );
        let status = window.within("detail-status");
        assert!(status.try_find_by_label("Address Type: IPv4").is_some());
        assert!(status.try_find_by_label("Ports: 8080/TCP").is_some());
        assert!(
            status
                .try_find_by_label("Endpoints: 10.0.0.1, 10.0.0.2")
                .is_some()
        );
        assert!(window.try_find("detail-edit").is_some());
        window.click("detail-delete", cx);
    });
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("A controller that owns this EndpointSlice will recreate it.")
                .is_some()
        );
    });
}

#[gpui_kit::test]
fn a_forbidden_endpoint_slice_watch_still_says_so(cx: &mut TestAppContext) {
    let (mut ui, mut slices) = open_service_with_slices(cx);
    slices
        .try_send(Err(WatchError::forbidden("endpointslices is forbidden")))
        .unwrap();
    ui.settle();

    ui.frame(|window, _| {
        assert!(
            window
                .within("detail-related")
                .try_find_by_label("No permission to list endpoint slices here")
                .is_some()
        );
    });
}

#[gpui_kit::test]
fn an_ingress_detail_maps_rules_to_backends(cx: &mut TestAppContext) {
    let (mut ingresses, ingress_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Ingresses, "default"), ingress_events)],
    );
    let ingress = object(serde_json::json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
        "metadata": {"name": "shop", "namespace": "default", "uid": "uid-ing"},
        "spec": {"rules": [{"host": "shop.example", "http": {"paths": [
            {"path": "/", "pathType": "Prefix",
             "backend": {"service": {"name": "web", "port": {"number": 80}}}}
        ]}}]}
    }));
    open_detail(&mut ui, "ingresses", &mut ingresses, vec![ingress], "shop");

    ui.frame(|window, _| {
        let rules = window.within("detail-table-Rules");
        assert!(rules.try_find_by_label("shop.example").is_some());
        assert!(rules.try_find_by_label("http://shop.example/").is_some());
        assert!(rules.try_find_by_label("web:80").is_some());
        assert!(window.try_find("detail-edit").is_some());
        assert!(window.try_find("detail-forward").is_none());
    });
}

#[gpui_kit::test]
fn a_config_map_detail_previews_its_data(cx: &mut TestAppContext) {
    let (mut configs, config_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::ConfigMaps, "default"), config_events)],
    );
    let config = object(serde_json::json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": "app", "namespace": "default", "uid": "uid-cm"},
        "data": {"mode": "production"}
    }));
    open_detail(&mut ui, "configmaps", &mut configs, vec![config], "app");

    ui.frame(|window, _| {
        let data = window.within("detail-table-Data");
        assert!(data.try_find_by_label("mode").is_some());
        assert!(data.try_find_by_label("production").is_some());
    });
}

fn shows(window: &gpui_kit::Window, text: &str) -> bool {
    window.elements().iter().any(|element| {
        [element.label(), element.value()]
            .into_iter()
            .flatten()
            .any(|shown| shown.contains(text))
    })
}

#[gpui_kit::test]
fn a_secret_value_shows_only_once_revealed(cx: &mut TestAppContext) {
    let (mut secrets, secret_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Secrets, "default"), secret_events)],
    );
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click("nav-secrets", cx));
    snapshot(&mut secrets, vec![db_secret()]);
    ui.settle();
    ui.frame(|window, cx| {
        assert!(
            window.try_find_by_label("password").is_some(),
            "Keys column"
        );
        assert!(!shows(window, "hunter2"));
        window.click("row-default/db", cx);
    });
    ui.settle();

    ui.frame(|window, cx| {
        let data = window.within("detail-secret-data");
        assert!(
            data.try_find_by_label("password: <hidden> (7 bytes)")
                .is_some()
        );
        assert!(!shows(window, "hunter2"));
        assert!(!shows(window, "aHVudGVyMg=="), "the YAML preview is masked");
        window.click("secret-reveal-password", cx);
    });
    ui.frame(|window, cx| {
        assert!(
            window
                .within("detail-secret-data")
                .try_find_by_label("password: hunter2")
                .is_some()
        );
        window.click("secret-hide-password", cx);
    });
    ui.frame(|window, _| {
        assert!(!shows(window, "hunter2"));
    });
}

#[gpui_kit::test]
fn editing_a_secret_opens_its_real_manifest(cx: &mut TestAppContext) {
    let (mut secrets, secret_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Secrets, "default"), secret_events)],
    );
    open_detail(&mut ui, "secrets", &mut secrets, vec![db_secret()], "db");

    ui.frame(|window, cx| window.click("detail-edit", cx));
    ui.settle();
    ui.frame(|window, _| {
        assert!(window.try_find_by_label("Edit Secret default/db").is_some());
        assert!(shows(window, "aHVudGVyMg=="), "Edit applies the real data");
    });
}
