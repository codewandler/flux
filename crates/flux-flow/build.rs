//! Build script: parse the `Node` enum in `src/ast.rs` via `syn`, extract every variant's
//! doc-comment, and write `$OUT_DIR/node_kinds.rs` containing a `pub const NODE_KIND_CATALOG`
//! markdown table. Included by `lib.rs` — zero hand-maintenance, auto-updates on new variants.

use std::{env, fs, path::Path};

fn main() {
    // Re-run only when ast.rs changes.
    println!("cargo:rerun-if-changed=src/ast.rs");

    let ast_src = fs::read_to_string("src/ast.rs").expect("read src/ast.rs");
    let file: syn::File = syn::parse_str(&ast_src).expect("parse src/ast.rs");

    // Find the `Node` enum.
    let node_enum = file
        .items
        .iter()
        .find_map(|item| {
            if let syn::Item::Enum(e) = item {
                if e.ident == "Node" {
                    return Some(e);
                }
            }
            None
        })
        .expect("Node enum not found in src/ast.rs");

    // Build a markdown table row per variant.
    let mut rows = String::new();
    rows.push_str("| kind | description |\n");
    rows.push_str("|---|---|\n");
    for variant in &node_enum.variants {
        // The serde rename_all = snake_case rule: convert CamelCase to snake_case.
        let kind = to_snake_case(&variant.ident.to_string());
        // Collect doc-comment lines.
        let doc: String = variant
            .attrs
            .iter()
            .filter_map(|a| {
                if a.path().is_ident("doc") {
                    if let syn::Meta::NameValue(nv) = &a.meta {
                        if let syn::Expr::Lit(el) = &nv.value {
                            if let syn::Lit::Str(s) = &el.lit {
                                return Some(s.value().trim().to_string());
                            }
                        }
                    }
                }
                None
            })
            .collect::<Vec<_>>()
            .join(" ");
        rows.push_str(&format!("| `{kind}` | {doc} |\n"));
    }

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("node_kinds.rs"), rows).expect("write node_kinds.rs");
}

/// Convert `CamelCase` to `snake_case` (mirrors serde's rename_all = snake_case).
fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_lowercase().next().unwrap());
    }
    out
}
