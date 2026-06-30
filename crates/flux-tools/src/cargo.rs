//! Rust toolchain tools: cargo_check, cargo_build, cargo_test, cargo_clippy, cargo_fmt.
//!
//! These are argv-only invocations through the guarded System — no shell strings.
//! Risk is Medium (they mutate build artefacts / the local filesystem) except cargo_fmt
//! which is Idempotent.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::Result;
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{
    tool_input_schema, AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty,
    IntentRole, IntentSet, IntentTarget, Risk, ToolSpec,
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

#[derive(Clone, Copy, Default)]
struct ManagedCargoArgs {
    workspace: bool,
    all: bool,
    all_targets: bool,
    package: bool,
}

#[derive(Default)]
struct ExtraCargoArgs {
    cargo: Vec<String>,
    after_dash: Vec<String>,
    had_dash: bool,
}

fn raw_args(params: &Value) -> Vec<String> {
    params
        .get("args")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

fn normalized_extra_args(params: &Value, managed: ManagedCargoArgs) -> ExtraCargoArgs {
    let mut out = ExtraCargoArgs::default();
    let mut before_dash = true;
    let mut skip_next = false;
    for arg in raw_args(params) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if before_dash && arg == "--" {
            out.had_dash = true;
            before_dash = false;
            continue;
        }
        if before_dash {
            match arg.as_str() {
                "--workspace" if managed.workspace => continue,
                "--all" if managed.all => continue,
                "--all-targets" if managed.all_targets => continue,
                "-p" | "--package" if managed.package => {
                    skip_next = true;
                    continue;
                }
                _ if managed.package && arg.starts_with("--package=") => continue,
                _ => out.cargo.push(arg),
            }
        } else {
            out.after_dash.push(arg);
        }
    }
    out
}

fn remove_flag_pair(args: &mut Vec<String>, flag: &str, value: &str) {
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i] == flag && args[i + 1] == value {
            args.drain(i..=i + 1);
        } else {
            i += 1;
        }
    }
}

