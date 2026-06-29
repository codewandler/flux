//! The reflexive op-pack: `plan` (ask the model for a plan) and `run_plan` (execute an emitted plan in
//! the current session). Both are **thin delegators** over the [`LoopHost`](flux_runtime::LoopHost)
//! capability installed on the [`ToolContext`] per turn (by flux-flow's `EngineLoopHost`); they hold no
//! engine state themselves, so this pack depends on nothing beyond `flux-runtime`. The reflexivity that
//! lets the agent loop be written *in flux-lang* lives entirely behind that one trait — these ops only
//! marshal JSON across it, then dispatch like any other op (no bypass: `plan` re-enters the planner,
//! `run_plan` re-enters the interpreter, both through the same audited envelope).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_runtime::{LoopHost, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};

/// Register the reflexive ops (`plan`, `run_plan`). Kept **out** of
/// [`register_builtins`](crate::register_builtins) on purpose: they are only meaningful when a
/// [`LoopHost`] is installed (a model-in-the-loop run), and advertising them in every ordinary turn
/// would invite the planner to recurse. A host installs the capability and registers this pack together.
pub fn register_reflect(registry: &mut ToolRegistry) {
    registry.register(Arc::new(PlanOp));
    registry.register(Arc::new(RunPlanOp));
}

/// The installed reflexive capability, or a clear error if this context has none (the ops are
/// registered but no model-in-the-loop host is wired — e.g. an ordinary dispatch outside a loop run).
fn loop_host(ctx: &ToolContext) -> Result<&dyn LoopHost> {
    ctx.loop_host.as_deref().ok_or_else(|| {
        Error::Other(
            "`plan`/`run_plan` need a model-in-the-loop host, but none is installed in this context"
                .into(),
        )
    })
}

/// `plan(feedback?) -> Plan` — re-enter the planner (the model) to produce a plan from the working
/// feedback/conversation. Returns a `Plan` object `{kind: "plan"|"chat"|"error", text?, ast?,
/// complete?}` as JSON text. The model stays the planner; this op only wraps the audited compile step.
struct PlanOp;

#[async_trait]
impl Tool for PlanOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "plan".into(),
            description: "Ask the model to emit a plan from the working feedback/conversation. Returns \
                          a Plan object {kind: \"plan\"|\"chat\"|\"error\", text?, ast?, complete?}. The \
                          model stays the planner — this re-enters only the compile step, through the \
                          same audited envelope as any op."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "feedback": {
                        "type": "string",
                        "description": "working feedback / conversation seed for the planner"
                    }
                }
            }),
            output_schema: None,
            // A model call travels over the network and needs the provider; lowered like a cognition op.
            effects: vec![Effect::Network],
            risk: Risk::Low,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Provider],
            // The `reflect` group never surfaces from workspace signals, so these ops stay OUT of the
            // model-facing catalog in ordinary turns — yet a pre-authored flow can still call them
            // (`OpRegistry::get` resolves any registered op; gating only filters advertising).
            group: Some(flux_runtime::REFLECT_GROUP.into()),
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let plan = loop_host(ctx)?.plan(params).await?;
        Ok(ToolResult::ok(
            serde_json::to_string(&plan).unwrap_or_default(),
        ))
    }
}

/// `run_plan(plan) -> Outcome` — execute an emitted plan in the CURRENT session and return its Outcome
/// `{transcript, result, steps, suspension?}` as JSON text. The plan is re-validated and every inner op
/// runs through the same approval+IO envelope; bounded by a host reentry-depth cap.
struct RunPlanOp;

#[async_trait]
impl Tool for RunPlanOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_plan".into(),
            description: "Execute an emitted plan in the current session and return its Outcome \
                          {transcript, result, steps, suspension?}. The plan is re-validated and every \
                          op runs through the same approval+IO envelope; bounded by a reentry-depth cap."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": { "description": "the Plan emitted by `plan` (its `ast` is executed)" }
                },
                "required": ["plan"]
            }),
            output_schema: None,
            // No host effects of its own: the inner ops declare and gate their own effects at their own
            // dispatch. Medium + non-idempotent so a risk pass never mistakes it for an inert read.
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            // Hidden from the model-facing catalog (see `plan`), reachable by pre-authored flows.
            group: Some(flux_runtime::REFLECT_GROUP.into()),
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        // The plan reaches us either named (`{"plan": …}`) or — when a lone object arg is passed
        // straight through as the input — as the whole `params`. And a stored op result is a JSON
        // *string*, so accept a string and parse it. All three shapes collapse to the Plan value.
        let raw = match params.get("plan") {
            Some(v) => v.clone(),
            None => params,
        };
        let plan = match raw {
            Value::String(s) => serde_json::from_str::<Value>(&s)
                .map_err(|e| Error::Other(format!("run_plan: `plan` is not valid JSON: {e}")))?,
            other => other,
        };
        let outcome = loop_host(ctx)?.run_plan(plan).await?;
        Ok(ToolResult::ok(
            serde_json::to_string(&outcome).unwrap_or_default(),
        ))
    }
}
