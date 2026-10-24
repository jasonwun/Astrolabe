//! Services and Ingresses on the generic list/detail pipeline (#18).

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
fn services_list_kubectl_wide_columns() {
    let service = node_port_service();

    let table = project_services(&[&service], now());

    assert_eq!(
        table.columns,
        [
            "Name",
            "Type",
            "Cluster IP",
            "External IP",
            "Ports",
            "Selector",
            "Age"
        ]
    );
    assert_eq!(
        table.rows[0].cells,
        [
            "web",
            "NodePort",
            "10.96.0.10",
            "<none>",
            "80:30080/TCP,53/UDP,9000/TCP",
            "app=web,tier=front",
            "60m"
        ]
    );
}

#[test]
fn a_service_without_ports_or_selector_says_none() {
    let bare = service(
        serde_json::json!({"type": "ClusterIP", "clusterIP": "None", "clusterIPs": ["None"]}),
        serde_json::json!({}),
    );
    let row = cells(&bare);
    assert_eq!(row[2], "None");
    assert_eq!(row[4], "<none>");
    assert_eq!(row[5], "<none>");
}

#[test]
fn external_ips_follow_the_service_type() {
    let external = |spec: serde_json::Value, status: serde_json::Value| {
        cells(&service(spec, status))[3].clone()
    };
    assert_eq!(
        external(
            serde_json::json!({"type": "ClusterIP", "externalIPs": ["1.2.3.4", "5.6.7.8"]}),
            serde_json::json!({})
        ),
        "1.2.3.4,5.6.7.8"
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "NodePort", "externalIPs": ["1.2.3.4"]}),
            serde_json::json!({})
        ),
        "1.2.3.4"
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer"}),
            serde_json::json!({})
        ),
        "<pending>"
    );
    // Load-balancer addresses sort and dedupe; an IP wins over a hostname.
    let balanced = serde_json::json!({"loadBalancer": {"ingress": [
        {"ip": "9.9.9.9", "hostname": "ignored.example"},
        {"hostname": "lb.example"},
        {"ip": "9.9.9.9"}
    ]}});
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer"}),
            balanced.clone()
        ),
        "9.9.9.9,lb.example"
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer", "externalIPs": ["1.2.3.4"]}),
            balanced
        ),
        "9.9.9.9,lb.example,1.2.3.4"
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "LoadBalancer", "externalIPs": ["1.2.3.4"]}),
            serde_json::json!({})
        ),
        "1.2.3.4"
    );
    assert_eq!(
        external(
            serde_json::json!({"type": "ExternalName", "externalName": "db.example"}),
            serde_json::json!({})
        ),
        "db.example"
    );
    assert_eq!(
        external(serde_json::json!({"type": "Other"}), serde_json::json!({})),
        "<unknown>"
    );
}

#[test]
fn a_service_detail_summarises_type_ips_and_ports() {
    let model = service_detail(&node_port_service(), now());

    assert_eq!(model.title, "Service default/web");
    assert_eq!(
        model.status,
        [
            ("Type", "NodePort"),
            ("Cluster IPs", "10.96.0.10"),
            ("External IP", "<none>"),
            ("Session affinity", "None"),
            ("Selector", "app=web,tier=front"),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
    assert_eq!(
        model.tables,
        [SummaryTable {
            title: "Ports".to_string(),
            columns: ["Name", "Port", "Target port", "Node port", "Protocol"]
                .map(String::from)
                .to_vec(),
            rows: vec![
                ["http", "80", "8080", "30080", "TCP"]
                    .map(String::from)
                    .to_vec(),
                ["dns", "53", "53", "", "UDP"].map(String::from).to_vec(),
                ["admin", "9000", "admin", "", "TCP"]
                    .map(String::from)
                    .to_vec(),
            ],
        }]
    );
}

#[test]
fn an_external_name_service_names_its_target() {
    let model = service_detail(
        &service(
            serde_json::json!({"type": "ExternalName", "externalName": "db.example"}),
            serde_json::json!({}),
        ),
        now(),
    );
    assert!(
        model
            .status
            .contains(&("External name".to_string(), "db.example".to_string()))
    );
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
            "tls": [{"hosts": ["shop.example", "api.example"], "secretName": "shop-tls"}],
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
        "status": {"loadBalancer": {"ingress": [{"ip": "172.18.0.2"}, {"hostname": "lb.example"}]}}
    }))
    .unwrap()
}

#[test]
fn ingresses_list_kubectl_columns() {
    let ingress = web_ingress();

    let table = project_ingresses(&[&ingress], now());

    assert_eq!(
        table.columns,
        ["Name", "Class", "Hosts", "Address", "Ports", "Age"]
    );
    assert_eq!(
        table.rows[0].cells,
        [
            "web",
            "nginx",
            "shop.example,bare.example",
            "172.18.0.2,lb.example",
            "80, 443",
            "60m"
        ]
    );
}

#[test]
fn an_ingress_without_hosts_class_or_tls() {
    let ingress: Ingress = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "any"},
        "spec": {"rules": [{"http": {"paths": []}}]}
    }))
    .unwrap();
    let row = project_ingresses(&[&ingress], now()).rows[0].cells.clone();
    assert_eq!(row[1..5], ["<none>", "*", "", "80"]);
}

#[test]
fn ingress_hosts_beyond_three_are_counted() {
    let rules: Vec<_> = (1..=5)
        .map(|n| serde_json::json!({"host": format!("h{n}.example")}))
        .collect();
    let ingress: Ingress = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "many"}, "spec": {"rules": rules}
    }))
    .unwrap();
    assert_eq!(
        project_ingresses(&[&ingress], now()).rows[0].cells[2],
        "h1.example,h2.example,h3.example + 2 more..."
    );
}

#[test]
fn an_ingress_detail_maps_rules_to_backends() {
    let model = ingress_detail(&web_ingress(), now());

    assert_eq!(model.title, "Ingress default/web");
    assert_eq!(
        model.status,
        [
            ("Class", "nginx"),
            ("Address", "172.18.0.2,lb.example"),
            ("Default backend", "fallback:80"),
            ("TLS", "shop-tls: shop.example, api.example"),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
    assert_eq!(
        model.tables,
        [SummaryTable {
            title: "Rules".to_string(),
            columns: ["Host", "Path", "Path type", "Backend"]
                .map(String::from)
                .to_vec(),
            rows: vec![
                ["shop.example", "/", "Prefix", "web:http"]
                    .map(String::from)
                    .to_vec(),
                ["shop.example", "/static", "Exact", "Bucket assets"]
                    .map(String::from)
                    .to_vec(),
                ["*", "", "ImplementationSpecific", "api:8080"]
                    .map(String::from)
                    .to_vec(),
                ["bare.example", "", "", "fallback:80"]
                    .map(String::from)
                    .to_vec(),
            ],
        }]
    );
}
