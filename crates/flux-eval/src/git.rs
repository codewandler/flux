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
