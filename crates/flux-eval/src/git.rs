//! Git ops for the keep/commit/reset loop. All exec goes through `ctx.system().run` (argv-only, no
//! shell). These are registered on the **top-level** registry only — never a sub-agent's — so a worker
//! can edit files but cannot itself `git reset --hard`. `git_reset` is `Risk::Destructive` and so still
//! re-confirms at dispatch unless `--yes` (the autonomous loop runs with `--yes`).
//!
//! # What licenses these ops to destroy uncommitted work (C-278)
//!
//! Two ops here destroy uncommitted **and untracked** work: `git_reset` (`reset --hard` + an
//! unscoped `clean -fd`) and `guard_protected` (`checkout <head> --` + `clean -fd --`). `clean -fd`
//! deletes untracked files outright — the case C-249 rewrote the guarded family's refusal wording
//! for, because "commit or stash them first" is unactionable for a file git never tracked.
//!
//! C-249 made the `flux-tools` git family's clean-tree precondition policy. These two sit outside
//! that family and cannot reach its helper (`flux-tools` is a dev-dependency here, not a
//! dependency), so each states its own — and the statements are deliberately *different*, because
//! the two ops are not the same kind of operation:
//!
//! - **`git_reset` — precondition: a verified snapshot.** The loop's licence to destroy rests on an
//!   invariant, *everything in this tree was produced by the step I am undoing*. Before C-278 that
//!   invariant was **trusted**: the op read `head` out of the payload and reset. It is instead
//!   **checkable**, in two independent halves, and [`licence_to_restore`] checks both. This is what
//!   stops a future caller from being different: the licence is proved from the payload and the
//!   repository at each call, never inferred from where the op happens to be called today.
//! - **`guard_protected` — reasoned exemption: its blast radius is bounded by construction.** It is
//!   not a blanket restore. Both argv it builds are explicit pathspec lists it computed itself and
//!   filtered through [`is_protected`], so it cannot touch a path outside `PROTECTED` however dirty
//!   the tree is or whoever calls it. A clean-tree precondition would buy nothing and would break
//!   the op's entire purpose, which is to run *after* the worker has deliberately dirtied the tree.
//!   The exemption does not rest on the caller, so there is nothing about a future caller to
//!   defend against — but it does rest on the pathspec filtering staying in place, which
//!   `guard_protected_touches_nothing_outside_the_protected_paths` is what holds.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    tool_input_schema, AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty,
    IntentRole, IntentSet, IntentTarget, Risk, ToolSpec,
};

use crate::util::{arg, json_result};

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Paths the self-improvement loop must protect from the worker: the grader (`flux-eval`), the
/// terminal-bench harness + sub-agent roles (`bench/`), the loop flows + scripts, and CI. If the
/// worker could edit these it could "win" by gaming its own measurement. [`GuardProtectedTool`]
/// restores them from the round snapshot before scoring.
const PROTECTED: &[&str] = &[
    "crates/flux-eval",
    "bench",
    "scripts",
    ".github",
    "examples/improve-tbench.flux",
    "examples/improve-multi.flux",
    "examples/improve-synthetic.flux",
    "examples/eval-synthetic.flux",
    "examples/eval-smoke.flux",
];
fn is_protected(path: &str) -> bool {
    PROTECTED
        .iter()
        .any(|e| path == *e || path.starts_with(&format!("{e}/")))
}

/// Run `git <args>` in the workspace, returning trimmed stdout (or an error with stderr on failure).
async fn git(ctx: &ToolContext, args: &[&str]) -> Result<String> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let out = ctx.system().run(&argv, GIT_TIMEOUT).await?;
    if out.exit_code != 0 {
        return Err(Error::Other(format!(
            "git {}: {}",
            args.join(" "),
            out.stderr.trim()
        )));
    }
    Ok(out.stdout.trim().to_string())
}

