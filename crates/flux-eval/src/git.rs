//! Git ops for the keep/commit/reset loop. All exec goes through `ctx.system().run` (argv-only, no
//! shell). These are registered on the **top-level** registry only — never a sub-agent's — so a worker
//! can edit files but cannot itself `git reset --hard`. `git_reset` is `Risk::Destructive` and so still
//! re-confirms at dispatch unless `--yes` (the autonomous loop runs with `--yes`).

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

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

// ---------------------------------------------------------------------------
// git_snapshot
// ---------------------------------------------------------------------------

/// `git_snapshot()` — capture `HEAD` and **refuse a dirty tree** (so a round always starts clean and a
/// revert is exact). Erroring here aborts the flow — the safety floor for the autonomous loop.
pub struct GitSnapshotTool;

/// The dirty-tree refusal wording, reconciled with the guarded `git_*` family (C-249; the shared
/// policy and helper live in `flux-tools`, which this crate deliberately does not depend on).
///
/// `git status --porcelain` reports untracked (`??`) entries alongside tracked ones, and a plain
/// `git stash` leaves untracked files exactly where they are — so "commit or stash first" is advice
/// the caller cannot follow, and an agent that follows it retries and fails identically. Split the
/// two and give each the remedy that actually clears it.
fn dirty_tree_refusal(op: &str, because: &str, status: &str) -> String {
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
    format!("{op}: refusing — this checkout has uncommitted changes, and {because}.{body}")
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
// git_reset
// ---------------------------------------------------------------------------

/// `git_reset(snapshot)` — hard-reset to a snapshot and clean untracked files. **Destructive**: only
/// the top-level loop resets (never a sub-agent), discarding exactly the round's own changes.
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
        let head = snap
            .get("head")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("git_reset: snapshot has no `head`".to_string()))?;
        git(ctx, &["reset", "--hard", head]).await?;
        git(ctx, &["clean", "-fd"]).await?;
        json_result(
            &json!({ "reset_to": head }),
            format!("reset to {}", short(head)),
        )
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
