//! API discovery (#16): run once on connect and again whenever the CRD set
//! changes; the result is cached and a failed refresh keeps the old one.

use std::collections::HashMap;

use kube::Client;
use kube::core::GroupVersionKind;
use kube::discovery::{ApiGroup, ApiResource, Discovery, Scope};

/// One served kind at one version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedResource {
    pub resource: ApiResource,
    pub namespaced: bool,
}

/// Every (group, version, kind) the apiserver serves.
#[derive(Clone, Debug, Default)]
pub struct ServedResources {
    by_gvk: HashMap<GroupVersionKind, ServedResource>,
}

impl ServedResources {
    pub fn new(resources: impl IntoIterator<Item = ServedResource>) -> Self {
        let by_gvk = resources
            .into_iter()
            .map(|served| {
                let gvk = GroupVersionKind::gvk(
                    &served.resource.group,
                    &served.resource.version,
                    &served.resource.kind,
                );
                (gvk, served)
            })
            .collect();
        Self { by_gvk }
    }

    fn from_discovery(discovery: &Discovery) -> Self {
        Self::new(discovery.groups().flat_map(|group| {
            group.versions().flat_map(|version| {
                group
                    .versioned_resources(version)
                    .into_iter()
                    .map(|(resource, capabilities)| ServedResource {
                        resource,
                        namespaced: capabilities.scope == Scope::Namespaced,
                    })
            })
        }))
    }

    pub fn resolve(&self, group: &str, version: &str, kind: &str) -> Option<&ServedResource> {
        self.by_gvk
            .get(&GroupVersionKind::gvk(group, version, kind))
    }
}

/// Aggregated discovery (2 requests, K8s ≥ 1.26), falling back to per-group
/// discovery on servers that do not support it.
///
/// A pre-aggregated server does not reject the request: the Accept header
/// (`ACCEPT_AGGREGATED_DISCOVERY_V2`) ends in a plain `application/json`
/// fallback, the server answers 200 with an `APIGroupList` / `APIVersions`,
/// and `APIGroupDiscoveryList.items` is `#[serde(default)]`, so
/// `run_aggregated` succeeds with no groups at all (verified against
/// kube-client 4.2.0 / kube-core 4.2.0, and a v1.25.16 kind apiserver via
/// `curl` through `kubectl proxy` with that Accept header). Every real
/// server serves the core group, so its absence is the "unsupported" signal.
pub async fn discover(client: &Client) -> kube::Result<ServedResources> {
    match Discovery::new(client.clone()).run_aggregated().await {
        Ok(discovery) if discovery.has_group(ApiGroup::CORE_GROUP) => {
            return Ok(ServedResources::from_discovery(&discovery));
        }
        Ok(_) => tracing::debug!("aggregated discovery unsupported; using per-group discovery"),
        Err(error) => {
            tracing::debug!(error = %error, "aggregated discovery failed; using per-group discovery")
        }
    }
    let discovery = Discovery::new(client.clone()).run().await?;
    Ok(ServedResources::from_discovery(&discovery))
}

/// Last successful discovery plus the failure of the latest refresh, if it
/// failed: a failed refresh never discards a previous result.
#[derive(Debug, Default)]
pub struct DiscoveryCache {
    served: Option<ServedResources>,
    failure: Option<String>,
}

impl DiscoveryCache {
    pub fn record(&mut self, result: Result<ServedResources, String>) {
        match result {
            Ok(served) => {
                self.served = Some(served);
                self.failure = None;
            }
            Err(error) => self.failure = Some(error),
        }
    }

    pub fn served(&self) -> Option<&ServedResources> {
        self.served.as_ref()
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
}
