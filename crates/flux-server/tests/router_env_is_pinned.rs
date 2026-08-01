//! C-392: **no `flux-server` test may build a router from the operator's `$HOME`.**
//!
//! `router()` / `router_multi()` resolve the A2A session TTL and the C-189 resource limits from the
//! layered flux config at build time, and that config's user layer is `$HOME/.flux/config.toml`. A
//! test that goes through those entry points therefore asserts against whatever the developer keeps
//! in their own home — a verdict that is a function of the machine, not of the fixture, whose
//! failure looks exactly like a real regression in whatever diff is in flight.
//!
//! The migration to `router_in`/`router_multi_in` fixed that once. This check is what keeps it
//! fixed: it is a *census*, not an inspection, so the next router built in a test is flagged by a
//! red gate rather than by someone remembering. C-333 will generalize the rule across the
//! workspace; until then this crate — which owns the widest tranche in C-332's census — carries its
//! own guard.
//!
//! Scope and honesty about it: this scans **text**, not a syntax tree, so it sees a call the way a
//! reviewer's `grep` would. `//` comments are stripped first (which also means a call hidden after
//! a `//` inside a string literal on the same line would be missed — no such line exists, and a
//! syntax-aware version belongs in `flux-codegate` with C-333, not here).

use std::path::{Path, PathBuf};

/// The entry points that resolve their config from the **process** environment. Each needle carries
/// its `(` so the pinned counterparts (`router_in(`, `router_multi_in(`, `router_with_ttl_in(`,
/// `from_env_in(`) are different strings and never match.
const AMBIENT_ENTRY_POINTS: [&str; 4] =
    ["router(", "router_multi(", "router_with_ttl(", "from_env("];

/// The pinned counterparts, counted so this check cannot pass by scanning nothing.
const PINNED_ENTRY_POINTS: [&str; 3] = ["router_in(", "router_multi_in(", "router_with_ttl_in("];

/// Drop each line's `//` comment tail — prose naming `router()` is not a call.
fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every occurrence of `needle` in `code` that begins an identifier — so `_router(` and
/// `guarded_app(` are not mistaken for `router(`, while `flux_server::router(` still is.
fn call_lines(code: &str, needle: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut from = 0usize;
    while let Some(offset) = code[from..].find(needle) {
        let at = from + offset;
        let preceded_by_ident_char = code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !preceded_by_ident_char {
            hits.push(code[..at].matches('\n').count() + 1);
        }
        from = at + needle.len();
    }
    hits
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The marker that opens a `src/` module's inline test block. Anchored on the whole two-line form
/// rather than a bare `#[cfg(test)]`, which also decorates individual test-only *items* mid-file
/// (`a2a.rs`, `resource.rs`) and would drag production code into the scan.
const INLINE_TESTS_MARKER: &str = "#[cfg(test)]\nmod tests {";

/// The test corpus of this crate: every integration-test source, plus the inline `mod tests` tail
/// of each `src/` module (those blocks run to the end of their file).
fn test_sources() -> Vec<(String, String)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let this_file = crate_dir.join("tests").join("router_env_is_pinned.rs");
    let mut files = Vec::new();
    collect_rs(&crate_dir.join("tests"), &mut files);
    collect_rs(&crate_dir.join("src"), &mut files);

    let mut sources = Vec::new();
    for path in files {
        // This file names the banned spellings as data; scanning itself would be self-defeating.
        if path == this_file {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let is_src = path.starts_with(crate_dir.join("src"));
        let body = if is_src {
            match source.find(INLINE_TESTS_MARKER) {
                Some(at) => source[at..].to_string(),
                None => continue,
            }
        } else {
            source
        };
        let label = path
            .strip_prefix(crate_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        sources.push((label, strip_comments(&body)));
    }
    sources
}

/// The census itself: every router a `flux-server` test builds goes through the `*_in` seam, so no
/// test's verdict depends on the operator's `~/.flux/config.toml`.
#[test]
fn no_flux_server_test_builds_a_router_from_the_process_environment() {
    let sources = test_sources();
    assert!(
        sources.len() >= 10,
        "the scan found only {} test sources — the corpus moved and this check went vacuous",
        sources.len()
    );

    let mut violations = Vec::new();
    let mut pinned = 0usize;
    for (label, code) in &sources {
        for needle in AMBIENT_ENTRY_POINTS {
            for line in call_lines(code, needle) {
                violations.push(format!(
                    "{label}:{line}: `{needle}..)` reads the operator's ~/.flux/config.toml — \
                     use the `_in` form with a pinned DiscoveryEnv (C-392)"
                ));
            }
        }
        for needle in PINNED_ENTRY_POINTS {
            pinned += call_lines(code, needle).len();
        }
    }

    assert!(
        violations.is_empty(),
        "flux-server tests whose verdict depends on the machine's $HOME:\n  {}",
        violations.join("\n  ")
    );
    assert!(
        pinned >= 20,
        "only {pinned} pinned router constructions found across {} sources — either the suite \
         shrank drastically or the needles drifted and this check is no longer measuring anything",
        sources.len()
    );
}

/// The scanner's own pins: it must see a qualified call, ignore an identifier that merely ends in
/// the needle, ignore prose in a comment, and not confuse a pinned call for an ambient one.
#[test]
fn the_scanner_distinguishes_calls_from_identifiers_prose_and_the_pinned_form() {
    let code = strip_comments(
        r#"
let a = flux_server::router(engine, auth, card, bind);
let _router = router_in(engine, auth, card, bind, &env);
let b = my_router(x);
let c = router_multi_in(resolver, auth, bind, &env);
// router() in prose is not a call
"#,
    );
    assert_eq!(
        call_lines(&code, "router(").len(),
        1,
        "exactly the qualified ambient call — not `_router`, `my_router`, or the comment"
    );
    assert_eq!(call_lines(&code, "router_multi(").len(), 0);
    assert_eq!(call_lines(&code, "router_in(").len(), 1);
    assert_eq!(call_lines(&code, "router_multi_in(").len(), 1);
}
