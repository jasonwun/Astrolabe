//! Services and Ingresses on the generic list/detail pipeline (#18), with
//! Lens's columns and detail fields.

use astrolabe::projection::SummaryTable;
use astrolabe::projection::network::{
    ingress_detail, project_ingresses, project_services, service_detail, service_ports,
};
use astrolabe::projection::workloads::ForwardPort;
use astrolabe_kube::k8s_openapi::api::core::v1::Service;
use astrolabe_kube::k8s_openapi::api::networking::v1::Ingress;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

fn service(spec: serde_json::Value, status: serde_json::Value) -> Service {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-svc",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": spec,
        "status": status
    }))
    .unwrap()
}

/// Synthetic NodePort Service (plausible shape, not captured from a server).
fn node_port_service() -> Service {
    service(
        serde_json::json!({
            "type": "NodePort",
            "clusterIP": "10.96.0.10", "clusterIPs": ["10.96.0.10"],
            "ipFamilies": ["IPv4"], "ipFamilyPolicy": "SingleStack",
            "selector": {"app": "web", "tier": "front"},
            "sessionAffinity": "None",
            "ports": [
                {"name": "http", "port": 80, "targetPort": 8080, "nodePort": 30080, "protocol": "TCP"},
                {"name": "dns", "port": 53, "targetPort": 53, "protocol": "UDP"},
                {"name": "admin", "port": 9000, "targetPort": "admin", "protocol": "TCP"}
            ]
        }),
        serde_json::json!({}),
    )
}

fn cells(service: &Service) -> Vec<String> {
    project_services(&[service], now()).rows[0].cells.clone()
}

#[test]
fn services_list_lens_columns() {
    let service = node_port_service();

    let table = project_services(&[&service], now());

    assert_eq!(
        table.columns,
        [
            "Name",
            "Type",
            "Cluster IP",
            "Ports",
            "External IP",
            "Selector",
            "Age",
            "Status"
        ]
    );
    assert_eq!(
        table.rows[0].cells,
        [
            "web",
            "NodePort",
            "10.96.0.10",
            "80:30080/TCP, 53/UDP, 9000:admin/TCP",
            "-",
            "app=web, tier=front",
            "60m",
            "Active"
        ]
    );
}

#[test]
fn a_bare_service_shows_blanks_and_a_dash() {
    let bare = service(
        serde_json::json!({"type": "ClusterIP", "clusterIP": "None"}),
        serde_json::json!({}),
    );
    let row = cells(&bare);
    assert_eq!(row[2..6], ["None", "", "-", ""]);
}

#[test]
fn a_port_names_its_target_only_when_it_differs() {
    let ports = |ports: serde_json::Value| {
        cells(&service(
            serde_json::json!({"type": "ClusterIP", "ports": ports}),
            serde_json::json!({}),
        ))[3]
            .clone()
    };
    assert_eq!(
        ports(serde_json::json!([{"port": 80, "targetPort": 80, "protocol": "TCP"}])),
        "80/TCP"
    );
    assert_eq!(
        ports(serde_json::json!([{"port": 80, "targetPort": 8080, "protocol": "TCP"}])),
        "80:8080/TCP"
    );
}

#[test]
fn external_ips_prefer_the_load_balancer() {
    let external = |spec: serde_json::Value, status: serde_json::Value| {
        let row = cells(&service(spec, status));
        (row[4].clone(), row[7].clone())
    };
    let pair = |ip: &str, status: &str| (ip.to_string(), status.to_string());
    assert_eq!(
        external(
            serde_json::json!({"type": "ClusterIP", "externalIPs": ["1.2.3.4", "5.6.7.8"]}),
            serde_json::json!({})
        ),
        pair("1.2.3.4, 5.6.7.8", "Active")
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer"}),
            serde_json::json!({})
        ),
        pair("-", "Pending")
    );
    // Each ingress point's IP, else its hostname, in order; the spec's
    // external IPs only count while the status lists none.
    let balanced = serde_json::json!({"loadBalancer": {"ingress": [
        {"ip": "9.9.9.9", "hostname": "ignored.example"},
        {"hostname": "lb.example"}
    ]}});
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer", "externalIPs": ["1.2.3.4"]}),
            balanced
        ),
        pair("9.9.9.9, lb.example", "Active")
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer", "externalIPs": ["1.2.3.4"]}),
            serde_json::json!({})
        ),
        pair("1.2.3.4", "Active")
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "ExternalName", "externalName": "db.example"}),
            serde_json::json!({})
        ),
        pair("db.example", "Active")
    );
}