fn push_extra_args(argv: &mut Vec<String>, extra: ExtraCargoArgs) {
    argv.extend(extra.cargo);
    if extra.had_dash {
        argv.push("--".to_string());
        argv.extend(extra.after_dash);
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
    push_extra_args(
        &mut argv,
        normalized_extra_args(
            params,
            ManagedCargoArgs {
                workspace: true,
                package: true,
                ..ManagedCargoArgs::default()
            },
        ),
    );
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
    push_extra_args(
        &mut argv,
        normalized_extra_args(
            params,
            ManagedCargoArgs {
                workspace: true,
                package: true,
                ..ManagedCargoArgs::default()
            },
        ),
    );
    argv
}

fn cargo_test_argv(params: &Value) -> Vec<String> {
    let mut argv = vec!["cargo".to_string(), "test".to_string()];
    push_manifest_path(&mut argv, params);
    push_package_or_workspace(&mut argv, params, "--workspace");
    let mut extra = normalized_extra_args(
        params,
        ManagedCargoArgs {
            workspace: true,
            package: true,
            ..ManagedCargoArgs::default()
        },
    );
    argv.extend(extra.cargo);
    if let Some(f) = params.get("filter").and_then(|v| v.as_str()) {
        extra.had_dash = true;
        extra.after_dash.push(f.to_string());
    }
    if extra.had_dash {
        argv.push("--".to_string());
        argv.extend(extra.after_dash);
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
    let deny_warnings = params
        .get("deny_warnings")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut extra = normalized_extra_args(
        params,
        ManagedCargoArgs {
            workspace: true,
            all_targets: true,
            package: true,
            ..ManagedCargoArgs::default()
        },
    );
    if deny_warnings {
        remove_flag_pair(&mut extra.cargo, "-D", "warnings");
        remove_flag_pair(&mut extra.after_dash, "-D", "warnings");
    }
    argv.extend(extra.cargo);
    if extra.had_dash || deny_warnings {
        argv.push("--".to_string());
        argv.extend(extra.after_dash);
    }
    if deny_warnings {
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CargoCheckInput {
    /// Specific package to check (omit for --workspace)
    #[serde(default)]
    package: Option<String>,
    /// Path to Cargo.toml for a nested workspace
    #[serde(default)]
    manifest_path: Option<String>,
    /// Extra cargo flags
    #[serde(default)]
    args: Option<Vec<String>>,
}

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
                          extra cargo flags only; use typed fields for package/workspace scope. Risk: Medium."
                    .into(),
            input_schema: tool_input_schema::<CargoCheckInput>(),
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CargoBuildInput {
    /// Specific package (omit for --workspace)
    #[serde(default)]
    package: Option<String>,
    /// Path to Cargo.toml for a nested workspace
    #[serde(default)]
    manifest_path: Option<String>,
    /// Build in release mode
    #[serde(default)]
    release: Option<bool>,
    /// Extra cargo flags
    #[serde(default)]
    args: Option<Vec<String>>,
}

pub struct CargoBuildTool;

#[async_trait]
impl Tool for CargoBuildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_build".into(),
            description: "Run `cargo build` in the workspace (or a specific package with `package`). \
                          Use `manifest_path` for a nested workspace Cargo.toml. Pass `release: true` \
                          for an optimised build. Optional `args` for extra cargo flags only; use typed \
                          fields for package/workspace scope."
                .into(),
            input_schema: tool_input_schema::<CargoBuildInput>(),
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CargoTestInput {
    /// Specific package (omit for --workspace)
    #[serde(default)]
    package: Option<String>,
    /// Path to Cargo.toml for a nested workspace
    #[serde(default)]
    manifest_path: Option<String>,
    /// Test name filter substring
    #[serde(default)]
    filter: Option<String>,
    /// Extra cargo flags
    #[serde(default)]
    args: Option<Vec<String>>,
}

pub struct CargoTestTool;

#[async_trait]
impl Tool for CargoTestTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_test".into(),
            description: "Run `cargo test` in the workspace (or a specific package with `package`). \
                          Use `manifest_path` for a nested workspace Cargo.toml. \
                          Optional `filter` is passed as the test-name filter, `args` for extra cargo \
                          flags only; use typed fields for package/workspace scope."
                .into(),
            input_schema: tool_input_schema::<CargoTestInput>(),
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CargoClippyInput {
    /// Specific package (omit for --workspace)
    #[serde(default)]
    package: Option<String>,
    /// Path to Cargo.toml for a nested workspace
    #[serde(default)]
    manifest_path: Option<String>,
    /// Fail on any warning (-D warnings)
    #[serde(default)]
    deny_warnings: Option<bool>,
    /// Extra cargo flags
    #[serde(default)]
    args: Option<Vec<String>>,
}

pub struct CargoClippyTool;

#[async_trait]
impl Tool for CargoClippyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cargo_clippy".into(),
            description: "Run `cargo clippy` in the workspace (or a specific package). Use \
                          `manifest_path` for a nested workspace Cargo.toml. Pass \
                          `deny_warnings: true` to add `-- -D warnings` (CI-clean check). \
                          Optional `args` for extra cargo flags only; use typed fields for package, \
                          workspace/all-targets scope, and warning denial."
                .into(),
            input_schema: tool_input_schema::<CargoClippyInput>(),
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CargoFmtInput {
    /// Specific package (omit for --all)
    #[serde(default)]
    package: Option<String>,
    /// Path to Cargo.toml for a nested workspace
    #[serde(default)]
    manifest_path: Option<String>,
    /// Check only, don't write (-- --check)
    #[serde(default)]
    check: Option<bool>,
}

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
            input_schema: tool_input_schema::<CargoFmtInput>(),
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
    use serde_json::json;

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
    fn cargo_wrappers_drop_duplicate_model_supplied_scope_flags() {
        assert_eq!(
            cargo_build_argv(&json!({ "args": ["--workspace"] })),
            vec!["cargo", "build", "--workspace"]
        );
        assert_eq!(
            cargo_test_argv(&json!({ "args": ["--workspace"] })),
            vec!["cargo", "test", "--workspace"]
        );
        assert_eq!(
            cargo_clippy_argv(&json!({
                "deny_warnings": true,
                "args": ["--workspace", "--all-targets", "--", "-D", "warnings"]
            })),
            vec![
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn cargo_package_param_is_authoritative_over_scope_args() {
        assert_eq!(
            cargo_test_argv(&json!({
                "package": "flux-cli",
                "args": ["--workspace", "-p", "other-crate"]
            })),
            vec!["cargo", "test", "-p", "flux-cli"]
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
    fn cargo_test_schema_has_no_positional_order_extension() {
        // `x-param-order` is gone: parameter order is non-load-bearing (calls name args via an
        // object). The derived schema must not carry the deprecated ordering extension.
        let schema = CargoTestTool.spec().input_schema;
        assert!(
            schema.get("x-param-order").is_none(),
            "cargo_test schema should not declare x-param-order: {schema}"
        );
    }
}
