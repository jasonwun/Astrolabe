//! Map a normalized field path back to a line in the YAML buffer.
//!
//! Runs only when an apply failed (#10): the snapshot taken at apply time is
//! re-parsed with saphyr's span-carrying AST and the path is walked down.
//!
//! Parser choice note: the #10 resolution names `MarkedYamlOwned`, but in
//! saphyr 0.0.6 that type does not record spans for collection nodes (the
//! loader's `with_start_marker`/`with_end_marker` hooks are no-ops for it),
//! which would break ancestor-level mapping. The borrowing `MarkedYaml`
//! records all spans correctly and the snapshot outlives the walk, so this
//! module uses it.
//!
//! Degradation chain (each level is distinguishable in the UI):
//! 1. [`Location::Exact`] — the field's own line;
//! 2. [`Location::Ancestor`] — nearest existing ancestor, naming the missing
//!    field (e.g. `Required value` on an absent field);
//! 3. [`Location::DocHeader`] — the document's first line, when not even the
//!    first path segment resolves;
//! 4. [`Location::Global`] — nothing locatable (parse failure, doc gone).
//!
//! Anchors/aliases land on their natural saphyr spans: aliases are resolved
//! at load time and cloned with the use-site span, so a field reached
//! through `*alias` points at the alias use — the place the user edits.
//! Merge keys (`<<`) are resolved manually here (saphyr does not expand
//! them); a field reached through a merge notes the anchor name when the
//! source text at the merge value is a plain `*name` alias.

use saphyr::{LoadableYamlNode, MarkedYaml, YamlData};

use super::fieldpath::Segment;

/// Where a field cause landed in the buffer. Lines are 1-based absolute
/// lines across the whole (possibly multi-document) buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Exact {
        line: usize,
    },
    /// `missing` renders the first path segment that did not resolve.
    Ancestor {
        line: usize,
        missing: String,
    },
    DocHeader {
        line: usize,
    },
    Global,
}

/// Result of mapping one field cause onto the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub location: Location,
    /// Anchor name when the field was reached through a `<<: *name` merge.
    pub anchor_note: Option<String>,
}

/// Walk `path` down document `doc_index` of `snapshot`.
///
/// `doc_index` counts all documents in the buffer including empty ones, so
/// it lines up with [`crate::apply::DocPayload::index`] (which keeps the
/// original enumeration while skipping empty documents).
pub fn locate_field(snapshot: &str, doc_index: usize, path: &[Segment]) -> Located {
    let docs = match MarkedYaml::load_from_str(snapshot) {
        Ok(docs) => docs,
        Err(_) => {
            return Located {
                location: Location::Global,
                anchor_note: None,
            };
        }
    };
    let Some(root) = docs.get(doc_index) else {
        return Located {
            location: Location::Global,
            anchor_note: None,
        };
    };
    let header_line = root.span.start.line();
    if path.is_empty() {
        return Located {
            location: Location::DocHeader { line: header_line },
            anchor_note: None,
        };
    }

    let mut walker = Walker {
        snapshot,
        node: root,
        // Line the user associates with the current node: the key line we
        // arrived through, or the node's own span for sequence elements.
        line: header_line,
        anchor_note: None,
    };
    let mut i = 0;
    while i < path.len() {
        match walker.step(&path[i..]) {
            Ok(consumed) => i += consumed,
            Err(missing) => {
                return Located {
                    location: if walker.line == header_line && i == 0 {
                        Location::DocHeader { line: header_line }
                    } else {
                        Location::Ancestor {
                            line: walker.line,
                            missing,
                        }
                    },
                    anchor_note: walker.anchor_note,
                };
            }
        }
    }
    Located {
        location: Location::Exact { line: walker.line },
        anchor_note: walker.anchor_note,
    }
}

struct Walker<'a, 'input> {
    snapshot: &'a str,
    node: &'a MarkedYaml<'input>,
    line: usize,
    anchor_note: Option<String>,
}

impl<'a, 'input> Walker<'a, 'input> {
    /// Consume as many segments as one lookup step needs (a key lookup may
    /// re-join dotted keys, consuming several `Key` segments).
    fn step(&mut self, rest: &[Segment]) -> Result<usize, String> {
        match &rest[0] {
            Segment::Key(_) => self.step_key(rest),
            Segment::Index(index) => self.step_index(*index),
            Segment::Selector(pairs) => self.step_selector(pairs),
        }
    }

