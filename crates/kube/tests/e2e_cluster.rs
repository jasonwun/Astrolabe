//! End-to-end verification of the #21 apply loop against a real cluster.
//!
//! Requires a reachable cluster via the ambient kubeconfig (the dev loop
//! uses `kind-astrolabe-dev`). All tests are `#[ignore]`d so the default
//! suite stays hermetic; run them with:
//!
//! ```sh
//! cargo test -p astrolabe-kube --test e2e_cluster -- --ignored --test-threads=1
//! ```

use astrolabe_kube::apply::{ApplyError, Location, apply_documents, precheck_documents};
use astrolabe_kube::default_client;
use astrolabe_kube::kube;
use astrolabe_kube::kube::api::{Api, DeleteParams};
use astrolabe_kube::kube::core::{DynamicObject, GroupVersionKind};
use astrolabe_kube::kube::discovery::ApiResource;

/// Unique namespace per test run; deleted on drop-best-effort at the end.
fn unique_ns() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("e2e-{nanos:x}")
}

fn ns_doc(ns: &str) -> String {
    format!("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {ns}\n")
}

fn configmap_doc(ns: &str, name: &str, value: &str) -> String {
    format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {name}\n  namespace: {ns}\ndata:\n  value: {value}\n"
    )
}

fn deployment_doc(ns: &str, image_pull_policy: &str) -> String {
    format!(
        "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
  namespace: {ns}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: nginx
  template:
    metadata:
      labels:
        app: nginx
    spec:
      containers:
        - name: nginx
          image: nginx:1.27
          imagePullPolicy: {image_pull_policy}
"
    )
}

fn join_docs(docs: &[String]) -> String {
    docs.join("---\n")
}

/// 1-based line number of the first line containing `needle`.
fn line_of(snapshot: &str, needle: &str) -> usize {
    snapshot
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in snapshot"))
        + 1
}

async fn delete_ns(client: &kube::Client, ns: &str) {
    let gvk = GroupVersionKind::gvk("", "v1", "Namespace");
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &ApiResource::from_gvk(&gvk));
    let _ = api.delete(ns, &DeleteParams::default()).await;
}

/// Happy path: a multi-doc buffer (Namespace + ConfigMap + Deployment)
/// applies cleanly, outcomes arrive in send order, and the objects exist.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn apply_multi_doc_happy_path() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let snapshot = join_docs(&[
        ns_doc(&ns),
        configmap_doc(&ns, "cm", "hello"),
        deployment_doc(&ns, "IfNotPresent"),
    ]);

    let report = apply_documents(&client, &snapshot, false).await.unwrap();
    assert!(report.ok, "all docs should apply: {:?}", report.outcomes);
    assert_eq!(report.outcomes.len(), 3);
    assert_eq!(report.outcomes[0].doc, format!("v1 Namespace/{ns}"));
    assert_eq!(report.outcomes[1].doc, "v1 ConfigMap/cm");
    assert_eq!(report.outcomes[2].doc, "apps/v1 Deployment/nginx");
    for (i, o) in report.outcomes.iter().enumerate() {
        assert_eq!(o.doc_index, i);
        assert!(o.result.is_ok(), "doc {i}: {:?}", o.result);
    }

    let gvk = GroupVersionKind::gvk("", "v1", "ConfigMap");
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &ns, &ApiResource::from_gvk(&gvk));
    api.get("cm").await.expect("ConfigMap should exist");

    delete_ns(&client, &ns).await;
}

/// 422: an invalid enum value produces a Validation error whose cause maps
/// to the exact buffer line of the offending field, and the healthy docs in
/// the same buffer still apply (allSettled).
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn apply_422_maps_cause_to_field_line() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let snapshot = join_docs(&[
        ns_doc(&ns),
        configmap_doc(&ns, "cm", "hello"),
        deployment_doc(&ns, "NotARealPolicy"),
    ]);

    let report = apply_documents(&client, &snapshot, false).await.unwrap();
    assert!(!report.ok);
    assert!(report.outcomes[0].result.is_ok(), "Namespace applies");
    assert!(report.outcomes[1].result.is_ok(), "ConfigMap applies");

    let err = report.outcomes[2]
        .result
        .as_ref()
        .expect_err("Deployment must fail");
    let ApplyError::Validation(causes) = &err.error else {
        panic!("expected Validation, got {:?}", err.error);
    };
    let cause = causes
        .iter()
        .find(|c| c.raw_field.contains("imagePullPolicy"))
        .expect("a cause should name imagePullPolicy");
    assert!(cause.path.len() > 1, "path should parse: {cause:?}");

    let located = err
        .causes
        .iter()
        .find(|lc| lc.cause.raw_field.contains("imagePullPolicy"))
        .expect("cause should be located");
    assert_eq!(
        located.located.location,
        Location::Exact {
            line: line_of(&snapshot, "imagePullPolicy")
        },
        "cause should land on the imagePullPolicy line"
    );

    delete_ns(&client, &ns).await;
}

