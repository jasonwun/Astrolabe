//! Built-in network kinds on the generic pipeline (#18): Services and
//! Ingresses, with Lens's list columns and detail fields (lensapp/lens
//! master, the `network-services/` and `network-ingresses/` components and
//! the `Service`/`Ingress` kube objects). Lens's Namespace column is left
//! out, as for every built-in kind.

use astrolabe_kube::k8s_openapi::api::core::v1::{Service, ServicePort};
use astrolabe_kube::k8s_openapi::api::networking::v1::{Ingress, IngressBackend, IngressRule};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::workloads::{ForwardPort, int_or_string};
use super::{DetailModel, ResourceRow, ResourceTable, SummaryTable, age_of, detail};
use crate::store::ObjectKey;

/// Name / Type / Cluster IP / Ports / External IP / Selector / Age /
/// Status, as Lens's Services list shows them.
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
                    spec.cluster_ip.clone().unwrap_or_default(),
                    port_list(service),
                    list_external_ips(service),
                    selector(service).join(", "),
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
            "Ports",
            "External IP",
            "Selector",
            "Age",
            "Status",
        ]
        .map(String::from)
        .to_vec(),
        rows,
    }
}

/// Lens's `Service.getExternalIps`: the load balancer's ingress points
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

/// Lens's `Service.getStatus`: a LoadBalancer is Pending until it has an
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

/// Lens's `ServicePort.toString`: `port:nodePort/protocol`, else
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

/// Lens's Service details: selector, type, session affinity, then the
/// connection (cluster IPs, IP families and policy, external IPs, ports,
/// a LoadBalancer's requested IP), each optional one only when set.
pub fn service_detail(service: &Service, now: Timestamp) -> DetailModel {
    let spec = service.spec.clone().unwrap_or_default();
    let mut summary = vec![
        ("Selector", selector(service).join(", ")),
        ("Type", spec.type_.clone().unwrap_or_default()),
        (
            "Session Affinity",
            spec.session_affinity.clone().unwrap_or_default(),
        ),
        ("Cluster IP", spec.cluster_ip.clone().unwrap_or_default()),
    ];
    let mut optional = |label, values: Vec<String>| {
        if !values.is_empty() {
            summary.push((label, values.join(", ")));
        }
    };
    optional("Cluster IPs", spec.cluster_ips.clone().unwrap_or_default());
    optional("IP families", spec.ip_families.clone().unwrap_or_default());
    optional(
        "IP family policy",
        spec.ip_family_policy.clone().into_iter().collect(),
    );
    optional("External IPs", external_ips(service));
    summary.push(("Ports", port_list(service)));
    if spec.type_.as_deref() == Some("LoadBalancer")
        && let Some(ip) = spec.load_balancer_ip.clone().filter(|ip| !ip.is_empty())
    {
        summary.push(("Load Balancer IP", ip));
    }
    let value = serde_json::to_value(service).unwrap_or_default();
    let summary = summary
        .into_iter()
        .map(|(label, value)| (label.to_string(), value))
        .collect();
    detail("Service", service, &value, summary, now)
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

/// Name / LoadBalancers / Rules / Age, as Lens's Ingresses list shows
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

/// Lens's `Ingress.getLoadBalancers`: each ingress point's hostname, else
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

/// One path of a rule, as Lens's `computeRuleDeclarations` reads it.
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

/// Lens's `getBackendServiceNamePort`: `service:port` (the port's number,
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

/// Lens's `Ingress.getPorts`: 80 when a rule has paths (or, with no rules,
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

/// Lens's Ingress details: ports, TLS secrets, the default backend
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
    // Lens names the default backend only when it has both name and port.
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
            // Lens titles a rule without paths by its host alone.
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