    fn step_key(&mut self, rest: &[Segment]) -> Result<usize, String> {
        let YamlData::Mapping(mapping) = &self.node.data else {
            return Err(rest[0].to_string());
        };
        // Greedy dotted-key re-join: label/annotation keys contain dots and
        // arrive as separate path segments. Try progressively longer
        // dot-joined keys against this mapping.
        let mut joined = String::new();
        let mut consumed = 0;
        for segment in rest {
            let Segment::Key(key) = segment else { break };
            if !joined.is_empty() {
                joined.push('.');
            }
            joined.push_str(key);
            consumed += 1;
            if let Some((key_node, value)) = mapping
                .iter()
                .find(|(k, _)| k.data.as_str() == Some(joined.as_str()))
            {
                self.node = value;
                self.line = key_node.span.start.line();
                return Ok(consumed);
            }
        }
        // Merge key: look the field up in `<<` expansions, deepest merge
        // first (later merge entries win in YAML, check in reverse order).
        if let Some(merge) = mapping
            .iter()
            .find(|(k, _)| k.data.as_str() == Some("<<"))
            .map(|(_, v)| v)
        {
            self.note_anchor(merge);
            let first_key = match &rest[0] {
                Segment::Key(k) => k.as_str(),
                _ => unreachable!(),
            };
            let merged: Vec<&MarkedYaml> = match &merge.data {
                YamlData::Sequence(seq) => seq.iter().collect(),
                _ => vec![merge],
            };
            for merged_map in merged.into_iter().rev() {
                if let YamlData::Mapping(m) = &merged_map.data
                    && let Some((key_node, value)) =
                        m.iter().find(|(k, _)| k.data.as_str() == Some(first_key))
                {
                    self.node = value;
                    self.line = key_node.span.start.line();
                    return Ok(1);
                }
            }
        }
        Err(rest[0].to_string())
    }

    fn step_index(&mut self, index: usize) -> Result<usize, String> {
        let YamlData::Sequence(seq) = &self.node.data else {
            return Err(format!("[{index}]"));
        };
        let Some(element) = seq.get(index) else {
            return Err(format!("[{index}]"));
        };
        self.node = element;
        self.line = element.span.start.line();
        Ok(1)
    }

    fn step_selector(&mut self, pairs: &[(String, String)]) -> Result<usize, String> {
        let missing = Segment::Selector(pairs.to_vec()).to_string();
        let YamlData::Sequence(seq) = &self.node.data else {
            return Err(missing);
        };
        for element in seq {
            let YamlData::Mapping(m) = &element.data else {
                continue;
            };
            let matches = pairs.iter().all(|(k, v)| {
                m.iter()
                    .find(|(key, _)| key.data.as_str() == Some(k.as_str()))
                    .is_some_and(|(_, value)| scalar_eq(&value.data, v))
            });
            if matches {
                self.line = element.span.start.line();
                self.node = element;
                return Ok(1);
            }
        }
        Err(missing)
    }

    /// If the merge value is a plain `*anchor` alias in the source, record
    /// its name for the panel note. saphyr pre-resolves aliases and exposes
    /// no anchor names, so read the raw text at the value's span.
    fn note_anchor(&mut self, merge_value: &MarkedYaml) {
        if self.anchor_note.is_some() {
            return;
        }
        let span = merge_value.span;
        let text = &self.snapshot[span.start.index()..span.end.index().min(self.snapshot.len())];
        if let Some(name) = text.trim().strip_prefix('*') {
            self.anchor_note = Some(name.to_string());
        }
    }
}