/// Run `git <args>` for its **exit code**, returning `(code, trimmed stderr)`.
///
/// [`git`] turns any non-zero exit into an error, which is right for a command whose only useful
/// outcome is its stdout. A predicate like `merge-base --is-ancestor` answers with its exit status
/// instead — `0` yes, `1` no, anything else "could not tell" — and those three need to be told
/// apart, because only the middle one is a refusal about the caller's snapshot.
async fn git_exit(ctx: &ToolContext, args: &[&str]) -> Result<(i32, String)> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let out = ctx.system().run(&argv, GIT_TIMEOUT).await?;
    Ok((out.exit_code, out.stderr.trim().to_string()))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

// ---------------------------------------------------------------------------
// git_snapshot
// ---------------------------------------------------------------------------

/// `git_snapshot()` — capture `HEAD` and **refuse a dirty tree** (so a round always starts clean and a
/// revert is exact). Erroring here aborts the flow — the safety floor for the autonomous loop.
pub struct GitSnapshotTool;

/// A `git status --porcelain` listing rendered as the two kinds of dirt it actually reports, each
/// with the remedy that clears *it*. Reconciled with the guarded `git_*` family (C-249; the shared
/// policy and helper live in `flux-tools`, which this crate deliberately does not depend on).
///
/// `git status --porcelain` reports untracked (`??`) entries alongside tracked ones, and a plain
/// `git stash` leaves untracked files exactly where they are — so "commit or stash first" is advice
/// the caller cannot follow, and an agent that follows it retries and fails identically. Split the
/// two and give each the remedy that actually clears it.
///
/// Every refusal in this module that has a working tree to describe uses this, so the wording
/// cannot drift between them the way C-241's hand-copied variant drifted from `git_revert`'s.
fn status_breakdown(status: &str) -> String {
    let (untracked, tracked): (Vec<&str>, Vec<&str>) = status
        .lines()
        .filter(|l| !l.trim().is_empty())
        .partition(|l| l.starts_with("??"));
    let mut body = String::new();
    if !tracked.is_empty() {
        body.push_str(&format!(
            "\nTracked changes ({}) — `git commit` or `git stash` clears these:\n{}",
            tracked.len(),
            tracked.join("\n")
        ));
    }
    if !untracked.is_empty() {
        body.push_str(&format!(
            "\nUntracked files ({}) — a plain `git stash` does NOT clear these; use \
             `git stash -u`, `git clean -fd`, or move them aside:\n{}",
            untracked.len(),
            untracked.join("\n")
        ));
    }
    body
}

/// The dirty-tree refusal wording.
fn dirty_tree_refusal(op: &str, because: &str, status: &str) -> String {
    format!(
        "{op}: refusing — this checkout has uncommitted changes, and {because}.{}",
        status_breakdown(status)
    )
}

/// Arguments for the `git_snapshot` op (none).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitSnapshotInput {}

#[async_trait]
impl Tool for GitSnapshotTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "git_snapshot",
            "Capture HEAD for later revert; errors if the working tree is dirty.",
            tool_input_schema::<GitSnapshotInput>(),
        )
        .with_access(vec![AccessKind::Process])
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let head = git(ctx, &["rev-parse", "HEAD"]).await?;
        let status = git(ctx, &["status", "--porcelain"]).await?;
        if !status.is_empty() {
            return Ok(ToolResult::error(dirty_tree_refusal(
                "git_snapshot",
                "the snapshot is the exact point `git_reset` restores the round to, so work that \
                 is not committed here is indistinguishable from the round's own changes and \
                 would be discarded with them",
                &status,
            )));
        }
        json_result(
            &json!({ "head": head, "clean": true }),
            format!("snapshot @ {}", short(&head)),
        )
    }
}

// Note: there is no `git_commit` here on purpose — the built-in `git_commit` (flux-tools) already
// commits staged changes, and `git_stage(["."])` stages all (modern `git add .` includes deletions).
// The improve loop reuses those; this module adds only the ops the built-ins lack: snapshot, tag, revert.

// ---------------------------------------------------------------------------
// git_tag
// ---------------------------------------------------------------------------

/// `git_tag(name, message?)` — tag the current commit (annotated if a message is given).
pub struct GitTagTool;

/// Arguments for the `git_tag` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitTagInput {
    /// Tag name prefix (the short HEAD sha is appended for uniqueness).
    name: String,
    /// Optional annotation message; when given, an annotated tag is created.
    #[serde(default)]
    message: Option<String>,
}

