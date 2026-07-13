//! `FnTool` — a closure-backed [`Tool`]: build a tool from a [`ToolSpec`] and an async handler
//! function instead of a bespoke `impl Tool` struct. Lets a consumer (or flux itself) define a
//! *family* of tools programmatically — e.g. one per record type in a catalog — from data, the same
//! way [`flux_spec::object_schema`] builds a schema from a field list instead of a hand-rolled type.
//!
//! The handler's `Err` folds to a **soft** [`ToolResult::error`] — the same "a backend failure is a
//! model-visible error, not a crash" contract every hand-written tool in this workspace already
//! follows (`execute` itself never returns `Err`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::Result;
use flux_spec::{StagingDisposition, ToolSpec};

use crate::{Tool, ToolContext, ToolResult};

/// A handler's future, boxed so [`FnTool`] can hold it behind a plain `Fn` trait object (the
/// closure's concrete `Future` type is otherwise unnameable in a struct field).
type HandlerFuture = Pin<Box<dyn Future<Output = std::result::Result<Value, String>> + Send>>;

/// A permission-subjects function, boxed the same way as [`HandlerFuture`]'s handler.
type SubjectsFn = Arc<dyn Fn(&Value) -> Vec<String> + Send + Sync>;

/// Render a handler's successful `Value` as the [`ToolResult::content`] text: a bare `String` passes
/// through unquoted (matching how most hand-written tools in this workspace return plain text),
/// anything else (object, array, number, bool, null) becomes its compact JSON encoding — the same
/// `serde_json::to_string` convention the structured tools already use.
fn value_to_content(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// A closure-backed [`Tool`]. Its [`spec`](Tool::spec) is a plain captured [`ToolSpec`] — name,
/// description, schema, effects, risk, and group all come from the ordinary `ToolSpec` builders — and
/// its [`execute`](Tool::execute) delegates to a captured async handler. `Ok(value)` becomes the
/// result content ([`value_to_content`]); `Err(message)` folds to a **soft** [`ToolResult::error`] so
/// the dispatcher sees a normal, retryable result, never a hard `execute` failure.
///
/// Permission subjects default to none, matching [`Tool::permission_subjects`]'s own default; attach
/// [`with_permission_subjects`](Self::with_permission_subjects) to derive them from the call's params.
pub struct FnTool {
    spec: ToolSpec,
    handler: Arc<dyn Fn(Value) -> HandlerFuture + Send + Sync>,
    subjects: Option<SubjectsFn>,
    staging: StagingDisposition,
}

impl FnTool {
    /// Build a tool from a spec and an async handler `Fn(Value) -> impl Future<Output =
    /// Result<Value, String>>`. Prefer [`tool_fn`] at call sites — it hands back the `Arc<dyn Tool>`
    /// a [`ToolRegistry`](crate::ToolRegistry) wants directly.
    pub fn new<F, Fut>(spec: ToolSpec, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<Value, String>> + Send + 'static,
    {
        Self {
            spec,
            handler: Arc::new(move |params| Box::pin(handler(params)) as HandlerFuture),
            subjects: None,
            staging: StagingDisposition::Infer,
        }
    }

    /// Derive permission subjects from the call's params (default: none — [`Tool`]'s own default).
    pub fn with_permission_subjects<S>(mut self, subjects: S) -> Self
    where
        S: Fn(&Value) -> Vec<String> + Send + Sync + 'static,
    {
        self.subjects = Some(Arc::new(subjects));
        self
    }

    /// Declare how the adaptive loop may stage this closure-backed operation. The runtime still
    /// applies the conservative effect/risk/idempotency checks, so `Gather` cannot make a mutating
    /// operation execute during exploration.
    pub fn with_staging_disposition(mut self, staging: StagingDisposition) -> Self {
        self.staging = staging;
        self
    }
}

#[async_trait]
impl Tool for FnTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        match &self.subjects {
            Some(f) => f(params),
            None => Vec::new(),
        }
    }

    fn staging_disposition(&self) -> StagingDisposition {
        self.staging
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        match (self.handler)(params).await {
            Ok(value) => Ok(ToolResult::ok(value_to_content(value))),
            Err(message) => Ok(ToolResult::error(message)),
        }
    }
}

