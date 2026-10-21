//! SSA orchestration: send each document of a buffer as an independent
//! server-side apply, collect every outcome (allSettled semantics — one
//! doc's failure never blocks the others), and map failures back to buffer
//! lines on the snapshot that was applied.
//!
//! The buffer snapshot IS the payload the server saw; error mapping always
//! runs against it. Callers that kept editing may re-map against the current
//! buffer themselves (best-effort, same [`locate_field`] entry point).

use kube::api::{Patch, PatchParams};
use kube::core::{DynamicObject, GroupVersionKind};
use kube::{Api, Client, ResourceExt as _, discovery};

use super::error::{ApplyError, FieldCause, parse_status};
use super::locate::{Located, locate_field};
use super::payload::{self, DocPayload};

/// Field manager this app applies with (#21).
pub const FIELD_MANAGER: &str = "astrolabe";

/// A field cause with its mapped buffer location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedCause {
    pub cause: FieldCause,
    pub located: Located,
}

/// One document's apply/precheck outcome.
#[derive(Debug, Clone)]
pub struct DocOutcome {
    /// Index into the buffer's documents (see [`DocPayload::index`]).
    pub doc_index: usize,
    /// `apiVersion kind/name` identity for the panel.
    pub doc: String,
    pub result: Result<(), DocError>,
}

/// A failed (or failed-to-send) document operation.
#[derive(Debug, Clone)]
pub struct DocError {
    /// Parsed server error; `Global` for 403/webhook/network/unparsable.
    pub error: ApplyError,
    /// Causes mapped onto the snapshot. Empty for `Global` errors.
    pub causes: Vec<LocatedCause>,
}

/// Result of applying a whole buffer.
#[derive(Debug)]
pub struct ApplyReport {
    /// One entry per sent document, in send order.
    pub outcomes: Vec<DocOutcome>,
    /// True when every document applied cleanly.
    pub ok: bool,
}

/// Apply every document of `snapshot` with SSA (allSettled).
///
/// `force` re-sends with `force=true` (the panel's explicit Force Apply
/// button for 409s; never automatic).
pub async fn apply_documents(
    client: &Client,
    snapshot: &str,
    force: bool,
) -> Result<ApplyReport, payload::PayloadError> {
    let docs = payload::parse_documents(snapshot)?;
    let mut params = PatchParams::apply(FIELD_MANAGER);
    if force {
        params = params.force();
    }
    let outcomes = send_all(
        docs,
        snapshot,
        &ClusterApply {
            client,
            params: &params,
        },
    )
    .await;
    let ok = outcomes.iter().all(|o| o.result.is_ok());
    Ok(ApplyReport { outcomes, ok })
}

/// Dry-run precheck (#15): `dry_run` + strict validation. Same per-doc
/// allSettled semantics; strict-decoding causes are flagged for warning
/// rendering via [`FieldCause::is_strict_decoding`].
pub async fn precheck_documents(
    client: &Client,
    snapshot: &str,
) -> Result<ApplyReport, payload::PayloadError> {
    let docs = payload::parse_documents(snapshot)?;
    let params = PatchParams::apply(FIELD_MANAGER)
        .dry_run()
        .validation_strict();
    let outcomes = send_all(
        docs,
        snapshot,
        &ClusterApply {
            client,
            params: &params,
        },
    )
    .await;
    let ok = outcomes.iter().all(|o| o.result.is_ok());
    Ok(ApplyReport { outcomes, ok })
}

/// Per-document apply operation: the seam between orchestration and the
/// cluster call. Production uses [`ClusterApply`]; tests inject canned
/// outcomes.
trait DocApply {
    async fn apply(&self, doc: &DocPayload) -> Result<(), ApplyError>;
}

/// Applies each document to the cluster with the given patch params.
struct ClusterApply<'a> {
    client: &'a Client,
    params: &'a PatchParams,
}

impl DocApply for ClusterApply<'_> {
    async fn apply(&self, doc: &DocPayload) -> Result<(), ApplyError> {
        apply_one(self.client, doc, self.params).await
    }
}

