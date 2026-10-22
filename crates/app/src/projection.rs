use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRow {
    pub key: String,
    pub cells: Vec<String>,
}

pub fn project_namespaces(namespaces: &[&Namespace]) -> Vec<ResourceRow> {
    namespaces
        .iter()
        .map(|namespace| {
            let name = namespace.metadata.name.clone().unwrap_or_default();
            let phase = namespace
                .status
                .as_ref()
                .and_then(|status| status.phase.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            ResourceRow {
                key: name.clone(),
                cells: vec![name, phase],
            }
        })
        .collect()
}
