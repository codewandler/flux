//! Single-source-of-truth guard: the node-kind table embedded in the **engine** skill
//! (`.flux/skills/flux-flow/SKILL.md`) is GENERATED from `flux_lang`'s `Node` doc-comments, not
//! hand-maintained. This test regenerates the marked region and fails if the on-disk copy drifts.
//! (The language reference + language skill are checked in `flux-lang`'s own `docs_in_sync` test.)
//!
//! Regenerate with: `UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync`

use std::path::PathBuf;

const BEGIN: &str = "<!-- BEGIN generated:node-kinds -->";
const END: &str = "<!-- END generated:node-kinds -->";

/// The files carrying a generated node-kind table, relative to this crate's manifest dir.
fn targets() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![root.join("../../.flux/skills/flux-flow/SKILL.md")]
}

/// The canonical generated block: the markers wrapping the schema-derived catalog.
fn generated_block() -> String {
    format!("{BEGIN}\n{}{END}", flux_flow::schema::node_kind_catalog())
}

/// Splice `block` in place of the existing `BEGIN..=END` span. Returns `None` if the markers are
/// absent (a doc that opted out, or lost its markers).
fn replace_block(content: &str, block: &str) -> Option<String> {
    let start = content.find(BEGIN)?;
    let end = content[start..].find(END)? + start + END.len();
    Some(format!("{}{}{}", &content[..start], block, &content[end..]))
}

#[test]
fn generated_node_kind_tables_are_in_sync() {
    let block = generated_block();
    let update = std::env::var("UPDATE").is_ok();
    let mut stale = Vec::new();

    for path in targets() {
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let expected = replace_block(&content, &block).unwrap_or_else(|| {
            panic!(
                "{} is missing the `generated:node-kinds` markers",
                path.display()
            )
        });
        if content != expected {
            if update {
                std::fs::write(&path, &expected)
                    .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            } else {
                stale.push(path.display().to_string());
            }
        }
    }

    assert!(
        stale.is_empty(),
        "node-kind tables are out of date in:\n  {}\nregenerate with: \
         `UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync`",
        stale.join("\n  ")
    );
}
