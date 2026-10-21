//! Payload parse: split a (possibly multi-document) YAML buffer into
//! per-document JSON values for `Patch::Apply`.
//!
//! Runs on every apply, successful or not (#10 resolution: serde_saphyr is
//! the payload parser — the same crate family kube-client itself uses, so no
//! new dependency family enters the tree). Line-number mapping is a separate
//! parse that only runs on failure (`crate::apply::locate`).

/// One YAML document ready for server-side apply.
#[derive(Debug, Clone)]
pub struct DocPayload {
    /// Position of the document within the buffer (0-based, after skipping
    /// empty documents). Used to attribute apply results back to editor
    /// documents by send order.
    pub index: usize,
    /// The full document as JSON, fed verbatim into `Patch::Apply`.
    pub value: serde_json::Value,
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

impl DocPayload {
    /// `apps/v1 Deployment/nginx`-style identity for panels and logs.
    pub fn describe(&self) -> String {
        format!("{} {}/{}", self.api_version, self.kind, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// The buffer is not parseable YAML at all; `line` is the 1-based line
    /// serde_saphyr reports, when available.
    #[error("invalid YAML: {message}")]
    InvalidYaml {
        message: String,
        line: Option<usize>,
    },
    /// A document parsed but is not a Kubernetes object.
    #[error("document {index}: {message}")]
    InvalidDocument { index: usize, message: String },
}

/// Parse a buffer into applyable documents. Empty documents (bare `---`
/// separators, trailing newlines) are skipped without error.
pub fn parse_documents(buffer: &str) -> Result<Vec<DocPayload>, PayloadError> {
    let values: Vec<serde_json::Value> =
        serde_saphyr::from_multiple(buffer).map_err(|e| PayloadError::InvalidYaml {
            message: e.without_snippet().to_string(),
            line: e.location().map(|l| l.line() as usize),
        })?;

    let mut docs = Vec::with_capacity(values.len());
    for (index, value) in values.into_iter().enumerate() {
        if value.is_null() {
            continue;
        }
        docs.push(doc_payload(index, &value)?);
    }
    Ok(docs)
}

fn doc_payload(index: usize, value: &serde_json::Value) -> Result<DocPayload, PayloadError> {
    let invalid = |message: &str| PayloadError::InvalidDocument {
        index,
        message: message.to_string(),
    };
    let obj = value
        .as_object()
        .ok_or_else(|| invalid("document is not a mapping"))?;
    let get_str = |field: &str| -> Result<String, PayloadError> {
        obj.get(field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| invalid(&format!("missing or non-string `{field}`")))
    };
    let metadata = obj.get("metadata").and_then(|m| m.as_object());
    Ok(DocPayload {
        index,
        value: value.clone(),
        api_version: get_str("apiVersion")?,
        kind: get_str("kind")?,
        name: metadata
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| invalid("missing or non-string `metadata.name`"))?,
        namespace: metadata
            .and_then(|m| m.get("namespace"))
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_document() {
        let docs = parse_documents(
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n  namespace: default\n",
        )
        .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].index, 0);
        assert_eq!(docs[0].api_version, "v1");
        assert_eq!(docs[0].kind, "ConfigMap");
        assert_eq!(docs[0].name, "cm");
        assert_eq!(docs[0].namespace.as_deref(), Some("default"));
        assert_eq!(docs[0].value["kind"], "ConfigMap");
    }

    #[test]
    fn multi_document_indices_follow_send_order() {
        let docs = parse_documents(
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: first\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: second\n",
        )
        .unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].name, "first");
        assert_eq!(docs[1].name, "second");
        assert_eq!(docs[1].index, 1);
    }

    #[test]
    fn empty_documents_are_skipped() {
        let docs = parse_documents(
            "---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n---\n---\n",
        )
        .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].name, "cm");
    }

    #[test]
    fn rejects_malformed_yaml_with_line() {
        let err =
            parse_documents("apiVersion: v1\nkind: ConfigMap\nmetadata:\n\tname: tab-indent\n")
                .unwrap_err();
        match err {
            PayloadError::InvalidYaml { line, .. } => {
                assert!(line.is_some(), "parse error should carry a line: {line:?}")
            }
            other => panic!("expected InvalidYaml, got {other:?}"),
        }
    }

    #[test]
    fn rejects_non_mapping_document() {
        let err = parse_documents("- just\n- a\n- list\n").unwrap_err();
        assert!(matches!(
            err,
            PayloadError::InvalidDocument { index: 0, .. }
        ));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let err = parse_documents("apiVersion: v1\nmetadata:\n  name: x\n").unwrap_err();
        match err {
            PayloadError::InvalidDocument { message, .. } => {
                assert!(message.contains("kind"), "unexpected: {message}")
            }
            other => panic!("expected InvalidDocument, got {other:?}"),
        }
        let err = parse_documents("apiVersion: v1\nkind: ConfigMap\n").unwrap_err();
        match err {
            PayloadError::InvalidDocument { message, .. } => {
                assert!(message.contains("metadata.name"), "unexpected: {message}")
            }
            other => panic!("expected InvalidDocument, got {other:?}"),
        }
    }
}
