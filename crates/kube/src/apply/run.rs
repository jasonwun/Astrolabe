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
    let outcomes = send_all(client, docs, snapshot, &params).await;
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
    let outcomes = send_all(client, docs, snapshot, &params).await;
    let ok = outcomes.iter().all(|o| o.result.is_ok());
    Ok(ApplyReport { outcomes, ok })
}

async fn send_all(
    client: &Client,
    docs: Vec<DocPayload>,
    snapshot: &str,
    params: &PatchParams,
) -> Vec<DocOutcome> {
    let mut outcomes = Vec::with_capacity(docs.len());
    for doc in docs {
        let result = apply_one(client, &doc, params).await.map_err(|error| {
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
}
