use astrolabe::projection::{ResourceRow, project_namespaces};
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, NamespaceStatus};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

fn namespace(name: &str, phase: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        status: Some(NamespaceStatus {
            phase: Some(phase.to_string()),
            ..NamespaceStatus::default()
        }),
        ..Namespace::default()
    }
}

#[test]
fn namespaces_project_to_handwritten_columns() {
    let active = namespace("default", "Active");
    let terminating = namespace("old", "Terminating");

    let rows = project_namespaces(&[&active, &terminating]);

    assert_eq!(
        rows,
        vec![
            ResourceRow {
                key: "default".to_string(),
                cells: vec!["default".to_string(), "Active".to_string()],
            },
            ResourceRow {
                key: "old".to_string(),
                cells: vec!["old".to_string(), "Terminating".to_string()],
            },
        ]
    );
}
