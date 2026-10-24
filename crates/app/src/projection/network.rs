//! Built-in network kinds on the generic pipeline (#18): Services and
//! Ingresses, with kubectl's printer columns (k8s `printers.go`).

use std::collections::BTreeSet;

use astrolabe_kube::k8s_openapi::api::core::v1::{LoadBalancerStatus, Service};
use astrolabe_kube::k8s_openapi::api::networking::v1::{
    Ingress, IngressBackend, IngressLoadBalancerStatus,
};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::workloads::{ForwardPort, NONE, int_or_string};
use super::{DetailModel, ResourceRow, ResourceTable, SummaryTable, age_of, detail};
use crate::store::ObjectKey;

/// Name / Type / Cluster IP / External IP / Ports / Selector / Age, as
/// `kubectl get services -o wide` prints them.
pub fn project_services(services: &[&Service], now: Timestamp) -> ResourceTable {
    let rows = services
        .iter()
        .map(|service| {
            let spec = service.spec.clone().unwrap_or_default();
            ResourceRow {
                key: ObjectKey::of(*service),
                cells: vec![
                    service.metadata.name.clone().unwrap_or_default(),
                    spec.type_.clone().unwrap_or_default(),
                    spec.cluster_ips
                        .iter()
                        .flatten()
                        .next()
                        .cloned()
                        .unwrap_or_else(|| NONE.to_string()),
                    external_ip(service),
                    port_text(service),
                    service_selector(service),
                    age_of(&service.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable {
        columns: [
            "Name",
            "Type",
            "Cluster IP",
            "External IP",
            "Ports",
            "Selector",
            "Age",
        ]
        .map(String::from)
        .to_vec(),
        rows,
    }
}

/// kubectl's `getServiceExternalIP` (wide, so never truncated).
fn external_ip(service: &Service) -> String {
    let spec = service.spec.clone().unwrap_or_default();
    let external = spec.external_ips.unwrap_or_default();
    match spec.type_.as_deref() {
        Some("ClusterIP" | "NodePort") if external.is_empty() => NONE.to_string(),
        Some("ClusterIP" | "NodePort") => external.join(","),
        Some("LoadBalancer") => {
            let balanced = service
                .status
                .as_ref()
                .and_then(|status| status.load_balancer.as_ref())
                .map(service_balancers)
                .unwrap_or_default();
            let addresses: Vec<String> = balanced.into_iter().chain(external).collect();
            if addresses.is_empty() {
                "<pending>".to_string()
            } else {
                addresses.join(",")
            }
        }
        Some("ExternalName") => spec.external_name.unwrap_or_default(),
        _ => "<unknown>".to_string(),
    }
}

/// `loadBalancerStatusStringer`: each ingress point's IP, else its
/// hostname, sorted and deduplicated.
fn service_balancers(status: &LoadBalancerStatus) -> Vec<String> {
    balancer_addresses(
        status
            .ingress
            .iter()
            .flatten()
            .map(|point| (point.ip.as_deref(), point.hostname.as_deref())),
    )
}

fn ingress_balancers(status: &IngressLoadBalancerStatus) -> Vec<String> {
    balancer_addresses(
        status
            .ingress
            .iter()
            .flatten()
            .map(|point| (point.ip.as_deref(), point.hostname.as_deref())),
    )
}

fn balancer_addresses<'a>(
    points: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> Vec<String> {
    let addresses: BTreeSet<&str> = points
        .filter_map(|(ip, hostname)| {
            ip.filter(|ip| !ip.is_empty())
                .or(hostname.filter(|hostname| !hostname.is_empty()))
        })
        .collect();
    addresses.into_iter().map(String::from).collect()
}

/// `makePortString`: `port[:nodePort]/protocol`, comma-separated.
fn port_text(service: &Service) -> String {
    let ports: Vec<String> = service
        .spec
        .iter()
        .flat_map(|spec| spec.ports.iter().flatten())
        .map(|port| {
            let protocol = port.protocol.as_deref().unwrap_or("TCP");
            match port.node_port {
                Some(node) if node > 0 => format!("{}:{node}/{protocol}", port.port),
                _ => format!("{}/{protocol}", port.port),
            }
        })
        .collect();
    if ports.is_empty() {
        NONE.to_string()
    } else {
        ports.join(",")
    }
}

/// `labels.FormatLabels` of the selector.
fn service_selector(service: &Service) -> String {
    let pairs: Vec<String> = service
        .spec
        .iter()
        .flat_map(|spec| spec.selector.iter().flatten())
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    if pairs.is_empty() {
        NONE.to_string()
    } else {
        pairs.join(",")
    }
}

/// Status summary = type, IPs, session affinity, selector (and an
/// ExternalName's target), plus a Ports table.
pub fn service_detail(service: &Service, now: Timestamp) -> DetailModel {
    let spec = service.spec.clone().unwrap_or_default();
    let mut summary = vec![
        ("Type", spec.type_.clone().unwrap_or_default()),
        (
            "Cluster IPs",
            spec.cluster_ips.clone().unwrap_or_default().join(","),
        ),
        ("External IP", external_ip(service)),
        (
            "Session affinity",
            spec.session_affinity.clone().unwrap_or_default(),
        ),
        ("Selector", service_selector(service)),
    ];
    if let Some(name) = &spec.external_name {
        summary.push(("External name", name.clone()));
    }
    let rows = spec
        .ports
        .iter()
        .flatten()
        .map(|port| {
            vec![
                port.name.clone().unwrap_or_default(),
                port.port.to_string(),
                port.target_port
                    .as_ref()
                    .map(int_or_string)
                    .unwrap_or_else(|| port.port.to_string()),
                port.node_port
                    .map(|node| node.to_string())
                    .unwrap_or_default(),
                port.protocol.clone().unwrap_or_else(|| "TCP".to_string()),
            ]
        })
        .collect();
    let value = serde_json::to_value(service).unwrap_or_default();
    let summary = summary
        .into_iter()
        .map(|(label, value)| (label.to_string(), value))
        .collect();
    let mut model = detail("Service", service, &value, summary, now);
    model.tables.push(SummaryTable {
        title: "Ports".to_string(),
        columns: ["Name", "Port", "Target port", "Node port", "Protocol"]
            .map(String::from)
            .to_vec(),
        rows,
    });
    model
}

/// The service ports Port Forward offers: TCP only, as port-forward
/// carries nothing else, each labelled `port/name → target`.
pub fn service_ports(service: &Service) -> Vec<ForwardPort> {
    service
        .spec
        .iter()
        .flat_map(|spec| spec.ports.iter().flatten())
        .filter(|port| port.protocol.as_deref().is_none_or(|p| p == "TCP"))
        .filter_map(|port| {
            let number = u16::try_from(port.port).ok()?;
            let name = port
                .name
                .as_ref()
                .map(|name| format!("/{name}"))
                .unwrap_or_default();
            let target = port
                .target_port
                .as_ref()
                .map(|target| format!(" → {}", int_or_string(target)))
                .unwrap_or_default();
            Some(ForwardPort {
                port: number,
                label: format!("{number}{name}{target}"),
            })
        })
        .collect()
}

/// Name / Class / Hosts / Address / Ports / Age, as `kubectl get ingresses
/// -o wide` prints them.
pub fn project_ingresses(ingresses: &[&Ingress], now: Timestamp) -> ResourceTable {
    let rows = ingresses
        .iter()
        .map(|ingress| {
            let spec = ingress.spec.clone().unwrap_or_default();
            ResourceRow {
                key: ObjectKey::of(*ingress),
                cells: vec![
                    ingress.metadata.name.clone().unwrap_or_default(),
                    ingress_class(ingress),
                    hosts(ingress),
                    ingress_address(ingress),
                    if spec.tls.unwrap_or_default().is_empty() {
                        "80"
                    } else {
                        "80, 443"
                    }
                    .to_string(),
                    age_of(&ingress.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Class", "Hosts", "Address", "Ports", "Age"]
            .map(String::from)
            .to_vec(),
        rows,
    }
}

fn ingress_class(ingress: &Ingress) -> String {
    ingress
        .spec
        .as_ref()
        .and_then(|spec| spec.ingress_class_name.clone())
        .unwrap_or_else(|| NONE.to_string())
}

fn ingress_address(ingress: &Ingress) -> String {
    ingress
        .status
        .as_ref()
        .and_then(|status| status.load_balancer.as_ref())
        .map(ingress_balancers)
        .unwrap_or_default()
        .join(",")
}

/// kubectl's `formatHosts`: the first three hosts, `+ N more...` counting
/// every further rule, `*` when no rule names a host.
fn hosts(ingress: &Ingress) -> String {
    const MAX: usize = 3;
    let rules = ingress
        .spec
        .as_ref()
        .and_then(|spec| spec.rules.clone())
        .unwrap_or_default();
    let mut list: Vec<String> = Vec::new();
    let mut more = false;
    for rule in &rules {
        if list.len() == MAX {
            more = true;
        }
        if let Some(host) = rule.host.as_ref().filter(|host| !host.is_empty())
            && !more
        {
            list.push(host.clone());
        }
    }
    if list.is_empty() {
        return "*".to_string();
    }
    let joined = list.join(",");
    if more {
        format!("{joined} + {} more...", rules.len() - MAX)
    } else {
        joined
    }
}

/// `service:port` for a Service backend, `Kind name` for a resource one.
fn backend_text(backend: &IngressBackend) -> String {
    if let Some(service) = &backend.service {
        let port = service
            .port
            .as_ref()
            .and_then(|port| port.name.clone().or(port.number.map(|n| n.to_string())))
            .unwrap_or_default();
        return format!("{}:{port}", service.name);
    }
    backend
        .resource
        .as_ref()
        .map(|resource| format!("{} {}", resource.kind, resource.name))
        .unwrap_or_default()
}

/// Status summary = class, address, default backend, TLS, plus a Rules
/// table mapping each host and path to its backend.
pub fn ingress_detail(ingress: &Ingress, now: Timestamp) -> DetailModel {
    let spec = ingress.spec.clone().unwrap_or_default();
    let default_backend = spec.default_backend.as_ref().map(backend_text);
    let mut summary = vec![
        ("Class".to_string(), ingress_class(ingress)),
        ("Address".to_string(), ingress_address(ingress)),
    ];
    if let Some(backend) = &default_backend {
        summary.push(("Default backend".to_string(), backend.clone()));
    }
    let tls: Vec<String> = spec
        .tls
        .iter()
        .flatten()
        .map(|tls| {
            format!(
                "{}: {}",
                tls.secret_name.clone().unwrap_or_default(),
                tls.hosts.clone().unwrap_or_default().join(", ")
            )
        })
        .collect();
    if !tls.is_empty() {
        summary.push(("TLS".to_string(), tls.join("; ")));
    }
    let mut rows = Vec::new();
    for rule in spec.rules.iter().flatten() {
        let host = rule
            .host
            .clone()
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "*".to_string());
        let Some(http) = &rule.http else {
            // A rule without paths sends everything to the default backend.
            rows.push(vec![
                host,
                String::new(),
                String::new(),
                default_backend.clone().unwrap_or_default(),
            ]);
            continue;
        };
        for path in &http.paths {
            rows.push(vec![
                host.clone(),
                path.path.clone().unwrap_or_default(),
                path.path_type.clone(),
                backend_text(&path.backend),
            ]);
        }
    }
    let value = serde_json::to_value(ingress).unwrap_or_default();
    let mut model = detail("Ingress", ingress, &value, summary, now);
    model.tables.push(SummaryTable {
        title: "Rules".to_string(),
        columns: ["Host", "Path", "Path type", "Backend"]
            .map(String::from)
            .to_vec(),
        rows,
    });
    model
}
