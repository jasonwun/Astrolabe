//! Built-in configuration kinds on the generic pipeline (#18): ConfigMaps
//! and Secrets, with Lens's list columns and detail fields (lensapp/lens
//! master, the `config-maps/` and `config-secrets/` components; Lens's
//! Namespace column is left out, as for every built-in kind). A Secret's
//! values stay masked everywhere the panel shows them until the user
//! reveals one.

use astrolabe_kube::k8s_openapi::ByteString;
use astrolabe_kube::k8s_openapi::api::core::v1::{ConfigMap, Secret};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use serde_json::Value;

use super::{DetailModel, ResourceRow, ResourceTable, SummaryTable, age_of, detail, object_yaml};
use crate::store::ObjectKey;

/// What a masked value shows.
pub const HIDDEN: &str = "<hidden>";

/// `kubectl apply` keeps the whole applied manifest here, a Secret's data
/// included.
const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

/// How much of a ConfigMap value the detail previews.
const PREVIEW_CHARS: usize = 80;

/// Name / Keys / Age, as Lens's ConfigMaps list shows them: the keys of
/// `data` (Lens leaves `binaryData` out).
pub fn project_config_maps(config_maps: &[&ConfigMap], now: Timestamp) -> ResourceTable {
    let rows = config_maps
        .iter()
        .map(|config| ResourceRow {
            key: ObjectKey::of(*config),
            cells: vec![
                config.metadata.name.clone().unwrap_or_default(),
                key_list(config.data.iter().flatten().map(|(key, _)| key)),
                age_of(&config.metadata, now),
            ],
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Keys", "Age"].map(String::from).to_vec(),
        rows,
    }
}

fn key_list<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    keys.map(String::as_str).collect::<Vec<_>>().join(", ")
}

fn binary(bytes: &ByteString) -> String {
    format!("<binary, {} bytes>", bytes.0.len())
}

/// A value's first line, cut at [`PREVIEW_CHARS`]; `…` marks what is left
/// out.
fn preview(value: &str) -> String {
    let line = value.lines().next().unwrap_or_default();
    let shown: String = line.chars().take(PREVIEW_CHARS).collect();
    if shown.len() < value.trim_end_matches(['\r', '\n']).len() {
        format!("{shown}…")
    } else {
        shown
    }
}

/// Lens's ConfigMap details: a Data table of `data`, each value
/// previewed (#12), and nothing else.
pub fn config_map_detail(config: &ConfigMap, now: Timestamp) -> DetailModel {
    let rows: Vec<Vec<String>> = config
        .data
        .iter()
        .flatten()
        .map(|(key, value)| vec![key.clone(), preview(value)])
        .collect();
    let value = serde_json::to_value(config).unwrap_or_default();
    let mut model = detail("ConfigMap", config, &value, Vec::new(), now);
    if rows.is_empty() {
        return model;
    }
    model.tables.push(SummaryTable {
        title: "Data".to_string(),
        columns: ["Key", "Value"].map(String::from).to_vec(),
        rows,
    });
    model
}

/// Name / Labels / Keys / Type / Age, as Lens's Secrets list shows them.
pub fn project_secrets(secrets: &[&Secret], now: Timestamp) -> ResourceTable {
    let rows = secrets
        .iter()
        .map(|secret| ResourceRow {
            key: ObjectKey::of(*secret),
            cells: vec![
                secret.metadata.name.clone().unwrap_or_default(),
                secret
                    .metadata
                    .labels
                    .iter()
                    .flatten()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                key_list(secret.data.iter().flatten().map(|(key, _)| key)),
                secret.type_.clone().unwrap_or_default(),
                age_of(&secret.metadata, now),
            ],
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Labels", "Keys", "Type", "Age"]
            .map(String::from)
            .to_vec(),
        rows,
    }
}

/// One of a Secret's keys, its value left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretKey {
    pub key: String,
    /// The decoded value's size.
    pub bytes: usize,
}

/// Status summary = type, plus the keys without their values. The YAML
/// preview masks every value (and the last-applied annotation that would
/// repeat them); Edit still gets the real manifest.
pub fn secret_detail(secret: &Secret, now: Timestamp) -> DetailModel {
    let summary = vec![("Type".to_string(), secret.type_.clone().unwrap_or_default())];
    let mut masked = secret.clone();
    if let Some(applied) = masked
        .metadata
        .annotations
        .as_mut()
        .and_then(|annotations| annotations.get_mut(LAST_APPLIED))
    {
        *applied = HIDDEN.to_string();
    }
    let value = serde_json::to_value(&masked).unwrap_or_default();
    let mut model = detail("Secret", &masked, &value, summary, now);
    model.secret_keys = secret
        .data
        .iter()
        .flatten()
        .map(|(key, value)| SecretKey {
            key: key.clone(),
            bytes: value.0.len(),
        })
        .collect();
    model.yaml = masked_yaml(value);
    model.edit_yaml = Some(object_yaml(secret));
    model
}

fn masked_yaml(mut value: Value) -> String {
    if let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("managedFields");
    }
    for field in ["data", "stringData"] {
        for entry in value
            .get_mut(field)
            .and_then(Value::as_object_mut)
            .into_iter()
            .flat_map(|data| data.values_mut())
        {
            *entry = Value::String(HIDDEN.to_string());
        }
    }
    serde_saphyr::to_string(&value).unwrap_or_else(|error| format!("# {error}"))
}

/// A revealed value: the decoded text, or its size when it is not UTF-8.
pub fn secret_value(secret: &Secret, key: &str) -> Option<String> {
    let bytes = secret.data.as_ref()?.get(key)?;
    Some(match std::str::from_utf8(&bytes.0) {
        Ok(text) => text.to_string(),
        Err(_) => binary(bytes),
    })
}
