use astrolabe::jsonpath::evaluate;
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
