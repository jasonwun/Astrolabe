//! Services, Ingresses, ConfigMaps and Secrets (#18) through the workspace
//! UI, on a scripted cluster. Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use astrolabe::builtin::BuiltinKind;
use astrolabe::session::{ForwardRequest, ForwardTarget};
use astrolabe::store::ObjectKey;
use astrolabe_kube::kube::core::DynamicObject;
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
            "Custom Resources",
        ]
        .map(position);
        assert!(order.is_sorted(), "{labels:?}");
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
