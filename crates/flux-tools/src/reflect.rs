//! Typed adaptive-stage operations and `op.register`. These are thin delegators over capabilities
//! installed on the [`ToolContext`] by flux-flow; they hold no engine state themselves.

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

/// Register authored outer-loop ops. Kept **out** of [`register_builtins`](crate::register_builtins) on
/// purpose: these ops are only meaningful when a model-in-the-loop host is installed. Adaptive
/// machinery is tagged to the hidden `reflect` group; `op.register` is model-facing.
pub fn try_register_reflect(registry: &mut ToolRegistry) -> Result<()> {
    registry.try_register_all_from(
        "flux-tools authored-loop reflect pack",
        vec![
            Arc::new(DetectIntentOp) as Arc<dyn Tool>,
            Arc::new(ExploreOp),
            Arc::new(ApproveBatchOp),
            Arc::new(ExecuteBatchOp),
            Arc::new(PresentResultsOp),
            Arc::new(AiSegmentOp),
            Arc::new(RegisterCompositeOp),
        ],
    )
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_reflect`].
pub fn register_reflect(registry: &mut ToolRegistry) {
    try_register_reflect(registry).expect("flux-tools reflect pack registration failed");
}

/// Install the canonical authored-loop control plane after a surface has narrowed/composed its
/// catalog.
///
/// These names are engine-owned. An independently supplied handler with the same name must never
/// survive agent assembly, while an ordinary duplicate registration must still fail closed. The
/// source-labelled replacement is therefore deliberate and auditable rather than last-write-wins.
pub fn install_reflect(registry: &mut ToolRegistry) -> Result<()> {
    const SOURCE: &str = "flux-agent canonical authored-loop control plane";
    let mut assembled = registry.clone();
    for tool in [
        Arc::new(DetectIntentOp) as Arc<dyn Tool>,
        Arc::new(ExploreOp),
        Arc::new(ApproveBatchOp),
        Arc::new(ExecuteBatchOp),
        Arc::new(PresentResultsOp),
        Arc::new(AiSegmentOp),
        Arc::new(RegisterCompositeOp),
    ] {
        assembled.replace_from(SOURCE, tool)?;
    }
    *registry = assembled;
    Ok(())
}

/// Register one config-defined model stage as an ordinary typed operation. The operation itself is
/// a thin delegator; the engine host owns the provider call and gather-only tool ceiling.
pub fn try_register_model_stage(
    registry: &mut ToolRegistry,
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    output_schema: Value,
) -> Result<()> {
    registry.try_register_from(
        "configured model stage",
        Arc::new(ModelStageOp {
            spec: ToolSpec {
                name: name.into(),
                description: description.into(),
                input_schema,
                output_schema: Some(output_schema),
                effects: vec![Effect::Network],
                // `Medium`, not `Low` (C-208). A config-authored stage is a model call the
                // operator pays for; `Risk::Low` would advertise a billable operation as free at
                // the approval prompt and admit it to the pre-approval gather path. Not resolved
                // by adding `Effect::Read` — cost, not mutation, is what sets these apart from a
                // network fetch. See docs/designs/security-assurance.md.
                risk: Risk::Medium,
                idempotency: Idempotency::NonIdempotent,
                access: vec![AccessKind::Provider],
                group: None,
            },
        }),
    )
}

/// Compatibility wrapper for callers that cannot yet propagate registry assembly failures.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_model_stage`].
pub fn register_model_stage(
    registry: &mut ToolRegistry,
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    output_schema: Value,
) {
    try_register_model_stage(registry, name, description, input_schema, output_schema)
        .expect("configured model-stage registration failed");
}

struct ModelStageOp {
    spec: ToolSpec,
}