/// 409: a field owned by another manager conflicts; Force Apply wins.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn apply_409_then_force_apply() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    // Set up: namespace + ConfigMap owned by a *different* manager.
    let setup = join_docs(&[ns_doc(&ns), configmap_doc(&ns, "cm", "owned-by-other")]);
    let report = apply_documents(&client, &setup, false).await.unwrap();
    assert!(report.ok);

    // Re-own data.value under a foreign manager.
    let gvk = GroupVersionKind::gvk("", "v1", "ConfigMap");
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &ns, &ApiResource::from_gvk(&gvk));
    let foreign = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": "cm", "namespace": ns },
        "data": { "value": "foreign" },
    });
    api.patch(
        "cm",
        &kube::api::PatchParams::apply("e2e-foreign").force(),
        &kube::api::Patch::Apply(foreign),
    )
    .await
    .unwrap();

    // Our manager now conflicts on data.value.
    let snapshot = join_docs(&[configmap_doc(&ns, "cm", "ours")]);
    let report = apply_documents(&client, &snapshot, false).await.unwrap();
    assert!(!report.ok);
    let err = report.outcomes[0]
        .result
        .as_ref()
        .expect_err("apply must conflict");
    let ApplyError::Conflict(managers) = &err.error else {
        panic!("expected Conflict, got {:?}", err.error);
    };
    let conflict = managers
        .iter()
        .find(|m| m.manager == "e2e-foreign")
        .expect("conflict should name the foreign manager");
    assert!(
        conflict
            .causes
            .iter()
            .any(|c| c.raw_field.contains("value")),
        "conflict should name data.value: {conflict:?}"
    );
    let located = err
        .causes
        .iter()
        .find(|lc| lc.cause.raw_field.contains("value"))
        .expect("conflict cause should be located");
    assert_eq!(
        located.located.location,
        Location::Exact {
            line: line_of(&snapshot, "value:")
        }
    );

    // Force Apply takes ownership and succeeds.
    let report = apply_documents(&client, &snapshot, true).await.unwrap();
    assert!(
        report.ok,
        "force apply should succeed: {:?}",
        report.outcomes
    );
    let cm = api.get("cm").await.unwrap();
    assert_eq!(
        cm.data["data"]["value"].as_str().unwrap(),
        "ours",
        "force apply should win the field"
    );

    delete_ns(&client, &ns).await;
}

/// Unknown fields under SSA fail BOTH the dry-run precheck and the real
/// apply with a typed-patch schema error (verified against kind v1.32 —
/// the strict-decoding 422 path assumed by #15 never runs for apply
/// requests). The cause must be parsed, hard (not a warning), and located
/// on the offending line.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn unknown_field_fails_precheck_and_apply_with_located_cause() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let mut cm = configmap_doc(&ns, "cm", "hello");
    cm.push_str("unknownField: oops\n");
    let snapshot = join_docs(&[ns_doc(&ns), cm]);

    for (label, report) in [
        (
            "precheck",
            precheck_documents(&client, &snapshot).await.unwrap(),
        ),
        (
            "apply",
            apply_documents(&client, &snapshot, false).await.unwrap(),
        ),
    ] {
        assert!(!report.ok, "{label} should flag the unknown field");
        assert!(
            report.outcomes[0].result.is_ok(),
            "{label}: Namespace doc is fine"
        );
        let err = report.outcomes[1]
            .result
            .as_ref()
            .err()
            .unwrap_or_else(|| panic!("{label}: ConfigMap doc must fail"));
        let ApplyError::Validation(causes) = &err.error else {
            panic!("{label}: expected Validation, got {:?}", err.error);
        };
        let cause = causes
            .iter()
            .find(|c| c.raw_field.contains("unknownField"))
            .unwrap_or_else(|| panic!("{label}: a cause should name unknownField: {causes:?}"));
        assert!(
            !cause.is_strict_decoding(),
            "{label}: typed-patch failure is a hard error, not a warning"
        );

        let located = err
            .causes
            .iter()
            .find(|lc| lc.cause.raw_field.contains("unknownField"))
            .unwrap_or_else(|| panic!("{label}: cause should be located"));
        assert_eq!(
            located.located.location,
            Location::Exact {
                line: line_of(&snapshot, "unknownField")
            },
            "{label}: cause should land on the unknownField line"
        );
    }

    delete_ns(&client, &ns).await;
}
