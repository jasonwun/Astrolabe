//! The Custom Resources navigation model (#16): CRDs grouped by API group,
//! each kind resolved at its preferred version against discovery.

use std::collections::BTreeMap;

use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceColumnDefinition, CustomResourceDefinition, CustomResourceDefinitionVersion,
};
use astrolabe_kube::kube::ResourceExt as _;
use astrolabe_kube::kube::discovery::ApiResource;

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
    /// Whether the kind is namespaced. One rule: a kind that is
    /// [`KindResource::Served`] takes discovery's answer (what the apiserver
    /// actually serves); any other kind takes the CRD's `spec.scope`, which
    /// cannot change after creation. Every consumer reads this field, not the
    /// CRD's scope string.
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
        let resolved = served
            .filter(|_| version.served)
            .and_then(|served| served.resolve(&spec.group, &version.name, &spec.names.kind));
        let resource = match (served, resolved) {
            (None, _) => KindResource::Undiscovered,
            (Some(_), None) => KindResource::NotServed,
            (Some(_), Some(resolved)) => KindResource::Served(resolved.resource.clone()),
        };
        let namespaced =
            resolved.map_or(spec.scope == "Namespaced", |resolved| resolved.namespaced);
        groups.entry(&spec.group).or_default().push(CustomKind {
            crd_name: crd.name_any(),
            group: spec.group.clone(),
            kind: spec.names.kind.clone(),
            plural: spec.names.plural.clone(),
            version: version.name.clone(),
            namespaced,
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