#[test]
fn a_service_detail_follows_lens() {
    let model = service_detail(&node_port_service(), now());

    assert_eq!(model.title, "Service default/web");
    assert_eq!(
        model.status,
        [
            ("Selector", "app=web, tier=front"),
            ("Type", "NodePort"),
            ("Session Affinity", "None"),
            ("Cluster IP", "10.96.0.10"),
            ("Cluster IPs", "10.96.0.10"),
            ("IP families", "IPv4"),
            ("IP family policy", "SingleStack"),
            ("Ports", "80:30080/TCP, 53/UDP, 9000:admin/TCP"),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
    assert!(model.tables.is_empty());
}

#[test]
fn a_load_balancer_detail_adds_its_external_and_requested_ips() {
    let model = service_detail(
        &service(
            serde_json::json!({"type": "LoadBalancer", "loadBalancerIP": "9.9.9.9"}),
            serde_json::json!({"loadBalancer": {"ingress": [{"ip": "9.9.9.9"}]}}),
        ),
        now(),
    );
    let field = |label: &str| {
        model
            .status
            .iter()
            .find(|(known, _)| known == label)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(field("External IPs"), Some("9.9.9.9"));
    assert_eq!(field("Load Balancer IP"), Some("9.9.9.9"));
    assert_eq!(field("Cluster IPs"), None);
}

#[test]
fn service_ports_offer_tcp_ports_with_their_target() {
    assert_eq!(
        service_ports(&node_port_service()),
        [
            ForwardPort {
                port: 80,
                label: "80/http → 8080".to_string()
            },
            ForwardPort {
                port: 9000,
                label: "9000/admin → admin".to_string()
            },
        ]
    );
    let unnamed = service(
        serde_json::json!({"ports": [{"port": 7000}]}),
        serde_json::json!({}),
    );
    assert_eq!(
        service_ports(&unnamed),
        [ForwardPort {
            port: 7000,
            label: "7000".to_string()
        }]
    );
}

/// Synthetic Ingress (plausible shape, not captured from a server).
fn web_ingress() -> Ingress {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-ing",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {
            "ingressClassName": "nginx",
            "defaultBackend": {"service": {"name": "fallback", "port": {"number": 80}}},
            "tls": [{"hosts": ["shop.example"], "secretName": "shop-tls"}],
            "rules": [
                {"host": "shop.example", "http": {"paths": [
                    {"path": "/", "pathType": "Prefix",
                     "backend": {"service": {"name": "web", "port": {"name": "http"}}}},
                    {"path": "/static", "pathType": "Exact",
                     "backend": {"resource": {"apiGroup": "k8s.example", "kind": "Bucket", "name": "assets"}}}
                ]}},
                {"http": {"paths": [
                    {"pathType": "ImplementationSpecific",
                     "backend": {"service": {"name": "api", "port": {"number": 8080}}}}
                ]}},
                {"host": "bare.example"}
            ]
        },
        "status": {"loadBalancer": {"ingress": [
            {"ip": "172.18.0.2"},
            {"ip": "172.18.0.3", "hostname": "lb.example"}
        ]}}
    }))
    .unwrap()
}

#[test]
fn ingresses_list_lens_columns() {
    let ingress = web_ingress();

    let table = project_ingresses(&[&ingress], now());

    assert_eq!(table.columns, ["Name", "LoadBalancers", "Rules", "Age"]);
    assert_eq!(
        table.rows[0].cells,
        [
            "web",
            "172.18.0.2, lb.example",
            "https://shop.example/ ⇢ web:http, \
             https://shop.example/static ⇢ <unknown>, \
             https://*/ ⇢ api:8080",
            "60m"
        ]
    );
}

#[test]
fn an_ingress_without_tls_routes_over_http() {
    let ingress: Ingress = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "any"},
        "spec": {"rules": [{"host": "a.example", "http": {"paths": [
            {"path": "/api", "pathType": "Prefix",
             "backend": {"service": {"name": "api", "port": {"number": 80}}}}
        ]}}]}
    }))
    .unwrap();
    let row = project_ingresses(&[&ingress], now()).rows[0].cells.clone();
    assert_eq!(row[1..3], ["", "http://a.example/api ⇢ api:80"]);
    assert_eq!(
        ingress_detail(&ingress, now()).status,
        [("Ports".to_string(), "80".to_string())]
    );
}

#[test]
fn an_ingress_without_rules_listens_on_its_default_backend_port() {
    let ingress: Ingress = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "any"},
        "spec": {"defaultBackend": {"service": {"name": "fallback", "port": {"number": 8080}}}}
    }))
    .unwrap();
    assert_eq!(
        ingress_detail(&ingress, now()).status,
        [
            ("Ports".to_string(), "8080".to_string()),
            ("Service".to_string(), "fallback:8080".to_string()),
        ]
    );
}

#[test]
fn an_ingress_detail_follows_lens() {
    let model = ingress_detail(&web_ingress(), now());

    assert_eq!(model.title, "Ingress default/web");
    assert_eq!(
        model.status,
        [
            ("Ports", "80, 443"),
            ("TLS", "shop-tls"),
            ("Service", "fallback:80"),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
    let rows = |rows: &[[&str; 4]]| {
        rows.iter()
            .map(|row| row.map(String::from).to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        model.tables,
        [
            SummaryTable {
                title: "Rules".to_string(),
                columns: ["Host", "Path", "Link", "Backends"]
                    .map(String::from)
                    .to_vec(),
                rows: rows(&[
                    ["shop.example", "/", "https://shop.example/", "web:http"],
                    [
                        "shop.example",
                        "/static",
                        "https://shop.example/static",
                        "<unknown>"
                    ],
                    ["", "/", "https://*/", "api:8080"],
                    ["bare.example", "", "", ""],
                ]),
            },
            SummaryTable {
                title: "Load-Balancer Ingress Points".to_string(),
                columns: ["Hostname", "IP"].map(String::from).to_vec(),
                rows: vec![
                    vec!["-".to_string(), "172.18.0.2".to_string()],
                    vec!["lb.example".to_string(), "172.18.0.3".to_string()],
                ],
            },
        ]
    );
}
