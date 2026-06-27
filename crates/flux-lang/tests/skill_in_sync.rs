//! Single-source-of-truth guards for flux-lang's generated docs:
//!  - `skill/SKILL.md` is the whole rendered language skill (`flux_lang::skill::render()`).
//!  - `docs/reference.md` carries a generated `node-kinds` block (`node_kind_catalog()`).
//!
//! Both derive from the `Node` doc-comments via `flux_lang::schema`, so they can never drift from the
//! types. Regenerate with: `UPDATE=1 cargo test -p flux-lang --test skill_in_sync`

use std::path::PathBuf;

const BEGIN: &str = "<!-- BEGIN generated:node-kinds -->";
const END: &str = "<!-- END generated:node-kinds -->";

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn update() -> bool {
    std::env::var("UPDATE").is_ok()
}

#[test]
fn skill_artifact_is_in_sync() {
    let path = crate_path("skill/SKILL.md");
    let expected = flux_lang::skill::render();

    if update() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir {}: {e}", dir.display()));
        }
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }

    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}\nrun `UPDATE=1 cargo test -p flux-lang --test skill_in_sync` to generate it",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "skill/SKILL.md is out of date — regenerate with \
         `UPDATE=1 cargo test -p flux-lang --test skill_in_sync`"
    );
}

#[test]
fn reference_node_kinds_block_is_in_sync() {
    let path = crate_path("docs/reference.md");
    let block = format!("{BEGIN}\n{}{END}", flux_lang::schema::node_kind_catalog());

    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = content.find(BEGIN).unwrap_or_else(|| {
        panic!(
            "{} is missing the generated:node-kinds markers",
            path.display()
        )
    });
    let end = content[start..].find(END).expect("END marker after BEGIN") + start + END.len();
    let expected = format!("{}{}{}", &content[..start], block, &content[end..]);

    if update() {
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }
    assert_eq!(
        content, expected,
        "docs/reference.md node-kinds block is out of date — regenerate with \
         `UPDATE=1 cargo test -p flux-lang --test skill_in_sync`"
    );
}
