//! Rust toolchain tools: cargo_check, cargo_build, cargo_test, cargo_clippy, cargo_fmt.
//!
//! These are argv-only invocations through the guarded System — no shell strings.
//! Risk is Medium (they mutate build artefacts / the local filesystem) except cargo_fmt
//! which is Idempotent.

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

/// Timeout for cargo commands — cold workspace builds can be slow.
const CARGO_TIMEOUT_SECS: u64 = 600;

fn cargo_intent(subcommand: &str) -> IntentSet {
    let mut set = IntentSet::new();
    set.push(Intent {
        behavior: IntentBehavior::CommandExecution,
        target: IntentTarget::Process {
            command: format!("cargo {subcommand}"),
        },
        role: IntentRole::ProcessCommand,
        certainty: IntentCertainty::Certain,
    });
    set
}

fn cargo_run(
    ctx: &ToolContext,
    argv: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>> {
    Box::pin(async move {
        let out = ctx
            .system
            .run(&argv, Duration::from_secs(CARGO_TIMEOUT_SECS))
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

fn push_manifest_path(argv: &mut Vec<String>, params: &Value) {
    if let Some(path) = params.get("manifest_path").and_then(|v| v.as_str()) {
        argv.push("--manifest-path".to_string());
        argv.push(path.to_string());
    }
}

fn push_package_or_workspace(argv: &mut Vec<String>, params: &Value, workspace_flag: &str) {
    match params.get("package").and_then(|v| v.as_str()) {
        Some(p) => {
            argv.push("-p".to_string());
            argv.push(p.to_string());
        }
        None => argv.push(workspace_flag.to_string()),
    }
}

fn push_args(argv: &mut Vec<String>, params: &Value) {
    if let Some(extra) = params.get("args").and_then(|v| v.as_array()) {
        argv.extend(extra.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
}

fn cargo_subjects(tool: &str, params: &Value) -> Vec<String> {
    let pkg = params
        .get("package")
        .and_then(|v| v.as_str())
        .unwrap_or("*");
    let mut subjects = vec![format!("{tool}:{pkg}")];
    if let Some(manifest) = params.get("manifest_path").and_then(|v| v.as_str()) {
        subjects.push(format!("manifest:{manifest}"));
    }
    subjects
}

fn cargo_check_argv(params: &Value) -> Vec<String> {
    let mut argv = vec!["cargo".to_string(), "check".to_string()];
    push_manifest_path(&mut argv, params);
    push_package_or_workspace(&mut argv, params, "--workspace");
    push_args(&mut argv, params);
    argv
}

fn cargo_build_argv(params: &Value) -> Vec<String> {
    let mut argv = vec!["cargo".to_string(), "build".to_string()];
    push_manifest_path(&mut argv, params);
    push_package_or_workspace(&mut argv, params, "--workspace");
    if params
        .get("release")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        argv.push("--release".to_string());
    }
    push_args(&mut argv, params);
    argv
}

fn cargo_test_argv(params: &Value) -> Vec<String> {
    let mut argv = vec!["cargo".to_string(), "test".to_string()];
    push_manifest_path(&mut argv, params);
    push_package_or_workspace(&mut argv, params, "--workspace");
    push_args(&mut argv, params);
    if let Some(f) = params.get("filter").and_then(|v| v.as_str()) {
        argv.push("--".to_string());
        argv.push(f.to_string());
    }
    argv
}

fn cargo_clippy_argv(params: &Value) -> Vec<String> {
    let mut argv = vec!["cargo".to_string(), "clippy".to_string()];
    push_manifest_path(&mut argv, params);
    match params.get("package").and_then(|v| v.as_str()) {
        Some(p) => {
            argv.push("-p".to_string());
            argv.push(p.to_string());
        }
        None => {
            argv.push("--workspace".to_string());
            argv.push("--all-targets".to_string());
        }
    }
    push_args(&mut argv, params);
    if params
        .get("deny_warnings")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        argv.push("--".to_string());
        argv.push("-D".to_string());
        argv.push("warnings".to_string());
    }
    argv
}

fn cargo_fmt_argv(params: &Value) -> Vec<String> {
    let mut argv = vec!["cargo".to_string(), "fmt".to_string()];
    push_manifest_path(&mut argv, params);
    push_package_or_workspace(&mut argv, params, "--all");
    if params
        .get("check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        argv.push("--".to_string());
        argv.push("--check".to_string());
    }
    argv
}

// ---------------------------------------------------------------------------
// cargo_check
// ---------------------------------------------------------------------------

pub struct CargoCheckTool;

#[async_trait]
impl Tool for CargoCheckTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_check".into(),
            description:
                "Run `cargo check` in the workspace (or a specific package with `package`). \
                          Use `manifest_path` for a nested workspace Cargo.toml. \
                          Faster than build — only type-checks, no codegen. Optional `args` passes \
                          extra flags (e.g. `[\"--all-targets\"]`). Risk: Medium."
                    .into(),
            input_schema: json!({
                "type": "object",
                "x-param-order": ["package", "manifest_path", "args"],
                "properties": {
                    "package": {"type": "string", "description": "Specific package to check (omit for --workspace)"},
                    "manifest_path": {"type": "string", "description": "Path to Cargo.toml for a nested workspace"},
                    "args": {"type": "array", "items": {"type": "string"}, "description": "Extra cargo flags"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: Some("rust".into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        cargo_subjects("cargo_check", params)
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        cargo_intent("check")
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        cargo_run(ctx, cargo_check_argv(&params)).await
    }
}

// ---------------------------------------------------------------------------
// cargo_build
// ---------------------------------------------------------------------------

pub struct CargoBuildTool;

#[async_trait]
impl Tool for CargoBuildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_build".into(),
            description: "Run `cargo build` in the workspace (or a specific package with `package`). \
                          Use `manifest_path` for a nested workspace Cargo.toml. Pass `release: true` \
                          for an optimised build. Optional `args` for extra flags."
                .into(),
            input_schema: json!({
                "type": "object",
                "x-param-order": ["package", "manifest_path", "release", "args"],
                "properties": {
                    "package": {"type": "string", "description": "Specific package (omit for --workspace)"},
                    "manifest_path": {"type": "string", "description": "Path to Cargo.toml for a nested workspace"},
                    "release": {"type": "boolean", "description": "Build in release mode"},
                    "args": {"type": "array", "items": {"type": "string"}, "description": "Extra cargo flags"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: Some("rust".into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        cargo_subjects("cargo_build", params)
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        cargo_intent("build")
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        cargo_run(ctx, cargo_build_argv(&params)).await
    }
}

// ---------------------------------------------------------------------------
// cargo_test
// ---------------------------------------------------------------------------

pub struct CargoTestTool;

#[async_trait]
impl Tool for CargoTestTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_test".into(),
            description: "Run `cargo test` in the workspace (or a specific package with `package`). \
                          Use `manifest_path` for a nested workspace Cargo.toml. \
                          Optional `filter` is passed as the test-name filter, `args` for extra cargo flags."
                .into(),
            input_schema: json!({
                "type": "object",
                "x-param-order": ["package", "manifest_path", "filter", "args"],
                "properties": {
                    "package": {"type": "string", "description": "Specific package (omit for --workspace)"},
                    "manifest_path": {"type": "string", "description": "Path to Cargo.toml for a nested workspace"},
                    "filter": {"type": "string", "description": "Test name filter substring"},
                    "args": {"type": "array", "items": {"type": "string"}, "description": "Extra cargo flags"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: Some("rust".into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        cargo_subjects("cargo_test", params)
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        cargo_intent("test")
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        cargo_run(ctx, cargo_test_argv(&params)).await
    }
}

// ---------------------------------------------------------------------------
// cargo_clippy
// ---------------------------------------------------------------------------

pub struct CargoClippyTool;

#[async_trait]
impl Tool for CargoClippyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_clippy".into(),
            description: "Run `cargo clippy` in the workspace (or a specific package). Use \
                          `manifest_path` for a nested workspace Cargo.toml. Pass \
                          `deny_warnings: true` to add `-- -D warnings` (CI-clean check). \
                          Optional `args` for extra flags."
                .into(),
            input_schema: json!({
                "type": "object",
                "x-param-order": ["package", "manifest_path", "deny_warnings", "args"],
                "properties": {
                    "package": {"type": "string", "description": "Specific package (omit for --workspace)"},
                    "manifest_path": {"type": "string", "description": "Path to Cargo.toml for a nested workspace"},
                    "deny_warnings": {"type": "boolean", "description": "Fail on any warning (-D warnings)"},
                    "args": {"type": "array", "items": {"type": "string"}, "description": "Extra cargo flags"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: Some("rust".into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        cargo_subjects("cargo_clippy", params)
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        cargo_intent("clippy")
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        cargo_run(ctx, cargo_clippy_argv(&params)).await
    }
}

// ---------------------------------------------------------------------------
// cargo_fmt
// ---------------------------------------------------------------------------

pub struct CargoFmtTool;

#[async_trait]
impl Tool for CargoFmtTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_fmt".into(),
            description: "Run `cargo fmt --all` to format the entire workspace (or a specific \
                          package). Use `manifest_path` for a nested workspace Cargo.toml. Pass \
                          `check: true` to only check formatting without writing."
                .into(),
            input_schema: json!({
                "type": "object",
                "x-param-order": ["package", "manifest_path", "check"],
                "properties": {
                    "package": {"type": "string", "description": "Specific package (omit for --all)"},
                    "manifest_path": {"type": "string", "description": "Path to Cargo.toml for a nested workspace"},
                    "check": {"type": "boolean", "description": "Check only, don't write (-- --check)"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: Some("rust".into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        cargo_subjects("cargo_fmt", params)
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        cargo_intent("fmt")
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        cargo_run(ctx, cargo_fmt_argv(&params)).await
    }
}

/// Register all cargo tools into a registry.
pub fn register_cargo(registry: &mut ToolRegistry) {
    registry.register(Arc::new(CargoCheckTool));
    registry.register(Arc::new(CargoBuildTool));
    registry.register(Arc::new(CargoTestTool));
    registry.register(Arc::new(CargoClippyTool));
    registry.register(Arc::new(CargoFmtTool));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_test_argv_supports_nested_manifest_package_and_filter() {
        let argv = cargo_test_argv(&json!({
            "manifest_path": "plugins/Cargo.toml",
            "package": "slack",
            "filter": "mentions",
            "args": ["--no-default-features"]
        }));
        assert_eq!(
            argv,
            vec![
                "cargo",
                "test",
                "--manifest-path",
                "plugins/Cargo.toml",
                "-p",
                "slack",
                "--no-default-features",
                "--",
                "mentions"
            ]
        );
    }

    #[test]
    fn cargo_check_and_build_default_to_workspace_after_manifest() {
        assert_eq!(
            cargo_check_argv(&json!({ "manifest_path": "plugins/Cargo.toml" })),
            vec![
                "cargo",
                "check",
                "--manifest-path",
                "plugins/Cargo.toml",
                "--workspace"
            ]
        );
        assert_eq!(
            cargo_build_argv(&json!({ "manifest_path": "plugins/Cargo.toml", "release": true })),
            vec![
                "cargo",
                "build",
                "--manifest-path",
                "plugins/Cargo.toml",
                "--workspace",
                "--release"
            ]
        );
    }

    #[test]
    fn cargo_clippy_and_fmt_keep_trailing_tool_args_after_double_dash() {
        assert_eq!(
            cargo_clippy_argv(&json!({
                "manifest_path": "plugins/Cargo.toml",
                "deny_warnings": true
            })),
            vec![
                "cargo",
                "clippy",
                "--manifest-path",
                "plugins/Cargo.toml",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings"
            ]
        );
        assert_eq!(
            cargo_fmt_argv(&json!({
                "manifest_path": "plugins/Cargo.toml",
                "check": true
            })),
            vec![
                "cargo",
                "fmt",
                "--manifest-path",
                "plugins/Cargo.toml",
                "--all",
                "--",
                "--check"
            ]
        );
    }

    #[test]
    fn cargo_subjects_include_package_and_manifest_scope() {
        assert_eq!(
            cargo_subjects(
                "cargo_test",
                &json!({ "package": "slack", "manifest_path": "plugins/Cargo.toml" })
            ),
            vec!["cargo_test:slack", "manifest:plugins/Cargo.toml"]
        );
    }

    #[test]
    fn cargo_test_schema_declares_positional_order() {
        let schema = CargoTestTool.spec().input_schema;
        assert_eq!(
            schema["x-param-order"],
            json!(["package", "manifest_path", "filter", "args"])
        );
    }
}
