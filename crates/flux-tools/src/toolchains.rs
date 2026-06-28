//! Non-Rust toolchain tools, grouped by language signal so they surface only in the right workspace:
//!
//! - **python** (`pyproject.toml`/`requirements.txt`): `python_run`, `pytest`
//! - **node** (`package.json`): `npm`, `node_run`
//! - **go** (`go.mod`): `go_build`, `go_test`, `go_vet`
//! - **make** (`Makefile`): `make`
//!
//! Like the `cargo_*` ops these are **argv-only** invocations through the guarded `System` — no shell
//! strings, so model input can never be interpreted by a shell. They mutate the local filesystem /
//! run processes, so each declares `Effect::Process + Effect::LocalSystem` at `Risk::Medium`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::Result;
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};

/// Builds and test runs can be slow on a cold cache — match the cargo timeout.
const TOOLCHAIN_TIMEOUT_SECS: u64 = 600;

/// Spec for an argv-only process tool gated behind `group`.
fn proc_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    idempotency: Idempotency,
    group: &str,
) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema: None,
        effects: vec![Effect::Process, Effect::LocalSystem],
        risk: Risk::Medium,
        idempotency,
        access: vec![AccessKind::Process],
        group: Some(group.into()),
    }
}

fn proc_intent(command: String) -> IntentSet {
    let mut set = IntentSet::new();
    set.push(Intent {
        behavior: IntentBehavior::CommandExecution,
        target: IntentTarget::Process { command },
        role: IntentRole::ProcessCommand,
        certainty: IntentCertainty::Certain,
    });
    set
}

/// Append a string `args` array param onto an argv, ignoring non-string entries.
fn push_args(argv: &mut Vec<String>, params: &Value) {
    if let Some(extra) = params.get("args").and_then(|v| v.as_array()) {
        argv.extend(extra.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
}

/// Run an argv through the guarded system, folding stdout+stderr and surfacing a non-zero exit as an
/// error result (same shape as the `cargo_*` ops).
fn run_argv(
    ctx: &ToolContext,
    argv: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>> {
    Box::pin(async move {
        let out = ctx
            .system
            .run(&argv, Duration::from_secs(TOOLCHAIN_TIMEOUT_SECS))
            .await?;
        let mut body = String::new();
        if !out.stdout.is_empty() {
            body.push_str(&out.stdout);
        }
        if !out.stderr.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&out.stderr);
        }
        if out.exit_code != 0 {
            body.push_str(&format!("\n[exit {}]", out.exit_code));
            return Ok(ToolResult::error(body));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "ok".to_string()
        } else {
            body
        }))
    })
}

/// `extra flags` schema fragment reused by most tools.
fn args_prop() -> Value {
    json!({"type": "array", "items": {"type": "string"}, "description": "Extra command-line flags"})
}

// ---------------------------------------------------------------------------
// python: python_run, pytest
// ---------------------------------------------------------------------------

pub struct PythonRunTool;

#[async_trait]
impl Tool for PythonRunTool {
    fn spec(&self) -> ToolSpec {
        proc_spec(
            "python_run",
            "Run Python: a script file (`script`) or a module (`module`, like `python -m`). \
             Optional `args` are passed through. Uses `python3`.",
            json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string", "description": "Path to a .py file to run"},
                    "module": {"type": "string", "description": "Module to run with -m (alternative to script)"},
                    "args": args_prop()
                }
            }),
            Idempotency::NonIdempotent,
            "python",
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let target = params
            .get("script")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("module").and_then(|v| v.as_str()))
            .unwrap_or("*");
        vec![format!("python_run:{target}")]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("python3".into())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut argv = vec!["python3".to_string()];
        if let Some(m) = params.get("module").and_then(|v| v.as_str()) {
            argv.push("-m".to_string());
            argv.push(m.to_string());
        } else if let Some(s) = params.get("script").and_then(|v| v.as_str()) {
            argv.push(s.to_string());
        } else {
            return Ok(ToolResult::error(
                "python_run: provide either `script` (a .py file) or `module` (for -m)".to_string(),
            ));
        }
        push_args(&mut argv, &params);
        run_argv(ctx, argv).await
    }
}

