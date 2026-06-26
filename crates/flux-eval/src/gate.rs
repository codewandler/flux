//! `gate_check()` — run the project dev-gate (build / test / clippy / fmt) and return `"pass"`/`"fail"`.
//!
//! This is the **hard precondition for keeping** a candidate: the improve loop only commits when the
//! gate is green (and, separately, the eval score improved). It runs the same commands AGENTS.md
//! mandates, via `ctx.system.run` (argv-only), in the workspace/worktree the loop operates on.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::Result;
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};

/// One gate step: a label, its argv, and the params key that toggles it.
const STEPS: &[(&str, &[&str], &str)] = &[
    ("build", &["cargo", "build", "--workspace"], "build"),
    ("test", &["cargo", "test", "--workspace"], "test"),
    (
        "clippy",
        &[
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        "clippy",
    ),
    ("fmt", &["cargo", "fmt", "--all", "--", "--check"], "fmt"),
];

/// `gate_check()` — build/test/clippy/fmt; `"pass"` only if every enabled step exits 0.
pub struct GateCheckTool;

#[async_trait]
impl Tool for GateCheckTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "gate_check".into(),
            description: "Run the dev-gate (cargo build/test/clippy/fmt --check) and return \"true\" \
                          (all green) or \"false\". Toggle steps with booleans build/test/clippy/fmt \
                          (default all on); `timeout_secs` bounds each step."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "build": {"type": "boolean"},
                    "test": {"type": "boolean"},
                    "clippy": {"type": "boolean"},
                    "fmt": {"type": "boolean"},
                    "timeout_secs": {"type": "integer"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let enabled = |key: &str| params.get(key).and_then(|v| v.as_bool()).unwrap_or(true);
        let timeout = Duration::from_secs(
            params
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(1800),
        );

        for (label, argv, key) in STEPS {
            if !enabled(key) {
                continue;
            }
            let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            let out = ctx.system.run(&argv, timeout).await?;
            if out.exit_code != 0 {
                // Show a tail of the failure so the loop's transcript is diagnostic.
                let mut blob = format!("{}\n{}", out.stdout, out.stderr);
                let tail: String = blob.chars().rev().take(1200).collect::<String>();
                blob = tail.chars().rev().collect();
                return Ok(ToolResult::ok_view(
                    "false",
                    format!(
                        "gate FAILED at `{label}` (exit {}):\n…{}",
                        out.exit_code,
                        blob.trim()
                    ),
                ));
            }
        }
        Ok(ToolResult::ok_view(
            "true",
            "gate green (build · test · clippy · fmt)",
        ))
    }
}
