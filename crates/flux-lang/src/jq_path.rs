//! The deliberately small path language carried by [`crate::ast::Node::Jq`].
//!
//! The AST keeps ordinary object fields as dot segments (`.headers`), array indexes as numeric dot
//! segments (`.items.0`), and object keys that are not identifiers as JSON-string brackets
//! (`.headers["content-type"]`). JSON string encoding is the one escaping rule, so a dot, bracket,
//! quote, backslash, empty string, Unicode scalar, or numeric-looking object key is data rather than
//! syntax. Runtime parsing also accepts the legacy numeric-bracket AST spelling (`.items[0]`), but
//! [`native_field_path`] rejects it so formatting an AST never changes its path string on reparse.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathSegment {
    Key(String),
    Index { value: usize, spelling: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedPath {
    pub segments: Vec<PathSegment>,
    canonical: String,
    native: String,
}

fn is_identifier_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse the jq-compatible field/index subset used by the interpreter.
pub(crate) fn parse_path(path: &str) -> Result<ParsedPath, String> {
    let path = path.trim();
    if path.is_empty() || path == "." {
        return Ok(ParsedPath {
            segments: Vec::new(),
            canonical: String::new(),
            native: String::new(),
        });
    }

    let bytes = path.as_bytes();
    let mut pos = usize::from(bytes.first() == Some(&b'.'));
    let mut segments = Vec::new();
    let mut canonical = String::new();
    let mut native = String::new();

    while pos < bytes.len() {
        if bytes[pos] == b'[' {
            let bracket_start = pos;
            pos += 1;
            if pos >= bytes.len() {
                return Err("`jq` path: unmatched `[`".into());
            }

            if bytes[pos] == b'"' {
                let value_start = pos;
                let mut stream =
                    serde_json::Deserializer::from_str(&path[value_start..]).into_iter::<String>();
                let key = match stream.next() {
                    Some(Ok(key)) => key,
                    Some(Err(error)) => {
                        return Err(format!("`jq` path: invalid quoted key: {error}"));
                    }
                    None => return Err("`jq` path: unmatched `[`".into()),
                };
                pos = value_start + stream.byte_offset();
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                if bytes.get(pos) != Some(&b']') {
                    return Err("`jq` path: unmatched `[`".into());
                }
                pos += 1;

                let encoded = serde_json::to_string(&key)
                    .expect("serializing a Rust string to JSON cannot fail");
                if segments.is_empty() {
                    canonical.push('.');
                }
                canonical.push('[');
                canonical.push_str(&encoded);
                canonical.push(']');
                native.push('[');
                native.push_str(&encoded);
                native.push(']');
                segments.push(PathSegment::Key(key));
            } else {
                let Some(relative_end) = path[pos..].find(']') else {
                    return Err("`jq` path: unmatched `[`".into());
                };
                let end = pos + relative_end;
                let spelling = path[pos..end].trim();
                let value = spelling
                    .parse::<usize>()
                    .map_err(|_| format!("`jq` path: invalid index `{spelling}`"))?;
                pos = end + 1;

                // Numeric brackets remain runtime-compatible, but the AST's canonical spelling is
                // a numeric dot segment. This intentional mismatch makes the formatter fall back
                // to `@json` for a hand-built legacy path instead of silently changing the AST.
                canonical.push('.');
                canonical.push_str(spelling);
                native.push('[');
                native.push_str(spelling);
                native.push(']');
                segments.push(PathSegment::Index {
                    value,
                    spelling: spelling.to_string(),
                });
            }

            debug_assert!(pos > bracket_start);
        } else {
            let start = pos;
            while pos < bytes.len() && !matches!(bytes[pos], b'.' | b'[' | b']') {
                pos += 1;
            }
            let spelling = path[start..pos].trim();
            if spelling.is_empty() {
                return Err("`jq` path: empty field segment".into());
            }
            if spelling.bytes().all(|byte| byte.is_ascii_digit()) {
                let value = spelling
                    .parse::<usize>()
                    .map_err(|_| format!("`jq` path: invalid index `{spelling}`"))?;
                canonical.push('.');
                canonical.push_str(spelling);
                native.push('[');
                native.push_str(spelling);
                native.push(']');
                segments.push(PathSegment::Index {
                    value,
                    spelling: spelling.to_string(),
                });
            } else {
                if is_identifier_key(spelling) {
                    canonical.push('.');
                    canonical.push_str(spelling);
                } else {
                    let encoded = serde_json::to_string(spelling)
                        .expect("serializing a Rust string to JSON cannot fail");
                    if segments.is_empty() {
                        canonical.push('.');
                    }
                    canonical.push('[');
                    canonical.push_str(&encoded);
                    canonical.push(']');
                }
                native.push('.');
                native.push_str(spelling);
                segments.push(PathSegment::Key(spelling.to_string()));
            }
        }

        if pos == bytes.len() {
            break;
        }
        match bytes[pos] {
            b'.' => {
                pos += 1;
                if pos == bytes.len() {
                    return Err("`jq` path: empty field segment".into());
                }
            }
            b'[' => {}
            b']' => return Err("`jq` path: unmatched `]`".into()),
            _ => {
                return Err(format!(
                    "`jq` path: unexpected content after path segment at byte {pos}"
                ));
            }
        }
    }

    Ok(ParsedPath {
        segments,
        canonical,
        native,
    })
}

/// Render a canonical AST path as native source. Returns `None` for non-canonical/legacy spellings
/// so `parse(format(ast)) == ast` remains exact rather than merely semantically equivalent.
pub(crate) fn native_field_path(path: &str) -> Option<String> {
    let parsed = parse_path(path).ok()?;
    (!parsed.segments.is_empty() && parsed.canonical == path).then_some(parsed.native)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_keys_are_data_and_numeric_keys_are_not_indexes() {
        let parsed = parse_path(r#".[""]["雪"]["a.b"]["br[ack]"]["quote\"slash\\"]["0"]"#).unwrap();
        assert_eq!(
            parsed.segments,
            vec![
                PathSegment::Key("".into()),
                PathSegment::Key("雪".into()),
                PathSegment::Key("a.b".into()),
                PathSegment::Key("br[ack]".into()),
                PathSegment::Key("quote\"slash\\".into()),
                PathSegment::Key("0".into()),
            ]
        );
        assert_eq!(
            native_field_path(r#".[""]["雪"]["a.b"]["br[ack]"]["quote\"slash\\"]["0"]"#).as_deref(),
            Some(r#"[""]["雪"]["a.b"]["br[ack]"]["quote\"slash\\"]["0"]"#)
        );
    }

    #[test]
    fn legacy_numeric_brackets_execute_but_do_not_format_natively() {
        assert_eq!(
            parse_path(".items[0].name").unwrap().segments,
            vec![
                PathSegment::Key("items".into()),
                PathSegment::Index {
                    value: 0,
                    spelling: "0".into(),
                },
                PathSegment::Key("name".into()),
            ]
        );
        assert_eq!(native_field_path(".items[0].name"), None);
        assert_eq!(
            native_field_path(".items.0.name").as_deref(),
            Some(".items[0].name")
        );
    }

    #[test]
    fn ambiguous_unquoted_keys_do_not_format_as_native_source() {
        assert_eq!(native_field_path(".content-type"), None);
        assert_eq!(
            native_field_path(r#".["content-type"]"#).as_deref(),
            Some(r#"["content-type"]"#)
        );
    }

    /// C-320 review failing-first: `Node::Jq.path` is a public string, so malformed bytes after a
    /// complete bracket segment must be an ordinary diagnostic, never an `unreachable!` panic.
    #[test]
    fn malformed_suffixes_after_brackets_are_recoverable() {
        for path in [r#".a[0]suffix"#, r#".["key"] suffix"#] {
            let error = parse_path(path).expect_err("a bracket suffix is malformed");
            assert!(error.contains("unexpected"), "{path}: {error}");
            assert_eq!(native_field_path(path), None);
        }
    }
}