pub struct PytestTool;

#[async_trait]
impl Tool for PytestTool {
    fn spec(&self) -> ToolSpec {
        proc_spec(
            "pytest",
            "Run `pytest`. Optional `path` scopes the run to a file/directory; `args` passes extra \
             flags (e.g. `[\"-k\", \"name\"]`).",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File or directory to test (omit for the whole suite)"},
                    "args": args_prop()
                }
            }),
            Idempotency::NonIdempotent,
            "python",
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("*");
        vec![format!("pytest:{path}")]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("pytest".into())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut argv = vec!["pytest".to_string()];
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            argv.push(p.to_string());
        }
        push_args(&mut argv, &params);
        run_argv(ctx, argv).await
    }
}

// ---------------------------------------------------------------------------
// node: npm, node_run
// ---------------------------------------------------------------------------

pub struct NpmTool;

#[async_trait]
impl Tool for NpmTool {
    fn spec(&self) -> ToolSpec {
        proc_spec(
            "npm",
            "Run an `npm` command — `args` is the full argument vector (e.g. `[\"install\"]`, \
             `[\"run\", \"build\"]`, `[\"test\"]`).",
            json!({
                "type": "object",
                "properties": {"args": args_prop()},
                "required": ["args"]
            }),
            Idempotency::NonIdempotent,
            "node",
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let first = params
            .get("args")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("*");
        vec![format!("npm:{first}")]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("npm".into())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut argv = vec!["npm".to_string()];
        push_args(&mut argv, &params);
        if argv.len() == 1 {
            return Ok(ToolResult::error(
                "npm: `args` must contain at least one argument (e.g. [\"install\"])".to_string(),
            ));
        }
        run_argv(ctx, argv).await
    }
}

pub struct NodeRunTool;

#[async_trait]
impl Tool for NodeRunTool {
    fn spec(&self) -> ToolSpec {
        proc_spec(
            "node_run",
            "Run a JavaScript file with `node`. `script` is the file path; optional `args` are passed \
             through.",
            json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string", "description": "Path to a .js file to run"},
                    "args": args_prop()
                },
                "required": ["script"]
            }),
            Idempotency::NonIdempotent,
            "node",
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let s = params.get("script").and_then(|v| v.as_str()).unwrap_or("*");
        vec![format!("node_run:{s}")]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("node".into())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let script = match params.get("script").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return Ok(ToolResult::error(
                    "node_run: required param `script` missing".to_string(),
                ))
            }
        };
        let mut argv = vec!["node".to_string(), script];
        push_args(&mut argv, &params);
        run_argv(ctx, argv).await
    }
}

// ---------------------------------------------------------------------------
// go: go_build, go_test, go_vet
// ---------------------------------------------------------------------------

/// Shared executor for the `go <sub>` tools: defaults the package set to `./...`.
fn go_tool_spec(name: &str, sub: &str, idempotency: Idempotency) -> ToolSpec {
    proc_spec(
        name,
        &format!(
            "Run `go {sub}` (defaults to `./...`). Optional `package` overrides the target; `args` \
             passes extra flags."
        ),
        json!({
            "type": "object",
            "properties": {
                "package": {"type": "string", "description": "Package/path to target (default ./...)"},
                "args": args_prop()
            }
        }),
        idempotency,
        "go",
    )
}