#[async_trait]
impl Tool for GitTagTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_tag".into(),
            description: "Tag the current commit (annotated when a message is given).".into(),
            input_schema: tool_input_schema::<GitTagInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: GitTagInput = crate::util::parse_params(&params, "git_tag")?;
        let prefix = args.name.as_str();
        // Append the short HEAD sha so each adopted improvement gets a unique, discoverable tag
        // (the autonomous loop tags every round; identical score scalars would otherwise collide).
        let sha = git(ctx, &["rev-parse", "HEAD"]).await?;
        let name = format!("{prefix}-{}", short(&sha));
        match args.message.as_deref() {
            Some(msg) => {
                git(ctx, &["tag", "-a", &name, "-m", msg]).await?;
            }
            None => {
                git(ctx, &["tag", &name]).await?;
            }
        }
        json_result(
            &json!({ "tag": name, "sha": sha }),
            format!("tagged {name}"),
        )
    }
}

// ---------------------------------------------------------------------------
// The precondition a blanket restore owes (C-278)
// ---------------------------------------------------------------------------

/// Whether a blanket restore is licensed, or the refusal to hand back instead.
///
/// A refusal is a recoverable [`ToolResult::error`], never a raw `Err`: the whole promise of a
/// preflight is that nothing was touched, and the caller should be able to read why.
enum Licence {
    /// Both halves of the invariant hold. Carries the verified commit to restore to.
    Granted(String),
    /// They do not. Nothing was changed.
    Refused(ToolResult),
}

/// Prove that `snap` licenses destroying everything not committed in this checkout.
///
/// The loop's invariant is *everything in this tree was produced by the step I am undoing*, and it
/// decomposes into two halves that are each independently checkable. Neither is redundant:
///
/// 1. **The snapshot established a clean tree.** [`GitSnapshotTool`] refuses a dirty tree and only
///    then emits `clean: true`, so that field is a proof-carrying token: the tree held nothing but
///    committed work at `head`, therefore everything differing from `head` now was produced after
///    the snapshot. A payload without it is a caller *asserting* the invariant rather than having
///    established it — which is exactly the trust C-278 exists to remove.
/// 2. **The snapshot is on this checkout's line.** `clean: true` is a fact about the tree at *that*
///    commit; it says nothing about the tree we are standing in unless `head` is an ancestor of
///    `HEAD`. Resetting to a commit off this line rewinds onto a divergent history and discards
///    commits and working-tree state no snapshot ever accounted for. Equality counts — the loop's
///    reject path has moved HEAD nowhere, and `--is-ancestor` is reflexive.
///
/// Both hold for every round of `examples/improve-tbench.flux` and `examples/improve-synthetic.flux`
/// (`snapshot = git_snapshot()`, then `git_reset(snapshot)` with HEAD at or ahead of it), so the
/// self-improvement loop is unaffected — which is the point: the precondition refuses the calls
/// that were never licensed, not the one the loop makes.
async fn licence_to_restore(ctx: &ToolContext, op: &str, snap: &Value) -> Result<Licence> {
    let head = snap
        .get("head")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("{op}: snapshot has no `head`")))?;

    if snap.get("clean").and_then(|v| v.as_bool()) != Some(true) {
        let status = git(ctx, &["status", "--porcelain"]).await?;
        return Ok(Licence::Refused(ToolResult::error(format!(
            "{op}: refusing — this snapshot never established that the working tree was clean when \
             it was taken (`git_snapshot` sets `clean: true` only after checking), so nothing \
             distinguishes the round's own changes from work that was already here. \
             `git reset --hard` plus `git clean -fd` would discard all of it, and `clean -fd` \
             deletes untracked files outright. Take the snapshot with `git_snapshot()` before the \
             step you intend to undo. The working tree currently holds:{}",
            status_breakdown(&status)
        ))));
    }

    match git_exit(ctx, &["merge-base", "--is-ancestor", head, "HEAD"]).await? {
        (0, _) => {}
        (1, _) => {
            let status = git(ctx, &["status", "--porcelain"]).await?;
            return Ok(Licence::Refused(ToolResult::error(format!(
                "{op}: refusing — snapshot {} is not an ancestor of this checkout's HEAD, so it was \
                 not taken on the line this round advanced. Restoring to it would rewind onto a \
                 divergent history and discard commits and working-tree state no snapshot accounts \
                 for. The working tree currently holds:{}",
                short(head),
                status_breakdown(&status)
            ))));
        }
        (_, err) => {
            return Ok(Licence::Refused(ToolResult::error(format!(
                "{op}: could not run `git merge-base --is-ancestor {} HEAD` to check its \
                 preconditions ({err}); refusing to start — nothing was changed",
                short(head)
            ))))
        }
    }

    Ok(Licence::Granted(head.to_string()))
}

