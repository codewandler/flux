//! Schema generation — the single source of truth for the Flux-Lang AST surface.
//!
//! The AST types in [`crate::ast`] derive [`schemars::JsonSchema`]; this module projects that into
//! (a) the full JSON Schema of the AST ([`ast_schema`]) and (b) the markdown node-kind catalog
//! ([`node_kind_catalog`]) that feeds the generated skill/docs and tooling. There is no
//! hand-maintained table and no build-time `syn` parsing: change a `Node` variant or its doc-comment
//! and every downstream surface updates automatically.

use crate::ast::{DraftAst, Node};

/// The full JSON Schema of the Draft AST, as a `serde_json::Value`. Memoized — the schema is a
/// compile-time constant, and `schema_for!` over the recursive AST is a non-trivial reflective build
/// that would otherwise be rebuilt by every tooling consumer.
pub fn ast_schema() -> serde_json::Value {
    static CELL: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(DraftAst)).expect("DraftAst schema serializes")
    })
    .clone()
}

/// The compact merged AST schema (L-71): [`ast_schema`] with the `Node` definition's
/// 43-variant `oneOf` collapsed into ONE object schema via [`merge_node_schema`]. Same wire format
/// (the internally-tagged `{"kind": …, …}` objects serde already speaks), a fraction of the tokens
/// — per-kind field/semantics documentation stays in [`node_kind_catalog`]. Retained for language
/// workbench experiments and external hosts; the agent does not ask a model to emit this schema.
/// Memoized like [`ast_schema`].
pub fn model_schema() -> serde_json::Value {
    static CELL: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut schema = ast_schema();
        merge_node_schema(&mut schema);
        schema
    })
    .clone()
}

/// Collapse the `Node` definition inside `schema` (any schema whose `$defs`/`definitions` map
/// carries the schemars-derived 43-variant `oneOf`) into a single object schema:
///
/// - `kind` becomes a `string` enum of every variant tag, in declaration order;
/// - the other properties are the **union** across variants, each declared once and all optional
///   (`required` is just `["kind"]`) — placement/requiredness stays with the analyzer + repair
///   loop, which are the enforcement authority in every emission arm;
/// - a property whose shape differs across variants (e.g. `branches`: `Branch` vs
///   `FallbackBranch`) merges to an `anyOf` of the distinct shapes; a shape that already accepts
///   anything (`lit.value`) absorbs the rest.
///
/// Purely a projection of the derived schema — the AST types, serde encoding, and every consumer of
/// [`ast_schema`] are untouched. A schema without a `oneOf` `Node` definition is left unchanged, so
/// the merge is idempotent. Works on both the bare [`ast_schema`] and a tool-input schema embedding
/// it, whichever defs key schemars emitted. This compact form is a language-workbench projection;
/// the agent runtime does not ask a model to generate it.
pub fn merge_node_schema(schema: &mut serde_json::Value) {
    let Some(defs_key) = ["$defs", "definitions"]
        .into_iter()
        .find(|k| schema.get(k).is_some())
    else {
        return;
    };
    let Some(node) = schema[defs_key].get("Node") else {
        return;
    };
    let Some(variants) = node.get("oneOf").and_then(|v| v.as_array()).cloned() else {
        return;
    };

    struct MergedProp {
        shapes: Vec<serde_json::Value>,
        /// The field description — kept only while every kind carrying the property agrees on it
        /// (a shared property's meaning is kind-dependent, and kind semantics are the node-kind
        /// catalog's job, not the merged schema's).
        desc: Option<serde_json::Value>,
        desc_consistent: bool,
    }
    let mut kinds: Vec<serde_json::Value> = Vec::new();
    // Property name → merged shape/description. A `Vec` keyed by linear search keeps first-seen
    // declaration order for ~60 properties (a map would reorder them).
    let mut merged_props: Vec<(String, MergedProp)> = Vec::new();
    for variant in &variants {
        let Some(props) = variant.get("properties").and_then(|p| p.as_object()) else {
            continue;
        };
        for (name, prop) in props {
            if name == "kind" {
                if let Some(tag) = variant_kind(variant) {
                    kinds.push(serde_json::Value::String(tag));
                }
                continue;
            }
            // Compare shapes with the field description split off, so the same shape documented
            // differently in two variants still merges to one declaration.
            let mut shape = prop.clone();
            let desc = shape.as_object_mut().and_then(|o| o.remove("description"));
            let entry = match merged_props.iter_mut().find(|(n, _)| n == name) {
                Some((_, entry)) => {
                    if entry.desc != desc {
                        entry.desc_consistent = false;
                    }
                    entry
                }
                None => {
                    merged_props.push((
                        name.clone(),
                        MergedProp {
                            shapes: Vec::new(),
                            desc,
                            desc_consistent: true,
                        },
                    ));
                    &mut merged_props.last_mut().expect("just pushed").1
                }
            };
            if !entry.shapes.contains(&shape) {
                entry.shapes.push(shape);
            }
        }
    }

    let mut properties = serde_json::Map::new();
    properties.insert(
        "kind".to_string(),
        serde_json::json!({
            "type": "string",
            "enum": kinds,
            "description": "Selects the node type. Only the fields that kind uses apply — see the \
                            node-kind catalog for each kind's fields and semantics.",
        }),
    );
    for (name, prop) in merged_props {
        let mut merged = if prop.shapes.iter().any(|s| s.as_bool() == Some(true)) {
            // One variant already accepts anything (`lit.value`) — the union is "anything".
            serde_json::json!({})
        } else if prop.shapes.len() == 1 {
            prop.shapes.into_iter().next().expect("one shape")
        } else {
            serde_json::json!({ "anyOf": prop.shapes })
        };
        if prop.desc_consistent {
            if let (Some(obj), Some(d)) = (merged.as_object_mut(), prop.desc) {
                obj.insert("description".to_string(), d);
            }
        }
        properties.insert(name, merged);
    }

    let description = format!(
        "{} Model-facing merged form: `kind` selects the node type and the remaining properties \
         are the union across all kinds — set only the fields your kind uses.",
        node.get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
    );
    schema[defs_key]["Node"] = serde_json::json!({
        "description": description.trim(),
        "type": "object",
        "properties": properties,
        "required": ["kind"],
    });
}

