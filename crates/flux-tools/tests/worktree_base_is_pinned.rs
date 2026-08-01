//! C-391: **no `flux-tools` test may allocate a git worktree under the operator's `$HOME`.**
//!
//! `git_worktree_enter` and `fleet.isolate` allocate a real parent directory and run a real
//! `git worktree add` inside it. Unpinned, that parent is `$HOME/.flux/worktrees/flux-worktree-*`
//! — so the unit suite creates directory trees in the developer's home, and a test that dies
//! between `enter` and `leave` leaves one there. That is not merely the verdict hazard C-332's
//! census is about (a result that depends on the machine, not the fixture): it is a test suite
//! writing into someone's home directory. Five abandoned trees were found in a real
//! `~/.flux/worktrees`, three days stale, when this story was written.
//!
//! Pinning every test system fixed it once. This check is what keeps it fixed: it is a *census*,
//! not an inspection, so the next unpinned system is flagged by a red gate rather than by someone
//! remembering. Same role — and deliberately the same shape — as `flux-server`'s
//! `router_env_is_pinned.rs` (C-392).
//!
//! Scope, and honesty about it. This scans **text**, not a syntax tree, so it sees a call the way a
//! reviewer's `grep` would: `//` comment tails are stripped first, and a call hidden inside a string
//! literal after a `//` on the same line would be missed. It covers this crate only — the crate that
//! owns every worktree-allocating test today; a workspace-wide, syntax-aware version of the rule
//! belongs in `flux-codegate` with C-333.

use std::path::{Path, PathBuf};

/// The **process-reading** entry points. Each needle is matched only where it begins an
/// identifier and is not preceded by `.`, so the pinned method forms — `system.allocate_worktree_dir(`,
/// `base.remove(` — are never mistaken for the free functions `flux_system::allocate_worktree_dir(`
/// and `flux_system::remove_worktree_dir(`.
const AMBIENT_CALLS: [&str; 3] = [
    "allocate_worktree_dir(",
    "remove_worktree_dir(",
    "WorktreeBase::from_process(",
];

/// Constructors that leave a `System` on the process-resolved base. Every one of them in test code
/// must be followed by the pin — a `System` a test drives worktree ops through and never pinned is
/// exactly how an allocation reaches `~/.flux/worktrees`.
const UNPINNED_CONSTRUCTORS: [&str; 2] = ["System::new(", "System::from_env("];

/// The pin itself, counted so this check cannot pass by scanning nothing.
const PIN: &str = ".with_worktree_base(";

/// How far after a constructor the pin may appear. Generous enough for the real spelling
/// (`System::new(Workspace::new(&dir).unwrap()).with_worktree_base(..)`) and far short of the next
/// statement.
const PIN_WINDOW: usize = 200;

/// Drop each line's `//` comment tail — prose naming `allocate_worktree_dir()` is not a call.
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

/// Byte offsets of every occurrence of `needle` in `code` that begins an identifier and is not a
/// method call on a receiver — so `system.allocate_worktree_dir(` and `my_remove_worktree_dir(` are
/// skipped while `flux_system::allocate_worktree_dir(` is not.
fn call_offsets(code: &str, needle: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut from = 0usize;
    while let Some(offset) = code[from..].find(needle) {
        let at = from + offset;
        let previous = code[..at].chars().next_back();
        let is_call = !previous.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
        if is_call {
            hits.push(at);
        }
        from = at + needle.len();
    }
    hits
}

/// The 1-based line number of a byte offset.
fn line_of(code: &str, offset: usize) -> usize {
    code[..offset].matches('\n').count() + 1
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

/// The marker that opens a `src/` module's inline test block — anchored on the whole two-line form,
/// since a bare `#[cfg(test)]` also decorates individual test-only *items* mid-file and would drag
/// production code (which resolves the base from the process on purpose) into the scan.
const INLINE_TESTS_MARKER: &str = "#[cfg(test)]\nmod tests {";

/// The test corpus of this crate: every integration-test source, plus the inline `mod tests` tail of
/// each `src/` module (those blocks run to the end of their file). Each entry carries the line the
/// scanned text starts on, so a violation points at the real file line rather than at an offset
/// into a slice — a report that sends the reader to the wrong line is worse than no line at all.
fn test_sources() -> Vec<(String, String, usize)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let this_file = crate_dir.join("tests").join("worktree_base_is_pinned.rs");
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
        let (body, first_line) = if is_src {
            match source.find(INLINE_TESTS_MARKER) {
                Some(at) => (source[at..].to_string(), line_of(&source, at)),
                None => continue,
            }
        } else {
            (source, 1)
        };
        let label = path
            .strip_prefix(crate_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        sources.push((label, strip_comments(&body), first_line));
    }
    sources
}