#[async_trait]
impl Tool for ModelStageOp {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn staging_disposition(&self) -> flux_spec::StagingDisposition {
        flux_spec::StagingDisposition::Capture
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let out = loop_host(ctx)?.model_stage(&self.spec.name, params).await?;
        Ok(ToolResult::ok(out.to_string()))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExploreInput {
    state: Value,
    #[serde(default)]
    decision: Option<Value>,
    #[serde(default)]
    report: Option<Value>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApproveBatchInput {
    batch: Value,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExecuteBatchInput {
    batch: Value,
    receipt: Value,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PresentResultsInput {
    #[serde(default)]
    step: Option<Value>,
    #[serde(default)]
    approval: Option<Value>,
}

fn adaptive_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    output: &str,
    effects: Vec<Effect>,
    access: Vec<AccessKind>,
) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema: Some(serde_json::json!({"$ref": format!("#/$defs/{output}")})),
        effects,
        risk: Risk::Low,
        idempotency: Idempotency::NonIdempotent,
        access,
        group: Some(flux_runtime::REFLECT_GROUP.into()),
    }
}

struct DetectIntentOp;

#[async_trait]
impl Tool for DetectIntentOp {
    fn spec(&self) -> ToolSpec {
        // `Medium` (C-208): the stage behind this op is a provider call
        // (`LoopHost::detect_intent` → `flux_flow`'s `detect_intent_stage`, which records model
        // usage), so it spends money on every invocation. That is the property that keeps it out
        // of the gather path — see docs/designs/security-assurance.md.
        let mut spec = adaptive_spec(
            "detect_intent",
            "Detect the current turn's intent and resolve capability signals into a durable IntentSet artifact. Signals narrow visibility only; they grant no authority.",
            flux_spec::empty_schema(),
            "IntentSet",
            vec![Effect::Network],
            vec![AccessKind::Provider],
        );
        spec.risk = Risk::Medium;
        spec
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        if !params.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(Error::Other(
                "detect_intent: expected an empty argument object".into(),
            ));
        }
        let out = loop_host(ctx)?.detect_intent().await?;
        Ok(ToolResult::ok(out.to_string()))
    }
}

struct ExploreOp;

#[async_trait]
impl Tool for ExploreOp {
    fn spec(&self) -> ToolSpec {
        // `Medium` (C-208), for the same reason as `detect_intent`: `LoopHost::explore` drives a
        // provider-native stage, so each call is billable. It shares the shape of the eight
        // `Network`-at-`Risk::Low` violations even though it predates the story's own table.
        let mut spec = adaptive_spec(
            "explore",
            "Continue evidence gathering and native-schema action proposal from a durable exploration state. May return chat, decision, or ActionBatch.",
            flux_spec::tool_input_schema::<ExploreInput>(),
            "ExploreResult",
            vec![Effect::Network],
            vec![AccessKind::Provider],
        );
        spec.risk = Risk::Medium;
        spec
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _input: ExploreInput = crate::parse_params(params.clone(), "explore")?;
        let out = loop_host(ctx)?.explore(params).await?;
        Ok(ToolResult::ok(out.to_string()))
    }
}

struct ApproveBatchOp;

#[async_trait]
impl Tool for ApproveBatchOp {
    fn spec(&self) -> ToolSpec {
        adaptive_spec(
            "approve_batch",
            "Request aggregate approval for one immutable ActionBatch and return a one-shot receipt bound to its session, caller, and policy context.",
            flux_spec::tool_input_schema::<ApproveBatchInput>(),
            "ApprovalReceipt",
            Vec::new(),
            Vec::new(),
        )
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _input: ApproveBatchInput = crate::parse_params(params.clone(), "approve_batch")?;
        let out = loop_host(ctx)?.approve_batch(params).await?;
        Ok(ToolResult::ok(out.to_string()))
    }
}

struct ExecuteBatchOp;

#[async_trait]
impl Tool for ExecuteBatchOp {
    fn spec(&self) -> ToolSpec {
        let mut spec = adaptive_spec(
            "execute_batch",
            "Consume a matching one-shot approval receipt and dispatch every ActionBatch operation through authorization, approval, and guarded IO.",
            flux_spec::tool_input_schema::<ExecuteBatchInput>(),
            "ExecutionReport",
            Vec::new(),
            Vec::new(),
        );
        spec.risk = Risk::Medium;
        spec
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _input: ExecuteBatchInput = crate::parse_params(params.clone(), "execute_batch")?;
        let out = loop_host(ctx)?.execute_batch(params).await?;
        Ok(ToolResult::ok(out.to_string()))
    }
}

struct PresentResultsOp;

#[async_trait]
impl Tool for PresentResultsOp {
    fn spec(&self) -> ToolSpec {
        adaptive_spec(
            "present_results",
            "Render a terminal adaptive stage artifact into channel-neutral answer text.",
            flux_spec::tool_input_schema::<PresentResultsInput>(),
            "String",
            Vec::new(),
            Vec::new(),
        )
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _input: PresentResultsInput = crate::parse_params(params.clone(), "present_results")?;
        let out = loop_host(ctx)?.present_results(params).await?;
        Ok(ToolResult::ok(out.as_str().unwrap_or_default().to_string()))
    }
}

/// The installed outer-loop capability, or a clear error if this context has none (the ops are
/// registered but no model-in-the-loop host is wired — e.g. an ordinary dispatch outside a loop run).
fn loop_host(ctx: &ToolContext) -> Result<&dyn LoopHost> {
    ctx.loop_host.as_deref().ok_or_else(|| {
        Error::Other(
            "adaptive stages need a model-in-the-loop host, but none is installed in this context"
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

/// Arguments for the `ai_segment` op (D-131).
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AiSegmentInput {
    /// the instruction handed to the delegated model loop (the segment's goal)
    goal: String,
    /// the capability scope for the segment's leaf ops — the model can call nothing outside it
    tools: Vec<String>,
    /// the required cap on delegated model rounds (the segment is always bounded)
    max_rounds: u32,
}

/// Where a registered composite op is reusable.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum RegisterScope {
    Turn,
    Session,
    Project,
    Global,
}

/// Arguments for the `op.register` op.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RegisterCompositeInput {
    /// Flux-Lang source containing exactly one top-level `op ...` declaration
    source: String,
    /// where the op is reusable; use session by default for ad-hoc user-created operations, project/global only when explicitly requested
    scope: RegisterScope,
    /// replace an existing op of the same name; defaults to false
    #[serde(default)]
    replace: Option<bool>,
    /// override the op declaration's model-facing exposure flag
    #[serde(default)]
    expose: Option<bool>,
}

/// `ai_segment(goal, tools, max_rounds) -> {result}` — hand a bounded native-schema stage run to the
/// loop under an exact capability scope, then return control. Proposed effects become action batches
/// and traverse the same approval and execution seams as the default adaptive loop.
struct AiSegmentOp;

#[async_trait]
impl Tool for AiSegmentOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ai_segment".into(),
            description: "Hand a bounded run of model turns to the loop under a capability scope and \
                          an explicit exit condition, then return control to the flow. `tools` is the \
                          scope the delegated model may call within; `max_rounds` caps the run; \
                          Returns {result}. Gather calls and approved actions run through the same \
                          authorization, approval, and guarded-IO envelope."
                .into(),
            input_schema: flux_spec::tool_input_schema::<AiSegmentInput>(),
            output_schema: None,
            // The delegated model calls travel the network; leaf ops declare and gate their own
            // effects at their own dispatch.
            effects: vec![Effect::Network],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Provider],
            // Hidden from the model-facing catalog, reachable by pre-authored flows.
            group: Some(flux_runtime::REFLECT_GROUP.into()),
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        // Validate the shape, then forward the raw object to the host (which reads goal/tools/
        // max_rounds/until directly and runs the bounded, cap-scoped delegation loop).
        let _args: AiSegmentInput = crate::parse_params(params.clone(), "ai_segment")?;
        let out = loop_host(ctx)?.ai_segment(params).await?;
        Ok(ToolResult::ok(
            serde_json::to_string(&out).unwrap_or_default(),
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
            description: "Define a reusable Flux-Lang operation from `source`. Use this when a \
                          requested operation can be composed from existing ops instead of adding a \
                          native Rust tool. `scope` chooses the lifetime: turn, session, project, \
                          or global; use session for ordinary user-requested helpers unless persistence \
                          is requested. The registered op can only call existing ops, and every inner \
                          call still runs through the same approval and guarded-IO envelope."
                .into(),
            input_schema: flux_spec::tool_input_schema::<RegisterCompositeInput>(),
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