/// The `(kind, description)` pairs behind [`node_kind_catalog`], for consumers that need to
/// render the table differently than the verbatim catalog (e.g. escaping literal `|` characters
/// for a strict markdown-table renderer, as the website generator does).
pub fn node_kind_rows() -> Vec<(String, String)> {
    // Memoized: building this runs `schema_for!(Node)` (a full reflective build of the 40+-variant
    // AST enum) plus a per-variant walk. Cache the rows for docs, skills, LSP, and CLI consumers.
    static CELL: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let schema =
            serde_json::to_value(schemars::schema_for!(Node)).expect("Node schema serializes");
        let mut rows = Vec::new();
        if let Some(variants) = schema.get("oneOf").and_then(|v| v.as_array()) {
            for v in variants {
                let kind = variant_kind(v).unwrap_or_default();
                // Doc-comments arrive multi-line; collapse to one row the way the old build script did.
                let desc = v
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default()
                    .replace('\n', " ");
                rows.push((kind, desc));
            }
        }
        rows
    })
    .clone()
}

/// A markdown `| kind | description |` table of every [`Node`] variant, generated from the derived
/// schema's per-variant doc-comments. Replaces the former build-time `NODE_KIND_CATALOG` (the same
/// content, now derived from the type rather than parsed out of `ast.rs` by `syn`).
pub fn node_kind_catalog() -> String {
    static CELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut out = String::from("| kind | description |\n|---|---|\n");
        for (kind, desc) in node_kind_rows() {
            out.push_str(&format!("| `{kind}` | {desc} |\n"));
        }
        out
    })
    .clone()
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

        // Every language kind must have a row.
        for kind in [
            "call",
            "bind",
            "when",
            "repeat",
            "each",
            "assert",
            "pipe",
            "seq",
            "memo",
            "parallel",
            "await",
            "retry",
            "try",
            "confirm",
            "loop",
            "race",
            "throttle",
            "debounce",
            "unless",
            "verify",
            "return",
            "peek",
            "var",
            "lit",
            "thing",
            "expr",
            "fmt",
            "jq",
            "parse",
            "ctx",
            "ctx_append",
            "match",
            "route",
            "fallback",
            "timeout",
            "budget",
            "cap_scope",
            "scope",
            "saga",
            "once",
            "checkpoint",
            "obj",
            "list",
        ] {
            assert!(
                catalog.contains(&format!("| `{kind}` |")),
                "node-kind catalog is missing `{kind}`"
            );
        }

        // 43 variants + 2 header lines, and no description bleeds onto its own line (newlines collapsed).
        assert_eq!(
            catalog.lines().count(),
            43 + 2,
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

    fn defs(schema: &serde_json::Value) -> &serde_json::Value {
        schema
            .get("definitions")
            .or_else(|| schema.get("$defs"))
            .expect("schema carries a definitions map")
    }

    /// L-71: the merged model-facing schema collapses `Node` to ONE object whose `kind` enum covers
    /// every variant in declaration order — the same tags [`node_kind_rows`] derives, so a new or
    /// renamed variant can't silently fall out of the model surface.
    #[test]
    fn model_schema_kind_enum_matches_the_catalog() {
        let merged = model_schema();
        let node = &defs(&merged)["Node"];
        assert!(node.get("oneOf").is_none(), "the oneOf is merged away");
        assert_eq!(node["type"], "object");
        assert_eq!(node["required"], serde_json::json!(["kind"]));
        let kinds: Vec<String> = node["properties"]["kind"]["enum"]
            .as_array()
            .expect("kind is an enum")
            .iter()
            .map(|k| k.as_str().expect("kind tags are strings").to_string())
            .collect();
        let catalog: Vec<String> = node_kind_rows().into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, catalog, "kind enum = every variant, in order");
    }

    /// C-56: the generic merged schema cannot specialize args per operation, but it must put the
    /// named-object convention on the exact field the model fills instead of relying on remote
    /// system-prompt prose alone.
    #[test]
    fn model_schema_call_args_describes_named_object_convention() {
        let merged = model_schema();
        let description = defs(&merged)["Node"]["properties"]["args"]["description"]
            .as_str()
            .expect("call args carries model-facing guidance");
        assert!(description.contains("exactly one"));
        assert!(description.contains("named object"));
        assert!(description.contains("kind\":\"obj"));
    }

    /// Every property of every `oneOf` variant survives the merge — the union is complete, so no
    /// field a kind needs is hidden from the model.
    #[test]
    fn model_schema_unions_every_variant_property() {
        let strict = ast_schema();
        let merged = model_schema();
        let merged_props = defs(&merged)["Node"]["properties"]
            .as_object()
            .expect("merged Node has properties");
        for variant in defs(&strict)["Node"]["oneOf"]
            .as_array()
            .expect("strict Node is a oneOf")
        {
            let kind = variant_kind(variant).unwrap_or_default();
            for name in variant["properties"].as_object().expect("props").keys() {
                assert!(
                    merged_props.contains_key(name),
                    "merged Node is missing `{name}` (from `{kind}`)"
                );
            }
        }
    }

    /// The merge never leaves a dangling `$ref`, and it pays for itself: the merged schema is well
    /// under half the strict schema's serialized size (the measured motivation for the arm).
    #[test]
    fn model_schema_is_closed_and_much_smaller() {
        let merged = model_schema();
        let def_names: Vec<String> = defs(&merged)
            .as_object()
            .expect("defs map")
            .keys()
            .cloned()
            .collect();
        fn walk(v: &serde_json::Value, names: &[String]) {
            match v {
                serde_json::Value::Object(map) => {
                    if let Some(r) = map.get("$ref").and_then(|r| r.as_str()) {
                        let target = r.rsplit('/').next().unwrap_or_default();
                        assert!(
                            names.iter().any(|n| n == target),
                            "dangling $ref `{r}` after the merge"
                        );
                    }
                    map.values().for_each(|c| walk(c, names));
                }
                serde_json::Value::Array(arr) => arr.iter().for_each(|c| walk(c, names)),
                _ => {}
            }
        }
        walk(&merged, &def_names);

        let strict_len = ast_schema().to_string().len();
        let merged_len = merged.to_string().len();
        assert!(
            merged_len * 2 < strict_len,
            "merged schema ({merged_len} B) must be < 50% of the strict schema ({strict_len} B)"
        );
    }

    /// The merge is a targeted, idempotent projection: a second application is a no-op, and a
    /// schema without a `oneOf` `Node` definition passes through untouched.
    #[test]
    fn merge_node_schema_is_idempotent_and_tolerant() {
        let mut once = ast_schema();
        merge_node_schema(&mut once);
        let mut twice = once.clone();
        merge_node_schema(&mut twice);
        assert_eq!(
            once, twice,
            "re-merging an already-merged schema is a no-op"
        );

        let mut unrelated = serde_json::json!({ "type": "object" });
        merge_node_schema(&mut unrelated);
        assert_eq!(unrelated, serde_json::json!({ "type": "object" }));
    }
}