/// Send each document through `apply`, collecting every outcome
/// (allSettled — one document's failure never blocks the others).
async fn send_all(docs: Vec<DocPayload>, snapshot: &str, apply: &impl DocApply) -> Vec<DocOutcome> {
    let mut outcomes = Vec::with_capacity(docs.len());
    for doc in docs {
        let result = apply.apply(&doc).await.map_err(|error| {
            let causes = located_causes(&error, snapshot, doc.index);
            DocError { error, causes }
        });
        outcomes.push(DocOutcome {
            doc_index: doc.index,
            doc: doc.describe(),
            result,
        });
    }
    outcomes
}

async fn apply_one(
    client: &Client,
    doc: &DocPayload,
    params: &PatchParams,
) -> Result<(), ApplyError> {
    let object: DynamicObject =
        serde_json::from_value(doc.value.clone()).map_err(|e| ApplyError::Global {
            message: format!("document is not a Kubernetes object: {e}"),
        })?;
    let (group, version) = match doc.api_version.split_once('/') {
        Some((group, version)) => (group, version),
        None => ("", doc.api_version.as_str()),
    };
    if version.is_empty() {
        return Err(ApplyError::Global {
            message: format!("invalid apiVersion {:?}", doc.api_version),
        });
    }
    let gvk = GroupVersionKind::gvk(group, version, &doc.kind);
    let (resource, caps) =
        discovery::pinned_kind(client, &gvk)
            .await
            .map_err(|e| ApplyError::Global {
                message: format!("failed to discover {}: {e}", gvk.kind),
            })?;

    let api = if caps.scope == discovery::Scope::Namespaced {
        Api::<DynamicObject>::namespaced_with(
            client.clone(),
            doc.namespace.as_deref().unwrap_or("default"),
            &resource,
        )
    } else {
        Api::<DynamicObject>::all_with(client.clone(), &resource)
    };

    api.patch(object.name_any().as_str(), params, &Patch::Apply(object))
        .await
        .map(|_| ())
        .map_err(|e| match e {
            kube::Error::Api(status) => parse_status(&status),
            other => ApplyError::Global {
                message: other.to_string(),
            },
        })
}

