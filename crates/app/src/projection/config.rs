//! Built-in configuration kinds on the generic pipeline (#18): ConfigMaps
//! and Secrets. A Secret's values stay masked everywhere the panel shows
//! them until the user reveals one.

use std::collections::BTreeMap;

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

/// Name / Data / Keys / Age: `kubectl get configmaps` plus the key names.
pub fn project_config_maps(config_maps: &[&ConfigMap], now: Timestamp) -> ResourceTable {
    let rows = config_maps
        .iter()
        .map(|config| {
            let keys = config_entries(config);
            ResourceRow {
                key: ObjectKey::of(*config),
                cells: vec![
                    config.metadata.name.clone().unwrap_or_default(),
                    keys.len().to_string(),
                    key_list(keys.keys().copied()),
                    age_of(&config.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Data", "Keys", "Age"].map(String::from).to_vec(),
        rows,
    }
}

enum Entry<'a> {
    Text(&'a str),
    Binary(&'a ByteString),
}

/// `data` and `binaryData` together, by key (the API keeps them disjoint).
fn config_entries(config: &ConfigMap) -> BTreeMap<&str, Entry<'_>> {
    let text = config
        .data
        .iter()
        .flatten()
        .map(|(key, value)| (key.as_str(), Entry::Text(value)));
    let binary = config
        .binary_data
        .iter()
        .flatten()
        .map(|(key, value)| (key.as_str(), Entry::Binary(value)));
    text.chain(binary).collect()
}

fn key_list<'a>(keys: impl Iterator<Item = &'a str>) -> String {
    keys.collect::<Vec<_>>().join(", ")
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

/// Status summary = immutability when set, plus a Data table previewing
/// each value.
pub fn config_map_detail(config: &ConfigMap, now: Timestamp) -> DetailModel {
    let summary = config
        .immutable
        .map(|immutable| vec![("Immutable".to_string(), immutable.to_string())])
        .unwrap_or_default();
    let rows = config_entries(config)
        .into_iter()
        .map(|(key, entry)| {
            let value = match entry {
                Entry::Text(text) => preview(text),
                Entry::Binary(bytes) => binary(bytes),
            };
            vec![key.to_string(), value]
        })
        .collect();
    let value = serde_json::to_value(config).unwrap_or_default();
    let mut model = detail("ConfigMap", config, &value, summary, now);
    model.tables.push(SummaryTable {
        title: "Data".to_string(),
        columns: ["Key", "Value"].map(String::from).to_vec(),
        rows,
    });
    model
}

/// Name / Type / Data / Keys / Age: `kubectl get secrets` plus the key
/// names.
pub fn project_secrets(secrets: &[&Secret], now: Timestamp) -> ResourceTable {
    let rows = secrets
        .iter()
        .map(|secret| {
            let data = secret.data.clone().unwrap_or_default();
            ResourceRow {
                key: ObjectKey::of(*secret),
                cells: vec![
                    secret.metadata.name.clone().unwrap_or_default(),
                    secret.type_.clone().unwrap_or_default(),
                    data.len().to_string(),
                    key_list(data.keys().map(String::as_str)),
                    age_of(&secret.metadata, now),
                ],
            }
        })
        .collect();
    ResourceTable {
        columns: ["Name", "Type", "Data", "Keys", "Age"]
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
