//! The interpreter. M1 executes a single operation: dispatch it through the safety envelope, store
//! the result as an immutable value, optionally bind a symbol, and record the run-event trace.
//!
//! The interpreter is the *only* caller of [`Executor::dispatch`](flux_runtime::Executor) in
//! flux-flow — every op runs through the same gate as any other tool, so there is no new bypass
//! surface. Symbol-placeholder resolution in op inputs arrives with multi-op flows (M3); for now the
//! input is dispatched as given.

use sha2::{Digest, Sha256};

use flux_core::Result;
use flux_runtime::Executor;

use crate::ast::{RunEvent, StepId, SymbolName, Value, ValueId, Visibility};
use crate::state::FlowStore;

/// How to bind a single op's result to a session symbol.
pub struct BindSpec<'a> {
    pub name: &'a SymbolName,
    pub ty: Option<&'a str>,
    pub visibility: Visibility,
}

/// The outcome of executing a single operation.
#[derive(Debug, Clone)]
pub struct CallOutcome {
    /// The stored value id, or `None` if the op errored (nothing is bound on error).
    pub value_id: Option<ValueId>,
    pub is_error: bool,
    pub content: String,
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// A one-line, length-bounded summary of a value for the symbol table (never the raw bytes).
fn summarize(content: &str) -> String {
    let line = content.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 {
        let head: String = line.chars().take(77).collect();
        format!("{head}...")
    } else {
        line.to_string()
    }
}

/// Execute one registered operation through the envelope, store its result as an immutable value,
/// optionally bind it to a symbol, and append the run-event trace.
pub async fn execute_call(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    op: &str,
    input: serde_json::Value,
    bind: Option<BindSpec<'_>>,
) -> Result<CallOutcome> {
    let input_hash = sha256_hex(&serde_json::to_string(&input).unwrap_or_default());
    let step = StepId(format!("step_{}", &input_hash[..16]));

    store.append_event(
        session_id,
        &RunEvent::StepStarted {
            step: step.clone(),
            op: op.to_string(),
            input_hash,
        },
    )?;

    let result = executor.dispatch(op, input).await;

    if result.is_error {
        store.append_event(
            session_id,
            &RunEvent::StepFailed {
                step,
                error: result.content.clone(),
            },
        )?;
        return Ok(CallOutcome {
            value_id: None,
            is_error: true,
            content: result.content,
        });
    }

    let value_id = store.put_value(session_id, &Value::String(result.content.clone()))?;
    store.append_event(
        session_id,
        &RunEvent::StepSucceeded {
            step,
            output: value_id.clone(),
        },
    )?;
    if let Some(b) = bind {
        store.bind(
            session_id,
            b.name,
            &value_id,
            b.ty,
            &summarize(&result.content),
            b.visibility,
        )?;
    }

    Ok(CallOutcome {
        value_id: Some(value_id),
        is_error: false,
        content: result.content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    /// A tool that echoes its `text` param back as content.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("echo", "echo text", json!({"type": "object"}))
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            params: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    fn temp_executor(allow: bool) -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-{}-{}",
            std::process::id(),
            if allow { "allow" } else { "deny" }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let perms = if allow {
            PermissionManager::from_rules(&["echo".into()], &[])
        } else {
            PermissionManager::from_rules(&[], &["echo".into()])
        };
        Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    #[tokio::test]
    async fn single_op_stores_value_binds_symbol_and_traces() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let draft = SymbolName("draft".into());

        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "echo",
            json!({"text": "renewal follow-up"}),
            Some(BindSpec {
                name: &draft,
                ty: Some("Draft"),
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert!(!outcome.is_error);
        let vid = outcome.value_id.clone().unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("renewal follow-up".into()))
        );
        assert_eq!(store.resolve("sess", &draft).unwrap(), Some(vid));

        // the view projects a summary, not the raw value bytes
        let view = store.view("sess").unwrap();
        assert_eq!(view.symbols.len(), 1);
        assert_eq!(view.symbols[0].name, draft);
        assert_eq!(view.symbols[0].summary, "renewal follow-up");

        let events = store.events("sess").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepStarted { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepSucceeded { .. })));
    }

    #[tokio::test]
    async fn denied_op_is_traced_as_failed_and_not_bound() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(false);
        let draft = SymbolName("draft".into());

        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "echo",
            json!({"text": "x"}),
            Some(BindSpec {
                name: &draft,
                ty: Some("Draft"),
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert!(outcome.is_error, "a denied op yields an error outcome");
        assert!(outcome.value_id.is_none());
        assert_eq!(store.resolve("sess", &draft).unwrap(), None);
        let events = store.events("sess").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepFailed { .. })));
    }
}
