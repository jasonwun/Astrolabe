//! Services and Ingresses on the generic list/detail pipeline (#18), with
//! Freelens 088d085's columns and detail fields.

use astrolabe::projection::network::{
    ingress_detail, project_ingresses, project_services, service_detail, service_detail_with_raw,
    service_ports,
};
use astrolabe::projection::workloads::ForwardPort;
use astrolabe::projection::{DetailField, DetailItem, DetailSection, DetailValue, SummaryTable};
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

fn section<'a>(sections: &'a [DetailSection], title: &str) -> Option<&'a DetailSection> {
    sections.iter().find(|section| section.title == title)
}

fn fields(section: &DetailSection) -> Vec<DetailField> {
    section
        .items
        .iter()
        .filter_map(|item| match item {
            DetailItem::Field(field) => Some(field.clone()),
            DetailItem::Table(_) => None,
        })
        .collect()
}

fn field<'a>(section: &'a DetailSection, label: &str) -> Option<&'a DetailField> {
    section.items.iter().find_map(|item| match item {
        DetailItem::Field(field) if field.label == label => Some(field),
        _ => None,
    })
}

fn value(field: Option<&DetailField>) -> Option<&str> {
    field?.values.first().map(|value| value.text.as_str())
}

#[test]
fn services_list_follows_freelens_columns() {
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
            "-",
            "80:30080/TCP, 53/UDP, 9000:admin/TCP",
            "60m",
            "Active"
        ]
    );
}

#[test]
fn a_bare_service_shows_blanks_and_a_dash() {
    let bare = service(
        serde_json::json!({"clusterIP": "None"}),
        serde_json::json!({}),
    );
    let row = cells(&bare);
    assert_eq!(row[1], "-");
    assert_eq!(row[2..5], ["None", "-", ""]);
}

