//! Single-source-of-truth guard for the public website's Docusaurus copies of the node-kind and
//! prelude-type tables:
//!  - `website/docs/language/node-reference.md` carries a generated `node-kinds` block
//!    (`flux_lang::schema::node_kind_rows()`).
//!  - `website/docs/language/types-and-effects.md` carries a generated `prelude-types` block
//!    (`flux_lang::prelude::prelude_type_rows()`).
//!  - `website/docs/whats-new.md` carries the customer-facing release history from the root
//!    `WHATS-NEW.md` (minus its maintainer comment and duplicate H1).
//!  - `website/docs/concepts.md` and `website/docs/ecosystem.md` carry the bodies of
//!    `docs/concepts.md` and `docs/ecosystem.md`. These two pages are authored in the contributor
//!    tree because they are read from both sides — a contributor deciding where a change belongs,
//!    and a user deciding what to install — and a fork between those audiences is how a public page
//!    ends up contradicting the tree it documents. The website copy owns only its frontmatter.
//!
//! Both derive from the same catalogs as `tests/skill_in_sync.rs` (the language skill + the
//! in-crate `docs/reference.md`), so the website can never carry a stale hand-edited mirror.
//!
//! The one rendering difference from the verbatim catalog: Docusaurus's markdown/MDX table parser
//! treats an unescaped `|` inside a cell as a new column boundary (unlike the plain-markdown crate
//! docs and skills, which tolerate it), so cell content is escaped with `\|` for this consumer only.
//!
//! Regenerate with:
//! `FLUX_UPDATE_GOLDEN=1 cargo test -p codewandler-flux-lang --test website_in_sync`
//! — which writes the mirrors and then **fails on purpose**, so a rewrite can never be mistaken for
//! a verified check (C-326). Re-run with the variable unset to verify.

#[path = "support/golden_mode.rs"]
mod golden_mode;

use golden_mode::Mode;
use std::path::PathBuf;

const BEGIN: &str = "<!-- BEGIN generated:node-kinds -->";
const END: &str = "<!-- END generated:node-kinds -->";
const BEGIN_PRELUDE: &str = "<!-- BEGIN generated:prelude-types -->";
const END_PRELUDE: &str = "<!-- END generated:prelude-types -->";
const BEGIN_WHATS_NEW: &str = "<!-- BEGIN generated:whats-new -->";
const END_WHATS_NEW: &str = "<!-- END generated:whats-new -->";

/// Resolve a path relative to the repo root (this crate lives at `crates/flux-lang`).
fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// Escape literal `|` characters in a cell's content — required by Docusaurus's markdown table
/// parser, which (unlike the plain-markdown crate docs and skills) treats an unescaped `|` inside
/// a cell as a new column boundary.
fn escape_table_pipes(desc: &str) -> String {
    desc.replace('|', "\\|")
}

fn website_node_kind_table() -> String {
    let mut out = String::from("| kind | description |\n|---|---|\n");
    for (kind, desc) in flux_lang::schema::node_kind_rows() {
        out.push_str(&format!("| `{kind}` | {} |\n", escape_table_pipes(&desc)));
    }
    out
}

fn website_prelude_type_table() -> String {
    let mut out = String::from("| type | description |\n|---|---|\n");
    for (name, desc) in flux_lang::prelude::prelude_type_rows() {
        out.push_str(&format!("| `{name}` | {} |\n", escape_table_pipes(&desc)));
    }
    out
}

fn website_whats_new() -> String {
    let source = std::fs::read_to_string(repo_path("WHATS-NEW.md")).expect("read WHATS-NEW.md");
    let after_comment = source
        .split_once("-->")
        .map_or(source.as_str(), |(_, rest)| rest)
        .trim_start();
    after_comment
        .strip_prefix("# What's new in flux")
        .expect("WHATS-NEW.md customer heading")
        .trim()
        .to_string()
}

/// Splice `block` in place of the existing `begin..=end` span. Panics if the markers are absent —
/// a website page that opted into this guard must carry them.
fn splice(content: &str, begin: &str, end: &str, block: &str) -> String {
    let start = content
        .find(begin)
        .unwrap_or_else(|| panic!("missing `{begin}` marker"));
    let stop = content[start..].find(end).expect("END marker after BEGIN") + start + end.len();
    format!("{}{}{}", &content[..start], block, &content[stop..])
}

#[test]
fn website_node_reference_node_kinds_block_is_in_sync() {
    let path = repo_path("website/docs/language/node-reference.md");
    let block = format!("{BEGIN}\n{}{END}", website_node_kind_table());

    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let expected = splice(&content, BEGIN, END, &block);

    if golden_mode::mode() == Mode::Rewrite {
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        golden_mode::rewrote(&path);
    }
    assert_eq!(
        content, expected,
        "website/docs/language/node-reference.md node-kinds block is out of date — regenerate \
         with `FLUX_UPDATE_GOLDEN=1 cargo test -p codewandler-flux-lang --test website_in_sync`"
    );
}

