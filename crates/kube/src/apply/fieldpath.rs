//! Normalization of Kubernetes field paths into a segment sequence.
//!
//! Two syntaxes show up in apiserver error payloads and must be unified
//! before drilling into a YAML document (#10):
//!
//! - status-cause paths from 422 validation errors, e.g.
//!   `spec.template.spec.containers[0].image` (no leading dot);
//! - SSA fieldpath syntax from 409 conflict messages, e.g.
//!   `.spec.containers[name="nginx"].image` (leading dot, listMapKey
//!   selectors with quoted values).
//!
//! Both parse into the same [`Segment`] sequence. Map keys containing dots
//! (label keys like `app.kubernetes.io/name`) arrive as separate segments
//! and are re-joined greedily at lookup time by the locator, so no escaping
//! is handled here.

/// One step down into a YAML document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Mapping key (`spec` in `.spec.replicas`).
    Key(String),
    /// Sequence index (`[0]` in `containers[0]`).
    Index(usize),
    /// listMapKey selector (`[name="nginx"]`; multiple keys for multi-key
    /// list maps: `[name="x",protocol="TCP"]`).
    Selector(Vec<(String, String)>),
}

impl std::fmt::Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Segment::Key(k) => write!(f, "{k}"),
            Segment::Index(i) => write!(f, "[{i}]"),
            Segment::Selector(pairs) => {
                let inner: Vec<String> =
                    pairs.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
                write!(f, "[{}]", inner.join(","))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FieldPathError {
    #[error("empty field path")]
    Empty,
    #[error("malformed bracket at {0:?}")]
    MalformedBracket(String),
    #[error("empty segment in {0:?}")]
    EmptySegment(String),
}

/// Parse a raw field path into segments. Accepts both syntax families and
/// their leading-dot variants.
pub fn parse_field_path(raw: &str) -> Result<Vec<Segment>, FieldPathError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(FieldPathError::Empty);
    }
    let mut segments = Vec::new();
    for element in split_top_level(raw, '.') {
        parse_element(element, &mut segments)?;
    }
    if segments.is_empty() {
        return Err(FieldPathError::Empty);
    }
    Ok(segments)
}

