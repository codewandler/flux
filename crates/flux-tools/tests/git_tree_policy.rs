//! C-249: the guarded `git_*` family's tree preconditions are policy, not per-op taste.
//!
//! An op whose failure path runs a **blanket restore** — `git merge --abort`, `git revert --abort`,
//! `git reset --hard` — can only run it safely if the state it restores is one *this call* created.
//! Before C-249 that reasoning was re-derived by whoever wrote the op: `git_worktree_leave` and
//! `git_revert` each grew their own clean-tree guard for the same reason and `git_merge` had none,
//! which is the defect C-238 shipped a point fix for.
//!
//! This test makes the precondition structural rather than remembered: **every guarded `git_*` op
//! that can abort or hard-reset must route through the shared `require_tree_precondition` helper**,
//! which is where the policy (and its per-op `CleanTree::Required` / `NotRequired` decision, each
//! with a stated reason) actually lives. An op that omits it cannot pass the suite, so the next
//! merging or aborting op has to confront the question instead of inheriting an accident.
//!
//! Scope: `crates/flux-tools/src/lib.rs`, the guarded family (`pub struct Git*Tool`). The
//! `flux-eval` loop ops (`git_reset`, `guard_protected`) are a separate, top-level-only family
//! with their own contract and are deliberately not covered here.

use std::path::PathBuf;

/// The file defining the guarded `git_*` family.
const FAMILY_SRC: &str = "crates/flux-tools/src/lib.rs";

/// The shared preflight every abort-capable op must call.
const PRECONDITION_HELPER: &str = "require_tree_precondition(";

/// Git invocations that restore state this call did not necessarily create. Any of these in an
/// op's body makes the op abort-capable.
const BLANKET_RESTORES: &[&str] = &["\"--abort\"", "\"--hard\"", "\"-fd\""];

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(rel)
}

/// Split `src` into one section per `pub struct <Name>` declaration, returning `(name, body)`.
fn sections(src: &str) -> Vec<(String, &str)> {
    let marker = "\npub struct ";
    let starts: Vec<usize> = src.match_indices(marker).map(|(i, _)| i + 1).collect();
    let mut out = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(src.len());
        let body = &src[start..end];
        let name: String = body["pub struct ".len()..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        out.push((name, body));
    }
    out
}

/// The first `"git_…"` string literal that is a bare identifier — the op's registered name.
/// (Prose messages also start with `"git_…`, hence the identifier shape.)
fn op_name(body: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = body[from..].find("\"git_") {
        let start = from + at + 1;
        let rest = &body[start..];
        if let Some(end) = rest.find('"') {
            let literal = &rest[..end];
            if literal
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Some(literal.to_string());
            }
        }
        from = start;
    }
    None
}

#[test]
fn abort_capable_git_ops_route_through_the_shared_tree_precondition() {
    let path = repo_path(FAMILY_SRC);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Test code below `#[cfg(test)]` sets up fixtures with raw git; only real op bodies count.
    let production = match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => &src[..],
    };

    let mut offenders: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (struct_name, body) in sections(production) {
        if !struct_name.starts_with("Git") {
            continue;
        }
        let restores: Vec<&str> = BLANKET_RESTORES
            .iter()
            .copied()
            .filter(|needle| body.contains(needle))
            .collect();
        if restores.is_empty() {
            continue;
        }
        checked += 1;
        if !body.contains(PRECONDITION_HELPER) {
            offenders.push(format!(
                "{struct_name} ({}) runs a blanket restore ({}) but never calls \
                 `{PRECONDITION_HELPER}`",
                op_name(body).unwrap_or_else(|| "<unnamed op>".to_string()),
                restores.join(", "),
            ));
        }
    }

    assert!(
        checked > 0,
        "no abort-capable git op found in {FAMILY_SRC} — the scan lost track of the family"
    );
    assert!(
        offenders.is_empty(),
        "C-249: the clean-tree/in-flight precondition is policy, not per-op taste. Every guarded \
         `git_*` op that aborts or hard-resets must state its precondition by calling \
         `{PRECONDITION_HELPER}` (declaring `CleanTree::Required` or `NotRequired` with a \
         reason), so the abort can only ever undo state this call created.\n\n{}",
        offenders
            .iter()
            .map(|o| format!("  - {o}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
