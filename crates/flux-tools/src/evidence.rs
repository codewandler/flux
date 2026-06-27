//! The evidence op-pack: `observe` (append an observation) and `evidence` (read observations back).
//! Both operate on the **shared** [`EvidenceLog`](flux_evidence::EvidenceLog) held on the
//! [`ToolContext`], so an observation a flow emits is the SAME audit trail the dispatcher writes
//! `tool_call` markers into and that [`Executor::evidence`](flux_runtime::Executor::evidence)
//! snapshots — there is no separate, parallel evidence channel. This is what lets a flux-lang loop be
//! *evidence-based*: it can emit and read its own runtime observations and branch on them.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::Result;
use flux_evidence::{Observation, Phase};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{Effect, Idempotency, Risk, ToolSpec};

/// Register the evidence ops (`observe`, `evidence`, `metrics`). Called from
/// [`register_builtins`](crate::register_builtins) — they are general-purpose audit primitives any flow
/// may use, advertised like the other built-ins.
pub fn register_evidence(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ObserveOp));
    registry.register(Arc::new(EvidenceOp));
    registry.register(Arc::new(MetricsOp));
}

/// `observe(kind, data?)` — append an observation to the run's shared evidence log.
struct ObserveOp;

#[async_trait]
impl Tool for ObserveOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "observe".into(),
            description: "Append an observation to the run's evidence log: `kind` is the label (e.g. \
                          \"turn.iteration\"), optional `data` is arbitrary JSON. This is the SAME log \
                          the runtime records tool calls into and that `evidence`/grading read back."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "the observation label" },
                    "data": { "type": "object", "description": "arbitrary JSON payload" }
                },
                "required": ["kind"]
            }),
            output_schema: None,
            // No host IO: an in-memory audit append. Non-idempotent (it accumulates).
            effects: Vec::new(),
            risk: Risk::Low,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let Some(kind) = params.get("kind").and_then(|v| v.as_str()) else {
            return Ok(ToolResult::error(
                "observe: missing required string param `kind`",
            ));
        };
        let data = params.get("data").cloned().unwrap_or_else(|| json!({}));
        ctx.evidence
            .lock()
            .unwrap()
            .record(Observation::new(kind, Phase::Turn, data));
        Ok(ToolResult::ok(format!("observed `{kind}`")))
    }
}

/// `evidence(kind?)` — read observations from the run's shared evidence log as a JSON array. With
/// `kind`, only observations of that label; without it, the whole log. Each item is `{kind, phase, data}`.
struct EvidenceOp;

#[async_trait]
impl Tool for EvidenceOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "evidence".into(),
            description:
                "Read observations from the run's evidence log as a JSON array. With `kind`, \
                          only observations of that label; without it, the whole log. Each item is \
                          {kind, phase, data} — so a flow can branch on what has happened so far."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "optional label filter" }
                }
            }),
            output_schema: None,
            effects: vec![Effect::Read],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let log = ctx.evidence.lock().unwrap();
        let matched: Vec<&Observation> = match params.get("kind").and_then(|v| v.as_str()) {
            Some(kind) => log.by_kind(kind).collect(),
            None => log.all().iter().collect(),
        };
        let arr = serde_json::to_string(&matched).unwrap_or_else(|_| "[]".into());
        Ok(ToolResult::ok(arr))
    }
}

/// `metrics() -> {tool_calls, tool_errors, iterations}` — summary counts from the run's shared evidence
/// log, so a model-in-the-loop can assess progress (e.g. stop when errors stop dropping). `tool_calls`
/// and `tool_errors` are the dispatcher's per-call markers; `iterations` counts the `turn.iteration`
/// observations a loop emits via `observe`.
struct MetricsOp;

#[async_trait]
impl Tool for MetricsOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "metrics".into(),
            description: "Summary counts from the run's evidence log as JSON {tool_calls, tool_errors, \
                          iterations}: tool_calls/tool_errors are dispatch markers; iterations counts \
                          the `turn.iteration` observations a loop emits. Branch on how the run is going."
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: None,
            effects: vec![Effect::Read],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let log = ctx.evidence.lock().unwrap();
        let metrics = json!({
            "tool_calls": log.by_kind("tool_call").count(),
            "tool_errors": log.by_kind("tool_error").count(),
            "iterations": log.by_kind("turn.iteration").count(),
        });
        Ok(ToolResult::ok(metrics.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_runtime::{AllowApprover, Executor, PermissionManager};
    use flux_system::{System, Workspace};

    /// The unification: an observation a flow emits via `observe` is readable back through `evidence`
    /// AND is part of the executor's own audit snapshot — one log, no separate evidence channel.
    #[tokio::test]
    async fn observe_then_evidence_share_one_log() {
        let dir = std::env::temp_dir().join(format!("flux-evidence-op-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        register_evidence(&mut reg);
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["observe".into(), "evidence".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );

        // A flow-emitted observation lands in the shared log.
        let r = ex
            .dispatch("observe", json!({ "kind": "x", "data": { "n": 1 } }))
            .await;
        assert!(!r.is_error, "observe ok: {}", r.content);

        // It reads back through the `evidence` op (filtered by kind).
        let r = ex.dispatch("evidence", json!({ "kind": "x" })).await;
        assert!(!r.is_error);
        let arr: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(
            arr.as_array().unwrap().len(),
            1,
            "evidence(\"x\") returns the one observation: {}",
            r.content
        );

        // And it is visible in the executor's own audit snapshot — the SAME log, not a parallel channel.
        assert_eq!(ex.evidence().by_kind("x").count(), 1);
    }

    /// `metrics` reflects the shared log: dispatch markers (`tool_call`/`tool_error`) and the
    /// `turn.iteration` markers a loop emits. A failed op (here: `observe` with no `kind`) records a
    /// `tool_error` at dispatch.
    #[tokio::test]
    async fn metrics_counts_calls_errors_and_iterations() {
        let dir = std::env::temp_dir().join(format!("flux-metrics-op-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        register_evidence(&mut reg);
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["observe".into(), "metrics".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );

        // Two iteration markers a loop would emit.
        ex.dispatch("observe", json!({ "kind": "turn.iteration", "data": {} }))
            .await;
        ex.dispatch("observe", json!({ "kind": "turn.iteration", "data": {} }))
            .await;
        // A failing call (missing `kind`) → dispatch records a `tool_error`.
        let r = ex.dispatch("observe", json!({})).await;
        assert!(r.is_error, "observe with no kind errors");

        let r = ex.dispatch("metrics", json!({})).await;
        let m: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(
            m["iterations"], 2,
            "two turn.iteration markers: {}",
            r.content
        );
        assert_eq!(m["tool_errors"], 1, "one failed dispatch: {}", r.content);
        assert!(
            m["tool_calls"].as_u64().unwrap() >= 3,
            "every dispatch is a tool_call: {}",
            r.content
        );
    }
}