/// Map a parsed error's field causes onto the snapshot.
fn located_causes(error: &ApplyError, snapshot: &str, doc_index: usize) -> Vec<LocatedCause> {
    let map = |causes: &[FieldCause]| -> Vec<LocatedCause> {
        causes
            .iter()
            .map(|cause| LocatedCause {
                cause: cause.clone(),
                located: locate_field(snapshot, doc_index, &cause.path),
            })
            .collect()
    };
    match error {
        ApplyError::Validation(causes) => map(causes),
        ApplyError::Conflict(conflicts) => conflicts.iter().flat_map(|c| map(&c.causes)).collect(),
        ApplyError::Global { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::response::{Status, StatusCause, StatusDetails};

    const DOC: &str = "\
apiVersion: v1
kind: ConfigMap
metadata:
  name: cm
data:
  bad: value
";

    fn validation_error(field: &str, message: &str) -> ApplyError {
        let status = Status {
            code: 422,
            message: "ConfigMap \"cm\" is invalid".to_string(),
            details: Some(StatusDetails {
                name: String::new(),
                group: String::new(),
                kind: String::new(),
                uid: String::new(),
                causes: vec![StatusCause {
                    field: field.to_string(),
                    message: message.to_string(),
                    reason: "FieldValueInvalid".to_string(),
                }],
                retry_after_seconds: 0,
            }),
            ..Default::default()
        };
        parse_status(&status)
    }

    #[test]
    fn validation_causes_locate_onto_snapshot_lines() {
        let error = validation_error("data.bad", "Invalid value");
        let causes = located_causes(&error, DOC, 0);
        assert_eq!(causes.len(), 1);
        assert_eq!(
            causes[0].located.location,
            super::super::locate::Location::Exact { line: 6 }
        );
    }

    #[test]
    fn global_errors_have_no_causes() {
        let error = ApplyError::Global {
            message: "forbidden".into(),
        };
        assert!(located_causes(&error, DOC, 0).is_empty());
    }

    // ---- send_all orchestration (allSettled) -------------------------

    const TWO_DOCS: &str = "\
apiVersion: v1
kind: ConfigMap
metadata:
  name: first
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: second
data:
  bad: value
";

    #[tokio::test]
    async fn send_all_collects_every_outcome_all_settled() {
        struct FakeApply;
        impl DocApply for FakeApply {
            async fn apply(&self, doc: &DocPayload) -> Result<(), ApplyError> {
                if doc.name == "second" {
                    Err(validation_error("data.bad", "Invalid value"))
                } else {
                    Ok(())
                }
            }
        }
        let docs = payload::parse_documents(TWO_DOCS).unwrap();
        let outcomes = send_all(docs, TWO_DOCS, &FakeApply).await;
        // One entry per document, in send order, even though one failed.
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].doc_index, 0);
        assert_eq!(outcomes[0].doc, "v1 ConfigMap/first");
        assert!(outcomes[0].result.is_ok());
        assert_eq!(outcomes[1].doc_index, 1);
        assert_eq!(outcomes[1].doc, "v1 ConfigMap/second");
        let err = outcomes[1].result.as_ref().unwrap_err();
        assert!(matches!(err.error, ApplyError::Validation(_)));
        assert_eq!(err.causes.len(), 1);
        assert_eq!(
            err.causes[0].located.location,
            super::super::locate::Location::Exact { line: 11 }
        );
    }

    // ---- apply_one against a mock apiserver ---------------------------

    fn json_response(code: u16, body: &str) -> http::Response<kube::client::Body> {
        http::Response::builder()
            .status(code)
            .header("content-type", "application/json")
            .body(kube::client::Body::from(body.as_bytes().to_vec()))
            .unwrap()
    }

    fn unexpected_request() -> http::Response<kube::client::Body> {
        json_response(
            500,
            r#"{"kind":"Status","apiVersion":"v1","status":"Failure","code":500,"reason":"InternalError","message":"unexpected request"}"#,
        )
    }

    /// `Client` over a canned handler, plus a log of `METHOD path` pairs
    /// for every request the client made.
    fn mock_client(
        handler: impl Fn(&http::Request<kube::client::Body>) -> http::Response<kube::client::Body>
        + Send
        + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_in_handler = std::sync::Arc::clone(&seen);
        let service = tower::service_fn(move |req: http::Request<kube::client::Body>| {
            seen_in_handler
                .lock()
                .unwrap()
                .push(format!("{} {}", req.method(), req.uri().path()));
            let resp = handler(&req);
            std::future::ready(Ok::<_, std::convert::Infallible>(resp))
        });
        (Client::new(service, "default"), seen)
    }

    const DEPLOYMENTS_DISCOVERY: &str = r#"{
        "kind": "APIResourceList",
        "apiVersion": "v1",
        "groupVersion": "apps/v1",
        "resources": [
            {
                "name": "deployments",
                "singularName": "deployment",
                "namespaced": true,
                "kind": "Deployment",
                "verbs": ["create", "delete", "get", "list", "patch", "update", "watch"]
            }
        ]
    }"#;

    const DEPLOYMENT_OBJECT: &str = r#"{
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {"name": "nginx", "namespace": "default"}
    }"#;

    const NAMESPACES_DISCOVERY: &str = r#"{
        "kind": "APIResourceList",
        "apiVersion": "v1",
        "groupVersion": "v1",
        "resources": [
            {
                "name": "namespaces",
                "singularName": "namespace",
                "namespaced": false,
                "kind": "Namespace",
                "verbs": ["create", "delete", "get", "list", "patch", "update", "watch"]
            }
        ]
    }"#;

    const NAMESPACE_OBJECT: &str = r#"{
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {"name": "cluster-wide"}
    }"#;

    fn only_doc(snapshot: &str) -> DocPayload {
        payload::parse_documents(snapshot).unwrap().remove(0)
    }

    #[tokio::test]
    async fn apply_one_rejects_api_version_with_empty_version() {
        let (client, _) = mock_client(|_| unexpected_request());
        let doc = only_doc("apiVersion: apps/\nkind: Deployment\nmetadata:\n  name: nginx\n");
        let err = apply_one(&client, &doc, &PatchParams::apply("test"))
            .await
            .unwrap_err();
        match err {
            ApplyError::Global { message } => {
                assert!(message.contains("apiVersion"), "unexpected: {message}")
            }
            other => panic!("expected Global, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_one_patches_namespaced_kind_under_its_namespace() {
        let (client, seen) = mock_client(|req| match (req.method().as_str(), req.uri().path()) {
            ("GET", "/apis/apps/v1") => json_response(200, DEPLOYMENTS_DISCOVERY),
            ("PATCH", "/apis/apps/v1/namespaces/default/deployments/nginx") => {
                json_response(200, DEPLOYMENT_OBJECT)
            }
            _ => unexpected_request(),
        });
        let doc = only_doc("apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: nginx\n");
        apply_one(&client, &doc, &PatchParams::apply("test"))
            .await
            .unwrap();
        let requests = seen.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r == "PATCH /apis/apps/v1/namespaces/default/deployments/nginx"),
            "requests: {requests:?}"
        );
    }

    #[tokio::test]
    async fn apply_one_patches_cluster_scoped_kind_without_namespace() {
        let (client, seen) = mock_client(|req| match (req.method().as_str(), req.uri().path()) {
            ("GET", "/api/v1") => json_response(200, NAMESPACES_DISCOVERY),
            ("PATCH", "/api/v1/namespaces/cluster-wide") => json_response(200, NAMESPACE_OBJECT),
            _ => unexpected_request(),
        });
        let doc = only_doc("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: cluster-wide\n");
        apply_one(&client, &doc, &PatchParams::apply("test"))
            .await
            .unwrap();
        let requests = seen.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r == "PATCH /api/v1/namespaces/cluster-wide"),
            "requests: {requests:?}"
        );
    }

    #[tokio::test]
    async fn apply_one_classifies_422_as_validation() {
        let (client, _) = mock_client(|req| match (req.method().as_str(), req.uri().path()) {
            ("GET", "/apis/apps/v1") => json_response(200, DEPLOYMENTS_DISCOVERY),
            ("PATCH", _) => json_response(
                422,
                r#"{
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "message": "Deployment.apps \"nginx\" is invalid",
                    "reason": "Invalid",
                    "details": {
                        "name": "nginx",
                        "group": "apps",
                        "kind": "Deployment",
                        "causes": [
                            {
                                "reason": "FieldValueInvalid",
                                "message": "Invalid value: -1",
                                "field": "spec.replicas"
                            }
                        ]
                    },
                    "code": 422
                }"#,
            ),
            _ => unexpected_request(),
        });
        let doc = only_doc("apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: nginx\n");
        let err = apply_one(&client, &doc, &PatchParams::apply("test"))
            .await
            .unwrap_err();
        match err {
            ApplyError::Validation(causes) => {
                assert_eq!(causes.len(), 1);
                assert_eq!(causes[0].raw_field, "spec.replicas");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_documents_aggregates_per_doc_outcomes() {
        let (client, _) = mock_client(|req| match (req.method().as_str(), req.uri().path()) {
            ("GET", "/api/v1") => json_response(
                200,
                r#"{
                    "kind": "APIResourceList",
                    "apiVersion": "v1",
                    "groupVersion": "v1",
                    "resources": [
                        {
                            "name": "configmaps",
                            "singularName": "configmap",
                            "namespaced": true,
                            "kind": "ConfigMap",
                            "verbs": ["create", "delete", "get", "list", "patch", "update", "watch"]
                        }
                    ]
                }"#,
            ),
            ("PATCH", "/api/v1/namespaces/default/configmaps/first") => json_response(
                200,
                r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"first","namespace":"default"}}"#,
            ),
            ("PATCH", "/api/v1/namespaces/default/configmaps/second") => json_response(
                422,
                r#"{
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "message": "ConfigMap \"second\" is invalid",
                    "reason": "Invalid",
                    "details": {
                        "name": "second",
                        "kind": "ConfigMap",
                        "causes": [
                            {
                                "reason": "FieldValueInvalid",
                                "message": "Invalid value",
                                "field": "data.bad"
                            }
                        ]
                    },
                    "code": 422
                }"#,
            ),
            _ => unexpected_request(),
        });
        let report = apply_documents(&client, TWO_DOCS, false).await.unwrap();
        assert!(!report.ok);
        assert_eq!(report.outcomes.len(), 2);
        assert_eq!(report.outcomes[0].doc, "v1 ConfigMap/first");
        assert!(report.outcomes[0].result.is_ok());
        let err = report.outcomes[1].result.as_ref().unwrap_err();
        assert!(matches!(err.error, ApplyError::Validation(_)));
        assert_eq!(
            err.causes[0].located.location,
            super::super::locate::Location::Exact { line: 11 }
        );
    }
}
