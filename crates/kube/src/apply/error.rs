//! Parse apiserver apply failures (`kube::core::Status`) into structured
//! causes ready for line mapping.
//!
//! Two parsing paths per the #10 resolution:
//!
//! - **422 Invalid**: structured, iterate `details.causes[].field`.
//! - **409 Conflict**: no structured causes; parse the message in two layers
//!   — per-manager segments of the shape
//!   `conflicts with "<manager>" using <group/version>: <fields>` (singular
//!   `conflict` for exactly one), then a comma-separated fieldpath list
//!   inside each segment. Self-parsing managedFields was rejected in #10.
//!
//! Anything else (403, admission webhook, network) degrades to
//! [`ApplyError::Global`]; the UI shows the full message without line jumps.

use kube::core::Status;

use super::fieldpath::{Segment, parse_field_path, split_top_level};

/// One field-level cause with its normalized path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldCause {
    /// The field string exactly as the server sent it.
    pub raw_field: String,
    /// Normalized path; empty when the cause targets the whole object or
    /// the field string failed to parse (locator degrades to doc header).
    pub path: Vec<Segment>,
    pub message: String,
    pub reason: String,
}

impl FieldCause {
    /// Strict-decoding causes (unknown/duplicate field, surfaced by
    /// `fieldValidation=Strict` dry-run prechecks) render as warnings in the
    /// UI: the field is ignored by a real apply rather than failing it.
    /// Strict decoding puts the description in the message, not a dedicated
    /// reason, so match on the message shape — locked by golden tests.
    pub fn is_strict_decoding(&self) -> bool {
        self.message.starts_with("unknown field") || self.message.starts_with("duplicate field")
    }
}

/// A 409 conflict block attributed to one field manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerConflict {
    pub manager: String,
    /// `group/version` the conflicting manager applied with, e.g. `apps/v1`.
    pub group_version: String,
    pub causes: Vec<FieldCause>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// 422: per-field validation causes.
    Validation(Vec<FieldCause>),
    /// 409: SSA conflicts; force-apply is meaningful. `None` managers when
    /// the message failed to parse (fall back to showing the raw message).
    Conflict(Vec<ManagerConflict>),
    /// Unmappable: 403, admission webhooks, network, unparsable payloads.
    Global { message: String },
}

