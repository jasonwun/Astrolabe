//! API discovery (#16): run once on connect and again whenever the CRD set
//! changes. Caching the result is the caller's job.

use std::collections::HashMap;

use kube::Client;
use kube::core::{GroupVersion, GroupVersionKind};
use kube::discovery::{ApiGroup, ApiResource, Discovery, Scope, pinned_group, pinned_kind};
use kube::error::DiscoveryError;

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

    fn from_groups<'a>(groups: impl IntoIterator<Item = &'a ApiGroup>) -> Self {
        Self::new(groups.into_iter().flat_map(|group| {
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
///
/// Per-group discovery skips a named group's version whose
/// `/apis/<group>/<version>` answers 404, as a CRD group mid-registration
/// does: it is listed under `/apis` before that path serves. Core
/// versions (`/api/<version>`) are not skipped.
///
/// # Errors
///
/// A [`kube::Error`] when per-group discovery fails other than by a 404
/// from `/apis/<group>/<version>`; a failed aggregated request only falls
/// back to it.
pub(crate) async fn discover(client: &Client) -> kube::Result<ServedResources> {
    match Discovery::new(client.clone()).run_aggregated().await {
        Ok(discovery) if discovery.has_group(ApiGroup::CORE_GROUP) => {
            return Ok(ServedResources::from_groups(discovery.groups()));
        }
        Ok(_) => tracing::debug!("aggregated discovery unsupported; using per-group discovery"),
        Err(error) => {
            tracing::debug!(error = %error, "aggregated discovery failed; using per-group discovery");
        }
    }
    discover_per_group(client).await
}

/// kube-client's `Discovery::run`, except that a named group's version
/// answering 404 is skipped with a warning instead of failing the whole
/// run. The request order and every other error match `Discovery::run`
/// (kube-client 4.2.0): `/apis`, each group version, `/api`, each core
/// version.
async fn discover_per_group(client: &Client) -> kube::Result<ServedResources> {
    let mut group_versions = Vec::new();
    for group in client.list_api_groups().await?.groups {
        if group.versions.is_empty() {
            return Err(kube::Error::Discovery(DiscoveryError::EmptyApiGroup(
                group.name,
            )));
        }
        for version in &group.versions {
            let group_version = GroupVersion::gv(&group.name, &version.version);
            match pinned_group(client, &group_version).await {
                Ok(pinned) => group_versions.push(pinned),
                Err(kube::Error::Api(status)) if status.code == 404 => {
                    tracing::warn!(
                        group_version = %group_version.api_version(),
                        "group version not served yet (404); skipping it in discovery"
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }
    let core = client.list_core_api_versions().await?;
    if core.versions.is_empty() {
        return Err(kube::Error::Discovery(DiscoveryError::EmptyApiGroup(
            ApiGroup::CORE_GROUP.to_string(),
        )));
    }
    for version in &core.versions {
        let group_version = GroupVersion::gv(ApiGroup::CORE_GROUP, version);
        group_versions.push(pinned_group(client, &group_version).await?);
    }
    Ok(ServedResources::from_groups(&group_versions))
}

/// Resolve `gvk` from `served` first, asking the apiserver (`pinned_kind`)
/// only for a kind `served` lacks, such as a CRD Discovery has not seen yet.
///
/// # Errors
///
/// A [`kube::Error`] when the kind is not in `served` and the apiserver
/// cannot resolve it either.
pub(crate) async fn resolve_kind(
    client: &Client,
    served: &ServedResources,
    gvk: &GroupVersionKind,
) -> kube::Result<(ApiResource, bool)> {
    if let Some(found) = served.resolve(&gvk.group, &gvk.version, &gvk.kind) {
        return Ok((found.resource.clone(), found.namespaced));
    }
    let (resource, capabilities) = pinned_kind(client, gvk).await?;
    Ok((resource, capabilities.scope == Scope::Namespaced))
}
