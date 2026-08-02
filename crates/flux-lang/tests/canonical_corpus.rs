//! The canonicalizer, held against the **real** corpus rather than a fixture (L-103).
//!
//! A formatter that quietly drops or relocates a comment is worse than no formatter, and a fixture
//! only proves the cases someone thought of. So these walk every `.flux` file in the repository and
//! every fenced `flux` block in the documentation, and assert the three properties that make the
//! output trustworthy on a file someone actually wrote:
//!
//! 1. **Meaning is preserved** — the rewrite lowers to the same [`flux_lang::program::Module`].
//! 2. **Every comment survives** — same comment multiset, before and after.
//! 3. **It is a fixed point** — `fmt(fmt(x)) == fmt(x)`.
//!
//! This lives outside the `cli` feature on purpose: it is the property the *library* guarantees, so
//! `cargo test --workspace` must reach it (`crates/flux-lang/AGENTS.md` — the feature-gated leg is a
//! backstop, not the gate).

use std::path::{Path, PathBuf};

use flux_lang::canonicalize::{canonicalize_source, Canonical};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/flux-lang is two levels below the repo root")
        .to_path_buf()
}

/// Every file under `dir` with `ext`, skipping build output and vendored trees.
fn files_with_extension(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" || name.starts_with("target") {
            continue;
        }
        if path.is_dir() {
            files_with_extension(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

/// The `flux`-fenced code blocks of a markdown document.
fn fenced_flux_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match current.as_mut() {
            Some(body) => {
                if line.trim_start().starts_with("```") {
                    blocks.push(std::mem::take(body));
                    current = None;
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
            None => {
                let fence = line.trim_start();
                if fence == "```flux" || fence.starts_with("```flux ") {
                    current = Some(String::new());
                }
            }
        }
    }
    blocks
}

/// Documentation fences are often flow *bodies* with the header elided. Wrap one in a throwaway
/// flow so the canonicalizer sees it, mirroring `crates/flux-cli/tests/website_contract.rs`'s own
/// fragment handling — including its rule about multi-line strings, whose interior must not be
/// indented by the scaffolding.
fn as_flux_module(block: &str) -> String {
    let block = format!("{}\n", block.trim_end());
    if flux_lang::parser::parse_cst(&block).errors.is_empty()
        && flux_lang::lower_cst::cst_to_module(&flux_lang::parser::parse_cst(&block)).is_ok()
    {
        return block;
    }
    let mut wrapped = String::from("flow __fragment\n");
    let mut in_multiline_string = false;
    for line in block.lines() {
        if !line.is_empty() && !in_multiline_string {
            wrapped.push_str("  ");
        }
        wrapped.push_str(line);
        wrapped.push('\n');
        if line.matches("\"\"\"").count() % 2 == 1 {
            in_multiline_string = !in_multiline_string;
        }
    }
    wrapped
}

/// Assert the three properties for one buffer. Returns whether it was a canonicalizable input at
/// all — a doc fence is often a fragment or deliberately-invalid pseudo-code, which is not this
/// test's business.
fn assert_canonicalization_is_sound(label: &str, src: &str) -> bool {
    let parsed = flux_lang::parser::parse_cst(src);
    if !parsed.errors.is_empty() {
        return false;
    }
    let Ok(before) = flux_lang::lower_cst::cst_to_module(&parsed) else {
        return false;
    };

    let once = match canonicalize_source(src) {
        Canonical::Unchanged => return true,
        Canonical::Rewritten(text) => text,
        // The guard is the canonicalizer's own contract; tripping it on the shipped corpus is the
        // defect this test exists to catch, not an input we are allowed to skip.
        Canonical::Rejected => panic!("{label}: the equivalence guard rejected the rewrite"),
        Canonical::Unparsed => return false,
    };

    let reparsed = flux_lang::parser::parse_cst(&once);
    assert!(
        reparsed.errors.is_empty(),
        "{label}: canonical output does not parse: {:?}\n{once}",
        reparsed.errors
    );
    assert_eq!(
        flux_lang::lower_cst::cst_to_module(&reparsed)
            .expect("canonical output lowers")
            .module,
        before.module,
        "{label}: canonicalization changed the meaning of the module\n{once}"
    );
    assert_eq!(
        flux_lang::format_cst::comment_multiset(&reparsed.syntax()),
        flux_lang::format_cst::comment_multiset(&parsed.syntax()),
        "{label}: canonicalization lost or invented a comment\n{once}"
    );
    assert_eq!(
        canonicalize_source(&once),
        Canonical::Unchanged,
        "{label}: canonicalization is not idempotent\n{once}"
    );
    true
}

#[test]
fn every_flux_file_in_the_repository_canonicalizes_soundly() {
    let root = repo_root();
    let mut paths = Vec::new();
    files_with_extension(&root, "flux", &mut paths);
    paths.sort();

    let mut checked = 0;
    for path in &paths {
        let src = std::fs::read_to_string(path).expect("readable .flux file");
        let label = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        if assert_canonicalization_is_sound(&label, &src) {
            checked += 1;
        }
    }
    // The shipped corpus at the time of writing: 17 `examples/`, the built-in `agent-loop.flux`,
    // two `flux-app` examples, the portable-parity flow, and the LSP's large fixture.
    assert!(
        checked >= 22,
        "expected the whole shipped .flux corpus, checked {checked} of {} files",
        paths.len()
    );
}

#[test]
fn every_documented_flux_block_canonicalizes_soundly() {
    let root = repo_root();
    let mut docs = Vec::new();
    for dir in ["docs", "website", "crates", "README.md"] {
        let path = root.join(dir);
        if path.is_dir() {
            files_with_extension(&path, "md", &mut docs);
        }
    }
    docs.sort();

    let mut checked = 0;
    for path in &docs {
        let markdown = std::fs::read_to_string(path).expect("readable markdown");
        for (index, block) in fenced_flux_blocks(&markdown).into_iter().enumerate() {
            let label = format!(
                "{} flux block {}",
                path.strip_prefix(&root).unwrap_or(path).display(),
                index + 1
            );
            if assert_canonicalization_is_sound(&label, &as_flux_module(&block)) {
                checked += 1;
            }
        }
    }
    // Whole modules plus wrapped body fragments. What is left over is prose-shaped pseudo-code and
    // deliberately-invalid snippets illustrating a diagnostic, which have no canonical form.
    assert!(
        checked >= 250,
        "expected the documented Flux corpus, checked {checked} blocks"
    );
}
