//! Watch churn for the notify-cost measurement (#106,
//! `docs/agents/notify-cost.md`).
//!
//! ```text
//! cargo run --release -p astrolabe-kube --example churn -- CONTEXT NAMESPACE seed COUNT
//! cargo run --release -p astrolabe-kube --example churn -- CONTEXT NAMESPACE run RATE SECONDS
//! ```
//!
//! `seed` server-side-applies COUNT each of Secrets `s-N`, Services
//! `svc-N`, Deployments `d-N` (zero replicas) and ConfigMaps `cm-N`, the
//! steady objects the other watches hold. `run` cycles the ConfigMaps
//! `churn-N` through create, patch and delete at RATE operations a second
//! for SECONDS, each operation one watch event, and prints the rate it
//! achieved each second. The namespace must exist.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use k8s_openapi::NamespaceResourceScope;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, PodSpec, PodTemplateSpec, Secret, Service, ServicePort, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::api::{DeleteParams, Patch, PatchParams};
use kube::config::KubeConfigOptions;
use kube::{Api, Client, Config, Resource};
use tokio::sync::Semaphore;

/// The field manager both subcommands apply as.
const MANAGER: &str = "astrolabe-churn";

/// How many `churn-N` ConfigMaps `run` cycles through. A name's next
/// operation comes POOL operations after its last, so at 100/s the
/// create, patch and delete of one name are 3 s apart and never race.
const POOL: u64 = 300;

/// Requests in flight at most; above it `run` falls behind RATE and says so.
const IN_FLIGHT: usize = 64;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: churn CONTEXT NAMESPACE seed COUNT | run RATE SECONDS";
    let [context, namespace, command, rest @ ..] = args.as_slice() else {
        return Err(usage.into());
    };
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..KubeConfigOptions::default()
    };
    let client = Client::try_from(Config::from_kubeconfig(&options).await?)?;
    match (command.as_str(), rest) {
        ("seed", [count]) => seed(client, namespace, count.parse()?).await,
        ("run", [rate, seconds]) => run(client, namespace, rate.parse()?, seconds.parse()?).await,
        _ => Err(usage.into()),
    }
}

fn meta(namespace: &str, name: String) -> ObjectMeta {
    ObjectMeta {
        name: Some(name),
        namespace: Some(namespace.to_string()),
        ..ObjectMeta::default()
    }
}

/// The namespaced API of `object`'s kind.
fn api_for<K>(_: &K, client: &Client, namespace: &str) -> Api<K>
where
    K: Resource<DynamicType = (), Scope = NamespaceResourceScope>,
{
    Api::namespaced(client.clone(), namespace)
}

/// Server-side-applies `object`, a namespaced built-in, as [`MANAGER`].
macro_rules! apply {
    ($client:expr, $namespace:expr, $object:expr) => {{
        let object = $object;
        let api = api_for(&object, &$client, $namespace);
        let name = object.metadata.name.clone().unwrap_or_default();
        async move {
            api.patch(
                &name,
                &PatchParams::apply(MANAGER).force(),
                &Patch::Apply(&object),
            )
            .await
            .map(drop)
        }
    }};
}

async fn seed(client: Client, namespace: &str, count: u32) -> Result<(), Error> {
    for n in 0..count {
        let labels = BTreeMap::from([("app".to_string(), format!("d-{n}"))]);
        let secret = Secret {
            metadata: meta(namespace, format!("s-{n}")),
            string_data: Some(BTreeMap::from([("key".to_string(), format!("value-{n}"))])),
            ..Secret::default()
        };
        let service = Service {
            metadata: meta(namespace, format!("svc-{n}")),
            spec: Some(ServiceSpec {
                selector: Some(labels.clone()),
                ports: Some(vec![ServicePort {
                    port: 80,
                    ..ServicePort::default()
                }]),
                ..ServiceSpec::default()
            }),
            ..Service::default()
        };
        let deployment = Deployment {
            metadata: meta(namespace, format!("d-{n}")),
            spec: Some(DeploymentSpec {
                replicas: Some(0),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..LabelSelector::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels),
                        ..ObjectMeta::default()
                    }),
                    spec: Some(PodSpec {
                        containers: vec![Container {
                            name: "app".to_string(),
                            image: Some("busybox:1.36".to_string()),
                            ..Container::default()
                        }],
                        ..PodSpec::default()
                    }),
                },
                ..DeploymentSpec::default()
            }),
            ..Deployment::default()
        };
        let config_map = ConfigMap {
            metadata: meta(namespace, format!("cm-{n}")),
            data: Some(BTreeMap::from([("key".to_string(), format!("value-{n}"))])),
            ..ConfigMap::default()
        };
        tokio::try_join!(
            apply!(client, namespace, secret),
            apply!(client, namespace, service),
            apply!(client, namespace, deployment),
            apply!(client, namespace, config_map),
        )?;
    }
    println!("seeded {count} each of Secrets, Services, Deployments and ConfigMaps in {namespace}");
    Ok(())
}

/// Operation `k`: the `churn-N` it touches, and whether it creates (0),
/// patches (1) or deletes (2) it.
fn operation(k: u64) -> (String, u64) {
    (format!("churn-{}", k % POOL), (k / POOL) % 3)
}

async fn run(client: Client, namespace: &str, rate: u32, seconds: u64) -> Result<(), Error> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let permits = Arc::new(Semaphore::new(IN_FLIGHT));
    let done = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let mut ticks = tokio::time::interval(Duration::from_secs_f64(1. / f64::from(rate.max(1))));
    let start = Instant::now();
    let mut next_print = Duration::from_secs(1);
    let mut printed = 0;
    let mut k = 0;
    while start.elapsed() < Duration::from_secs(seconds) {
        ticks.tick().await;
        if rate == 0 {
            continue;
        }
        let permit = permits.clone().acquire_owned().await?;
        let (api, counts) = (api.clone(), (done.clone(), failed.clone()));
        let (name, phase) = operation(k);
        let namespace = namespace.to_string();
        tokio::spawn(async move {
            let outcome = match phase {
                2 => api.delete(&name, &DeleteParams::default()).await.map(drop),
                _ => {
                    let object = ConfigMap {
                        metadata: meta(&namespace, name.clone()),
                        data: Some(BTreeMap::from([("k".to_string(), k.to_string())])),
                        ..ConfigMap::default()
                    };
                    api.patch(
                        &name,
                        &PatchParams::apply(MANAGER).force(),
                        &Patch::Apply(&object),
                    )
                    .await
                    .map(drop)
                }
            };
            let counter = if outcome.is_ok() {
                &counts.0
            } else {
                &counts.1
            };
            counter.fetch_add(1, Ordering::Relaxed);
            drop(permit);
        });
        k += 1;
        if start.elapsed() >= next_print {
            let total = done.load(Ordering::Relaxed);
            println!(
                "{:>4}s {:>5} ops/s ({} failed)",
                next_print.as_secs(),
                total - printed,
                failed.load(Ordering::Relaxed)
            );
            printed = total;
            next_print += Duration::from_secs(1);
        }
    }
    let _ = permits.acquire_many(IN_FLIGHT as u32).await?;
    let total = done.load(Ordering::Relaxed);
    println!(
        "{total} ops in {:.1}s ({:.1}/s), {} failed",
        start.elapsed().as_secs_f64(),
        total as f64 / start.elapsed().as_secs_f64(),
        failed.load(Ordering::Relaxed)
    );
    Ok(())
}