/// Classify and parse a failed apply's status body.
pub fn parse_status(status: &Status) -> ApplyError {
    if let Some(cause) = parse_typed_patch_failure(&status.message) {
        return ApplyError::Validation(vec![cause]);
    }
    match status.code {
        422 => {
            let causes = status
                .details
                .as_ref()
                .map(|d| {
                    d.causes
                        .iter()
                        .map(|c| FieldCause {
                            raw_field: c.field.clone(),
                            path: parse_field_path(&c.field).unwrap_or_default(),
                            message: c.message.clone(),
                            reason: c.reason.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if causes.is_empty() {
                ApplyError::Global {
                    message: status.message.clone(),
                }
            } else {
                ApplyError::Validation(causes)
            }
        }
        409 => ApplyError::Conflict(parse_conflict_message(&status.message)),
        _ => ApplyError::Global {
            message: if status.message.is_empty() {
                format!("request failed with status {}", status.code)
            } else {
                status.message.clone()
            },
        },
    }
}

/// SSA schema failures arrive as 500s, not 422s:
/// `failed to create typed patch object (<ns>/<name>; <g/v>): <path>: field
/// not declared in schema`. This is how unknown fields surface under
/// server-side apply (the strict-decoding 422 path never runs for apply
/// requests), and it fails real applies too — so map it to a hard,
/// locatable field cause instead of the global error bar.
fn parse_typed_patch_failure(message: &str) -> Option<FieldCause> {
    let rest = message.strip_prefix("failed to create typed patch object (")?;
    let rest = rest.split_once("): ").map(|(_, r)| r)?.trim();
    let path = rest.strip_suffix(": field not declared in schema")?;
    Some(FieldCause {
        raw_field: path.to_string(),
        path: parse_field_path(path).unwrap_or_default(),
        message: "field not declared in schema".to_string(),
        reason: "FieldValueInvalid".to_string(),
    })
}

/// Layer 1 of conflict parsing: split the message into per-manager blocks.
///
/// Real shapes (see golden tests):
/// - `Apply failed with 1 conflict: conflict with "m" using apps/v1: .a.b`
/// - `Apply failed with 2 conflicts: conflicts with "m" using v1: .a, .b`
/// - multiple managers joined by newlines.
fn parse_conflict_message(message: &str) -> Vec<ManagerConflict> {
    let mut conflicts = Vec::new();
    let mut rest = message;
    while let Some(marker_at) = find_conflict_marker(rest) {
        let block = &rest[marker_at..];
        let end = find_conflict_marker(&block[1..])
            .map(|i| 1 + i)
            .unwrap_or(block.len());
        if let Some(conflict) = parse_conflict_block(block[..end].trim()) {
            conflicts.push(conflict);
        }
        rest = &block[end..];
    }
    conflicts
}

/// Find the start of the earliest `conflict(s) with "` segment.
fn find_conflict_marker(s: &str) -> Option<usize> {
    ["conflicts with \"", "conflict with \""]
        .iter()
        .filter_map(|marker| s.find(marker))
        .min()
}

/// Layer 2: parse one `conflicts with "<manager>" [using <g/v>]: <fields>`
/// block. The `using <g/v>` clause is optional — some apiservers (e.g. the
/// v1.32 kind node) omit it.
fn parse_conflict_block(block: &str) -> Option<ManagerConflict> {
    let after_with = block
        .trim_start()
        .strip_prefix("conflicts with \"")
        .or_else(|| block.trim_start().strip_prefix("conflict with \""))?;
    let manager_end = after_with.find('"')?;
    let manager = &after_with[..manager_end];
    let after_manager = after_with[manager_end + 1..].trim_start();
    let (group_version, fields) = match after_manager.strip_prefix("using ") {
        Some(gv_and_fields) => {
            let (gv, fields) = gv_and_fields.split_once(':')?;
            (gv.trim().to_string(), fields)
        }
        None => (String::new(), after_manager.strip_prefix(':')?),
    };

    let causes = split_field_list(fields)
        .into_iter()
        .filter(|f| !f.is_empty())
        .map(|raw| FieldCause {
            raw_field: raw.to_string(),
            path: parse_field_path(raw).unwrap_or_default(),
            message: format!("conflicts with \"{manager}\""),
            reason: "Conflict".to_string(),
        })
        .collect();
    Some(ManagerConflict {
        manager: manager.to_string(),
        group_version: group_version.trim().to_string(),
        causes,
    })
}

/// Split a conflict block's field list on top-level commas, respecting
/// quotes and brackets inside fieldpath selectors.
fn split_field_list(fields: &str) -> Vec<&str> {
    split_top_level(fields, ',')
        .into_iter()
        .map(str::trim)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::response::{StatusCause, StatusDetails};

    fn status(code: u16, message: &str, causes: Vec<StatusCause>) -> Status {
        Status {
            code,
            message: message.to_string(),
            details: Some(StatusDetails {
                name: String::new(),
                group: String::new(),
                kind: String::new(),
                uid: String::new(),
                causes,
                retry_after_seconds: 0,
            }),
            ..Default::default()
        }
    }

    fn cause(field: &str, message: &str, reason: &str) -> StatusCause {
        StatusCause {
            field: field.to_string(),
            message: message.to_string(),
            reason: reason.to_string(),
        }
    }

    // ---- 422 ---------------------------------------------------------

    #[test]
    fn validation_422_maps_causes_to_field_paths() {
        let s = status(
            422,
            "Deployment.apps \"nginx\" is invalid",
            vec![
                cause(
                    "spec.template.spec.containers[0].image",
                    "Required value",
                    "FieldValueRequired",
                ),
                cause("spec.replicas", "Invalid value: -1", "FieldValueInvalid"),
            ],
        );
        match parse_status(&s) {
            ApplyError::Validation(causes) => {
                assert_eq!(causes.len(), 2);
                assert_eq!(
                    causes[0].path,
                    vec![
                        Segment::Key("spec".into()),
                        Segment::Key("template".into()),
                        Segment::Key("spec".into()),
                        Segment::Key("containers".into()),
                        Segment::Index(0),
                        Segment::Key("image".into()),
                    ]
                );
                assert_eq!(causes[1].message, "Invalid value: -1");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn validation_422_strict_decoding_causes_flag_as_warnings() {
        // Golden: strict decoding 422 cause structure (locked per #15).
        let s = status(
            422,
            "ConfigMap \"cm\" is invalid",
            vec![
                cause(
                    "spec.replcas",
                    "unknown field \"spec.replcas\"",
                    "FieldValueInvalid",
                ),
                cause(
                    "spec.replicas",
                    "duplicate field \"spec.replicas\"",
                    "FieldValueDuplicate",
                ),
            ],
        );
        match parse_status(&s) {
            ApplyError::Validation(causes) => {
                assert!(causes.iter().all(|c| c.is_strict_decoding()));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn non_strict_decoding_causes_are_not_flagged_as_warnings() {
        let s = status(
            422,
            "Deployment.apps \"nginx\" is invalid",
            vec![
                cause("spec.replicas", "Invalid value: -1", "FieldValueInvalid"),
                cause("spec.template", "Required value", "FieldValueRequired"),
            ],
        );
        match parse_status(&s) {
            ApplyError::Validation(causes) => {
                assert!(causes.iter().all(|c| !c.is_strict_decoding()));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn validation_422_without_causes_degrades_to_global() {
        let s = status(422, "something unparseable", vec![]);
        assert!(matches!(parse_status(&s), ApplyError::Global { .. }));
    }

    // ---- 409 golden messages ----------------------------------------
    //
    // Real apiserver conflict messages, locked per #10 so a format change
    // in a future Kubernetes release fails loudly here instead of silently
    // degrading to the global error bar.

    #[test]
    fn conflict_409_single_conflict_single_manager() {
        // kubernetes v1.29+ shape, one conflict.
        let s = status(
            409,
            "Apply failed with 1 conflict: conflict with \"kubectl-client-side-apply\" using apps/v1: .spec.replicas",
            vec![],
        );
        match parse_status(&s) {
            ApplyError::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                let c = &conflicts[0];
                assert_eq!(c.manager, "kubectl-client-side-apply");
                assert_eq!(c.group_version, "apps/v1");
                assert_eq!(
                    c.causes[0].path,
                    vec![Segment::Key("spec".into()), Segment::Key("replicas".into())]
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn conflict_409_multiple_fields_one_manager() {
        let s = status(
            409,
            "Apply failed with 2 conflicts: conflicts with \"kubectl\" using v1: .data.foo, .data.bar",
            vec![],
        );
        match parse_status(&s) {
            ApplyError::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].causes.len(), 2);
                assert_eq!(conflicts[0].causes[1].raw_field, ".data.bar");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn conflict_409_multiple_managers_newline_separated() {
        let s = status(
            409,
            "Apply failed with 2 conflicts: conflicts with \"helm\" using apps/v1: .spec.replicas\nconflicts with \"kubectl\" using apps/v1: .spec.template.spec.containers[name=\"nginx\"].image",
            vec![],
        );
        match parse_status(&s) {
            ApplyError::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 2);
                assert_eq!(conflicts[0].manager, "helm");
                assert_eq!(conflicts[1].manager, "kubectl");
                assert_eq!(
                    conflicts[1].causes[0].path,
                    vec![
                        Segment::Key("spec".into()),
                        Segment::Key("template".into()),
                        Segment::Key("spec".into()),
                        Segment::Key("containers".into()),
                        Segment::Selector(vec![("name".into(), "nginx".into())]),
                        Segment::Key("image".into()),
                    ]
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn conflict_409_adjacent_blocks_keep_full_field_list() {
        // Defensive: manager blocks concatenated without a whitespace
        // separator. The block boundary must not eat the last character of
        // the previous block's field list.
        let s = status(
            409,
            "Apply failed with 2 conflicts: conflicts with \"a\" using v1: .spec.replicasconflicts with \"b\" using v1: .spec.paused",
            vec![],
        );
        match parse_status(&s) {
            ApplyError::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 2);
                assert_eq!(conflicts[0].manager, "a");
                assert_eq!(conflicts[0].causes[0].raw_field, ".spec.replicas");
                assert_eq!(conflicts[1].manager, "b");
                assert_eq!(conflicts[1].causes[0].raw_field, ".spec.paused");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn conflict_409_unparsable_message_yields_no_conflicts() {
        let s = status(409, "the server had a conflict, good luck", vec![]);
        match parse_status(&s) {
            ApplyError::Conflict(conflicts) => assert!(conflicts.is_empty()),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn conflict_409_without_using_clause() {
        // Golden captured from kind (v1.32) during #21 e2e: this apiserver
        // omits the `using <g/v>` clause entirely.
        let s = status(
            409,
            "Apply failed with 1 conflict: conflict with \"e2e-foreign\": .data.value",
            vec![],
        );
        match parse_status(&s) {
            ApplyError::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                let c = &conflicts[0];
                assert_eq!(c.manager, "e2e-foreign");
                assert_eq!(c.group_version, "");
                assert_eq!(
                    c.causes[0].path,
                    vec![Segment::Key("data".into()), Segment::Key("value".into())]
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    // ---- typed patch failures (SSA schema errors) -----------------------

    #[test]
    fn typed_patch_undeclared_field_maps_to_field_cause() {
        // Golden captured from kind (v1.32) during #21 e2e: an unknown field
        // under SSA fails client-side-in-server with 500, not a strict-
        // decoding 422 — and it fails real applies too, so the cause is a
        // hard error, not a warning.
        let s = status(
            500,
            "failed to create typed patch object (e2e-x/cm; /v1, Kind=ConfigMap): .unknownField: field not declared in schema",
            vec![],
        );
        match parse_status(&s) {
            ApplyError::Validation(causes) => {
                assert_eq!(causes.len(), 1);
                assert_eq!(causes[0].raw_field, ".unknownField");
                assert_eq!(causes[0].path, vec![Segment::Key("unknownField".into())]);
                assert!(!causes[0].is_strict_decoding());
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn typed_patch_other_failures_degrade_to_global() {
        let s = status(
            500,
            "failed to create typed patch object (default/nginx; apps/v1, Kind=Deployment): some other problem",
            vec![],
        );
        assert!(matches!(parse_status(&s), ApplyError::Global { .. }));
    }

    // ---- other codes ---------------------------------------------------

    #[test]
    fn forbidden_403_is_global() {
        let s = status(403, "deployments.apps is forbidden", vec![]);
        match parse_status(&s) {
            ApplyError::Global { message } => assert!(message.contains("forbidden")),
            other => panic!("expected Global, got {other:?}"),
        }
    }

    #[test]
    fn conflict_field_list_respects_selector_commas() {
        let fields = ".spec.ports[name=\"http\",protocol=\"TCP\"], .spec.replicas";
        assert_eq!(
            split_field_list(fields),
            vec![
                ".spec.ports[name=\"http\",protocol=\"TCP\"]",
                ".spec.replicas"
            ]
        );
    }
}
