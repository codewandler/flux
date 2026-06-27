//! Schema generation — the single source of truth for the Flux-Lang AST surface.
//!
//! The AST types in [`crate::ast`] derive [`schemars::JsonSchema`]; this module projects that into
//! (a) the full JSON Schema of the AST ([`ast_schema`]) and (b) the markdown node-kind catalog
//! ([`node_kind_catalog`]) that feeds the planner prompt and the generated skill/docs. There is no
//! hand-maintained table and no build-time `syn` parsing: change a `Node` variant or its doc-comment
//! and every downstream surface updates automatically.

use crate::ast::{DraftAst, Node};

/// The full JSON Schema of the Draft AST, as a `serde_json::Value`.
pub fn ast_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(DraftAst)).expect("DraftAst schema serializes")
}

/// A markdown `| kind | description |` table of every [`Node`] variant, generated from the derived
/// schema's per-variant doc-comments. Replaces the former build-time `NODE_KIND_CATALOG` (the same
/// content, now derived from the type rather than parsed out of `ast.rs` by `syn`).
pub fn node_kind_catalog() -> String {
    let schema = serde_json::to_value(schemars::schema_for!(Node)).expect("Node schema serializes");
    let mut out = String::from("| kind | description |\n|---|---|\n");
    if let Some(variants) = schema.get("oneOf").and_then(|v| v.as_array()) {
        for v in variants {
            let kind = variant_kind(v).unwrap_or_default();
            // Doc-comments arrive multi-line; collapse to one row the way the old build script did.
            let desc = v
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .replace('\n', " ");
            out.push_str(&format!("| `{kind}` | {desc} |\n"));
        }
    }
    out
}

/// Extract the internally-tagged `kind` constant from a variant subschema, tolerating both the
/// `const` and single-element `enum` shapes schemars emits across versions.
fn variant_kind(variant: &serde_json::Value) -> Option<String> {
    let kind = variant.get("properties")?.get("kind")?;
    if let Some(c) = kind.get("const").and_then(|c| c.as_str()) {
        return Some(c.to_string());
    }
    kind.get("enum")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is one markdown row per `Node` variant, in declaration order, with the documented
    /// header. If a variant is added/renamed or a doc-comment edited, this table updates automatically;
    /// the count guard catches a variant that silently fails to schematize.
    #[test]
    fn node_kind_catalog_covers_every_variant() {
        let catalog = node_kind_catalog();
        assert!(catalog.starts_with("| kind | description |\n|---|---|\n"));

        // Every kind the planner relies on must have a row.
        for kind in [
            "call", "bind", "when", "repeat", "each", "assert", "pipe", "seq", "memo", "parallel",
            "await", "retry", "try", "confirm", "loop", "race", "throttle", "debounce", "unless",
            "verify", "return", "peek", "var", "lit", "thing", "expr", "fmt", "jq", "parse",
        ] {
            assert!(
                catalog.contains(&format!("| `{kind}` |")),
                "node-kind catalog is missing `{kind}`"
            );
        }

        // 29 variants + 2 header lines, and no description bleeds onto its own line (newlines collapsed).
        assert_eq!(
            catalog.lines().count(),
            29 + 2,
            "every variant is exactly one row"
        );
    }

    /// The first row is generated from the `Call` variant's doc-comment verbatim — proving the schema
    /// carries doc-comments through as descriptions (the property the whole SSOT relies on).
    #[test]
    fn descriptions_come_from_doc_comments() {
        let catalog = node_kind_catalog();
        assert!(catalog
            .contains("| `call` | Invoke a registered operation with argument expressions. |"));
    }

    /// The full AST schema is a real object schema (not the former `{"type":"object"}` placeholder),
    /// and references the `Node` definitions.
    #[test]
    fn ast_schema_is_a_real_schema() {
        let schema = ast_schema();
        assert_eq!(schema["type"], "object");
        let defs = schema
            .get("definitions")
            .or_else(|| schema.get("$defs"))
            .expect("schema carries a definitions map");
        assert!(defs.get("Node").is_some(), "Node is defined in the schema");
    }
}