/// Split on `delimiter`, ignoring delimiters inside brackets or quotes
/// (selector values may contain both).
pub(crate) fn split_top_level(s: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '[' if !in_quotes => depth += 1,
            ']' if !in_quotes => depth = depth.saturating_sub(1),
            c if c == delimiter && depth == 0 && !in_quotes => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Parse one dot-separated element: a bare key, or a key followed by one or
/// more brackets (`containers[0]`, `containers[name="nginx"]`).
fn parse_element(element: &str, out: &mut Vec<Segment>) -> Result<(), FieldPathError> {
    let Some(bracket_at) = element.find('[') else {
        if element.is_empty() {
            // Leading dot (SSA syntax) or trailing/duplicate dots.
            return if out.is_empty() {
                Ok(())
            } else {
                Err(FieldPathError::EmptySegment(element.to_string()))
            };
        }
        out.push(Segment::Key(element.to_string()));
        return Ok(());
    };

    let key = &element[..bracket_at];
    if !key.is_empty() {
        out.push(Segment::Key(key.to_string()));
    }
    let mut rest = &element[bracket_at..];
    while !rest.is_empty() {
        let Some(close) = find_closing_bracket(rest) else {
            return Err(FieldPathError::MalformedBracket(element.to_string()));
        };
        let inner = &rest[1..close];
        if let Ok(index) = inner.parse::<usize>() {
            out.push(Segment::Index(index));
        } else {
            out.push(Segment::Selector(parse_selector(inner)?));
        }
        rest = &rest[close + 1..];
    }
    Ok(())
}

/// Find the `]` that closes the bracket at `s[0]`, skipping quoted text so
/// `]` inside a selector value does not terminate early.
fn find_closing_bracket(s: &str) -> Option<usize> {
    debug_assert!(s.starts_with('['));
    let mut in_quotes = false;
    for (i, c) in s.char_indices().skip(1) {
        match c {
            '"' => in_quotes = !in_quotes,
            ']' if !in_quotes => return Some(i),
            _ => {}
        }
    }
    None
}

/// Parse the inside of a selector bracket: `name="nginx"` or a
/// comma-separated list of `key="value"` pairs (multi-key list maps).
fn parse_selector(inner: &str) -> Result<Vec<(String, String)>, FieldPathError> {
    let mut pairs = Vec::new();
    let mut rest = inner;
    loop {
        let Some(eq) = rest.find('=') else {
            return Err(FieldPathError::MalformedBracket(inner.to_string()));
        };
        let key = rest[..eq].trim();
        let value_part = &rest[eq + 1..];
        if !value_part.starts_with('"') {
            return Err(FieldPathError::MalformedBracket(inner.to_string()));
        }
        let Some(end_quote) = value_part[1..].find('"') else {
            return Err(FieldPathError::MalformedBracket(inner.to_string()));
        };
        let value = &value_part[1..1 + end_quote];
        if key.is_empty() {
            return Err(FieldPathError::MalformedBracket(inner.to_string()));
        }
        pairs.push((key.to_string(), value.to_string()));
        rest = value_part[2 + end_quote..].trim_start();
        if rest.is_empty() {
            break;
        }
        let Some(after_comma) = rest.strip_prefix(',') else {
            return Err(FieldPathError::MalformedBracket(inner.to_string()));
        };
        rest = after_comma.trim_start();
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_cause_style_dotted_keys() {
        let path = parse_field_path("spec.template.spec.replicas").unwrap();
        assert_eq!(
            path,
            vec![
                Segment::Key("spec".into()),
                Segment::Key("template".into()),
                Segment::Key("spec".into()),
                Segment::Key("replicas".into()),
            ]
        );
    }

    #[test]
    fn status_cause_style_with_index() {
        let path = parse_field_path("spec.containers[0].image").unwrap();
        assert_eq!(
            path,
            vec![
                Segment::Key("spec".into()),
                Segment::Key("containers".into()),
                Segment::Index(0),
                Segment::Key("image".into()),
            ]
        );
    }

    #[test]
    fn ssa_style_with_leading_dot_and_selector() {
        let path = parse_field_path(".spec.containers[name=\"nginx\"].image").unwrap();
        assert_eq!(
            path,
            vec![
                Segment::Key("spec".into()),
                Segment::Key("containers".into()),
                Segment::Selector(vec![("name".into(), "nginx".into())]),
                Segment::Key("image".into()),
            ]
        );
    }

    #[test]
    fn ssa_style_multi_key_selector() {
        let path = parse_field_path(".spec.ports[name=\"http\",protocol=\"TCP\"]").unwrap();
        assert_eq!(
            path,
            vec![
                Segment::Key("spec".into()),
                Segment::Key("ports".into()),
                Segment::Selector(vec![
                    ("name".into(), "http".into()),
                    ("protocol".into(), "TCP".into()),
                ]),
            ]
        );
    }

    #[test]
    fn selector_value_may_contain_bracket_and_dot() {
        let path = parse_field_path(".spec.containers[name=\"weird]x.y\"]").unwrap();
        assert_eq!(
            path,
            vec![
                Segment::Key("spec".into()),
                Segment::Key("containers".into()),
                Segment::Selector(vec![("name".into(), "weird]x.y".into())]),
            ]
        );
    }

    #[test]
    fn consecutive_brackets_attach_to_one_key() {
        // Two-dimensional-ish: `matrix[0][1]` shows up for nested lists.
        let path = parse_field_path("matrix[0][1]").unwrap();
        assert_eq!(
            path,
            vec![
                Segment::Key("matrix".into()),
                Segment::Index(0),
                Segment::Index(1),
            ]
        );
    }

    #[test]
    fn rejects_empty_path() {
        assert_eq!(parse_field_path(""), Err(FieldPathError::Empty));
        assert_eq!(parse_field_path("."), Err(FieldPathError::Empty));
    }

    #[test]
    fn rejects_malformed_brackets() {
        assert!(matches!(
            parse_field_path("spec.containers["),
            Err(FieldPathError::MalformedBracket(_))
        ));
        assert!(matches!(
            parse_field_path("spec.containers[name=]"),
            Err(FieldPathError::MalformedBracket(_))
        ));
        assert!(matches!(
            parse_field_path("spec.containers[=nginx]"),
            Err(FieldPathError::MalformedBracket(_))
        ));
    }

    #[test]
    fn segment_display_roundtrips_syntax() {
        assert_eq!(Segment::Index(3).to_string(), "[3]");
        assert_eq!(
            Segment::Selector(vec![("name".into(), "nginx".into())]).to_string(),
            "[name=\"nginx\"]"
        );
    }
}
