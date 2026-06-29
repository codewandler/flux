//! `---`-delimited YAML frontmatter parsing.
//!
//! [`split_frontmatter`] separates the leading `---` block from the body (borrowing, BOM/CRLF
//! tolerant). [`parse_frontmatter`] deserializes that block into any serde type via [`serde_norway`]
//! — describe the format you expect as a struct and the type *is* the schema. A missing frontmatter
//! block deserializes from an empty mapping, so a type whose fields are all `#[serde(default)]`
//! parses leniently to its defaults.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// A parsed markdown document: typed frontmatter (`meta`) plus the remaining `body`.
#[derive(Debug, Clone)]
pub struct Document<T> {
    pub meta: T,
    pub body: String,
}

/// Split `---`-delimited YAML frontmatter from the body.
///
/// Returns `(Some(yaml), body)` when a leading `---` fence with a closing `\n---` line is present,
/// otherwise `(None, content)`. Tolerant of a leading UTF-8 BOM and CRLF line endings. The returned
/// slices borrow from `content` (no allocation).
pub fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let t = content.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("---") {
        // `rest` begins right after the opening `---`; find the closing fence.
        if let Some(end) = rest.find("\n---") {
            let fm = rest[..end].trim_start_matches(['\r', '\n']);
            let after = &rest[end + 4..]; // skip "\n---"
            let body = after.split_once('\n').map(|x| x.1).unwrap_or("");
            return (Some(fm), body);
        }
    }
    (None, t)
}

/// Parse `content` into typed frontmatter + body.
///
/// When no frontmatter block is present the metadata is deserialized from an empty YAML mapping, so
/// an all-optional `T` yields its defaults and the whole input becomes the body. Returns an error
/// only when a present frontmatter block is malformed YAML or is missing a field `T` requires.
pub fn parse_frontmatter<T: DeserializeOwned>(
    content: &str,
) -> Result<Document<T>, serde_norway::Error> {
    let (fm, body) = split_frontmatter(content);
    let meta: T = match fm {
        Some(yaml) => serde_norway::from_str(yaml)?,
        None => serde_norway::from_str("{}")?,
    };
    Ok(Document {
        meta,
        body: body.to_string(),
    })
}

/// Serialize `meta` into a `---`-delimited YAML frontmatter block (including both fences and a
/// trailing newline). The inverse of the block [`split_frontmatter`] extracts. Errors only if `meta`
/// is not YAML-serializable.
pub fn compose_frontmatter<T: Serialize>(meta: &T) -> Result<String, serde_norway::Error> {
    let yaml = serde_norway::to_string(meta)?;
    // `serde_norway` already ends its output with a newline; fence it.
    Ok(format!("---\n{yaml}---\n"))
}

/// Render a markdown document from typed frontmatter (`meta`) and a `body`, the inverse of
/// [`parse_frontmatter`]: a `---` YAML block followed by `body`. Round-trips —
/// `parse_frontmatter(&render_document(&m, b)?)` yields `m` and `b` (modulo a normalized trailing
/// newline on the body).
pub fn render_document<T: Serialize>(meta: &T, body: &str) -> Result<String, serde_norway::Error> {
    let front = compose_frontmatter(meta)?;
    Ok(format!("{front}{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
    #[serde(default)]
    struct Meta {
        name: String,
        tags: Vec<String>,
        metadata: BTreeMap<String, String>,
    }

    #[test]
    fn splits_and_parses_typed() {
        let doc: Document<Meta> =
            parse_frontmatter("---\nname: alpha\ntags: [x, y]\n---\nthe body\n").unwrap();
        assert_eq!(doc.meta.name, "alpha");
        assert_eq!(doc.meta.tags, vec!["x", "y"]);
        assert_eq!(doc.body.trim(), "the body");
    }

    #[test]
    fn nested_mapping() {
        let doc: Document<Meta> =
            parse_frontmatter("---\nname: a\nmetadata:\n  author: me\n  version: \"1.0\"\n---\nb")
                .unwrap();
        assert_eq!(
            doc.meta.metadata.get("author").map(String::as_str),
            Some("me")
        );
        assert_eq!(
            doc.meta.metadata.get("version").map(String::as_str),
            Some("1.0")
        );
    }

    #[test]
    fn no_frontmatter_uses_defaults() {
        let doc: Document<Meta> = parse_frontmatter("just a body, no fence").unwrap();
        assert_eq!(doc.meta, Meta::default());
        assert_eq!(doc.body, "just a body, no fence");
    }

    #[test]
    fn tolerates_bom_and_crlf() {
        let doc: Document<Meta> =
            parse_frontmatter("\u{feff}---\r\nname: beta\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(doc.meta.name, "beta");
        assert!(doc.body.contains("body"));
    }

    #[test]
    fn render_round_trips_through_parse() {
        let meta = Meta {
            name: "flux-plugins".into(),
            tags: vec!["gitlab".into(), "slack".into()],
            metadata: BTreeMap::from([("kind".into(), "generated".into())]),
        };
        let body = "# Installed plugins\n\nUse `flux plugin call`.\n";
        let rendered = render_document(&meta, body).unwrap();
        let doc: Document<Meta> = parse_frontmatter(&rendered).unwrap();
        assert_eq!(doc.meta, meta);
        assert_eq!(doc.body, body);
    }

    #[test]
    fn compose_frontmatter_is_fenced() {
        let meta = Meta {
            name: "x".into(),
            ..Meta::default()
        };
        let block = compose_frontmatter(&meta).unwrap();
        assert!(block.starts_with("---\n"));
        assert!(block.trim_end().ends_with("---"));
    }

    #[test]
    fn malformed_yaml_errors() {
        // A tab-indented mapping value is invalid YAML.
        let r: Result<Document<Meta>, _> = parse_frontmatter("---\nname: a\n\tbad: x\n---\nb");
        assert!(r.is_err());
    }
}
