//! A list is built from one definition per column (#99): its header, its
//! weight and its cell cannot drift apart.

use std::collections::BTreeMap;

use astrolabe::projection::{Column, ListColumn, ResourceRow, list_table};
use astrolabe::store::ObjectKey;
use astrolabe_kube::k8s_openapi::api::core::v1::ConfigMap;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

const COLUMNS: &[Column<ConfigMap>] = &[
    Column {
        header: "Name",
        weight: 2.,
        cell: |config, _| config.metadata.name.clone().unwrap_or_default(),
    },
    Column {
        header: "Keys",
        weight: 0.5,
        cell: |config, _| config.data.iter().flatten().count().to_string(),
    },
    Column {
        header: "Now",
        weight: 1.5,
        cell: |_, now| now.to_string(),
    },
];

fn config_map(name: &str, keys: &[&str]) -> ConfigMap {
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            ..ObjectMeta::default()
        },
        data: Some(
            keys.iter()
                .map(|key| (key.to_string(), String::new()))
                .collect::<BTreeMap<_, _>>(),
        ),
        ..ConfigMap::default()
    }
}

#[test]
fn the_table_carries_each_columns_header_with_its_weight() {
    let table = list_table(COLUMNS, &[], Timestamp::UNIX_EPOCH);

    assert_eq!(
        table.columns,
        [("Name", 2.), ("Keys", 0.5), ("Now", 1.5)].map(|(header, weight)| ListColumn {
            header: header.to_string(),
            weight,
        })
    );
    assert_eq!(table.headers(), ["Name", "Keys", "Now"]);
    assert!(table.rows.is_empty());
}

#[test]
fn each_object_is_one_row_with_one_cell_per_column_in_column_order() {
    let settings = config_map("settings", &["a", "b"]);
    let empty = config_map("empty", &[]);

    let table = list_table(COLUMNS, &[&settings, &empty], Timestamp::UNIX_EPOCH);

    assert_eq!(
        table.rows,
        [
            ResourceRow {
                key: ObjectKey::new(Some("default".to_string()), "settings"),
                cells: ["settings", "2", "1970-01-01T00:00:00Z"]
                    .map(String::from)
                    .to_vec(),
            },
            ResourceRow {
                key: ObjectKey::new(Some("default".to_string()), "empty"),
                cells: ["empty", "0", "1970-01-01T00:00:00Z"]
                    .map(String::from)
                    .to_vec(),
            },
        ]
    );
}