/// Selector values compare loosely: `name="nginx"` matches a string scalar,
/// but listMapKeys may also be integers or booleans (`port="80"`).
fn scalar_eq<'input>(data: &YamlData<'input, MarkedYaml<'input>>, expected: &str) -> bool {
    use saphyr::Scalar;
    match data {
        YamlData::Value(Scalar::String(s)) => s == expected,
        YamlData::Value(Scalar::Integer(i)) => i.to_string() == expected,
        YamlData::Value(Scalar::Boolean(b)) => b.to_string() == expected,
        YamlData::Representation(rep, _, _) => rep.trim_matches('"') == expected,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::fieldpath::parse_field_path;

    const DEPLOYMENT: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
spec:
  replicas: 2
  template:
    spec:
      containers:
        - name: nginx
          image: nginx:1.25
";

    fn locate(snapshot: &str, doc: usize, raw_path: &str) -> Located {
        locate_field(snapshot, doc, &parse_field_path(raw_path).unwrap())
    }

    #[test]
    fn exact_line_for_top_level_key() {
        let found = locate(DEPLOYMENT, 0, "spec.replicas");
        assert_eq!(
            found,
            Located {
                location: Location::Exact { line: 6 },
                anchor_note: None,
            }
        );
    }

    #[test]
    fn exact_line_through_sequence_index() {
        let found = locate(DEPLOYMENT, 0, "spec.template.spec.containers[0].image");
        assert_eq!(found.location, Location::Exact { line: 11 });
    }

    #[test]
    fn exact_line_through_listmapkey_selector() {
        let found = locate(
            DEPLOYMENT,
            0,
            ".spec.template.spec.containers[name=\"nginx\"].image",
        );
        assert_eq!(found.location, Location::Exact { line: 11 });
    }

    #[test]
    fn missing_field_lands_on_nearest_ancestor() {
        // `Required value` on a field absent from the manifest.
        let found = locate(DEPLOYMENT, 0, "spec.strategy.rollingUpdate");
        assert_eq!(
            found.location,
            Location::Ancestor {
                line: 5,
                missing: "strategy".to_string(),
            }
        );
    }

    #[test]
    fn missing_deep_field_names_the_missing_segment() {
        let found = locate(DEPLOYMENT, 0, "spec.template.spec.dnsPolicy");
        assert_eq!(
            found.location,
            Location::Ancestor {
                line: 8,
                missing: "dnsPolicy".to_string(),
            }
        );
    }

    #[test]
    fn unknown_first_segment_lands_on_doc_header() {
        let found = locate(DEPLOYMENT, 0, "bogus.field");
        assert_eq!(found.location, Location::DocHeader { line: 1 });
    }

    #[test]
    fn out_of_bounds_index_lands_on_the_sequence() {
        let found = locate(DEPLOYMENT, 0, "spec.template.spec.containers[3].image");
        match found.location {
            Location::Ancestor { line, missing } => {
                assert_eq!(line, 9, "the `containers:` key line");
                assert_eq!(missing, "[3]");
            }
            other => panic!("expected Ancestor, got {other:?}"),
        }
    }

    #[test]
    fn selector_miss_lands_on_the_sequence() {
        let found = locate(
            DEPLOYMENT,
            0,
            ".spec.template.spec.containers[name=\"sidecar\"].image",
        );
        match found.location {
            Location::Ancestor { line, missing } => {
                assert_eq!(line, 9);
                assert_eq!(missing, "[name=\"sidecar\"]");
            }
            other => panic!("expected Ancestor, got {other:?}"),
        }
    }

    #[test]
    fn multi_doc_absolute_lines() {
        let buffer =
            format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n---\n{DEPLOYMENT}");
        let found = locate(&buffer, 1, "spec.replicas");
        // Deployment doc starts at line 6; `replicas` is 5 lines below it.
        assert_eq!(found.location, Location::Exact { line: 11 });
    }

    #[test]
    fn dotted_map_keys_are_rejoined_greedily() {
        let buffer = "metadata:\n  labels:\n    app.kubernetes.io/name: nginx\n";
        let found = locate(buffer, 0, "metadata.labels.app.kubernetes.io/name");
        assert_eq!(found.location, Location::Exact { line: 3 });
    }

    #[test]
    fn alias_interior_field_lands_on_anchor_definition() {
        let buffer = "\
apiVersion: v1
kind: Pod
metadata: &meta
  name: nginx
spec:
  containers: []
other:
  metadata: *meta
";
        // Fields inside an aliased mapping keep definition-site spans: the
        // anchor definition is the single place the user edits the value.
        let found = locate(buffer, 0, "other.metadata.name");
        assert_eq!(found.location, Location::Exact { line: 4 });
    }

    #[test]
    fn alias_top_node_lands_on_use_site() {
        let buffer = "\
apiVersion: v1
kind: Pod
metadata: &meta
  name: nginx
other:
  metadata: *meta
";
        let found = locate(buffer, 0, "other.metadata");
        assert_eq!(found.location, Location::Exact { line: 6 });
    }

    #[test]
    fn merge_key_field_found_with_anchor_note() {
        let buffer = "\
apiVersion: v1
kind: Pod
metadata:
  name: nginx
base: &base
  imagePullPolicy: Always
spec:
  <<: *base
  containers: []
";
        // Merge-expanded fields land on the anchor definition line (#10),
        // and the panel notes the anchor the value came from.
        let found = locate(buffer, 0, "spec.imagePullPolicy");
        assert_eq!(found.location, Location::Exact { line: 6 });
        assert_eq!(found.anchor_note.as_deref(), Some("base"));
    }

    #[test]
    fn unparsable_snapshot_is_global() {
        let found = locate("a: [unclosed", 0, "a.b");
        assert_eq!(found.location, Location::Global);
    }

    #[test]
    fn missing_document_is_global() {
        let found = locate(DEPLOYMENT, 5, "spec.replicas");
        assert_eq!(found.location, Location::Global);
    }

    #[test]
    fn empty_path_is_doc_header() {
        let found = locate_field(DEPLOYMENT, 0, &[]);
        assert_eq!(found.location, Location::DocHeader { line: 1 });
    }
}
