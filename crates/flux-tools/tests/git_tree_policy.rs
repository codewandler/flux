//! C-249: the guarded git family's tree preconditions are policy, not per-op taste.
//!
//! Some ops in this family undo a failure with a **blanket restore**: `git merge --abort`,
//! `git revert --abort`. A blanket restore is only safe when the state it restores is state THIS
//! call created — otherwise it discards a half-finished, possibly hand-resolved operation the
//! caller never committed. Others need a clean tree for unrelated reasons (`fleet.isolate` hands a
//! worker a checkout of HEAD). Before C-249 each op decided all of that for itself: two grew the
//! same clean-tree guard independently, `git_merge` grew none — the defect C-238 point-fixed — and
//! `fleet.isolate` then shipped a hand-copied variant of `git_revert`'s wording.
//!
//! These two tests make the precondition structural rather than remembered.
//!
//! **Selection is by behaviour, never by name.** An earlier draft of this file keyed on a `Git`
//! struct-name prefix, which would have skipped `fleet.isolate` — a git op with a domain name —
//! entirely. Nothing here looks at what an op is called: [`blanket_restore`] asks what git
//! invocations an op's body contains, and [`no_op_hand_rolls_its_own_clean_tree_check`] asks the
//! whole file one question that no naming convention can dodge.
//!
//! Scope: `crates/flux-tools/src/lib.rs`. The `flux-eval` loop ops (`git_reset`,
//! `guard_protected`) are the most destructive git ops in the repo and carry no precondition at
//! all, but they are a separate top-level-only family with their own contract and cannot reach
//! this crate's helper (`flux-eval` depends on `flux-tools` only as a dev-dependency) — a named,
//! reasoned exclusion, filed as its own story rather than skipped by silence.

use std::path::PathBuf;

/// The file defining the guarded git family.
const FAMILY_SRC: &str = "crates/flux-tools/src/lib.rs";

/// The shared preflight. Every op the policy covers routes through it.
const PRECONDITION_HELPER: &str = "require_tree_precondition(";

/// The shared `git status --porcelain` reader — the ONE place in the family allowed to ask git
/// whether the tree is clean.
const SHARED_STATUS_READER: &str = "async fn tree_status(";

/// Git invocations that restore state this call did not necessarily create.
const BLANKET_RESTORES: &[&str] = &["\"--abort\"", "\"--hard\"", "\"-fd\""];

/// The working-tree cleanliness question, in the argv form the family uses.
const CLEANLINESS_PROBE: &str = "\"status\", \"--porcelain\"";

/// Ops the scan is known to select today. A behavioural selector that silently stops matching
/// passes vacuously, which is worse than no test at all — so the set it finds is pinned.
///
/// Only abort-capable ops can appear here, and that is not an oversight: an op whose *only* raw
/// signal was its own cleanliness probe stops being selectable the moment it is converted, because
/// the probe moves into the shared helper. That half of the rule is self-erasing by design — it is
/// a lint against the *next* hand-rolled check, and `no_op_hand_rolls_its_own_clean_tree_check`
/// is what holds the converted ops afterwards.
const SENTINELS: &[&str] = &["git_merge", "git_revert", "git_worktree_leave"];

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(rel)
}

/// The production half of the family source (test fixtures below `#[cfg(test)]` drive raw git on
/// purpose and are not op declarations).
fn production_source() -> String {
    let path = repo_path(FAMILY_SRC);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    match src.find("#[cfg(test)]") {
        Some(i) => src[..i].to_string(),
        None => src,
    }
}

/// The banner the file uses to separate declarations.
const BANNER: &str =
    "\n// ---------------------------------------------------------------------------";

/// Split `src` into one section per `pub struct <Name>` declaration, each running to the next
/// declaration or the next banner, whichever comes first — so shared helpers sitting under their
/// own banner are not attributed to whichever op happens to precede them.
fn op_sections(src: &str) -> Vec<(String, &str)> {
    let marker = "\npub struct ";
    let starts: Vec<usize> = src.match_indices(marker).map(|(i, _)| i + 1).collect();
    let banners: Vec<usize> = src.match_indices(BANNER).map(|(i, _)| i + 1).collect();
    let mut out = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let next_struct = starts.get(n + 1).copied().unwrap_or(src.len());
        let next_banner = banners
            .iter()
            .copied()
            .find(|&b| b > start)
            .unwrap_or(src.len());
        let body = &src[start..next_struct.min(next_banner)];
        let name: String = body["pub struct ".len()..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        out.push((name, body));
    }
    out
}

