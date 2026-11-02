#![expect(
    clippy::assert_is_empty,
    reason = "moved from the app with its assertions unchanged (#153)"
)]

use astrolabe_model::jsonpath::evaluate;
use serde_json::{Value, json};

fn object() -> Value {
    json!({
        "metadata": {"labels": {"app.kubernetes.io/name": "cron"}},
        "spec": {"replicas": 3, "cronSpec": "*/5 * * * *", "ports": [{"port": 80}, {"port": 443}]},
        "status": {"conditions": [
            {"type": "Synced", "status": "False"},
            {"type": "Ready", "status": "True"}
        ]}
    })
}

#[test]
fn printer_column_paths_select_values() {
    let object = object();
    assert_eq!(evaluate(".spec.replicas", &object), [&json!(3)]);
    assert_eq!(
        evaluate("{.spec.cronSpec}", &object),
        [&json!("*/5 * * * *")]
    );
    assert_eq!(evaluate(".spec.ports[1].port", &object), [&json!(443)]);
    assert_eq!(
        evaluate(".spec.ports[*].port", &object),
        [&json!(80), &json!(443)]
    );
    assert_eq!(
        evaluate(r#".status.conditions[?(@.type=="Ready")].status"#, &object),
        [&json!("True")]
    );
    assert_eq!(
        evaluate(r".metadata.labels.app\.kubernetes\.io/name", &object),
        [&json!("cron")]
    );
}

#[test]
fn missing_or_unsupported_paths_select_nothing() {
    let object = object();
    assert!(evaluate(".spec.missing", &object).is_empty());
    assert!(evaluate(".spec.ports[7].port", &object).is_empty());
    assert!(evaluate(r#".status.conditions[?(@.type=="Gone")].status"#, &object).is_empty());
    assert!(evaluate(".spec.replicas.deeper", &object).is_empty());
    assert!(evaluate(".spec.ports[", &object).is_empty());
}

#[test]
fn a_wildcard_selects_an_objects_values() {
    let object = object();
    assert_eq!(evaluate(".metadata.labels[*]", &object), [&json!("cron")]);
}

#[test]
fn a_bracket_inside_a_quoted_filter_value_does_not_close_the_filter() {
    let object = json!({"items": [
        {"name": "a]b", "id": 1},
        {"name": "it's", "id": 2}
    ]});
    assert_eq!(
        evaluate(r#".items[?(@.name=="a]b")].id"#, &object),
        [&json!(1)]
    );
    // The other quote kind does not end a quoted value.
    assert_eq!(
        evaluate(r#".items[?(@.name=="it's")].id"#, &object),
        [&json!(2)]
    );
}

#[test]
fn quoted_fields_and_filter_values_take_either_quote() {
    let object = object();
    assert_eq!(evaluate(".spec['replicas']", &object), [&json!(3)]);
    assert_eq!(evaluate(r#".spec["replicas"]"#, &object), [&json!(3)]);
    assert_eq!(
        evaluate(".status.conditions[?(@.type=='Ready')].status", &object),
        [&json!("True")]
    );
}
