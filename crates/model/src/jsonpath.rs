//! The JSONPath subset CRD `additionalPrinterColumns` use (Kubernetes
//! JSONPath, not RFC 9535): `.field`, `\.`-escaped dots, `[n]`, `[*]`,
//! `['field']` and `[?(@.path=="value")]` / `!=` filters, with optional
//! `{}` and `$`. Anything else selects nothing.

use serde_json::Value;

enum Segment {
    Field(String),
    Index(usize),
    Wildcard,
    Filter {
        path: Vec<String>,
        equals: bool,
        value: Value,
    },
}

/// Every value `path` selects in `root`; empty when nothing matches or the
/// path is outside the supported subset.
pub fn evaluate<'a>(path: &str, root: &'a Value) -> Vec<&'a Value> {
    let Some(segments) = parse(path) else {
        return Vec::new();
    };
    let mut current = vec![root];
    for segment in &segments {
        current = current
            .into_iter()
            .flat_map(|value| step(segment, value))
            .collect();
    }
    current
}

fn step<'a>(segment: &Segment, value: &'a Value) -> Vec<&'a Value> {
    match segment {
        Segment::Field(name) => value.get(name).into_iter().collect(),
        Segment::Index(index) => value.get(index).into_iter().collect(),
        Segment::Wildcard => match value {
            Value::Array(items) => items.iter().collect(),
            Value::Object(fields) => fields.values().collect(),
            _ => Vec::new(),
        },
        Segment::Filter {
            path,
            equals,
            value: expected,
        } => value
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| {
                let actual = path.iter().try_fold(*item, |node, field| node.get(field));
                actual.is_some_and(|actual| (actual == expected) == *equals)
            })
            .collect(),
    }
}

fn parse(path: &str) -> Option<Vec<Segment>> {
    let path = path.trim();
    let path = path
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(path);
    let path = path.strip_prefix('$').unwrap_or(path);
    let mut segments = Vec::new();
    let mut rest = path;
    while let Some(first) = rest.chars().next() {
        match first {
            '.' => {
                let (name, tail) = field_name(&rest[1..]);
                if name.is_empty() {
                    return None;
                }
                segments.push(Segment::Field(name));
                rest = tail;
            }
            '[' => {
                let close = bracket_end(rest)?;
                segments.push(bracket(&rest[1..close])?);
                rest = &rest[close + 1..];
            }
            _ => return None,
        }
    }
    Some(segments)
}

/// A dotted field name up to the next unescaped `.` or `[`.
fn field_name(input: &str) -> (String, &str) {
    let mut name = String::new();
    let mut chars = input.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    name.push(escaped);
                }
            }
            '.' | '[' => return (name, &input[index..]),
            _ => name.push(ch),
        }
    }
    (name, "")
}

/// Index of the `]` closing the bracket at the start of `input`, skipping
/// brackets and quotes inside a filter expression.
fn bracket_end(input: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    for (index, ch) in input.char_indices() {
        match (quote, ch) {
            (Some(open), ch) if ch == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(ch),
            (None, '[') => depth += 1,
            (None, ']') => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn bracket(inner: &str) -> Option<Segment> {
    let inner = inner.trim();
    if inner == "*" {
        return Some(Segment::Wildcard);
    }
    if let Ok(index) = inner.parse() {
        return Some(Segment::Index(index));
    }
    if let Some(name) = unquote(inner) {
        return Some(Segment::Field(name.to_string()));
    }
    let expression = inner.strip_prefix("?(")?.strip_suffix(')')?.trim();
    let (lhs, rhs, equals) = if let Some((lhs, rhs)) = expression.split_once("==") {
        (lhs, rhs, true)
    } else {
        let (lhs, rhs) = expression.split_once("!=")?;
        (lhs, rhs, false)
    };
    let path = lhs.trim().strip_prefix("@.")?;
    let rhs = rhs.trim();
    let value = match unquote(rhs) {
        Some(text) => Value::String(text.to_string()),
        None => serde_json::from_str(rhs).ok()?,
    };
    Some(Segment::Filter {
        path: path.split('.').map(str::to_string).collect(),
        equals,
        value,
    })
}

fn unquote(text: &str) -> Option<&str> {
    ['"', '\''].into_iter().find_map(|quote| {
        text.strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
    })
}
