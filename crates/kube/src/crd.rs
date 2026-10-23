//! The Custom Resources navigation model (#16): CRDs grouped by API group,
//! each kind resolved at its preferred version against discovery.

use std::collections::BTreeMap;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceColumnDefinition, CustomResourceDefinition, CustomResourceDefinitionVersion,
};
use kube::ResourceExt as _;
use kube::discovery::ApiResource;

use crate::discovery::ServedResources;

/// How a kind's preferred version resolves against discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindResource {
    Served(ApiResource),
    /// The apiserver does not serve the preferred version: the CRD marks it
    /// unserved, or discovery does not list it.
    NotServed,
    /// No discovery result yet.
    Undiscovered,
}

/// One CRD kind, pinned to its preferred version.
#[derive(Clone, Debug)]
pub struct CustomKind {
    pub crd_name: String,
    pub group: String,
    pub kind: String,
    pub plural: String,
    pub version: String,
    pub namespaced: bool,
    /// The preferred version's `additionalPrinterColumns`.
    pub columns: Vec<CustomResourceColumnDefinition>,
    pub resource: KindResource,
}

#[derive(Clone, Debug)]
pub struct CustomGroup {
    pub group: String,
    pub kinds: Vec<CustomKind>,
}

/// The preferred version: the storage version, as Freelens's
/// `getPreferedVersion` picks it — which is why it can be unserved.
pub fn preferred_version(
    crd: &CustomResourceDefinition,
) -> Option<&CustomResourceDefinitionVersion> {
    let versions = &crd.spec.versions;
    versions
        .iter()
        .find(|version| version.storage)
        .or_else(|| versions.first())
}

/// Group CRDs by API group (alphabetical), kinds alphabetical within.
pub fn catalog(
    crds: &[&CustomResourceDefinition],
    served: Option<&ServedResources>,
) -> Vec<CustomGroup> {
    let mut groups: BTreeMap<&str, Vec<CustomKind>> = BTreeMap::new();
    for crd in crds {
        let Some(version) = preferred_version(crd) else {
            continue;
        };
        let spec = &crd.spec;
        let resource = match served {
            None => KindResource::Undiscovered,
            Some(_) if !version.served => KindResource::NotServed,
            Some(served) => served
                .resolve(&spec.group, &version.name, &spec.names.kind)
                .map_or(KindResource::NotServed, |served| {
                    KindResource::Served(served.resource.clone())
                }),
        };
        groups.entry(&spec.group).or_default().push(CustomKind {
            crd_name: crd.name_any(),
            group: spec.group.clone(),
            kind: spec.names.kind.clone(),
            plural: spec.names.plural.clone(),
            version: version.name.clone(),
            namespaced: spec.scope == "Namespaced",
            columns: version
                .additional_printer_columns
                .clone()
                .unwrap_or_default(),
            resource,
        });
    }
    groups
        .into_iter()
        .map(|(group, mut kinds)| {
            kinds.sort_by(|a, b| a.kind.cmp(&b.kind));
            CustomGroup {
                group: group.to_string(),
                kinds,
            }
        })
        .collect()
}
