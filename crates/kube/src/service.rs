//! Service port-forward (#18): `kubectl port-forward svc/name port`, where
//! each local connection reaches the Service's most active Pod.

use std::cmp::Reverse;

use k8s_openapi::api::core::v1::{Pod, Service};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::jiff::Timestamp;
use kube::api::ListParams;
use kube::{Api, Client};
use tokio::net::TcpListener;

/// Where one Service forward connection goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedForward {
    pub pod: String,
    /// The container port the service port maps to.
    pub port: u16,
}

/// The Pod and container port a forward to `port` of `service` reaches,
/// as kubectl resolves them (verified against kubectl master,
/// `polymorphichelpers.SelectorsForObject`, `GetFirstPod` sorted by
/// `podutils.ActivePods`, and `util.LookupContainerPortNumberByServicePort`):
/// the most active of `pods` (the selector's matches), which must be
/// Running. Of `ActivePods`' criteria, ready time and restart counts are
/// not compared, and of equally active Pods the first listed wins.
pub fn forward_target(
    service: &Service,
    pods: &[Pod],
    port: u16,
) -> Result<ResolvedForward, String> {
    let name = service.metadata.name.clone().unwrap_or_default();
    let spec = service.spec.clone().unwrap_or_default();
    if spec
        .selector
        .as_ref()
        .is_none_or(|selector| selector.is_empty())
    {
        return Err(format!(
            "invalid service '{name}': Service is defined without a selector"
        ));
    }
    let mut chosen: Option<&Pod> = None;
    for pod in pods {
        if chosen.is_none_or(|best| activity(pod) > activity(best)) {
            chosen = Some(pod);
        }
    }
    let pod = chosen.ok_or_else(|| format!("no Pods match Service {name}'s selector"))?;
    let phase = pod
        .status
        .as_ref()
        .and_then(|status| status.phase.clone())
        .unwrap_or_default();
    if phase != "Running" {
        return Err(format!(
            "unable to forward port because pod is not running. Current status={phase}"
        ));
    }
    // A headless Service's port is the container port, declared or not.
    let container_port = if spec.cluster_ip.as_deref() == Some("None") {
        i32::from(port)
    } else {
        let declared = spec
            .ports
            .iter()
            .flatten()
            .find(|declared| declared.port == i32::from(port))
            .ok_or_else(|| format!("Service {name} does not have a service port {port}"))?;
        match &declared.target_port {
            None | Some(IntOrString::Int(0)) => i32::from(port),
            Some(IntOrString::Int(target)) => *target,
            Some(IntOrString::String(named)) => named_port(pod, named)?,
        }
    };
    Ok(ResolvedForward {
        pod: pod.metadata.name.clone().unwrap_or_default(),
        port: u16::try_from(container_port)
            .map_err(|_| format!("invalid container port {container_port}"))?,
    })
}

/// `ActivePods`' order, more active is greater: assigned, then Running
/// over Unknown over Pending, ready, not deleting, older.
fn activity(pod: &Pod) -> (bool, u8, bool, bool, Option<Reverse<Timestamp>>) {
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();
    let phase = match status.and_then(|status| status.phase.as_deref()) {
        Some("Running") => 2,
        Some("Unknown") => 1,
        _ => 0,
    };
    let ready = status
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        });
    (
        spec.and_then(|spec| spec.node_name.as_deref())
            .is_some_and(|node| !node.is_empty()),
        phase,
        ready,
        pod.metadata.deletion_timestamp.is_none(),
        pod.metadata
            .creation_timestamp
            .as_ref()
            .map(|created| Reverse(created.0)),
    )
}

/// `LookupContainerPortNumberByName`: app containers first, then init
/// containers.
fn named_port(pod: &Pod, name: &str) -> Result<i32, String> {
    pod.spec
        .iter()
        .flat_map(|spec| {
            spec.containers
                .iter()
                .chain(spec.init_containers.iter().flatten())
        })
        .flat_map(|container| container.ports.iter().flatten())
        .find(|declared| declared.name.as_deref() == Some(name))
        .map(|declared| declared.container_port)
        .ok_or_else(|| {
            format!(
                "Pod '{}' does not have a named port '{name}'",
                pod.metadata.name.as_deref().unwrap_or_default()
            )
        })
}

/// `kubectl port-forward svc/name port` over a listener the caller bound,
/// except that each accepted connection resolves the Service afresh
/// ([`forward_target`] over a GET of the Service and a LIST of its
/// selector's Pods), so a forward outlives the Pod it first reached. A
/// connection's failure, resolution included, goes to `on_error`.
pub async fn port_forward(
    client: &Client,
    namespace: &str,
    name: &str,
    port: u16,
    listener: TcpListener,
    on_error: impl Fn(String) + Send + Sync + 'static,
) -> std::io::Result<()> {
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let name = name.to_string();
    crate::pod::serve_forwards(listener, on_error, move || {
        let (services, pods, name) = (services.clone(), pods.clone(), name.clone());
        async move {
            let service = services
                .get(&name)
                .await
                .map_err(|error| error.to_string())?;
            let selector = service
                .spec
                .iter()
                .flat_map(|spec| spec.selector.iter().flatten())
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(",");
            let matched = if selector.is_empty() {
                Vec::new()
            } else {
                pods.list(&ListParams::default().labels(&selector))
                    .await
                    .map_err(|error| error.to_string())?
                    .items
            };
            let target = forward_target(&service, &matched, port)?;
            Ok((pods, target.pod, target.port))
        }
    })
    .await
}
