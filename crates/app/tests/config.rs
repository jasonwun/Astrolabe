//! ConfigMaps and Secrets on the generic list/detail pipeline (#18), with
//! Freelens 088d085's columns and detail fields.

use astrolabe::projection::SummaryTable;
use astrolabe::projection::config::{
    SecretKey, config_map_detail, project_config_maps, project_secrets, secret_detail, secret_value,
};
use astrolabe_kube::k8s_openapi::api::core::v1::{ConfigMap, Secret};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

/// Synthetic ConfigMap (plausible shape, not captured from a server).
fn app_config() -> ConfigMap {
    serde_json::from_value(serde_json::json!({
        "metadata": {"name": "app", "namespace": "default", "uid": "uid-cm",
                     "creationTimestamp": "2026-10-22T11:00:00Z"},
        "data": {
            "mode": "production",
            "crlf": "on\r\n",
            "nginx.conf": "server {\n  listen 80;\n}\n",
            "long": "x".repeat(100)
        },
        "binaryData": {"logo.png": "iVBORw0KGgo="}
    }))
    .unwrap()
}

#[test]
fn config_maps_list_names_their_data_keys() {
    let config = app_config();

    let table = project_config_maps(&[&config], now());

    assert_eq!(table.columns, ["Name", "Keys", "Age"]);
    assert_eq!(
        table.rows[0].cells,
        ["app", "crlf, long, mode, nginx.conf", "60m"]
    );
}

#[test]
fn a_config_map_detail_previews_each_value() {
    let model = config_map_detail(&app_config(), now());

    assert_eq!(model.title, "ConfigMap default/app");
    assert_eq!(
        model.tables,
        [SummaryTable {
            title: "Data".to_string(),
            columns: ["Key", "Value"].map(String::from).to_vec(),
            rows: vec![
                vec!["crlf".to_string(), "on".to_string()],
                vec!["long".to_string(), format!("{}…", "x".repeat(80))],
                vec!["mode".to_string(), "production".to_string()],
                vec!["nginx.conf".to_string(), "server {…".to_string()],
            ],
        }]
    );
}

#[test]
fn a_config_map_without_data_has_no_data_table() {
    let mut config = app_config();
    config.data = None;
    config.immutable = Some(true);
    let model = config_map_detail(&config, now());
    assert!(model.status.is_empty());
    assert!(model.tables.is_empty());
}

/// Synthetic Secret (plausible shape, not captured from a server):
/// `password` is "hunter2", `cert` is not UTF-8.
fn db_secret() -> Secret {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Secret",
        "metadata": {"name": "db", "namespace": "default", "uid": "uid-secret",
                     "creationTimestamp": "2026-10-22T11:00:00Z",
                     "labels": {"app": "db", "tier": "data"}},
        "type": "Opaque",
        "data": {"password": "aHVudGVyMg==", "cert": "/w=="}
    }))
    .unwrap()
}

#[test]
fn secrets_list_follows_freelens_columns() {
    let secret = db_secret();

    let table = project_secrets(&[&secret], now());

    assert_eq!(table.columns, ["Name", "Keys", "Type", "Age"]);
    assert_eq!(
        table.rows[0].cells,
        ["db", "cert, password", "Opaque", "60m"]
    );
}

#[test]
fn a_secret_detail_lists_keys_without_their_values() {
    let model = secret_detail(&db_secret(), now());

    assert_eq!(model.title, "Secret default/db");
    assert_eq!(model.status, [("Type".to_string(), "Opaque".to_string())]);
    assert_eq!(
        model.secret_keys,
        [
            SecretKey {
                key: "cert".to_string(),
                bytes: 1
            },
            SecretKey {
                key: "password".to_string(),
                bytes: 7
            },
        ]
    );
    let debug = format!("{model:?}");
    assert!(!debug.contains("hunter2"), "{debug}");
}

#[test]
fn a_secret_yaml_preview_masks_its_values_but_edit_gets_them() {
    let model = secret_detail(&db_secret(), now());

    assert!(!model.yaml.contains("aHVudGVyMg=="), "{}", model.yaml);
    assert!(model.yaml.contains("password: <hidden>"), "{}", model.yaml);
    assert!(model.yaml.contains("name: db"), "{}", model.yaml);
    let edit = model.edit_yaml.expect("Edit YAML gets the real manifest");
    assert!(edit.contains("aHVudGVyMg=="), "{edit}");
}

#[test]
fn a_revealed_value_is_decoded() {
    let secret = db_secret();
    assert_eq!(
        secret_value(&secret, "password").as_deref(),
        Some("hunter2")
    );
    assert_eq!(
        secret_value(&secret, "cert").as_deref(),
        Some("<binary, 1 bytes>")
    );
    assert_eq!(secret_value(&secret, "missing"), None);
}

/// `kubectl apply` records the whole manifest, data included, in an
/// annotation; the preview masks it like the data itself.
#[test]
fn a_secret_last_applied_annotation_is_masked_too() {
    let mut secret = db_secret();
    let applied = r#"{"apiVersion":"v1","data":{"password":"aHVudGVyMg=="},"kind":"Secret"}"#;
    secret.metadata.annotations = Some(
        [
            (
                "kubectl.kubernetes.io/last-applied-configuration".to_string(),
                applied.to_string(),
            ),
            ("team".to_string(), "db".to_string()),
        ]
        .into(),
    );

    let model = secret_detail(&secret, now());

    assert_eq!(
        model.annotations,
        [
            "kubectl.kubernetes.io/last-applied-configuration=<hidden>",
            "team=db"
        ]
    );
    assert!(!model.yaml.contains("aHVudGVyMg=="), "{}", model.yaml);
    assert!(model.yaml.contains("team: db"), "{}", model.yaml);
    assert!(model.edit_yaml.unwrap().contains("aHVudGVyMg=="));
}