/// Build a closure-backed tool ready for [`ToolRegistry::register`](crate::ToolRegistry::register):
/// `tool_fn(spec, |args| async move { ... })`. Equivalent to `Arc::new(FnTool::new(spec, handler))`
/// but returns `Arc<dyn Tool>` directly — the shape every registration call site wants.
pub fn tool_fn<F, Fut>(spec: ToolSpec, handler: F) -> Arc<dyn Tool>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = std::result::Result<Value, String>> + Send + 'static,
{
    Arc::new(FnTool::new(spec, handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowApprover, Executor, PermissionManager, ToolRegistry};
    use flux_system::{System, Workspace};
    use serde_json::json;

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-fn-tool-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    #[tokio::test]
    async fn handler_is_invoked_with_the_call_args() {
        let tool = FnTool::new(
            ToolSpec::read_only("double", "double a number", json!({"type": "object"})),
            |params: Value| async move {
                let n = params["n"].as_i64().unwrap_or(0);
                Ok(json!({ "doubled": n * 2 }))
            },
        );
        let result = tool.execute(&ctx(), json!({"n": 21})).await.unwrap();
        assert!(!result.is_error);
        let out: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(out["doubled"], 42);
    }

    #[tokio::test]
    async fn handler_error_folds_to_a_soft_tool_result_error() {
        let tool = tool_fn(
            ToolSpec::read_only("fails", "always fails", json!({"type": "object"})),
            |_params: Value| async move { Err("backend unavailable".to_string()) },
        );
        let result = tool.execute(&ctx(), json!({})).await.unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "backend unavailable");
    }

    #[tokio::test]
    async fn default_permission_subjects_are_empty_and_override_derives_from_params() {
        let no_subjects = FnTool::new(
            ToolSpec::read_only("bare", "no subjects", json!({"type": "object"})),
            |_p: Value| async move { Ok(Value::Null) },
        );
        assert!(no_subjects
            .permission_subjects(&json!({"path": "a"}))
            .is_empty());

        let with_subjects = FnTool::new(
            ToolSpec::read_only("scoped", "derives subjects", json!({"type": "object"})),
            |_p: Value| async move { Ok(Value::Null) },
        )
        .with_permission_subjects(|params: &Value| {
            params["path"]
                .as_str()
                .map(|s| vec![s.to_string()])
                .unwrap_or_default()
        });
        assert_eq!(
            with_subjects.permission_subjects(&json!({"path": "src/main.rs"})),
            vec!["src/main.rs".to_string()]
        );
    }

    #[tokio::test]
    async fn registers_and_dispatches_through_the_normal_executor_path() {
        let mut registry = ToolRegistry::new();
        registry.register(tool_fn(
            ToolSpec::read_only("greet", "say hi", json!({"type": "object"})),
            |params: Value| async move {
                let name = params["name"].as_str().unwrap_or("world").to_string();
                Ok(Value::String(format!("hello, {name}")))
            },
        ));
        let executor = Executor::new(
            registry,
            PermissionManager::new(),
            Arc::new(AllowApprover),
            ctx(),
        );
        let result = executor.dispatch("greet", json!({"name": "flux"})).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "hello, flux");
    }

    /// A struct-shaped `Value` still gets ordinary JSON content (`value_to_content` only special-cases
    /// a bare `Value::String`).
    #[tokio::test]
    async fn non_string_values_serialize_as_json_content() {
        let tool = tool_fn(
            ToolSpec::read_only("list", "list things", json!({"type": "object"})),
            |_p: Value| async move { Ok(json!(["a", "b"])) },
        );
        let result = tool.execute(&ctx(), json!({})).await.unwrap();
        assert_eq!(result.content, "[\"a\",\"b\"]");
    }
}