// ---------------------------------------------------------------------------
// git_reset
// ---------------------------------------------------------------------------

/// `git_reset(snapshot)` — hard-reset to a snapshot and clean untracked files. **Destructive**: only
/// the top-level loop resets (never a sub-agent), discarding exactly the round's own changes.
///
/// Its precondition is [`licence_to_restore`] — the snapshot must *prove* it accounts for
/// everything in the tree, rather than the call site being trusted to have passed a good one. What
/// the restore then consumed is reported in `discarded`, because the round's log is the only place
/// an untracked file that `clean -fd` removed is ever visible again.
pub struct GitResetTool;

/// Arguments for the `git_reset` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitResetInput {
    /// a git_snapshot result (JSON)
    #[allow(dead_code)]
    snapshot: String,
}

#[async_trait]
impl Tool for GitResetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_reset".into(),
            description:
                "Hard-reset the working tree to a git_snapshot (discards the round's changes)."
                    .into(),
            input_schema: tool_input_schema::<GitResetInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Destructive,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        // Declare the destructive reset so it escalates at dispatch (re-confirm unless --yes).
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git reset --hard".to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let snap = arg(&params, "snapshot");
        let head = match licence_to_restore(ctx, "git_reset", &snap).await? {
            Licence::Granted(head) => head,
            Licence::Refused(refusal) => return Ok(refusal),
        };
        // Read what the blanket restore is about to consume *before* it is gone. `clean -fd`
        // removes untracked files outright, so without this the round's log is the only record and
        // it holds nothing but a sha.
        let discarded: Vec<String> = git(ctx, &["status", "--porcelain"])
            .await?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        git(ctx, &["reset", "--hard", &head]).await?;
        git(ctx, &["clean", "-fd"]).await?;
        let view = if discarded.is_empty() {
            format!("reset to {} (nothing to discard)", short(&head))
        } else {
            format!(
                "reset to {} — discarded {} path(s)",
                short(&head),
                discarded.len()
            )
        };
        json_result(&json!({ "reset_to": head, "discarded": discarded }), view)
    }
}

// ---------------------------------------------------------------------------
// guard_protected
// ---------------------------------------------------------------------------

/// `guard_protected(snapshot)` — the loop's integrity enforcer. After the worker edits flux, restore
/// the protected paths (grader/suite/loop-flows/scripts/CI) to the round snapshot, so the agent cannot
/// "win" by editing its own measurement. Sub-agents run with empty permissions + an auto-allow approver
/// (they CAN write anywhere non-destructively), so this top-level op — which the worker doesn't control
/// — is the real enforcement. Returns `{tampered, restored:[…]}`.
///
/// **Stated exemption from a tree precondition (C-278).** This op runs `clean -fd`, but never
/// blanket: both argv it builds end in `--` followed by an explicit pathspec list it computed
/// itself and filtered through [`is_protected`]. Its blast radius is bounded by construction rather
/// than by its caller, so a dirty tree is not a hazard here — and requiring a clean one would be
/// incoherent, since the op exists precisely to run *after* the worker has dirtied the tree. The
/// bound is enforced by `guard_protected_touches_nothing_outside_the_protected_paths`, which holds
/// untracked non-protected files specifically: those are what an unscoped `clean -fd` would delete.
pub struct GuardProtectedTool;