#[test]
fn a_port_names_its_target_only_when_it_differs() {
    let ports = |ports: serde_json::Value| {
        cells(&service(
            serde_json::json!({"type": "ClusterIP", "ports": ports}),
            serde_json::json!({}),
        ))[4]
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
        (row[3].clone(), row[6].clone())
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
fn a_service_detail_separates_base_fields_from_connection() {
    let model = service_detail(&node_port_service(), now());

    assert_eq!(model.title, "Service default/web");
    assert_eq!(
        model.status,
        [
            ("Selector", "app=web, tier=front"),
            ("Type", "NodePort"),
            ("Session Affinity", "None"),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
    assert_eq!(
        model.sections,
        [DetailSection {
            title: "Connection".to_string(),
            items: [
                ("Cluster IP", "10.96.0.10"),
                ("Cluster IPs", "10.96.0.10"),
                ("IP family policy", "SingleStack"),
                ("IP families", "IPv4"),
                ("Ports", "80:30080/TCP, 53/UDP, 9000:admin/TCP"),
            ]
            .map(|field| DetailField::from(field).into())
            .to_vec(),
        }]
    );
    assert!(model.tables.is_empty());
}

#[test]
fn connection_array_fields_follow_array_length() {
    let model = service_detail(
        &service(
            serde_json::json!({"clusterIPs": [""], "ipFamilies": [""]}),
            serde_json::json!({}),
        ),
        now(),
    );
    let connection = section(&model.sections, "Connection").unwrap();
    assert_eq!(value(field(connection, "Cluster IPs")), Some(""));
    assert_eq!(value(field(connection, "IP families")), Some(""));
}

#[test]
fn optional_base_fields_and_session_affinity_config_follow_their_presence() {
    let full = service_detail(
        &service(
            serde_json::json!({
                "type": "ClusterIP", "sessionAffinity": "ClientIP",
                "internalTrafficPolicy": "Local",
                "trafficDistribution": "PreferClose",
                "publishNotReadyAddresses": false,
                "sessionAffinityConfig": {"clientIP": {"timeoutSeconds": 150}}
            }),
            serde_json::json!({}),
        ),
        now(),
    );
    assert_eq!(
        full.status,
        [
            ("Type", "ClusterIP"),
            ("Session Affinity", "ClientIP"),
            ("Internal Traffic Policy", "Local"),
            ("Traffic Distribution", "PreferClose"),
            ("Publish Not Ready Address", "false"),
        ]
        .map(|(label, value)| (label.to_string(), value.to_string()))
    );
    let affinity = section(&full.sections, "Session Affinity Config").unwrap();
    assert_eq!(value(field(affinity, "Client IP Timeout")), Some("2m 30s"));

    let bare = service_detail(
        &service(
            serde_json::json!({"type": "ClusterIP", "clusterIP": "", "ipFamilyPolicy": ""}),
            serde_json::json!({}),
        ),
        now(),
    );
    assert_eq!(
        bare.status,
        [
            ("Type".to_string(), "ClusterIP".to_string()),
            ("Session Affinity".to_string(), String::new()),
        ]
    );
    assert!(section(&bare.sections, "Session Affinity Config").is_none());
    assert!(
        section(&bare.sections, "Connection")
            .unwrap()
            .items
            .is_empty()
    );

    let no_client_ip = service_detail(
        &service(
            serde_json::json!({"sessionAffinityConfig": {}}),
            serde_json::json!({}),
        ),
        now(),
    );
    assert!(
        section(&no_client_ip.sections, "Session Affinity Config")
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn a_load_balancer_detail_shows_optional_fields_and_ingress_port_tables() {
    let model = service_detail(
        &service(
            serde_json::json!({
                "type": "LoadBalancer",
                "allocateLoadBalancerNodePorts": false,
                "loadBalancerIP": "9.9.9.9",
                "loadBalancerClass": "example.com/internal",
                "externalTrafficPolicy": "Local",
                "healthCheckNodePort": 30000
            }),
            serde_json::json!({"loadBalancer": {"ingress": [
                {
                    "hostname": "lb.example", "ip": "9.9.9.9", "ipMode": "VIP",
                    "ports": [
                        {"port": 80},
                        {"port": 53, "protocol": "UDP", "error": "PortUnavailable"}
                    ]
                },
                {"hostname": "second.example"}
            ]}}),
        ),
        now(),
    );
    let balancer = section(&model.sections, "Load Balancer").unwrap();
    let mut expected = [
        ("Allocate Load Balancer Node Ports", "false"),
        ("Load Balancer IP", "9.9.9.9"),
        ("Load Balancer Class", "example.com/internal"),
        ("External Traffic Policy", "Local"),
        ("Health Check Node Port", "30000"),
        ("Hostname", "lb.example"),
        ("IP", "9.9.9.9"),
        ("IP Mode", "VIP"),
        ("Hostname", "second.example"),
    ]
    .map(|field| DetailField::from(field).into())
    .to_vec();
    expected.insert(
        8,
        DetailItem::Table(SummaryTable {
            title: "Ports".to_string(),
            columns: ["Port", "Protocol", "Error"].map(String::from).to_vec(),
            rows: vec![
                vec!["80".to_string(), "TCP".to_string(), String::new()],
                vec![
                    "53".to_string(),
                    "UDP".to_string(),
                    "PortUnavailable".to_string(),
                ],
            ],
        }),
    );
    assert_eq!(balancer.items, expected);
    let connection = section(&model.sections, "Connection").unwrap();
    assert_eq!(
        field(connection, "External IPs").unwrap().values,
        ["9.9.9.9", "second.example"].map(|text| DetailValue {
            text: text.to_string(),
            href: None,
        })
    );
    assert!(field(connection, "Cluster IPs").is_none());

    let cluster_ip = service_detail(
        &service(
            serde_json::json!({"type": "ClusterIP", "loadBalancerIP": "9.9.9.9"}),
            serde_json::json!({"loadBalancer": {"ingress": [{"ip": "9.9.9.9"}]}}),
        ),
        now(),
    );
    assert!(section(&cluster_ip.sections, "Load Balancer").is_none());
}

#[test]
fn external_ips_use_service_port_protocols_and_external_name_fallback() {
    let external = |spec: serde_json::Value| {
        let model = service_detail(&service(spec, serde_json::json!({})), now());
        let connection = section(&model.sections, "Connection").unwrap();
        field(connection, "External IPs").cloned()
    };
    let linked = |ip: &str, protocol: &str| DetailValue {
        text: ip.to_string(),
        href: Some(format!("{protocol}://{ip}")),
    };
    assert_eq!(
        external(serde_json::json!({
            "externalIPs": ["1.2.3.4", "5.6.7.8"],
            "ports": [{"port": 80}, {"port": 443}]
        }))
        .unwrap()
        .values,
        [linked("1.2.3.4", "https"), linked("5.6.7.8", "https")]
    );
    assert_eq!(
        external(serde_json::json!({
            "externalIPs": ["1.2.3.4"], "ports": [{"port": 80}]
        }))
        .unwrap()
        .values,
        [linked("1.2.3.4", "http")]
    );
    assert_eq!(
        external(serde_json::json!({
            "externalIPs": ["1.2.3.4"], "ports": [{"port": 8080}]
        }))
        .unwrap()
        .values,
        [DetailValue {
            text: "1.2.3.4".to_string(),
            href: None
        }]
    );
    assert_eq!(
        value(
            external(serde_json::json!({
                "type": "ExternalName", "externalName": "db.example"
            }))
            .as_ref()
        ),
        Some("db.example")
    );
    assert!(external(serde_json::json!({"type": "ClusterIP"})).is_none());
}

#[test]
fn load_balancer_conditions_follow_their_presence() {
    let raw = serde_json::json!({
        "metadata": {"name": "web"},
        "spec": {"type": "LoadBalancer"},
        "status": {"loadBalancer": {"conditions": [{
            "lastTransitionTime": "2026-10-22T11:30:00Z",
            "reason": "Ready", "status": "True", "type": "LoadBalancerReady",
            "message": "load balancer is ready"
        }]}}
    });
    let model = service_detail_with_raw(&serde_json::from_value(raw.clone()).unwrap(), &raw, now());
    let conditions = section(&model.sections, "Conditions").unwrap();
    assert_eq!(
        fields(conditions),
        [
            ("Last Transition Time", "2026-10-22T11:30:00Z"),
            ("Reason", "Ready"),
            ("Status", "True"),
            ("Type", "LoadBalancerReady"),
            ("Message", "load balancer is ready"),
        ]
        .map(DetailField::from)
    );

    let without = service_detail(
        &service(
            serde_json::json!({"type": "LoadBalancer"}),
            serde_json::json!({"loadBalancer": {}}),
        ),
        now(),
    );
    assert!(section(&without.sections, "Conditions").is_none());
    assert!(
        section(&without.sections, "Load Balancer")
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn topology_keys_are_retained_from_the_original_service() {
    let raw = serde_json::json!({
        "metadata": {"name": "web"},
        "spec": {
            "type": "ClusterIP",
            "topologyKeys": ["kubernetes.io/hostname", "topology.kubernetes.io/zone"]
        }
    });
    let model = service_detail_with_raw(&serde_json::from_value(raw.clone()).unwrap(), &raw, now());
    assert_eq!(
        model.status,
        [
            ("Type".to_string(), "ClusterIP".to_string()),
            ("Session Affinity".to_string(), String::new()),
            (
                "Topology Keys".to_string(),
                "kubernetes.io/hostname, topology.kubernetes.io/zone".to_string()
            ),
        ]
    );

    let without = service_detail(
        &service(
            serde_json::json!({"type": "ClusterIP"}),
            serde_json::json!({}),
        ),
        now(),
    );
    assert!(
        !without
            .status
            .iter()
            .any(|(label, _)| label == "Topology Keys")
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