#[test]
fn website_types_and_effects_prelude_types_block_is_in_sync() {
    let path = repo_path("website/docs/language/types-and-effects.md");
    let block = format!(
        "{BEGIN_PRELUDE}\n{}{END_PRELUDE}",
        website_prelude_type_table()
    );

    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let expected = splice(&content, BEGIN_PRELUDE, END_PRELUDE, &block);

    if golden_mode::mode() == Mode::Rewrite {
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        golden_mode::rewrote(&path);
    }
    assert_eq!(
        content, expected,
        "website/docs/language/types-and-effects.md prelude-types block is out of date — \
         regenerate with `FLUX_UPDATE_GOLDEN=1 cargo test -p codewandler-flux-lang --test \
         website_in_sync`"
    );
}

#[test]
fn website_customer_changelog_is_in_sync() {
    let path = repo_path("website/docs/whats-new.md");
    let block = format!(
        "{BEGIN_WHATS_NEW}\n{}\n{END_WHATS_NEW}",
        website_whats_new()
    );
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let expected = splice(&content, BEGIN_WHATS_NEW, END_WHATS_NEW, &block);

    if golden_mode::mode() == Mode::Rewrite {
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        golden_mode::rewrote(&path);
    }
    assert_eq!(
        content, expected,
        "website/docs/whats-new.md is out of date — regenerate with `FLUX_UPDATE_GOLDEN=1 cargo \
         test -p codewandler-flux-lang --test website_in_sync`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Contributor-authored pages mirrored to the site
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The body of a `docs/` page: everything after any frontmatter block, trimmed.
///
/// These sources carry no Docusaurus frontmatter — the website copy owns that — so this is a plain
/// read today. The strip is kept because a source that grows frontmatter would otherwise duplicate
/// it into the mirror and break the site build, which is a failure worth making impossible rather
/// than remembering.
fn docs_page_body(rel: &str) -> String {
    let source =
        std::fs::read_to_string(repo_path(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let body = match source.strip_prefix("---") {
        Some(rest) => rest
            .split_once("\n---")
            .map_or(source.as_str(), |(_, after)| after),
        None => source.as_str(),
    };
    body.trim().to_string()
}

/// Assert (or regenerate) one mirrored page.
fn assert_mirrored(source: &str, target: &str, tag: &str) {
    let path = repo_path(target);
    let begin = format!("<!-- BEGIN generated:{tag} -->");
    let end = format!("<!-- END generated:{tag} -->");
    let block = format!("{begin}\n{}\n{end}", docs_page_body(source));

    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let expected = splice(&content, &begin, &end, &block);

    if golden_mode::mode() == Mode::Rewrite {
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        golden_mode::rewrote(&path);
    }
    assert_eq!(
        content, expected,
        "{target} is out of date with {source} — regenerate with \
         `FLUX_UPDATE_GOLDEN=1 cargo test -p codewandler-flux-lang --test website_in_sync`"
    );
}

#[test]
fn website_concepts_mirrors_the_contributor_page() {
    assert_mirrored("docs/concepts.md", "website/docs/concepts.md", "concepts");
}

#[test]
fn website_ecosystem_mirrors_the_contributor_page() {
    assert_mirrored(
        "docs/ecosystem.md",
        "website/docs/ecosystem.md",
        "ecosystem",
    );
}

/// Return a small line window around the first occurrence of `needle` so a contract can require
/// nearby qualification without pinning an entire documentation page byte-for-byte.
fn line_window(content: &str, needle: &str, radius: usize) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    let line = lines
        .iter()
        .position(|candidate| candidate.contains(needle))
        .unwrap_or_else(|| panic!("missing `{needle}`"));
    let start = line.saturating_sub(radius);
    let end = (line + radius + 1).min(lines.len());
    lines[start..end].join("\n")
}

#[test]
fn exchange_environment_token_is_explicitly_transitional_until_c509() {
    const TOKEN: &str = "FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN";
    for path in [
        "README.md",
        "website/docs/reference/config.md",
        "docs/ecosystem.md",
        "website/docs/direction/connector-native-integrations.md",
    ] {
        let content = std::fs::read_to_string(repo_path(path))
            .unwrap_or_else(|error| panic!("read {path}: {error}"));
        let window = line_window(&content, TOKEN, 6).to_ascii_lowercase();
        assert!(
            window.contains("transitional") && window.contains("c-509"),
            "{path} presents the C-503 environment token without naming its transitional status and C-509 replacement:\n{window}"
        );
    }

    let source = std::fs::read_to_string(repo_path("crates/flux-cli/src/execution.rs"))
        .expect("read Flux CLI assembly");
    let assembly = line_window(&source, "host startup configuration", 5).to_ascii_lowercase();
    assert!(
        assembly.contains("transitional") && assembly.contains("c-509"),
        "the C-503 assembly comment must name the transitional seam and its C-509 replacement"
    );
}
