//! `---`-delimited YAML frontmatter parsing.
//!
//! [`split_frontmatter`] separates the leading `---` block from the body (borrowing, BOM/CRLF
//! tolerant). [`parse_frontmatter`] deserializes that block into any serde type via [`serde_norway`]
//! — describe the format you expect as a struct and the type *is* the schema. A missing frontmatter
//! block deserializes from an empty mapping, so a type whose fields are all `#[serde(default)]`
//! parses leniently to its defaults.

use serde::de::DeserializeOwned;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Debug, Default, Deserialize, PartialEq)]
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
    fn malformed_yaml_errors() {
        // A tab-indented mapping value is invalid YAML.
        let r: Result<Document<Meta>, _> = parse_frontmatter("---\nname: a\n\tbad: x\n---\nb");
        assert!(r.is_err());
    }
}