/// Arguments for the `guard_protected` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardProtectedInput {
    /// a git_snapshot result (JSON)
    #[allow(dead_code)]
    snapshot: String,
}

#[async_trait]
impl Tool for GuardProtectedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "guard_protected".into(),
            description: "Restore the grader/suite/loop/CI paths to the round snapshot after the worker \
                          runs, so the agent cannot game its own measurement. Returns {tampered, restored}."
                .into(),
            input_schema: tool_input_schema::<GuardProtectedInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let snap = arg(&params, "snapshot");
        let head = snap
            .get("head")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("guard_protected: snapshot has no `head`".to_string()))?;

        // Detect protected-path changes: tracked diffs (exist in `head` → restore via checkout) vs
        // untracked additions (not in `head` → remove). Only touch paths that actually changed, so a
        // missing protected path (e.g. no `.github` in this repo) is never a spurious pathspec error.
        let changed = git(ctx, &["diff", "--name-only", head]).await?;
        let untracked = git(ctx, &["ls-files", "--others", "--exclude-standard"]).await?;
        let tracked: Vec<String> = changed
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|p| !p.is_empty() && is_protected(p))
            .collect();
        let added: Vec<String> = untracked
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|p| !p.is_empty() && is_protected(p))
            .collect();
        let mut restored: Vec<String> = tracked.iter().chain(added.iter()).cloned().collect();
        restored.sort();
        restored.dedup();
        let tampered = !restored.is_empty();

        if !tracked.is_empty() {
            // Restore modified/deleted protected files to the snapshot.
            let mut checkout: Vec<&str> = vec!["checkout", head, "--"];
            checkout.extend(tracked.iter().map(String::as_str));
            git(ctx, &checkout).await?;
        }
        if !added.is_empty() {
            // Remove untracked files the worker added under protected paths.
            let mut clean: Vec<&str> = vec!["clean", "-fd", "--"];
            clean.extend(added.iter().map(String::as_str));
            git(ctx, &clean).await?;
        }

        let view = if tampered {
            format!(
                "⚠ tampering reverted: restored {} protected path(s)",
                restored.len()
            )
        } else {
            "protected paths intact".to_string()
        };
        json_result(&json!({ "tampered": tampered, "restored": restored }), view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use flux_system::{System, Workspace};

    fn sh(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "command failed: {args:?}");
    }

    /// C-249: `git_snapshot` shares the guarded family's dirty-tree refusal wording — tracked and
    /// untracked told apart, each with a remedy that actually clears it. "commit or stash first"
    /// is unactionable for the `??` entries `git status --porcelain` also reports.
    #[tokio::test]
    async fn git_snapshot_refusal_separates_tracked_from_untracked() {
        let dir = crate::util::unique_temp_dir("flux-snapshot-test").unwrap();
        std::fs::write(dir.join("tracked.rs"), "fn main() {}\n").unwrap();
        sh(&dir, &["git", "init", "-q"]);
        sh(&dir, &["git", "config", "user.email", "a@b.c"]);
        sh(&dir, &["git", "config", "user.name", "t"]);
        sh(&dir, &["git", "add", "-A"]);
        sh(&dir, &["git", "commit", "-qm", "init"]);

        let ctx = ToolContext::new(std::sync::Arc::new(System::new(
            Workspace::new(&dir).unwrap(),
        )));
        // Clean: a snapshot is taken.
        let ok = GitSnapshotTool.execute(&ctx, json!({})).await.unwrap();
        assert!(!ok.is_error, "{}", ok.content);

        // Dirty in both ways at once.
        std::fs::write(dir.join("tracked.rs"), "fn main() { /* edited */ }\n").unwrap();
        std::fs::write(dir.join("scratch.txt"), "untracked\n").unwrap();
        let r = GitSnapshotTool.execute(&ctx, json!({})).await.unwrap();
        assert!(r.is_error, "a dirty tree is refused: {}", r.content);
        assert!(
            r.content.contains("Tracked changes (1)") && r.content.contains("tracked.rs"),
            "{}",
            r.content
        );
        assert!(
            r.content.contains("Untracked files (1)") && r.content.contains("scratch.txt"),
            "{}",
            r.content
        );
        assert!(
            r.content
                .contains("a plain `git stash` does NOT clear these")
                && r.content.contains("git stash -u")
                && r.content.contains("git clean -fd"),
            "untracked entries get advice that clears them: {}",
            r.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Build a one-commit repo at `dir` and return a context rooted there.
    async fn repo(prefix: &str) -> (std::path::PathBuf, ToolContext) {
        let dir = crate::util::unique_temp_dir(prefix).unwrap();
        std::fs::write(dir.join("tracked.rs"), "fn main() {}\n").unwrap();
        sh(&dir, &["git", "init", "-q"]);
        sh(&dir, &["git", "config", "user.email", "a@b.c"]);
        sh(&dir, &["git", "config", "user.name", "t"]);
        sh(&dir, &["git", "add", "-A"]);
        sh(&dir, &["git", "commit", "-qm", "init"]);
        let ctx = ToolContext::new(std::sync::Arc::new(System::new(
            Workspace::new(&dir).unwrap(),
        )));
        (dir, ctx)
    }

    /// C-278: `git_reset` is the most destructive op in the repository — `reset --hard` followed by
    /// an **unscoped** `git clean -fd` — and before this story it ran both off nothing but a `head`
    /// string. `clean -fd` deletes untracked files outright, which is precisely the case C-249
    /// rewrote the refusal wording for.
    ///
    /// The loop's licence to destroy uncommitted work rests on an invariant — *everything in this
    /// tree was produced by the step I am undoing* — and that invariant is **establishable**, not
    /// merely assertable: [`GitSnapshotTool`] refuses a dirty tree, so a snapshot carrying
    /// `clean: true` is a proof-carrying token that the tree held nothing but committed work at
    /// `head`. This test pins both halves of the decision: a snapshot that never established
    /// cleanliness is refused with the untracked file still on disk, and the loop's own verified
    /// snapshot is honoured — with what it destroyed reported rather than silently swallowed.
    #[tokio::test]
    async fn git_reset_refuses_a_snapshot_whose_cleanliness_was_never_established() {
        let (dir, ctx) = repo("flux-reset-unverified").await;
        let head = git(&ctx, &["rev-parse", "HEAD"]).await.unwrap();

        // Work a blanket restore cannot tell from the round's own: one tracked edit, one untracked
        // file. "commit or stash them first" does not help whoever owns `scratch.txt`.
        std::fs::write(dir.join("tracked.rs"), "fn main() { /* mine */ }\n").unwrap();
        std::fs::write(dir.join("scratch.txt"), "hours of untracked work\n").unwrap();

        // An unverified snapshot: well-formed, a real `head`, and asserting nothing whatsoever
        // about the tree that head was captured from. Nothing but the call site made this safe.
        let r = GitResetTool
            .execute(
                &ctx,
                json!({ "snapshot": json!({ "head": head }).to_string() }),
            )
            .await
            .unwrap();
        assert!(
            r.is_error,
            "an unverified snapshot must not license a blanket restore: {}",
            r.content
        );
        assert!(
            dir.join("scratch.txt").exists(),
            "a refusal leaves the untracked file exactly where it was"
        );
        assert!(
            std::fs::read_to_string(dir.join("tracked.rs"))
                .unwrap()
                .contains("mine"),
            "a refusal leaves the tracked edit intact too"
        );
        // C-249's reconciled wording: the two kinds are told apart and each gets a remedy that
        // actually clears it.
        assert!(
            r.content.contains("Tracked changes (1)") && r.content.contains("tracked.rs"),
            "{}",
            r.content
        );
        assert!(
            r.content.contains("Untracked files (1)") && r.content.contains("scratch.txt"),
            "{}",
            r.content
        );
        assert!(
            r.content
                .contains("a plain `git stash` does NOT clear these")
                && r.content.contains("git stash -u"),
            "untracked entries get advice that clears them: {}",
            r.content
        );

        // The loop's own token: `git_snapshot` verified this tree was clean at `head`, so
        // everything above was produced after it. The destruction is licensed — and reported.
        let ok = GitResetTool
            .execute(
                &ctx,
                json!({ "snapshot": json!({ "head": head, "clean": true }).to_string() }),
            )
            .await
            .unwrap();
        assert!(
            !ok.is_error,
            "a verified snapshot still resets: {}",
            ok.content
        );
        assert!(
            !dir.join("scratch.txt").exists(),
            "the licensed reset still cleans untracked files — that is the loop's contract"
        );
        assert!(
            ok.content.contains("scratch.txt") && ok.content.contains("tracked.rs"),
            "what the reset destroyed is reported, not swallowed: {}",
            ok.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-278, the other half of the invariant. `clean: true` proves the tree was clean *at that
    /// commit*; it says nothing about the tree we are standing in now unless the snapshot is on
    /// this checkout's own line. Resetting to a commit that is not an ancestor of `HEAD` rewinds
    /// onto a divergent history and destroys work no snapshot ever accounted for.
    #[tokio::test]
    async fn git_reset_refuses_a_snapshot_taken_off_this_checkouts_line() {
        let (dir, ctx) = repo("flux-reset-divergent").await;
        let base = git(&ctx, &["rev-parse", "HEAD"]).await.unwrap();

        // A divergent commit: real, in this repository, clean when made — and not an ancestor of
        // where the round actually is.
        sh(&dir, &["git", "checkout", "-q", "-b", "other"]);
        std::fs::write(dir.join("elsewhere.rs"), "fn other() {}\n").unwrap();
        sh(&dir, &["git", "add", "-A"]);
        sh(&dir, &["git", "commit", "-qm", "divergent"]);
        let divergent = git(&ctx, &["rev-parse", "HEAD"]).await.unwrap();
        sh(&dir, &["git", "checkout", "-q", "-"]);
        assert_eq!(git(&ctx, &["rev-parse", "HEAD"]).await.unwrap(), base);

        std::fs::write(dir.join("scratch.txt"), "the round's work\n").unwrap();
        let r = GitResetTool
            .execute(
                &ctx,
                json!({ "snapshot": json!({ "head": divergent, "clean": true }).to_string() }),
            )
            .await
            .unwrap();
        assert!(
            r.is_error,
            "a snapshot off this checkout's line must be refused: {}",
            r.content
        );
        assert!(
            dir.join("scratch.txt").exists(),
            "a refusal leaves the working tree untouched"
        );
        assert_eq!(
            git(&ctx, &["rev-parse", "HEAD"]).await.unwrap(),
            base,
            "and leaves HEAD where it was"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-278: `guard_protected`'s stated exemption, enforced rather than inferred from its call
    /// sites. It runs `checkout <head> --` and `clean -fd --`, but never blanket: both argv are
    /// built from paths it computed itself and filtered through [`is_protected`], so its blast
    /// radius is bounded by construction and does not depend on who calls it. This test is what
    /// holds that claim — including for **untracked** non-protected files, which an unscoped
    /// `clean -fd` would delete and which the existing tampering test never covered.
    #[tokio::test]
    async fn guard_protected_touches_nothing_outside_the_protected_paths() {
        let dir = crate::util::unique_temp_dir("flux-guard-radius").unwrap();
        std::fs::create_dir_all(dir.join("crates/flux-eval/src")).unwrap();
        std::fs::create_dir_all(dir.join("crates/flux-tools/src")).unwrap();
        std::fs::write(
            dir.join("crates/flux-eval/src/score.rs"),
            "pub const A: u8 = 1;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("crates/flux-tools/src/lib.rs"),
            "pub const B: u8 = 1;\n",
        )
        .unwrap();
        sh(&dir, &["git", "init", "-q"]);
        sh(&dir, &["git", "config", "user.email", "a@b.c"]);
        sh(&dir, &["git", "config", "user.name", "t"]);
        sh(&dir, &["git", "add", "-A"]);
        sh(&dir, &["git", "commit", "-qm", "init"]);

        let ctx = ToolContext::new(std::sync::Arc::new(System::new(
            Workspace::new(&dir).unwrap(),
        )));
        let head = git(&ctx, &["rev-parse", "HEAD"]).await.unwrap();

        // Every combination at once: tracked/untracked × protected/not.
        std::fs::write(
            dir.join("crates/flux-eval/src/score.rs"),
            "pub const A: u8 = 99;\n",
        )
        .unwrap();
        std::fs::write(dir.join("crates/flux-eval/src/cheat.rs"), "// sneaky\n").unwrap();
        std::fs::write(
            dir.join("crates/flux-tools/src/lib.rs"),
            "pub const B: u8 = 42;\n",
        )
        .unwrap();
        std::fs::write(dir.join("crates/flux-tools/src/new_op.rs"), "// the work\n").unwrap();

        let out = GuardProtectedTool
            .execute(
                &ctx,
                json!({ "snapshot": json!({"head": head}).to_string() }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);

        // Protected: restored / removed.
        assert_eq!(
            std::fs::read_to_string(dir.join("crates/flux-eval/src/score.rs")).unwrap(),
            "pub const A: u8 = 1;\n"
        );
        assert!(!dir.join("crates/flux-eval/src/cheat.rs").exists());
        // Not protected: untouched, whether git was tracking it or not.
        assert_eq!(
            std::fs::read_to_string(dir.join("crates/flux-tools/src/lib.rs")).unwrap(),
            "pub const B: u8 = 42;\n",
            "a tracked non-protected edit is the worker's legitimate output"
        );
        assert!(
            dir.join("crates/flux-tools/src/new_op.rs").exists(),
            "an UNTRACKED non-protected file must survive — the bounded blast radius is the whole \
             reason this op needs no clean-tree precondition"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn guard_protected_restores_grader_and_loop_tampering() {
        let dir = crate::util::unique_temp_dir("flux-guard-test").unwrap();
        std::fs::create_dir_all(dir.join("crates/flux-eval/src")).unwrap();
        std::fs::create_dir_all(dir.join("bench")).unwrap();
        // A committed grader file + loop-harness file + an unrelated source file.
        std::fs::write(
            dir.join("crates/flux-eval/src/score.rs"),
            "pub const A: u8 = 1;\n",
        )
        .unwrap();
        std::fs::write(dir.join("bench/run-tbench-loop.sh"), "echo run\n").unwrap();
        std::fs::write(dir.join("src.rs"), "fn main() {}\n").unwrap();
        sh(&dir, &["git", "init", "-q"]);
        sh(&dir, &["git", "config", "user.email", "a@b.c"]);
        sh(&dir, &["git", "config", "user.name", "t"]);
        sh(&dir, &["git", "add", "-A"]);
        sh(&dir, &["git", "commit", "-qm", "init"]);

        let ctx = ToolContext::new(std::sync::Arc::new(System::new(
            Workspace::new(&dir).unwrap(),
        )));
        let head = git(&ctx, &["rev-parse", "HEAD"]).await.unwrap();

        // Worker "tampers": edits the grader + loop harness, adds an untracked grader file, and edits
        // an allowed source file.
        std::fs::write(
            dir.join("crates/flux-eval/src/score.rs"),
            "pub const A: u8 = 99;\n",
        )
        .unwrap();
        std::fs::write(dir.join("bench/run-tbench-loop.sh"), "echo gamed\n").unwrap();
        std::fs::write(dir.join("crates/flux-eval/src/cheat.rs"), "// sneaky\n").unwrap();
        std::fs::write(dir.join("src.rs"), "fn main() { /* legit */ }\n").unwrap();

        let out = GuardProtectedTool
            .execute(
                &ctx,
                json!({ "snapshot": json!({"head": head}).to_string() }),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("\"tampered\":true"), "{}", out.content);

        // Protected paths restored to the snapshot; untracked grader file removed.
        assert_eq!(
            std::fs::read_to_string(dir.join("crates/flux-eval/src/score.rs")).unwrap(),
            "pub const A: u8 = 1;\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("bench/run-tbench-loop.sh")).unwrap(),
            "echo run\n"
        );
        assert!(!dir.join("crates/flux-eval/src/cheat.rs").exists());
        // The allowed (non-protected) edit survives.
        assert!(std::fs::read_to_string(dir.join("src.rs"))
            .unwrap()
            .contains("legit"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