/// The first bare-identifier string literal in a section — the op's registered name. Covers both
/// `git_*` and dotted `fleet.*` names; prose messages also open with the op name, hence the shape
/// test.
fn op_name(body: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = body[from..].find('"') {
        let start = from + at + 1;
        let rest = &body[start..];
        if let Some(end) = rest.find('"') {
            let literal = &rest[..end];
            let plausible = !literal.is_empty()
                && literal.len() <= 40
                && literal
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
                && literal.contains(|c: char| c.is_ascii_lowercase());
            if plausible {
                return Some(literal.to_string());
            }
        }
        from = start;
    }
    None
}

/// Which blanket restores an op's body invokes.
fn blanket_restore(body: &str) -> Vec<&'static str> {
    BLANKET_RESTORES
        .iter()
        .copied()
        .filter(|needle| body.contains(needle))
        .collect()
}

/// An op that can abort or hard-reset must state its precondition by calling the shared helper —
/// which is where the per-op `CleanTree::Required`/`NotRequired` decision, and its reason, live.
#[test]
fn abort_capable_ops_route_through_the_shared_tree_precondition() {
    let src = production_source();
    let mut offenders: Vec<String> = Vec::new();
    let mut selected: Vec<String> = Vec::new();

    for (struct_name, body) in op_sections(&src) {
        let restores = blanket_restore(body);
        if restores.is_empty() {
            continue;
        }
        let op = op_name(body).unwrap_or_else(|| format!("<{struct_name}>"));
        selected.push(op.clone());
        if !body.contains(PRECONDITION_HELPER) {
            offenders.push(format!(
                "{struct_name} ({op}) runs a blanket restore ({}) but never calls \
                 `{PRECONDITION_HELPER}`",
                restores.join(", ")
            ));
        }
    }

    for sentinel in SENTINELS {
        assert!(
            selected.iter().any(|op| op == sentinel),
            "the scan no longer selects `{sentinel}` — the selector has rotted and this test is \
             passing vacuously. Found: {selected:?}"
        );
    }
    assert!(
        offenders.is_empty(),
        "C-249: the tree precondition is policy, not per-op taste. Any op that aborts or \
         hard-resets must state its precondition by calling `{PRECONDITION_HELPER}` (declaring \
         `CleanTree::Required` or `NotRequired` WITH a reason), so its restore can only ever undo \
         state this call created.\n\n{}",
        offenders
            .iter()
            .map(|o| format!("  - {o}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The clean-tree question is asked in exactly one place.
///
/// This is the check that would have caught `fleet.isolate` the day C-241 landed: that op is named
/// `Fleet*`, aborts nothing, and would be selected by no name-based or abort-based rule — but it
/// ran its own `git status --porcelain` and refused with its own hand-copied wording, which is the
/// per-op accident this story exists to end. Unlike the rule above this one never erases itself:
/// it stays true for every op the family ever gains, whatever it is called.
#[test]
fn no_op_hand_rolls_its_own_clean_tree_check() {
    let src = production_source();
    let shared = src
        .find(SHARED_STATUS_READER)
        .unwrap_or_else(|| panic!("the shared `{SHARED_STATUS_READER}` helper is gone"));
    // The helper's own body is the one legitimate probe; bound it by its closing brace.
    let shared_end = src[shared..]
        .find("\n}\n")
        .map(|i| shared + i)
        .unwrap_or(src.len());

    let strays: Vec<String> = src
        .match_indices(CLEANLINESS_PROBE)
        .map(|(i, _)| i)
        .filter(|&i| !(shared..shared_end).contains(&i))
        .map(|i| format!("{FAMILY_SRC}:{}", src[..i].lines().count()))
        .collect();

    assert!(
        strays.is_empty(),
        "C-249: `git status --porcelain` is the family's clean-tree question, and it belongs in \
         the shared `tree_status` helper behind `{PRECONDITION_HELPER}` — an op that asks it \
         directly is deciding the policy for itself and will drift from its siblings' wording, \
         which is exactly how `fleet.isolate` shipped a stale copy of `git_revert`'s advice. \
         Declare a `TreePrecondition` instead.\n\n  - {}",
        strays.join("\n  - ")
    );
}