/// The census: every `System` a `flux-tools` test builds carries a pinned worktree base, and no
/// test reaches the process-reading allocation helpers directly.
#[test]
fn no_flux_tools_test_allocates_a_worktree_under_the_operators_home() {
    let sources = test_sources();
    assert!(
        sources.len() >= 3,
        "the scan found only {} test sources — the corpus moved and this check went vacuous",
        sources.len()
    );

    let mut violations = Vec::new();
    let mut constructors = 0usize;
    let mut pins = 0usize;
    let mut pinned_helper_calls = 0usize;
    for (label, code, first_line) in &sources {
        let line_in_file = |at: usize| line_of(code, at) + first_line - 1;
        for needle in AMBIENT_CALLS {
            for at in call_offsets(code, needle) {
                violations.push(format!(
                    "{label}:{}: `{needle}..)` resolves the base from the process environment, so \
                     it allocates in the operator's ~/.flux/worktrees — go through the `System` \
                     the test already holds (`c.system().allocate_worktree_dir()`), which carries \
                     a pinned base (C-391)",
                    line_in_file(at)
                ));
            }
        }
        for needle in UNPINNED_CONSTRUCTORS {
            for at in call_offsets(code, needle) {
                constructors += 1;
                let tail = &code[at..code.len().min(at + PIN_WINDOW)];
                if !tail.contains(PIN) {
                    violations.push(format!(
                        "{label}:{}: `{needle}..)` is not pinned — add \
                         `{PIN}pinned_worktree_base())`, or a worktree op driven through this \
                         system allocates in the operator's home (C-391)",
                        line_in_file(at)
                    ));
                }
            }
        }
        pins += code.matches(PIN).count();
        pinned_helper_calls += code.matches(".allocate_worktree_dir(").count()
            + code.matches(".remove_worktree_dir(").count();
    }

    assert!(
        violations.is_empty(),
        "flux-tools tests that can write into the operator's home:\n  {}",
        violations.join("\n  ")
    );

    // Vacuity floors: the needles must still be finding the things they are supposed to guard.
    assert!(
        constructors >= 1 && pins >= 1,
        "found {constructors} System constructions and {pins} pins across {} sources — the \
         spellings drifted and this check is no longer measuring anything",
        sources.len()
    );
    assert!(
        pinned_helper_calls >= 2,
        "found only {pinned_helper_calls} pinned allocate/remove calls — the worktree tests either \
         left this crate or stopped going through the System seam"
    );
}

/// The scanner's own pins: it must see a free-function call, ignore the pinned method form, ignore
/// an identifier that merely ends in the needle, ignore prose in a comment, and hold a constructor
/// to the pin that follows it.
#[test]
fn the_scanner_distinguishes_free_calls_from_methods_prose_and_the_pinned_constructor() {
    let code = strip_comments(
        r#"
let a = flux_system::allocate_worktree_dir().unwrap();
let b = c.system().allocate_worktree_dir().unwrap();
let d = my_allocate_worktree_dir();
// allocate_worktree_dir() in prose is not a call
let e = System::new(Workspace::new(&dir).unwrap()).with_worktree_base(pinned_worktree_base());
let f = System::new(Workspace::new(&other).unwrap());
"#,
    );
    let ambient = call_offsets(&code, "allocate_worktree_dir(");
    assert_eq!(
        ambient.len(),
        1,
        "exactly the free-function call — not the method, the lookalike, or the comment"
    );
    assert_eq!(line_of(&code, ambient[0]), 2);

    let constructors = call_offsets(&code, "System::new(");
    assert_eq!(constructors.len(), 2);
    let pinned = |at: usize| code[at..code.len().min(at + PIN_WINDOW)].contains(PIN);
    assert!(pinned(constructors[0]), "the pinned construction passes");
    assert!(!pinned(constructors[1]), "the bare construction is caught");
}