async fn go_run(ctx: &ToolContext, sub: &str, params: Value) -> Result<ToolResult> {
    let mut argv = vec!["go".to_string(), sub.to_string()];
    if let Some(extra) = params.get("args").and_then(|v| v.as_array()) {
        argv.extend(extra.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
    argv.push(
        params
            .get("package")
            .and_then(|v| v.as_str())
            .unwrap_or("./...")
            .to_string(),
    );
    run_argv(ctx, argv).await
}

fn go_subjects(name: &str, params: &Value) -> Vec<String> {
    let pkg = params
        .get("package")
        .and_then(|v| v.as_str())
        .unwrap_or("./...");
    vec![format!("{name}:{pkg}")]
}

pub struct GoBuildTool;

#[async_trait]
impl Tool for GoBuildTool {
    fn spec(&self) -> ToolSpec {
        go_tool_spec("go_build", "build", Idempotency::Idempotent)
    }
    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        go_subjects("go_build", params)
    }
    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("go build".into())
    }
    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        go_run(ctx, "build", params).await
    }
}

pub struct GoTestTool;

#[async_trait]
impl Tool for GoTestTool {
    fn spec(&self) -> ToolSpec {
        go_tool_spec("go_test", "test", Idempotency::NonIdempotent)
    }
    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        go_subjects("go_test", params)
    }
    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("go test".into())
    }
    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        go_run(ctx, "test", params).await
    }
}

pub struct GoVetTool;

#[async_trait]
impl Tool for GoVetTool {
    fn spec(&self) -> ToolSpec {
        go_tool_spec("go_vet", "vet", Idempotency::Idempotent)
    }
    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        go_subjects("go_vet", params)
    }
    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("go vet".into())
    }
    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        go_run(ctx, "vet", params).await
    }
}

// ---------------------------------------------------------------------------
// make
// ---------------------------------------------------------------------------

pub struct MakeTool;

#[async_trait]
impl Tool for MakeTool {
    fn spec(&self) -> ToolSpec {
        proc_spec(
            "make",
            "Run `make` with an optional `target` (e.g. `build`, `test`); `args` passes extra flags.",
            json!({
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "Make target (omit for the default target)"},
                    "args": args_prop()
                }
            }),
            Idempotency::NonIdempotent,
            "make",
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let target = params
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        vec![format!("make:{target}")]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        proc_intent("make".into())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut argv = vec!["make".to_string()];
        if let Some(t) = params.get("target").and_then(|v| v.as_str()) {
            argv.push(t.to_string());
        }
        push_args(&mut argv, &params);
        run_argv(ctx, argv).await
    }
}

/// Register all non-Rust toolchain tools into a registry.
pub fn register_toolchains(registry: &mut ToolRegistry) {
    registry.register(Arc::new(PythonRunTool));
    registry.register(Arc::new(PytestTool));
    registry.register(Arc::new(NpmTool));
    registry.register(Arc::new(NodeRunTool));
    registry.register(Arc::new(GoBuildTool));
    registry.register(Arc::new(GoTestTool));
    registry.register(Arc::new(GoVetTool));
    registry.register(Arc::new(MakeTool));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_grouped_and_medium_risk() {
        let mut reg = ToolRegistry::new();
        register_toolchains(&mut reg);
        let mut names = reg.names();
        names.sort();
        assert_eq!(
            names,
            vec![
                "go_build",
                "go_test",
                "go_vet",
                "make",
                "node_run",
                "npm",
                "pytest",
                "python_run"
            ]
        );
        for spec in reg.specs() {
            assert_eq!(spec.risk, Risk::Medium, "{} risk", spec.name);
            assert!(spec.has_effect(Effect::Process), "{} effect", spec.name);
            assert!(spec.group.is_some(), "{} is gated by a group", spec.name);
        }
    }

    #[test]
    fn toolchain_ops_map_to_expected_groups() {
        let mut reg = ToolRegistry::new();
        register_toolchains(&mut reg);
        let group_of = |n: &str| {
            reg.specs()
                .into_iter()
                .find(|s| s.name == n)
                .and_then(|s| s.group)
                .unwrap()
        };
        assert_eq!(group_of("python_run"), "python");
        assert_eq!(group_of("pytest"), "python");
        assert_eq!(group_of("npm"), "node");
        assert_eq!(group_of("node_run"), "node");
        assert_eq!(group_of("go_build"), "go");
        assert_eq!(group_of("make"), "make");
    }
}
