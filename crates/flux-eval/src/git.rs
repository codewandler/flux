//! Git ops for the keep/commit/revert loop. All exec goes through `ctx.system.run` (argv-only, no
//! shell). These are registered on the **top-level** registry only — never a sub-agent's — so a worker
//! can edit files but cannot itself `git reset --hard`. `git_revert` is `Risk::Destructive` and so still
//! re-confirms at dispatch unless `--yes` (the autonomous loop runs with `--yes`).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};

use crate::util::{arg, json_result, str_field};

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
    let out = ctx.system.run(&argv, GIT_TIMEOUT).await?;
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

#[async_trait]
impl Tool for GitSnapshotTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "git_snapshot",
            "Capture HEAD for later revert; errors if the working tree is dirty.",
            json!({ "type": "object", "properties": {} }),
        )
        .with_access(vec![AccessKind::Process])
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let head = git(ctx, &["rev-parse", "HEAD"]).await?;
        let status = git(ctx, &["status", "--porcelain"]).await?;
        if !status.is_empty() {
            return Ok(ToolResult::error(format!(
                "refusing to operate on a dirty working tree — commit or stash first:\n{status}"
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

#[async_trait]
impl Tool for GitTagTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_tag".into(),
            description: "Tag the current commit (annotated when a message is given).".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "name": {"type": "string"}, "message": {"type": "string"} },
                "required": ["name"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let prefix = str_field(&params, "name")
            .ok_or_else(|| Error::Other("git_tag: `name` required".to_string()))?;
        // Append the short HEAD sha so each adopted improvement gets a unique, discoverable tag
        // (the autonomous loop tags every round; identical score scalars would otherwise collide).
        let sha = git(ctx, &["rev-parse", "HEAD"]).await?;
        let name = format!("{prefix}-{}", short(&sha));
        match str_field(&params, "message") {
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
// git_revert
// ---------------------------------------------------------------------------

/// `git_revert(snapshot)` — hard-reset to a snapshot and clean untracked files. **Destructive**: only
/// the top-level loop reverts (never a sub-agent), discarding exactly the round's own changes.
pub struct GitRevertTool;

#[async_trait]
impl Tool for GitRevertTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_revert".into(),
            description:
                "Hard-reset the working tree to a git_snapshot (discards the round's changes)."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "snapshot": {"type": "string", "description": "a git_snapshot result (JSON)"} },
                "required": ["snapshot"]
            }),
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
            .ok_or_else(|| Error::Other("git_revert: snapshot has no `head`".to_string()))?;
        git(ctx, &["reset", "--hard", head]).await?;
        git(ctx, &["clean", "-fd"]).await?;
        json_result(
            &json!({ "reset_to": head }),
            format!("reverted to {}", short(head)),
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

#[async_trait]
impl Tool for GuardProtectedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "guard_protected".into(),
            description: "Restore the grader/suite/loop/CI paths to the round snapshot after the worker \
                          runs, so the agent cannot game its own measurement. Returns {tampered, restored}."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "snapshot": {"type": "string", "description": "a git_snapshot result (JSON)"} },
                "required": ["snapshot"]
            }),
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use flux_system::{System, Workspace};

    static N: AtomicU64 = AtomicU64::new(0);

    fn sh(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "command failed: {args:?}");
    }

    #[tokio::test]
    async fn guard_protected_restores_grader_and_loop_tampering() {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-guard-test-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
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
