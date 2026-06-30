//! The root/reflexive op-pack: `plan` (ask the model for a plan), `run_plan` (execute an emitted plan
//! in the current session), and `op.register` (install a Flux-Lang composite op for later reuse). These
//! are **thin delegators** over capabilities installed on the [`ToolContext`] per turn (by flux-flow's
//! `EngineLoopHost`); they hold no engine state themselves, so this pack depends on nothing beyond
//! `flux-runtime`. The reflexivity that lets the agent loop be written *in flux-lang* lives entirely
//! behind those traits — these ops only marshal JSON across them, then dispatch like any other op.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_runtime::{
    CompositeRegisterRequest, CompositeRegistrar, LoopHost, Tool, ToolContext, ToolRegistry,
    ToolResult,
};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};

/// Register root/reflexive ops. Kept **out** of [`register_builtins`](crate::register_builtins) on
/// purpose: these ops are only meaningful when a model-in-the-loop host is installed. `plan` and
/// `run_plan` are tagged to the hidden `reflect` group; `op.register` is model-facing.
pub fn register_reflect(registry: &mut ToolRegistry) {
    registry.register(Arc::new(PlanOp));
    registry.register(Arc::new(RunPlanOp));
    registry.register(Arc::new(RegisterCompositeOp));
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

fn composite_registrar(ctx: &ToolContext) -> Result<&dyn CompositeRegistrar> {
    ctx.composite_registrar.as_deref().ok_or_else(|| {
        Error::Other(
            "`op.register` needs a composite-op registrar, but none is installed in this context"
                .into(),
        )
    })
}

fn source_op_name(source: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("op ")?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        (!name.is_empty()).then_some(name)
    })
}

fn register_subject(params: &Value) -> Option<String> {
    let scope = params.get("scope").and_then(|v| v.as_str())?;
    let name = params
        .get("source")
        .and_then(|v| v.as_str())
        .and_then(source_op_name)
        .unwrap_or_else(|| "unknown".to_string());
    match scope {
        "project" => Some(format!(".flux/ops/{name}.flux")),
        "global" => Some(format!("@global_ops/{name}.flux")),
        "session" => Some(format!("session:{name}")),
        "turn" => Some(format!("turn:{name}")),
        _ => Some(format!("op:{name}")),
    }
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

/// `op.register(source, scope, replace?, expose?) -> Registration` — parse, validate, and install one
/// top-level Flux-Lang composite op. The engine owns all state mutation; this tool just delegates
/// through the audited dispatcher.
struct RegisterCompositeOp;

#[async_trait]
impl Tool for RegisterCompositeOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "op.register".into(),
            description: "Register exactly one Flux-Lang composite op from `source` for later reuse. \
                          `scope` chooses the lifetime: turn, session, project, or global. The registered \
                          op can only call existing ops, and every inner call still runs through the same \
                          approval and guarded-IO envelope."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Flux-Lang source containing exactly one top-level `op ...` declaration"
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["turn", "session", "project", "global"],
                        "description": "where the op is reusable"
                    },
                    "replace": {
                        "type": "boolean",
                        "description": "replace an existing op of the same name; defaults to false"
                    },
                    "expose": {
                        "type": "boolean",
                        "description": "override the op declaration's model-facing exposure flag"
                    }
                },
                "required": ["source", "scope"]
            }),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        register_subject(params).into_iter().collect()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(path) = register_subject(params) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path { path },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let request: CompositeRegisterRequest = serde_json::from_value(params)
            .map_err(|e| Error::Other(format!("op.register: invalid registration request: {e}")))?;
        let out = composite_registrar(ctx)?
            .register_composite(request)
            .await?;
        Ok(ToolResult::ok(
            serde_json::to_string(&out).unwrap_or_default(),
        ))
    }
}
