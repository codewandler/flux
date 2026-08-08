//! C-643 — generate and drift-check `assets/ops_docs.json`, the op → documentation-page index the
//! explorer's detail pane links from.
//!
//! No op→doc mapping exists anywhere in the tree. What does exist is the website contract's
//! guarantee that every registered op appears backticked in `website/docs/language/ops.md`, so the
//! index is built by exactly that scan: collect every `` `token` `` in every page under
//! `website/docs/`, keep the ones that name a registered op, and record which pages mentioned it.
//!
//! Generic single-word op names (`read`, `list`, `map`, …) appear in prose as ordinary words, so
//! they are pinned to the complete reference page rather than to whichever tutorial happened to say
//! "read" in backticks. That stop-list is the one hand-maintained input here.
//!
//! Regeneration is armed only by `FLUX_UPDATE_GOLDEN=1`, and a regenerating run *fails* — see
//! `support/golden_mode.rs` for both rules and why they are scars.

mod support {
    pub mod golden_mode;
}
use support::golden_mode;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Op names common enough as English words that a backtick scan finds them everywhere. Pinned to
/// the complete reference instead. Tune by eyeballing the generated diff.
const GENERIC_NAMES: &[&str] = &[
    "read", "write", "edit", "list", "map", "get", "set", "task", "run", "call", "glob", "grep",
    "ls", "fetch", "sleep", "wait", "echo", "print", "log", "diff", "test", "help", "shell",
    "bash", "todo", "think", "ask", "done",
];

const FALLBACK_PAGE: &str = "language/ops";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/flux-cli has a workspace root")
        .to_path_buf()
}

/// Page id for a docs file: its path under `website/docs/` without the `.md`.
fn page_id(root: &Path, file: &Path) -> String {
    file.strip_prefix(root.join("website/docs"))
        .expect("a file under website/docs")
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/")
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// Every `` `token` `` in one page. Single-backtick spans only: a fenced block is code, not a
/// reference, and including it would map every op to whatever tutorial demonstrates it.
fn backticked_tokens(source: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut in_fence = false;
    for line in source.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            let token = &after[..close];
            // A reference is a bare identifier. Anything with a space or a path separator is a
            // command line or a filename, and neither names an op.
            if !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                tokens.insert(token.to_string());
            }
            rest = &after[close + 1..];
        }
    }
    tokens
}

fn registered_ops() -> BTreeSet<String> {
    let mut registry = flux_runtime::ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry).expect("register built-ins");
    flux_web::try_register_web(&mut registry, &flux_web::WebOptions::default())
        .expect("register web ops");
    registry.specs().into_iter().map(|s| s.name).collect()
}

fn build_index(root: &Path) -> BTreeMap<String, Vec<String>> {
    let ops = registered_ops();
    let mut files = Vec::new();
    markdown_files(&root.join("website/docs"), &mut files);
    files.sort();

    let mut index: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &files {
        let page = page_id(root, file);
        let source = std::fs::read_to_string(file).expect("read a docs page");
        for token in backticked_tokens(&source) {
            if ops.contains(&token) && !GENERIC_NAMES.contains(&token.as_str()) {
                index.entry(token).or_default().insert(page.clone());
            }
        }
    }

    // Every registered op gets an entry, so the explorer never has to reason about a missing key.
    // The complete reference is both the fallback and the first choice for a generic name.
    ops.into_iter()
        .map(|op| {
            let mut pages: Vec<String> =
                index.remove(&op).unwrap_or_default().into_iter().collect();
            // The complete reference sorts first when present: it is the page that documents the
            // op, versus a tutorial that merely mentions it.
            pages.sort_by_key(|p| (p != FALLBACK_PAGE, p.clone()));
            if pages.is_empty() {
                pages.push(FALLBACK_PAGE.to_string());
            }
            (op, pages)
        })
        .collect()
}

#[test]
fn ops_doc_index_is_in_sync() {
    let root = repo_root();
    let asset = root.join("crates/flux-cli/assets/ops_docs.json");
    let generated = build_index(&root);
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&generated).expect("serialize the index")
    );

    if golden_mode::mode() == golden_mode::Mode::Rewrite {
        std::fs::write(&asset, &rendered).expect("write the ops doc index");
        golden_mode::rewrote(&asset);
    }

    let committed = std::fs::read_to_string(&asset).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. Generate it with \
             `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-cli --test ops_doc_index`",
            asset.display()
        )
    });
    assert_eq!(
        committed, rendered,
        "the committed ops doc index has drifted. Regenerate with \
         `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-cli --test ops_doc_index`, review the diff, \
         then re-run with the variable unset."
    );
}

/// The four properties the story asks the index to guarantee. Asserted against the *committed*
/// file, so a hand-edit is caught even when the generator would have produced something valid.
#[test]
fn committed_index_covers_the_registry_and_points_at_real_pages() {
    let root = repo_root();
    let committed: BTreeMap<String, Vec<String>> = serde_json::from_str(
        &std::fs::read_to_string(root.join("crates/flux-cli/assets/ops_docs.json"))
            .expect("read the committed index"),
    )
    .expect("parse the committed index");
    let ops = registered_ops();

    let missing: Vec<&String> = ops.iter().filter(|o| !committed.contains_key(*o)).collect();
    assert!(
        missing.is_empty(),
        "registered ops with no entry: {missing:?}"
    );

    let stale: Vec<&String> = committed.keys().filter(|k| !ops.contains(*k)).collect();
    assert!(
        stale.is_empty(),
        "entries for ops that are not registered: {stale:?}"
    );

    for (op, pages) in &committed {
        assert!(!pages.is_empty(), "`{op}` has an empty page list");
        for page in pages {
            let path = root.join("website/docs").join(format!("{page}.md"));
            assert!(
                path.exists(),
                "`{op}` references a page that does not exist: {page}"
            );
        }
    }

    // A stop-listed generic name must be pinned to the complete reference and nothing else —
    // otherwise "read" links to whichever tutorial said `read` in passing.
    for name in GENERIC_NAMES {
        if let Some(pages) = committed.get(*name) {
            assert_eq!(
                pages.as_slice(),
                [FALLBACK_PAGE.to_string()],
                "stop-listed `{name}` must point only at {FALLBACK_PAGE}"
            );
        }
    }
}

/// The arming rules are the whole reason this guard is trustworthy, so they are tested here too —
/// the same coverage `website_in_sync.rs` carries for its own copy of the support module.
#[test]
fn golden_arming() {
    use golden_mode::{mode_from, Mode};
    assert_eq!(mode_from(None), Ok(Mode::Check), "unset checks");
    assert_eq!(mode_from(Some("")), Ok(Mode::Check), "empty checks");
    assert_eq!(
        mode_from(Some("1")),
        Ok(Mode::Rewrite),
        "exactly 1 rewrites"
    );
    assert!(
        mode_from(Some("0")).is_err(),
        "0 is refused, not treated as off"
    );
    assert!(
        mode_from(Some("yes")).is_err(),
        "an unrecognized value is refused"
    );
}
