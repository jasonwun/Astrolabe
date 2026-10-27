//! Built-in network kinds on the generic pipeline (#18): Services and
//! Ingresses, with Freelens's list columns and detail fields (Freelens
//! 088d085, `network-services/`, `network-ingresses/`, and the
//! `Service`/`Ingress` kube objects). Freelens's Namespace column is left
//! out, as for every built-in kind.

use astrolabe_kube::k8s_openapi::api::core::v1::{Service, ServicePort};
use astrolabe_kube::k8s_openapi::api::networking::v1::{Ingress, IngressBackend, IngressRule};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::workloads::{ForwardPort, int_or_string};
use super::{
    DetailField, DetailModel, DetailSection, DetailValue, Forward, ForwardTo, ResourceRow,
    ResourceTable, SummaryTable, age_of, detail,
};
use crate::store::ObjectKey;

/// Name / Type / Cluster IP / External IP / Ports / Age / Status, as
/// Freelens's Services list shows them.
pub fn project_services(services: &[&Service], now: Timestamp) -> ResourceTable {
    let rows = services
        .iter()
        .map(|service| {
            let spec = service.spec.clone().unwrap_or_default();
            ResourceRow {
                key: ObjectKey::of(*service),
                cells: vec![
                    service.metadata.name.clone().unwrap_or_default(),
                    spec.type_.clone().unwrap_or_else(|| "-".to_string()),
                    spec.cluster_ip.clone().unwrap_or_default(),
                    list_external_ips(service),
                    port_list(service),
                    age_of(&service.metadata, now),
                    service_status(service).to_string(),
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
            "Age",
            "Status",
        ]
        .map(String::from)
        .to_vec(),
        rows,
    }
}

/// Freelens's `Service.getExternalIps`: the load balancer's ingress points
/// (each one's IP, else its hostname) once the status lists any, else
/// `spec.externalIPs`.
fn external_ips(service: &Service) -> Vec<String> {
    let points = service
        .status
        .as_ref()
        .and_then(|status| status.load_balancer.as_ref())
        .and_then(|balancer| balancer.ingress.as_ref());
    match points {
        Some(points) => points
            .iter()
            .filter_map(|point| {
                point
                    .ip
                    .clone()
                    .filter(|ip| !ip.is_empty())
                    .or(point.hostname.clone())
            })
            .collect(),
        None => service
            .spec
            .as_ref()
            .and_then(|spec| spec.external_ips.clone())
            .unwrap_or_default(),
    }
}

/// The list's External IP: the external IPs, else an ExternalName's
/// target, else `-`.
fn list_external_ips(service: &Service) -> String {
    let ips = external_ips(service);
    if !ips.is_empty() {
        return ips.join(", ");
    }
    service
        .spec
        .as_ref()
        .and_then(|spec| spec.external_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

/// Freelens's `Service.getStatus`: a LoadBalancer is Pending until it has an
/// external IP; every other Service is Active.
fn service_status(service: &Service) -> &'static str {
    let balancer = service
        .spec
        .as_ref()
        .is_some_and(|spec| spec.type_.as_deref() == Some("LoadBalancer"));
    if balancer && external_ips(service).is_empty() {
        "Pending"
    } else {
        "Active"
    }
}

/// Freelens's `ServicePort.toString`: `port:nodePort/protocol`, else
/// `port:targetPort/protocol` when the target differs, else
/// `port/protocol`.
fn port_text(port: &ServicePort) -> String {
    let protocol = port.protocol.as_deref().unwrap_or("TCP");
    let suffix = match (port.node_port, &port.target_port) {
        (Some(node), _) if node != 0 => format!(":{node}"),
        (_, Some(IntOrString::Int(target))) if *target == port.port => String::new(),
        (_, Some(target)) => format!(":{}", int_or_string(target)),
        (_, None) => String::new(),
    };
    format!("{}{suffix}/{protocol}", port.port)
}

fn port_list(service: &Service) -> String {
    service
        .spec
        .iter()
        .flat_map(|spec| spec.ports.iter().flatten())
        .map(port_text)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The selector as `key=value` labels.
fn selector(service: &Service) -> Vec<String> {
    service
        .spec
        .iter()
        .flat_map(|spec| spec.selector.iter().flatten())
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Freelens's non-compact `formatDuration`, with its input in seconds.
fn format_duration(seconds: i64) -> String {
    fn meaningful(parts: &[(i64, &str)]) -> String {
        parts
            .iter()
            .filter(|(value, _)| *value > 0)
            .map(|(value, suffix)| format!("{value}{suffix}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    if seconds < 0 {
        return "0s".to_string();
    }
    if seconds < 120 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 180 {
        return meaningful(&[(minutes, "m"), (seconds % 60, "s")]);
    }
    let hours = minutes / 60;
    if hours < 48 {
        return meaningful(&[(hours, "h"), (minutes % 60, "m")]);
    }
    let days = hours / 24;
    if days < 8 {
        return meaningful(&[(days, "d"), (hours % 24, "h"), (minutes % 60, "m")]);
    }
    let years = seconds * 400 / (86_400 * 146_097);
    if years < 2 {
        return meaningful(&[(days, "d"), (hours % 24, "h"), (minutes % 60, "m")]);
    }
    let months = days * 4_800 / 146_097;
    let remaining_days = days - (months * 146_097 + 4_799) / 4_800;
    meaningful(&[
        (years, "y"),
        (remaining_days, "d"),
        (hours % 24, "h"),
        (minutes % 60, "m"),
    ])
}

/// Freelens's Service details: base fields, optional Load Balancer fields,
/// then the Connection section. `raw` is the Service as the server sent it;
/// it retains fields that the current Kubernetes types no longer model.
pub fn service_detail_with_raw(
    service: &Service,
    raw: &serde_json::Value,
    now: Timestamp,
) -> DetailModel {
    let spec = service.spec.clone().unwrap_or_default();
    let mut summary = Vec::new();
    let selector = selector(service);
    if !selector.is_empty() {
        summary.push(("Selector".to_string(), selector.join(", ")));
    }
    summary.push(("Type".to_string(), spec.type_.clone().unwrap_or_default()));
    summary.push((
        "Session Affinity".to_string(),
        spec.session_affinity.clone().unwrap_or_default(),
    ));
    for (label, value) in [
        ("Internal Traffic Policy", &spec.internal_traffic_policy),
        ("Traffic Distribution", &spec.traffic_distribution),
    ] {
        if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
            summary.push((label.to_string(), value.clone()));
        }
    }
    if let Some(keys) = raw
        .pointer("/spec/topologyKeys")
        .and_then(serde_json::Value::as_array)
    {
        let keys = keys
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        summary.push(("Topology Keys".to_string(), keys));
    }
    if let Some(value) = spec.publish_not_ready_addresses {
        summary.push(("Publish Not Ready Address".to_string(), value.to_string()));
    }

    let mut sections = Vec::new();
    if let Some(config) = &spec.session_affinity_config {
        let mut items = Vec::new();
        if let Some(client_ip) = &config.client_ip {
            items.push(
                DetailField::from((
                    "Client IP Timeout",
                    format_duration(client_ip.timeout_seconds.unwrap_or_default() as i64).as_str(),
                ))
                .into(),
            );
        }
        sections.push(DetailSection {
            title: "Session Affinity Config".to_string(),
            items,
        });
    }
    if spec.type_.as_deref() == Some("LoadBalancer") {
        let mut items = Vec::new();
        if let Some(value) = spec.allocate_load_balancer_node_ports {
            items.push(
                DetailField::from((
                    "Allocate Load Balancer Node Ports",
                    value.to_string().as_str(),
                ))
                .into(),
            );
        }
        for (label, value) in [
            ("Load Balancer IP", &spec.load_balancer_ip),
            ("Load Balancer Class", &spec.load_balancer_class),
            ("External Traffic Policy", &spec.external_traffic_policy),
        ] {
            if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
                items.push(DetailField::from((label, value.as_str())).into());
            }
        }
        if let Some(port) = spec.health_check_node_port.filter(|port| *port != 0) {
            items.push(
                DetailField::from(("Health Check Node Port", port.to_string().as_str())).into(),
            );
        }
        for point in service
            .status
            .iter()
            .flat_map(|status| status.load_balancer.iter())
            .flat_map(|balancer| balancer.ingress.iter().flatten())
        {
            for (label, value) in [
                ("Hostname", &point.hostname),
                ("IP", &point.ip),
                ("IP Mode", &point.ip_mode),
            ] {
                if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
                    items.push(DetailField::from((label, value.as_str())).into());
                }
            }
            if let Some(ports) = &point.ports {
                items.push(
                    SummaryTable {
                        title: "Ports".to_string(),
                        columns: ["Port", "Protocol", "Error"].map(String::from).to_vec(),
                        rows: ports
                            .iter()
                            .map(|port| {
                                vec![
                                    port.port.to_string(),
                                    if port.protocol.is_empty() {
                                        "TCP".to_string()
                                    } else {
                                        port.protocol.clone()
                                    },
                                    port.error.clone().unwrap_or_default(),
                                ]
                            })
                            .collect(),
                    }
                    .into(),
                );
            }
        }
        sections.push(DetailSection {
            title: "Load Balancer".to_string(),
            items,
        });
    }

    let mut connection = Vec::new();
    let mut push = |label, value: String| {
        connection.push(DetailField::from((label, value.as_str())).into());
    };
    if let Some(value) = spec.cluster_ip.clone().filter(|value| !value.is_empty()) {
        push("Cluster IP", value);
    }
    if let Some(values) = spec.cluster_ips.clone().filter(|values| !values.is_empty()) {
        push("Cluster IPs", values.join(", "));
    }
    if let Some(value) = spec
        .ip_family_policy
        .clone()
        .filter(|value| !value.is_empty())
    {
        push("IP family policy", value);
    }
    if let Some(values) = spec.ip_families.clone().filter(|values| !values.is_empty()) {
        push("IP families", values.join(", "));
    }
    let ports = spec.ports.as_deref().unwrap_or_default();
    let protocol = if ports.iter().any(|port| port.port == 443) {
        Some("https")
    } else if ports.iter().any(|port| port.port == 80) {
        Some("http")
    } else {
        None
    };
    let mut ips = external_ips(service);
    if ips.is_empty()
        && let Some(name) = spec.external_name.as_ref().filter(|name| !name.is_empty())
    {
        ips.push(name.clone());
    }
    if !ips.is_empty() {
        connection.push(
            DetailField {
                label: "External IPs".to_string(),
                values: ips
                    .into_iter()
                    .map(|ip| DetailValue {
                        href: protocol.map(|protocol| format!("{protocol}://{ip}")),
                        text: ip,
                    })
                    .collect(),
            }
            .into(),
        );
    }
    if !ports.is_empty() {
        connection.push(DetailField::from(("Ports", port_list(service).as_str())).into());
    }
    sections.push(DetailSection {
        title: "Connection".to_string(),
        items: connection,
    });
    if let Some(conditions) = raw
        .pointer("/status/loadBalancer/conditions")
        .and_then(serde_json::Value::as_array)
    {
        let mut items = Vec::new();
        for condition in conditions {
            let text = |label| condition.get(label).and_then(serde_json::Value::as_str);
            for label in ["Last Transition Time", "Reason", "Status"] {
                let key = match label {
                    "Last Transition Time" => "lastTransitionTime",
                    "Reason" => "reason",
                    _ => "status",
                };
                items.push(DetailField::from((label, text(key).unwrap_or_default())).into());
            }
            if let Some(kind) = text("type").filter(|kind| !kind.is_empty()) {
                items.push(DetailField::from(("Type", kind)).into());
            }
            items.push(DetailField::from(("Message", text("message").unwrap_or_default())).into());
        }
        sections.push(DetailSection {
            title: "Conditions".to_string(),
            items,
        });
    }

    let mut model = detail("Service", service, raw, summary, now);
    model.sections = sections;
    model.forward = Some(Forward {
        to: ForwardTo::Service,
        ports: service_ports(service),
        empty: "No TCP ports",
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

/// Name / LoadBalancers / Rules / Age, as Freelens's Ingresses list shows
/// them.
pub fn project_ingresses(ingresses: &[&Ingress], now: Timestamp) -> ResourceTable {
    let rows = ingresses
        .iter()
        .map(|ingress| ResourceRow {
            key: ObjectKey::of(*ingress),
            cells: vec![
                ingress.metadata.name.clone().unwrap_or_default(),
                load_balancers(ingress).join(", "),
                rules(ingress)
                    .flat_map(|rule| routes(ingress, rule))
                    .map(|route| format!("{} ⇢ {}", route.url, route.backend))
                    .collect::<Vec<_>>()
                    .join(", "),
                age_of(&ingress.metadata, now),
            ],
        })
        .collect();
    ResourceTable {
        columns: ["Name", "LoadBalancers", "Rules", "Age"]
            .map(String::from)
            .to_vec(),
        rows,
    }
}

/// Freelens's `Ingress.getLoadBalancers`: each ingress point's hostname, else
/// its IP.
fn load_balancers(ingress: &Ingress) -> Vec<String> {
    ingress
        .status
        .iter()
        .flat_map(|status| status.load_balancer.iter())
        .flat_map(|balancer| balancer.ingress.iter().flatten())
        .filter_map(|point| {
            point
                .hostname
                .clone()
                .filter(|hostname| !hostname.is_empty())
                .or(point.ip.clone())
        })
        .collect()
}

fn rules(ingress: &Ingress) -> impl Iterator<Item = &IngressRule> {
    ingress
        .spec
        .iter()
        .flat_map(|spec| spec.rules.iter().flatten())
}

fn has_tls(ingress: &Ingress) -> bool {
    ingress
        .spec
        .as_ref()
        .and_then(|spec| spec.tls.as_ref())
        .is_some_and(|tls| !tls.is_empty())
}

/// One path of a rule, as Freelens's `computeRuleDeclarations` reads it.
struct Route {
    path: String,
    url: String,
    backend: String,
}

/// Each of `rule`'s paths: `/` when unset, under `https://` once the
/// Ingress has TLS, on host `*` when the rule names none.
fn routes(ingress: &Ingress, rule: &IngressRule) -> Vec<Route> {
    let protocol = if has_tls(ingress) { "https" } else { "http" };
    let host = rule.host.as_deref().unwrap_or("*");
    rule.http
        .iter()
        .flat_map(|http| &http.paths)
        .map(|path| {
            let pathname = path.path.clone().unwrap_or_else(|| "/".to_string());
            Route {
                url: format!("{protocol}://{host}{pathname}"),
                path: pathname,
                backend: backend_text(&path.backend),
            }
        })
        .collect()
}

/// Freelens's `getBackendServiceNamePort`: `service:port` (the port's number,
/// else its name), `<unknown>` for anything but a Service backend.
fn backend_text(backend: &IngressBackend) -> String {
    backend
        .service
        .as_ref()
        .map(|service| {
            let port = service
                .port
                .as_ref()
                .and_then(|port| port.number.map(|n| n.to_string()).or(port.name.clone()))
                .unwrap_or_default();
            format!("{}:{port}", service.name)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// Freelens's `Ingress.getPorts`: 80 when a rule has paths (or, with no rules,
/// the default backend's port number), plus 443 with TLS.
fn ingress_ports(ingress: &Ingress) -> String {
    let spec = ingress.spec.clone().unwrap_or_default();
    let rules = spec.rules.unwrap_or_default();
    let mut ports = Vec::new();
    if !rules.is_empty() {
        if rules.iter().any(|rule| rule.http.is_some()) {
            ports.push(80);
        }
    } else if let Some(port) = spec
        .default_backend
        .and_then(|backend| backend.service)
        .and_then(|service| service.port)
        .and_then(|port| port.number)
    {
        ports.push(port);
    }
    if has_tls(ingress) {
        ports.push(443);
    }
    ports
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Freelens's Ingress details: ports, TLS secrets, the default backend
/// Service, a Rules table mapping each host and path to its backend, and
/// the load balancer's ingress points.
pub fn ingress_detail(ingress: &Ingress, now: Timestamp) -> DetailModel {
    let spec = ingress.spec.clone().unwrap_or_default();
    let mut summary = vec![("Ports".to_string(), ingress_ports(ingress))];
    if let Some(tls) = spec.tls.as_ref().filter(|tls| !tls.is_empty()) {
        let secrets: Vec<String> = tls
            .iter()
            .filter_map(|tls| tls.secret_name.clone())
            .collect();
        summary.push(("TLS".to_string(), secrets.join(", ")));
    }
    // Freelens names the default backend only when it has both name and port.
    if let Some(backend) = spec.default_backend.as_ref().filter(|backend| {
        backend
            .service
            .as_ref()
            .is_some_and(|service| service.port.is_some())
    }) {
        summary.push(("Service".to_string(), backend_text(backend)));
    }
    let mut rows = Vec::new();
    for rule in rules(ingress) {
        let host = rule.host.clone().unwrap_or_default();
        if rule.http.is_none() {
            // Freelens titles a rule without paths by its host alone.
            if !host.is_empty() {
                rows.push(vec![host, String::new(), String::new(), String::new()]);
            }
            continue;
        }
        for route in routes(ingress, rule) {
            rows.push(vec![host.clone(), route.path, route.url, route.backend]);
        }
    }
    let value = serde_json::to_value(ingress).unwrap_or_default();
    let mut model = detail("Ingress", ingress, &value, summary, now);
    model.tables.push(SummaryTable {
        title: "Rules".to_string(),
        columns: ["Host", "Path", "Link", "Backends"]
            .map(String::from)
            .to_vec(),
        rows,
    });
    let or_dash = |value: &Option<String>| {
        value
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "-".to_string())
    };
    let points: Vec<Vec<String>> = ingress
        .status
        .iter()
        .flat_map(|status| status.load_balancer.iter())
        .flat_map(|balancer| balancer.ingress.iter().flatten())
        .map(|point| vec![or_dash(&point.hostname), or_dash(&point.ip)])
        .collect();
    if !points.is_empty() {
        model.tables.push(SummaryTable {
            title: "Load-Balancer Ingress Points".to_string(),
            columns: ["Hostname", "IP"].map(String::from).to_vec(),
            rows: points,
        });
    }
    model
}
