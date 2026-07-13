//! Typed intent routing, provider-native operation schemas, bounded evidence gathering, and inert
//! action capture for Flux-authored adaptive outer loops.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use flux_core::{ContentBlock, Error, Message, Result, Usage};
use flux_evidence::{Observation, Phase as EvidencePhase, ToolGroup, KIND_TURN_INTENT};
use flux_lang::ast::{DraftAst, Node, SymbolName};
use flux_provider::{Effort, Provider, Request, SystemSegment, ToolDef};
use flux_runtime::{effective_group, Executor};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::loop_host::{PlanningGuard, SharedSink};
use crate::model::{stream_blocks, StageOptions};
use crate::registry::OpRegistry;
use crate::runtime::execute_flow_with_composites;
use crate::state::FlowStore;
use crate::AgentSink;

const DECLARE_INTENT: &str = "declare_intent";
const FINALIZE_PLAN: &str = "finalize_plan";
const REQUEST_DECISION: &str = "request_decision";
const SIGNAL_CAPABILITIES: &str = "signal_capabilities";
const RETURN_STAGE_RESULT: &str = "return_stage_result";
const MAX_INTENT_ATTEMPTS: usize = 2;
const MAX_FAMILIES: usize = 4;
const MAX_NATIVE_ROUNDS: usize = 12;
const MAX_NATIVE_TOOLS: usize = 64;
const MAX_NATIVE_SCHEMA_CHARS: usize = 128_000;

const INTENT_SYSTEM: &str = "You are Flux's intent router. Understand the user's request and call \
declare_intent exactly once. Select only the smallest capability families needed. This is routing, \
not execution: do not answer the request, call any operation, or invent a family. An empty family \
list is correct when the request needs no operation. Do not select cognition/model families merely \
to reason, calculate, summarize, cite, or write an answer: you already do those things. Select them \
only when the user explicitly asks for a separate model-backed operation.";

const EXPLORE_SYSTEM: &str = "You are Flux's staged planning agent. The tools below are the only \
operations selected for this request and each carries its real input schema. Use gather tools to \
inspect evidence before answering; cite exact source identifiers from tool calls/results and never \
invent facts or paths. A filesystem path is known only when the user supplied it or a tool result \
listed it. If no exact relevant path is known, first inventory the workspace once with `glob`, set \
`pattern` to `*`, and omit `path`; never guess a likely filename. For a multi-part request, keep a \
checklist of every input fact and governing rule needed for every part; read an authoritative source \
for each before answering. Search hits only locate sources--read the source itself. Minimize provider \
rounds without skipping evidence: emit \
independent tool calls together in one response, and once multiple relevant paths are known, read \
them together with `read_many` instead of issuing sequential `read` calls. If the user supplies an \
as-of time, use it and do not fetch the current time. Flux may capture an action instead of \
executing it; the tool result will say so. \
If no actions are captured, answer the user directly only after the evidence checklist is complete. \
If actions are captured, call finalize_plan by itself with instructions for the post-execution \
answer. Never claim a captured action already happened. Flux carries gathered evidence into \
post-execution completion; do not copy facts into the finalization instructions.";

/// Everything the staged helper needs from the engine host. Values are snapshotted at the call
/// boundary so no mutex is held across a provider or operation await.
#[derive(Clone)]
pub(crate) struct StagedContext {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub executor: Arc<Executor>,
    pub store: Arc<FlowStore>,
    pub session_id: String,
    pub conversation: Vec<Message>,
    pub base_system: Option<String>,
    pub sink: Arc<Mutex<dyn AgentSink>>,
    /// Durable turn scope used to record host-built execution graphs for replay/fork. The model
    /// never supplied these graphs; the host derived them from validated native calls.
    pub audit: Option<(Arc<flux_events::EventStore>, i64)>,
    pub advertised: HashSet<String>,
    pub groups: Vec<ToolGroup>,
    pub opts: StageOptions,
    /// Maximum provider rounds one invocation of a native stage may consume.
    pub max_native_rounds: usize,
    /// Remaining billed-token budget when this stage call began.
    pub remaining_token_budget: Option<u64>,
}

/// Usage stays outside the result: a provider error after usage arrived still costs tokens and must
/// be recorded by the host.
pub(crate) struct StagedRun {
    pub result: Result<Value>,
    pub usages: Vec<Usage>,
}

/// Runtime definition for one config-authored model stage. Its operation contract is registered
/// separately as an ordinary [`ToolSpec`]; this value owns only the cognition policy used by the
/// adaptive host when that guarded operation is dispatched.
#[derive(Debug, Clone)]
pub struct ModelStageDefinition {
    pub prompt: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
}

/// Whether a registered operation is safe to expose as a config model stage's immediate gather
/// tool before call arguments exist. Argument-derived intent is checked again for every actual
/// invocation, so this is a startup floor rather than the complete runtime decision.
pub fn statically_gather_safe(tool: &dyn flux_runtime::Tool) -> bool {
    let spec = tool.spec();
    gather_safe(
        &spec,
        tool.staging_disposition(),
        flux_spec::IntentSet::new(),
    )
}

#[derive(Clone)]
struct Family {
    name: String,
    description: String,
    specs: Vec<ToolSpec>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct IntentDeclaration {
    intent: String,
    families: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProposedCall {
    op: String,
    input: Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GatheredEvidence {
    op: String,
    input: String,
    result: String,
}

/// One literal operation invocation captured by a model-backed stage. The host, never the model,
/// constructs this value after validating the provider-native call against the live tool schema.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Action {
    pub id: String,
    pub op: String,
    pub input: Value,
}

/// An ordered, immutable set of proposed effects shown and approved as one unit.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ActionBatch {
    pub id: String,
    pub session_id: String,
    pub actions: Vec<Action>,
}

/// Opaque proof that the current caller approved one exact [`ActionBatch`]. The token is meaningful
/// only to the process-local receipt book and is consumed on the first execution attempt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ApprovalReceipt {
    pub approved: bool,
    pub token: String,
    pub batch_id: String,
    pub destructive: bool,
    #[serde(default)]
    pub summary: String,
}

/// A question the adaptive loop must surface and suspend on before continuing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DecisionRequest {
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// Result of one action in an approved batch.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ActionResult {
    pub id: String,
    pub op: String,
    pub status: String,
    #[serde(default)]
    pub result: String,
}

/// Structural report returned to the same native exploration ledger after batch execution.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecutionReport {
    pub batch_id: String,
    pub ok: bool,
    pub actions: Vec<ActionResult>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PendingResponse {
    Decision {
        call_id: String,
    },
    Execution {
        call_id: String,
        instructions: String,
    },
}

/// Durable model-stage ledger carried as a normal Flux value. Because it is bound before an
/// `await`, the existing flow store preserves the exact native conversation and gathered evidence
/// across suspension; no model reconstruction is needed on resume.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AdaptiveState {
    version: u8,
    declaration: IntentDeclaration,
    selected: Vec<String>,
    messages: Vec<Message>,
    proposed: Vec<ProposedCall>,
    gathered: Vec<GatheredEvidence>,
    native_step: usize,
    #[serde(default)]
    last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingResponse>,
}

#[derive(Debug, Clone)]
struct ReceiptRecord {
    batch_fingerprint: String,
    session_id: String,
    context: String,
    destructive: bool,
}

/// One-shot in-memory approval capability store. A serialized receipt alone is insufficient: its
/// matching record must still exist here, and every bound field must match at consumption time.
#[derive(Default)]
pub(crate) struct ReceiptBook {
    next: AtomicU64,
    records: Mutex<HashMap<String, ReceiptRecord>>,
}

impl ReceiptBook {
    pub(crate) fn issue(
        &self,
        batch: &ActionBatch,
        session_id: &str,
        context: &str,
        destructive: bool,
    ) -> ApprovalReceipt {
        let nonce = self.next.fetch_add(1, Ordering::SeqCst);
        let fingerprint = batch_fingerprint(batch);
        let token = digest(&format!(
            "approval-receipt\0{nonce}\0{fingerprint}\0{session_id}\0{context}"
        ));
        self.records.lock().unwrap().insert(
            token.clone(),
            ReceiptRecord {
                batch_fingerprint: fingerprint,
                session_id: session_id.to_string(),
                context: context.to_string(),
                destructive,
            },
        );
        ApprovalReceipt {
            approved: true,
            token,
            batch_id: batch.id.clone(),
            destructive,
            summary: String::new(),
        }
    }

    pub(crate) fn consume(
        &self,
        batch: &ActionBatch,
        receipt: &ApprovalReceipt,
        session_id: &str,
        context: &str,
    ) -> Result<bool> {
        if !receipt.approved || receipt.token.is_empty() {
            return Err(Error::Other("action batch has no approved receipt".into()));
        }
        // Consume before comparison: a malformed or stale attempt cannot probe and then reuse the
        // same capability with altered bytes.
        let record = self
            .records
            .lock()
            .unwrap()
            .remove(&receipt.token)
            .ok_or_else(|| {
                Error::Other("approval receipt is missing, stale, or already used".into())
            })?;
        if receipt.batch_id != batch.id
            || record.batch_fingerprint != batch_fingerprint(batch)
            || record.session_id != session_id
            || batch.session_id != session_id
            || record.context != context
            || record.destructive != receipt.destructive
        {
            return Err(Error::Other(
                "approval receipt does not match this batch, session, caller, or policy context"
                    .into(),
            ));
        }
        Ok(record.destructive)
    }

    pub(crate) fn clear(&self) {
        self.records.lock().unwrap().clear();
    }
}

fn batch_fingerprint(batch: &ActionBatch) -> String {
    let canonical =
        canonical_json(serde_json::to_value(batch).expect("ActionBatch always serializes to JSON"));
    digest(&canonical.to_string())
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        other => other,
    }
}

fn digest(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Run only the intent stage and return a durable state artifact for [`explore_stage`].
pub(crate) async fn detect_intent_stage(ctx: StagedContext) -> StagedRun {
    let mut usages = Vec::new();
    let result = detect_intent_inner(&ctx, &mut usages).await;
    StagedRun { result, usages }
}

/// Seed a bounded, capability-scoped `ai_segment` without an extra intent-model call. The authored
/// segment already names its exact tool ceiling, so every discoverable family inside that ceiling is
/// selected deterministically and the normal exploration ledger starts from `goal`.
pub(crate) fn scoped_segment_state(ctx: &StagedContext, goal: &str) -> Result<Value> {
    let families = build_families(
        &ctx.executor.registry().specs(),
        &ctx.groups,
        &ctx.advertised,
    );
    if families.is_empty() {
        return Err(Error::Other(
            "ai_segment has no registered operation inside its capability scope".into(),
        ));
    }
    let declaration = IntentDeclaration {
        intent: goal.to_string(),
        families: families.keys().cloned().collect(),
    };
    let selected = selected_specs(&declaration, &families)?;
    let selected_names = selected
        .iter()
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    let state = AdaptiveState {
        version: 1,
        declaration: declaration.clone(),
        selected: selected_names.clone(),
        messages: vec![Message::user_text(goal)],
        proposed: Vec::new(),
        gathered: Vec::new(),
        native_step: 0,
        last_error: String::new(),
        pending: None,
    };
    adaptive_result(
        "intent",
        &state,
        json!({
            "intent": declaration.intent,
            "families": declaration.families,
            "operations": selected_names,
        }),
    )
}

/// Names the exploration stage claims for its own control tools. A registered operation sharing one
/// would collide with the control tool in the provider request and be misrouted, so reserve them.
fn ensure_control_names_free(ctx: &StagedContext) -> Result<()> {
    for reserved in [FINALIZE_PLAN, REQUEST_DECISION, SIGNAL_CAPABILITIES] {
        if ctx.executor.registry().get(reserved).is_some() {
            return Err(Error::Other(format!(
                "adaptive planning cannot reserve `{reserved}` because an operation already uses that name"
            )));
        }
    }
    Ok(())
}

async fn detect_intent_inner(ctx: &StagedContext, usages: &mut Vec<Usage>) -> Result<Value> {
    ensure_control_names_free(ctx)?;
    let families = build_families(
        &ctx.executor.registry().specs(),
        &ctx.groups,
        &ctx.advertised,
    );
    if families.is_empty() {
        return Err(Error::Other(
            "adaptive planning has no registered capability families".into(),
        ));
    }

    observe(ctx, "loop.phase", json!({"phase": "intent"}));
    let declaration = declare_intent(ctx, &families, usages).await?;
    let selected = selected_specs(&declaration, &families)?;
    let selected_names = selected
        .iter()
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    observe(
        ctx,
        "turn.intent",
        json!({
            "intent": declaration.intent,
            "families": declaration.families,
            "operations": selected_names,
        }),
    );

    let state = AdaptiveState {
        version: 1,
        declaration: declaration.clone(),
        selected: selected_names.clone(),
        messages: ctx.conversation.clone(),
        proposed: Vec::new(),
        gathered: Vec::new(),
        native_step: 0,
        last_error: String::new(),
        pending: None,
    };
    Ok(json!({
        "kind": "intent",
        "intent": declaration.intent,
        "families": declaration.families,
        "operations": selected_names,
        "state": state,
    }))
}

/// Continue the native-schema exploration stage from a durable state artifact. Optional
/// `decision` and `report` values close the pending provider-native call created by a prior
/// decision or action-batch result, preserving a valid provider history.
pub(crate) async fn explore_stage(ctx: StagedContext, input: Value) -> StagedRun {
    let mut usages = Vec::new();
    let result = explore_stage_inner(&ctx, input, &mut usages).await;
    StagedRun { result, usages }
}

/// Run one config-defined model stage. The model may only call the declared gather-safe tools and
/// must finish through a provider-native return tool whose `value` field carries the stage's exact
/// output schema. All gather calls still execute through the shared envelope and cassette.
pub(crate) async fn run_model_stage(
    ctx: StagedContext,
    name: &str,
    definition: ModelStageDefinition,
    input: Value,
) -> StagedRun {
    let mut usages = Vec::new();
    let result = run_model_stage_inner(&ctx, name, &definition, input, &mut usages).await;
    StagedRun { result, usages }
}

async fn run_model_stage_inner(
    ctx: &StagedContext,
    name: &str,
    definition: &ModelStageDefinition,
    input: Value,
    usages: &mut Vec<Usage>,
) -> Result<Value> {
    validate_json_schema(&definition.input_schema, &input, "input")?;

    let mut by_native = BTreeMap::<String, ToolSpec>::new();
    for operation in &definition.tools {
        if !ctx.advertised.contains(operation) {
            return Err(Error::Other(format!(
                "model stage `{name}` tool `{operation}` is outside the live capability ceiling"
            )));
        }
        let tool = ctx.executor.registry().get(operation).ok_or_else(|| {
            Error::Other(format!(
                "model stage `{name}` names unregistered tool `{operation}`"
            ))
        })?;
        let spec = tool.spec();
        if !statically_gather_safe(tool.as_ref()) {
            return Err(Error::Other(format!(
                "model stage `{name}` tool `{operation}` is not statically gather-safe"
            )));
        }
        let native = native_tool_name(operation);
        if native == RETURN_STAGE_RESULT {
            return Err(Error::Other(format!(
                "model stage `{name}` tool `{operation}` collides with the reserved return operation `{RETURN_STAGE_RESULT}`"
            )));
        }
        if let Some(previous) = by_native.insert(native.clone(), spec) {
            return Err(Error::Other(format!(
                "model stage `{name}` native alias collision: `{}` and `{operation}` map to `{native}`",
                previous.name
            )));
        }
    }
    let selected = definition.tools.iter().cloned().collect::<HashSet<_>>();
    let return_tool = ToolDef {
        name: RETURN_STAGE_RESULT.into(),
        description: format!(
            "Return the final typed value for model stage `{name}`. Call this exactly once and do not mix it with gather calls."
        ),
        input_schema: json!({
            "type": "object",
            "properties": { "value": definition.output_schema.clone() },
            "required": ["value"],
            "additionalProperties": false
        }),
    };
    let mut messages = vec![Message::user_text(format!(
        "Stage input (JSON): {}",
        canonical_json(input)
    ))];
    let mut last_error = String::new();

    observe(ctx, "stage.started", json!({ "stage": name }));
    let round_limit = native_round_limit(ctx);
    for round in 1..=round_limit {
        ensure_stage_budget(ctx, usages)?;
        let mut req = base_request(
            ctx,
            messages.clone(),
            definition.max_tokens.max(1).min(ctx.opts.max_tokens.max(1)),
        );
        req.model = definition
            .model
            .clone()
            .unwrap_or_else(|| ctx.model.clone());
        req.effort = definition.effort.or(ctx.opts.effort);
        req.system_segments = vec![SystemSegment {
            text: definition.prompt.clone(),
            cache: true,
        }];
        if let Some(base) = ctx
            .base_system
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            req.system_segments.push(SystemSegment {
                text: base.clone(),
                cache: true,
            });
        }
        req.tools = by_native
            .values()
            .map(tool_def)
            .chain(std::iter::once(return_tool.clone()))
            .collect();

        let (streamed, usage) = {
            let _planning = PlanningGuard::start(ctx.sink.clone());
            let mut sink = SharedSink::new(ctx.sink.clone());
            stream_blocks(ctx.provider.as_ref(), req, Some(&mut sink)).await
        };
        usages.push(usage);
        let (mut blocks, text, _, _) = streamed?;
        if blocks.is_empty() && !text.trim().is_empty() {
            blocks.push(ContentBlock::text(text));
        }
        let assistant = Message::assistant(blocks);
        let calls = tool_uses(&assistant);
        if !assistant.content.is_empty() {
            messages.push(assistant);
        }

        if calls.len() == 1 && calls[0].1 == RETURN_STAGE_RESULT {
            let value =
                calls[0].2.get("value").cloned().ok_or_else(|| {
                    Error::Other(format!("model stage `{name}` returned no `value`"))
                })?;
            validate_json_schema(&definition.output_schema, &value, "output")?;
            observe(
                ctx,
                "stage.completed",
                json!({ "stage": name, "rounds": round }),
            );
            return Ok(value);
        }

        let mut results = Vec::new();
        if calls.is_empty() {
            last_error = format!(
                "model stage `{name}` must call `{RETURN_STAGE_RESULT}` or one of its declared gather tools"
            );
            messages.push(Message::user_text(last_error.clone()));
            continue;
        }
        if calls.iter().any(|(_, tool, _)| tool == RETURN_STAGE_RESULT) {
            last_error =
                format!("model stage `{name}` must call `{RETURN_STAGE_RESULT}` by itself");
            for (id, _, _) in calls {
                results.push(ContentBlock::tool_result_text(id, last_error.clone(), true));
            }
            messages.push(Message::user(results));
            continue;
        }

        for (index, (id, native, call_input)) in calls.into_iter().enumerate() {
            let Some(spec) = by_native.get(&native) else {
                last_error = format!("model stage `{name}` called unavailable tool `{native}`");
                results.push(ContentBlock::tool_result_text(id, last_error.clone(), true));
                continue;
            };
            let operation = spec.name.clone();
            let Some(tool) = ctx.executor.registry().get(&operation) else {
                last_error = format!("model stage `{name}` tool `{operation}` disappeared");
                results.push(ContentBlock::tool_result_text(id, last_error.clone(), true));
                continue;
            };
            if let Err(diagnostics) = validate_call(spec, &call_input, ctx, &selected) {
                last_error = format!("invalid `{operation}` input: {}", diagnostics.join("; "));
                results.push(ContentBlock::tool_result_text(id, last_error.clone(), true));
                continue;
            }
            if !gather_safe(spec, tool.staging_disposition(), tool.intents(&call_input)) {
                last_error = format!(
                    "model stage `{name}` call to `{operation}` is not gather-safe for these arguments"
                );
                results.push(ContentBlock::tool_result_text(id, last_error.clone(), true));
                continue;
            }
            let ast = one_call_ast(
                &operation,
                call_input,
                format!("model_stage_{round}_{index}"),
            );
            record_host_flow(ctx, &ast, &format!("stage:{name}"), round as u32);
            let outcome = {
                let mut sink = SharedSink::new(ctx.sink.clone());
                execute_flow_with_composites(
                    ctx.store.as_ref(),
                    ctx.executor.as_ref(),
                    &ctx.session_id,
                    &ast,
                    &[],
                    &mut sink,
                )
                .await
            };
            match outcome {
                Ok(outcome) => {
                    results.push(ContentBlock::tool_result_text(id, outcome.result, false))
                }
                Err(error) => {
                    last_error = format!("`{operation}` gather call failed: {error}");
                    results.push(ContentBlock::tool_result_text(id, last_error.clone(), true));
                }
            }
        }
        messages.push(Message::user(results));
    }

    Err(Error::Other(format!(
        "model stage `{name}` exhausted {round_limit} rounds: {}",
        if last_error.is_empty() {
            "the model did not return a value"
        } else {
            &last_error
        }
    )))
}

fn validate_json_schema(schema: &Value, value: &Value, label: &str) -> Result<()> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| Error::Other(format!("invalid model-stage {label} schema: {error}")))?;
    let diagnostics = validator
        .iter_errors(value)
        .take(4)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "model-stage {label} failed schema validation: {}",
            diagnostics.join("; ")
        )))
    }
}

async fn explore_stage_inner(
    ctx: &StagedContext,
    input: Value,
    usages: &mut Vec<Usage>,
) -> Result<Value> {
    let raw_state = input.get("state").cloned().unwrap_or(input.clone());
    let raw_state = decode_json_value(raw_state, "explore.state")?;
    let mut state: AdaptiveState = serde_json::from_value(raw_state)
        .map_err(|e| Error::Other(format!("explore: invalid adaptive state: {e}")))?;
    if state.version != 1 {
        return Err(Error::Other(format!(
            "explore: unsupported adaptive state version {}",
            state.version
        )));
    }

    if let Some(decision) = input.get("decision") {
        let decision = value_as_text(decision);
        match state.pending.take() {
            Some(PendingResponse::Decision { call_id }) => {
                state
                    .messages
                    .push(Message::user(vec![ContentBlock::tool_result_text(
                        call_id, decision, false,
                    )]));
            }
            Some(other) => {
                state.pending = Some(other);
                return Err(Error::Other(
                    "explore: a decision was supplied but no decision request is pending".into(),
                ));
            }
            None => {
                return Err(Error::Other(
                    "explore: a decision was supplied but no decision request is pending".into(),
                ));
            }
        }
    }
    if let Some(report) = input.get("report") {
        let report = decode_json_value(report.clone(), "explore.report")?;
        match state.pending.take() {
            Some(PendingResponse::Execution {
                call_id,
                instructions,
            }) => {
                state.messages.push(Message::user(vec![
                    ContentBlock::tool_result_text(
                        call_id,
                        format!(
                            "Execution report (actual guarded results): {}\nFinal response instructions: {instructions}",
                            report
                        ),
                        false,
                    ),
                ]));
            }
            Some(other) => {
                state.pending = Some(other);
                return Err(Error::Other(
                    "explore: an execution report was supplied but no action batch is pending"
                        .into(),
                ));
            }
            None => {
                return Err(Error::Other(
                    "explore: an execution report was supplied but no action batch is pending"
                        .into(),
                ));
            }
        }
    }
    if state.pending.is_some() {
        return Err(Error::Other(
            "explore: the previous decision or action batch must be resolved first".into(),
        ));
    }

    observe(ctx, "loop.phase", json!({"phase": "explore"}));
    adaptive_explore(ctx, state, usages).await
}

async fn adaptive_explore(
    ctx: &StagedContext,
    mut state: AdaptiveState,
    usages: &mut Vec<Usage>,
) -> Result<Value> {
    // The `ai_segment` entry point seeds exploration without going through `detect_intent_inner`, so
    // re-assert the control-tool reservation here to cover every path into the exploration stage.
    ensure_control_names_free(ctx)?;
    let families = build_families(
        &ctx.executor.registry().specs(),
        &ctx.groups,
        &ctx.advertised,
    );

    let round_limit = native_round_limit(ctx);
    for _round in 1..=round_limit {
        ensure_stage_budget(ctx, usages)?;
        let selected = selected_specs_for_state(&state, &families, ctx)?;
        let mut selected_by_native = BTreeMap::<String, ToolSpec>::new();
        for spec in &selected {
            let native = native_tool_name(&spec.name);
            if let Some(existing) = selected_by_native.insert(native.clone(), spec.clone()) {
                return Err(Error::Other(format!(
                    "adaptive native operation alias collision: `{}` and `{}` both map to `{native}`",
                    existing.name, spec.name
                )));
            }
        }
        let selected_names: HashSet<String> =
            selected.iter().map(|spec| spec.name.clone()).collect();
        let mut req = base_request(ctx, state.messages.clone(), ctx.opts.max_tokens.min(8_192));
        req.system_segments = explore_segments(ctx, &state.declaration);
        req.tools = selected
            .iter()
            .map(tool_def)
            .chain([
                finalize_tool(),
                decision_tool(),
                capability_signal_tool(&families),
            ])
            .collect();
        let (result, usage) = {
            let _planning = PlanningGuard::start(ctx.sink.clone());
            let mut sink = SharedSink::new(ctx.sink.clone());
            stream_blocks(ctx.provider.as_ref(), req, Some(&mut sink)).await
        };
        usages.push(usage);
        let (mut blocks, text, _, _) = result?;
        if blocks.is_empty() && !text.trim().is_empty() {
            blocks.push(ContentBlock::text(text));
        }
        let assistant = Message::assistant(blocks);
        let calls = tool_uses(&assistant);
        let prose = assistant.text();
        if !assistant.content.is_empty() {
            state.messages.push(assistant);
        }

        if calls.is_empty() {
            if !prose.trim().is_empty() && state.proposed.is_empty() {
                return adaptive_result("chat", &state, json!({"text": prose}));
            }
            state.last_error = if state.proposed.is_empty() {
                "produce a grounded answer or call one of the selected operations".into()
            } else {
                format!(
                    "{} action(s) are captured but not executed; call `{FINALIZE_PLAN}` by itself",
                    state.proposed.len()
                )
            };
            state.messages.push(Message::user_text(format!(
                "Adaptive exploration is incomplete: {}.",
                state.last_error
            )));
            continue;
        }

        if calls.iter().any(|(_, name, _)| {
            matches!(
                name.as_str(),
                FINALIZE_PLAN | REQUEST_DECISION | SIGNAL_CAPABILITIES
            )
        }) && calls.len() != 1
        {
            state.last_error = format!(
                "`{FINALIZE_PLAN}`, `{REQUEST_DECISION}`, and `{SIGNAL_CAPABILITIES}` must each be called by themselves"
            );
            state.messages.push(Message::user(
                calls
                    .into_iter()
                    .map(|(id, _, _)| {
                        ContentBlock::tool_result_text(id, state.last_error.clone(), true)
                    })
                    .collect(),
            ));
            continue;
        }

        if calls[0].1 == REQUEST_DECISION {
            let (call_id, _, input) = &calls[0];
            let question = parse_decision(input)?;
            state.pending = Some(PendingResponse::Decision {
                call_id: call_id.clone(),
            });
            return adaptive_result("decision", &state, json!({"question": question}));
        }

        if calls[0].1 == SIGNAL_CAPABILITIES {
            let (call_id, _, input) = &calls[0];
            let added = apply_capability_signal(&mut state, input, &families)?;
            state.messages.push(Message::user(vec![
                ContentBlock::tool_result_text(
                    call_id.clone(),
                    format!(
                        "Capability signal accepted. Newly surfaced operations: {}. This changed visibility only; it granted no authority.",
                        if added.is_empty() { "(none)".into() } else { added.join(", ") }
                    ),
                    false,
                ),
            ]));
            continue;
        }

        if calls[0].1 == FINALIZE_PLAN {
            if state.proposed.is_empty() {
                state.last_error = format!(
                    "`{FINALIZE_PLAN}` has no captured actions; answer the user directly instead"
                );
                state
                    .messages
                    .push(Message::user(vec![ContentBlock::tool_result_text(
                        calls[0].0.clone(),
                        state.last_error.clone(),
                        true,
                    )]));
                continue;
            }
            let instructions = parse_finalize_instructions(&calls[0].2)?;
            let actions = std::mem::take(&mut state.proposed)
                .into_iter()
                .enumerate()
                .map(|(index, call)| Action {
                    id: format!("action-{}", index + 1),
                    op: call.op,
                    input: call.input,
                })
                .collect::<Vec<_>>();
            let id = action_batch_id(&ctx.session_id, state.native_step, &actions);
            let batch = ActionBatch {
                id,
                session_id: ctx.session_id.clone(),
                actions,
            };
            validate_action_batch(&batch, ctx, &selected_names)?;
            state.pending = Some(PendingResponse::Execution {
                call_id: calls[0].0.clone(),
                instructions,
            });
            return adaptive_result("batch", &state, json!({"batch": batch}));
        }

        let mut results = Vec::new();
        for (id, name, input) in calls {
            state.native_step += 1;
            let Some(spec) = selected_by_native.get(&name) else {
                state.last_error = format!(
                    "operation `{name}` was not selected by intent; available operations: {}",
                    selected_by_native
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                observe(
                    ctx,
                    "adaptive.call",
                    json!({"operation": name, "disposition": "rejected", "step": state.native_step}),
                );
                results.push(ContentBlock::tool_result_text(
                    id,
                    state.last_error.clone(),
                    true,
                ));
                continue;
            };
            let operation = spec.name.clone();
            if let Some(parse_error) = input
                .get(flux_core::ARGS_PARSE_ERROR_KEY)
                .and_then(Value::as_str)
            {
                state.last_error = format!(
                    "`{operation}` arguments were not valid JSON ({parse_error}); retry using its declared input schema"
                );
                results.push(ContentBlock::tool_result_text(
                    id,
                    state.last_error.clone(),
                    true,
                ));
                continue;
            }
            if let Err(diags) = validate_call(spec, &input, ctx, &selected_names) {
                state.last_error = format!("invalid `{operation}` input: {}", diags.join("; "));
                results.push(ContentBlock::tool_result_text(
                    id,
                    state.last_error.clone(),
                    true,
                ));
                continue;
            }

            let tool = ctx.executor.registry().get(&operation).ok_or_else(|| {
                Error::Other(format!(
                    "selected operation `{operation}` disappeared from the registry"
                ))
            })?;
            if gather_safe(spec, tool.staging_disposition(), tool.intents(&input)) {
                let ast = one_call_ast(
                    &operation,
                    input.clone(),
                    format!("adaptive_gather_{}", state.native_step),
                );
                record_host_flow(ctx, &ast, "gather", state.native_step as u32);
                let mut sink = SharedSink::new(ctx.sink.clone());
                match execute_flow_with_composites(
                    ctx.store.as_ref(),
                    ctx.executor.as_ref(),
                    &ctx.session_id,
                    &ast,
                    &[],
                    &mut sink,
                )
                .await
                {
                    Ok(outcome) => {
                        let redactor = &ctx.executor.context().redactor;
                        state.gathered.push(GatheredEvidence {
                            op: operation.clone(),
                            input: redactor.redact(&input.to_string()),
                            result: redactor.redact(&outcome.result),
                        });
                        observe(
                            ctx,
                            "adaptive.call",
                            json!({"operation": operation, "disposition": "gather", "step": state.native_step}),
                        );
                        results.push(ContentBlock::tool_result_text(id, outcome.result, false));
                    }
                    Err(error) => {
                        state.last_error = format!("`{operation}` gather call failed: {error}");
                        results.push(ContentBlock::tool_result_text(
                            id,
                            state.last_error.clone(),
                            true,
                        ));
                    }
                }
            } else {
                state.proposed.push(ProposedCall {
                    op: operation.clone(),
                    input,
                });
                observe(
                    ctx,
                    "adaptive.call",
                    json!({"operation": operation, "disposition": "captured", "step": state.native_step}),
                );
                results.push(ContentBlock::tool_result_text(
                    id,
                    format!(
                        "captured as proposed action {}; not executed. Continue gathering or call `{FINALIZE_PLAN}` by itself when the batch is complete.",
                        state.proposed.len()
                    ),
                    false,
                ));
            }
        }
        state.messages.push(Message::user(results));
    }

    Err(Error::Other(format!(
        "adaptive exploration exhausted {round_limit} native rounds ({} captured action(s)); last issue: {}",
        state.proposed.len(),
        if state.last_error.is_empty() { "the model did not finish" } else { &state.last_error }
    )))
}

fn build_families(
    specs: &[ToolSpec],
    groups: &[ToolGroup],
    advertised: &HashSet<String>,
) -> BTreeMap<String, Family> {
    let mut out: BTreeMap<String, Family> = BTreeMap::new();
    for spec in specs {
        if spec.name == FINALIZE_PLAN
            || effective_group(spec, groups) == Some(flux_runtime::REFLECT_GROUP)
        {
            continue;
        }

        let physical = effective_group(spec, groups);
        let (name, description, discoverable) = match physical {
            Some(name) => {
                let manifest = groups.iter().find(|g| g.name == name);
                let semantic = manifest
                    .is_some_and(|g| g.surface_when.iter().any(|m| m.kind == KIND_TURN_INTENT));
                let active = advertised.contains(&spec.name);
                let description = manifest
                    .map(|g| g.description.clone())
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| format!("Registered `{name}` operations."));
                (name.to_string(), description, semantic || active)
            }
            None => {
                let name = virtual_family(spec);
                (
                    name.to_string(),
                    virtual_description(name).to_string(),
                    advertised.contains(&spec.name),
                )
            }
        };
        if !discoverable {
            continue;
        }
        let family = out.entry(name.clone()).or_insert_with(|| Family {
            name,
            description,
            specs: Vec::new(),
        });
        family.specs.push(spec.clone());
    }
    for family in out.values_mut() {
        family.specs.sort_by(|a, b| a.name.cmp(&b.name));
    }
    out
}

fn virtual_family(spec: &ToolSpec) -> &'static str {
    if spec.effects.contains(&Effect::Write) {
        "workspace.write"
    } else if spec.access.contains(&AccessKind::Provider) {
        "model"
    } else if spec.effects.contains(&Effect::Process)
        || spec.effects.contains(&Effect::LocalSystem)
        || spec.access.contains(&AccessKind::Process)
        || spec.access.contains(&AccessKind::LocalSystem)
    {
        "process"
    } else if spec.effects.contains(&Effect::Network) && spec.effects.contains(&Effect::Read) {
        "network.read"
    } else if spec.effects.contains(&Effect::Filesystem)
        || spec.access.contains(&AccessKind::Filesystem)
    {
        "workspace.read"
    } else {
        "core"
    }
}

fn virtual_description(name: &str) -> &'static str {
    match name {
        "workspace.read" => "Read and search files in the allowed workspace.",
        "workspace.write" => "Create or modify files in the allowed workspace.",
        "network.read" => "Read external or datasource state over the network.",
        "model" => "Run model-backed cognition operations.",
        "process" => "Run fixed process or local-system operations when explicitly enabled.",
        _ => "Pure and generally useful deterministic operations.",
    }
}

fn family_index(families: &BTreeMap<String, Family>) -> String {
    let mut out = String::from("Registered capability families (only these names are valid):\n");
    for family in families.values() {
        let sample = family
            .specs
            .iter()
            .take(8)
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "- {} ({} operation{}; e.g. {}): {}\n",
            family.name,
            family.specs.len(),
            if family.specs.len() == 1 { "" } else { "s" },
            sample,
            family.description
        ));
    }
    out
}

fn intent_tool(families: &BTreeMap<String, Family>) -> ToolDef {
    let names: Vec<Value> = families.keys().cloned().map(Value::String).collect();
    ToolDef {
        name: DECLARE_INTENT.into(),
        description: "Declare what the user is asking for and the smallest registered capability families needed. This signal narrows visibility; it grants no authority.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "intent": {
                    "type": "string",
                    "description": "A concise description of the request in capability-neutral language."
                },
                "capability_families": {
                    "type": "array",
                    "items": {"type": "string", "enum": names},
                    "maxItems": MAX_FAMILIES,
                    "description": "The smallest set of registered families needed; empty for pure conversation."
                }
            },
            "required": ["intent", "capability_families"],
            "additionalProperties": false
        }),
    }
}

async fn declare_intent(
    ctx: &StagedContext,
    families: &BTreeMap<String, Family>,
    usages: &mut Vec<Usage>,
) -> Result<IntentDeclaration> {
    let mut messages = ctx.conversation.clone();
    let valid = families.keys().cloned().collect::<Vec<_>>().join(", ");
    let mut last_error = String::new();

    for _ in 0..MAX_INTENT_ATTEMPTS {
        ensure_stage_budget(ctx, usages)?;
        let mut req = base_request(ctx, messages.clone(), 1_024);
        req.system_segments = intent_segments(ctx, family_index(families));
        req.tools = vec![intent_tool(families)];
        let (result, usage) = {
            let _planning = PlanningGuard::start(ctx.sink.clone());
            let mut sink = SharedSink::new(ctx.sink.clone());
            stream_blocks(ctx.provider.as_ref(), req, Some(&mut sink)).await
        };
        usages.push(usage);
        let (mut blocks, text, _, _) = result?;
        if blocks.is_empty() && !text.trim().is_empty() {
            blocks.push(ContentBlock::text(text));
        }
        let assistant = Message::assistant(blocks);
        let calls = tool_uses(&assistant);
        if !assistant.content.is_empty() {
            messages.push(assistant);
        }

        let parsed = if calls.len() == 1 && calls[0].1 == DECLARE_INTENT {
            parse_intent(&calls[0].2, families)
        } else if calls.is_empty() {
            Err("you must call `declare_intent`; prose is not an intent signal".into())
        } else {
            Err(format!(
                "call `declare_intent` exactly once; received {} tool call(s): {}",
                calls.len(),
                calls
                    .iter()
                    .map(|(_, name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        };

        match parsed {
            Ok(intent) => return Ok(intent),
            Err(error) => {
                last_error = error;
                let repair = format!(
                    "Invalid intent declaration: {last_error}. Valid capability families: {valid}. Call `{DECLARE_INTENT}` exactly once with a non-empty `intent` and at most {MAX_FAMILIES} valid family names."
                );
                if let Some((id, _, _)) = calls.first() {
                    messages.push(Message::user(vec![ContentBlock::tool_result_text(
                        id.clone(),
                        repair,
                        true,
                    )]));
                } else {
                    messages.push(Message::user_text(repair));
                }
            }
        }
    }
    Err(Error::Other(format!(
        "intent routing failed after {MAX_INTENT_ATTEMPTS} attempts: {last_error}"
    )))
}

fn parse_intent(
    input: &Value,
    families: &BTreeMap<String, Family>,
) -> std::result::Result<IntentDeclaration, String> {
    if let Some(error) = input
        .get(flux_core::ARGS_PARSE_ERROR_KEY)
        .and_then(Value::as_str)
    {
        return Err(format!("arguments were not valid JSON: {error}"));
    }
    let obj = input
        .as_object()
        .ok_or_else(|| "input must be an object".to_string())?;
    let unknown = obj
        .keys()
        .filter(|k| k.as_str() != "intent" && k.as_str() != "capability_families")
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(format!("unknown field(s): {}", unknown.join(", ")));
    }
    let intent = obj
        .get("intent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "`intent` must be a non-empty string".to_string())?
        .to_string();
    let raw = obj
        .get("capability_families")
        .and_then(Value::as_array)
        .ok_or_else(|| "`capability_families` must be an array".to_string())?;
    if raw.len() > MAX_FAMILIES {
        return Err(format!(
            "selected {} families; the maximum is {MAX_FAMILIES}",
            raw.len()
        ));
    }
    let mut selected = Vec::new();
    for value in raw {
        let name = value
            .as_str()
            .ok_or_else(|| "every capability family must be a string".to_string())?;
        if !families.contains_key(name) {
            return Err(format!("unknown or unavailable capability family `{name}`"));
        }
        if !selected.iter().any(|seen| seen == name) {
            selected.push(name.to_string());
        }
    }
    Ok(IntentDeclaration {
        intent,
        families: selected,
    })
}

fn selected_specs(
    declaration: &IntentDeclaration,
    families: &BTreeMap<String, Family>,
) -> Result<Vec<ToolSpec>> {
    let mut selected: BTreeMap<String, ToolSpec> = BTreeMap::new();
    for name in &declaration.families {
        let family = families
            .get(name)
            .ok_or_else(|| Error::Other(format!("intent selected unavailable family `{name}`")))?;
        for spec in &family.specs {
            selected.insert(spec.name.clone(), spec.clone());
        }
    }
    if selected.len() > MAX_NATIVE_TOOLS {
        return Err(Error::Other(format!(
            "selected capability families expand to {} operations; the staged schema budget is {MAX_NATIVE_TOOLS}. Choose narrower families.",
            selected.len()
        )));
    }
    let schema_chars: usize = selected
        .values()
        .map(|spec| spec.input_schema.to_string().chars().count())
        .sum();
    if schema_chars > MAX_NATIVE_SCHEMA_CHARS {
        return Err(Error::Other(format!(
            "selected operation schemas total {schema_chars} characters; the staged schema budget is {MAX_NATIVE_SCHEMA_CHARS}. Choose narrower families."
        )));
    }
    Ok(selected.into_values().collect())
}

fn selected_specs_for_state(
    state: &AdaptiveState,
    families: &BTreeMap<String, Family>,
    ctx: &StagedContext,
) -> Result<Vec<ToolSpec>> {
    // Re-expand the accumulated family signals from the LIVE registry on every stage call. This
    // keeps wiring/policy/tool-subset changes fail-closed instead of trusting serialized operation
    // names from an earlier turn.
    let selected = selected_specs(&state.declaration, families)?;
    let selected = selected
        .into_iter()
        .filter(|spec| ctx.advertised.contains(&spec.name))
        .collect::<Vec<_>>();
    let names = selected
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<HashSet<_>>();
    if state
        .selected
        .iter()
        .any(|name| !names.contains(name.as_str()))
    {
        return Err(Error::Other(
            "adaptive capability state contains an operation that is no longer registered, wired, and surfaced"
                .into(),
        ));
    }
    Ok(selected)
}

fn adaptive_result(kind: &str, state: &AdaptiveState, extra: Value) -> Result<Value> {
    let mut out = extra.as_object().cloned().unwrap_or_default();
    out.insert("kind".into(), Value::String(kind.into()));
    out.insert(
        "state".into(),
        serde_json::to_value(state)
            .map_err(|e| Error::Other(format!("adaptive state did not serialize: {e}")))?,
    );
    Ok(Value::Object(out))
}

fn decode_json_value(value: Value, label: &str) -> Result<Value> {
    match value {
        Value::String(text) => serde_json::from_str(&text)
            .map_err(|e| Error::Other(format!("{label}: expected JSON value: {e}"))),
        other => Ok(other),
    }
}

fn value_as_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn decision_tool() -> ToolDef {
    ToolDef {
        name: REQUEST_DECISION.into(),
        description: "Pause and ask the user for a decision when newly discovered facts leave two or more materially different valid paths. Call by itself; do not use for facts an available gather operation can resolve.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "One concise question explaining the decision that is needed."
                },
                "options": {
                    "type": "array",
                    "items": {"type": "string"},
                    "maxItems": 5,
                    "description": "Concrete choices when known; may be empty for free-form input."
                }
            },
            "required": ["prompt", "options"],
            "additionalProperties": false
        }),
    }
}

fn parse_decision(input: &Value) -> Result<DecisionRequest> {
    if let Some(error) = input
        .get(flux_core::ARGS_PARSE_ERROR_KEY)
        .and_then(Value::as_str)
    {
        return Err(Error::Other(format!(
            "`{REQUEST_DECISION}` arguments were not valid JSON: {error}"
        )));
    }
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            Error::Other(format!(
                "`{REQUEST_DECISION}` requires a non-empty `prompt`"
            ))
        })?
        .to_string();
    let options = input
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Other(format!("`{REQUEST_DECISION}` requires `options` array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    Error::Other(format!(
                        "`{REQUEST_DECISION}` options must be non-empty strings"
                    ))
                })
        })
        .collect::<Result<Vec<_>>>()?;
    if options.len() > 5 {
        return Err(Error::Other(format!(
            "`{REQUEST_DECISION}` accepts at most 5 options"
        )));
    }
    Ok(DecisionRequest { prompt, options })
}

fn capability_signal_tool(families: &BTreeMap<String, Family>) -> ToolDef {
    let names = families
        .keys()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    ToolDef {
        name: SIGNAL_CAPABILITIES.into(),
        description: "Signal additional registered capability families justified by facts discovered during exploration. This only changes tool visibility; it neither calls an operation nor grants authority. Call by itself.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "capability_families": {
                    "type": "array",
                    "items": {"type": "string", "enum": names},
                    "maxItems": MAX_FAMILIES
                },
                "reason": {"type": "string"}
            },
            "required": ["capability_families", "reason"],
            "additionalProperties": false
        }),
    }
}

fn apply_capability_signal(
    state: &mut AdaptiveState,
    input: &Value,
    families: &BTreeMap<String, Family>,
) -> Result<Vec<String>> {
    let reason = input
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .ok_or_else(|| {
            Error::Other(format!(
                "`{SIGNAL_CAPABILITIES}` requires a non-empty `reason`"
            ))
        })?;
    let raw = input
        .get("capability_families")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::Other(format!(
                "`{SIGNAL_CAPABILITIES}` requires `capability_families` array"
            ))
        })?;
    if raw.len() > MAX_FAMILIES {
        return Err(Error::Other(format!(
            "`{SIGNAL_CAPABILITIES}` accepts at most {MAX_FAMILIES} families"
        )));
    }
    let mut new_families = state.declaration.families.clone();
    for value in raw {
        let family = value.as_str().ok_or_else(|| {
            Error::Other(format!(
                "`{SIGNAL_CAPABILITIES}` family names must be strings"
            ))
        })?;
        if !families.contains_key(family) {
            return Err(Error::Other(format!(
                "`{SIGNAL_CAPABILITIES}` named unavailable family `{family}`"
            )));
        }
        if !new_families.iter().any(|seen| seen == family) {
            new_families.push(family.to_string());
        }
    }
    let declaration = IntentDeclaration {
        intent: format!("{}; additional signal: {reason}", state.declaration.intent),
        families: new_families,
    };
    let selected = selected_specs(&declaration, families)?;
    let prior = state.selected.iter().cloned().collect::<HashSet<_>>();
    let added = selected
        .iter()
        .map(|spec| spec.name.clone())
        .filter(|name| !prior.contains(name))
        .collect::<Vec<_>>();
    state.declaration = declaration;
    state.selected = selected.into_iter().map(|spec| spec.name).collect();
    Ok(added)
}

fn parse_finalize_instructions(input: &Value) -> Result<String> {
    if let Some(error) = input
        .get(flux_core::ARGS_PARSE_ERROR_KEY)
        .and_then(Value::as_str)
    {
        return Err(Error::Other(format!(
            "`{FINALIZE_PLAN}` arguments were not valid JSON: {error}"
        )));
    }
    input
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::Other(format!(
                "`{FINALIZE_PLAN}` requires non-empty `instructions`"
            ))
        })
}

fn action_batch_id(session_id: &str, native_step: usize, actions: &[Action]) -> String {
    let actions = canonical_json(
        serde_json::to_value(actions).expect("captured actions always serialize to JSON"),
    );
    format!(
        "batch-{}",
        &digest(&format!("{session_id}\0{native_step}\0{actions}"))[..20]
    )
}

fn validate_action_batch(
    batch: &ActionBatch,
    ctx: &StagedContext,
    selected: &HashSet<String>,
) -> Result<()> {
    if batch.actions.is_empty() {
        return Err(Error::Other("an action batch cannot be empty".into()));
    }
    for action in &batch.actions {
        if !selected.contains(&action.op) || !ctx.advertised.contains(&action.op) {
            return Err(Error::Other(format!(
                "action `{}` is outside the intent-selected capability ceiling",
                action.op
            )));
        }
        let spec = ctx
            .executor
            .registry()
            .get(&action.op)
            .ok_or_else(|| Error::Other(format!("unknown action operation `{}`", action.op)))?
            .spec();
        validate_call(&spec, &action.input, ctx, selected).map_err(|diagnostics| {
            Error::Other(format!(
                "invalid action `{}`: {}",
                action.op,
                diagnostics.join("; ")
            ))
        })?;
    }
    Ok(())
}

fn base_request(ctx: &StagedContext, messages: Vec<Message>, max_tokens: u32) -> Request {
    let mut req = Request::new(ctx.model.clone(), "");
    req.messages = messages;
    req.max_tokens = max_tokens.max(1);
    req.thinking = ctx.opts.thinking;
    req.effort = ctx.opts.effort;
    req
}

fn ensure_stage_budget(ctx: &StagedContext, usages: &[Usage]) -> Result<()> {
    let Some(remaining) = ctx.remaining_token_budget else {
        return Ok(());
    };
    let used = usages.iter().map(Usage::total).sum::<u64>();
    if used < remaining {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "turn token budget exhausted before the next model round ({used} stage tokens used; {remaining} remained)"
        )))
    }
}

fn native_round_limit(ctx: &StagedContext) -> usize {
    ctx.max_native_rounds.clamp(1, MAX_NATIVE_ROUNDS)
}

fn intent_segments(ctx: &StagedContext, index: String) -> Vec<SystemSegment> {
    let mut segments = vec![
        SystemSegment {
            text: INTENT_SYSTEM.into(),
            cache: true,
        },
        SystemSegment {
            text: index,
            cache: true,
        },
    ];
    if let Some(base) = ctx.base_system.as_ref().filter(|s| !s.trim().is_empty()) {
        segments.push(SystemSegment {
            text: base.clone(),
            cache: true,
        });
    }
    segments
}

fn explore_segments(ctx: &StagedContext, declaration: &IntentDeclaration) -> Vec<SystemSegment> {
    let mut segments = vec![SystemSegment {
        text: EXPLORE_SYSTEM.into(),
        cache: true,
    }];
    if let Some(base) = ctx.base_system.as_ref().filter(|s| !s.trim().is_empty()) {
        segments.push(SystemSegment {
            text: base.clone(),
            cache: true,
        });
    }
    segments.push(SystemSegment {
        text: format!(
            "Accepted intent: {}\nSelected capability families: {}",
            declaration.intent,
            if declaration.families.is_empty() {
                "(none)".into()
            } else {
                declaration.families.join(", ")
            }
        ),
        cache: false,
    });
    segments
}

fn tool_def(spec: &ToolSpec) -> ToolDef {
    let name = native_tool_name(&spec.name);
    ToolDef {
        description: if name == spec.name {
            spec.description.clone()
        } else {
            format!("Flux operation `{}`. {}", spec.name, spec.description)
        },
        name,
        input_schema: spec.input_schema.clone(),
    }
}

/// Provider-native tool names have a stricter common denominator than Flux operation names:
/// OpenAI/Codex reject dots and cap names at 64 bytes, while plugin operations conventionally use
/// dotted namespaces and can be much longer. Preserve already-portable names; otherwise keep a
/// readable prefix and append a stable digest so the host can map calls back without collisions or
/// provider-specific branching.
fn native_tool_name(operation: &str) -> String {
    let portable = operation
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if portable && operation.len() <= 64 {
        return operation.to_string();
    }

    let mut prefix = operation
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
                byte as char
            } else {
                '_'
            }
        })
        .take(46)
        .collect::<String>();
    if prefix.is_empty() {
        prefix.push_str("op");
    }
    let digest = Sha256::digest(operation.as_bytes());
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}__{suffix}")
}

fn finalize_tool() -> ToolDef {
    ToolDef {
        name: FINALIZE_PLAN.into(),
        description: "Freeze the captured action calls into the immutable action batch shown for approval. Call by itself only after all gathering and action proposal calls are complete.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "instructions": {
                    "type": "string",
                    "description": "How to summarize the actual results after the approved plan executes."
                },
                "primer": {
                    "type": "string",
                    "description": "Optional short context already known before execution."
                }
            },
            "required": ["instructions"],
            "additionalProperties": false
        }),
    }
}

fn gather_safe(
    spec: &ToolSpec,
    disposition: flux_spec::StagingDisposition,
    intents: flux_spec::IntentSet,
) -> bool {
    if disposition == flux_spec::StagingDisposition::Capture {
        return false;
    }
    if spec.risk != Risk::Low
        || spec.idempotency != Idempotency::Idempotent
        || intents.is_mutating()
        || intents.is_destructive()
    {
        return false;
    }
    if spec.effects.is_empty() {
        // No declared effects does not mean inert: an operation that reaches a code-running or
        // local-system host capability can still act during exploration. Trust an empty effect set
        // as gather-safe only when the access set is equally inert (a pure op declares neither).
        return !spec
            .access
            .iter()
            .any(|access| matches!(access, AccessKind::Process | AccessKind::LocalSystem));
    }
    let allowed = spec
        .effects
        .iter()
        .all(|effect| matches!(effect, Effect::Read | Effect::Filesystem | Effect::Network));
    let network_is_read =
        !spec.effects.contains(&Effect::Network) || spec.effects.contains(&Effect::Read);
    allowed && network_is_read
}

fn one_call_ast(op: &str, input: Value, bind: String) -> DraftAst {
    DraftAst {
        body: vec![Node::Bind {
            name: SymbolName(bind),
            value: Box::new(Node::Call {
                op: op.to_string(),
                args: vec![Node::Lit { value: input }],
            }),
            ty: None,
            effect: None,
        }],
        ..DraftAst::default()
    }
}

/// Persist one host-derived execution graph using the existing accepted-plan source contract.
/// This is replay metadata, not model output: native calls remain the only model protocol and the
/// host owns every byte of this Flux-Lang program.
pub(crate) fn record_host_flow(ctx: &StagedContext, ast: &DraftAst, phase: &str, step: u32) {
    let Some((events, turn_id)) = &ctx.audit else {
        return;
    };
    record_host_flow_with(
        events,
        *turn_id,
        &ctx.session_id,
        ctx.executor.as_ref(),
        ast,
        phase,
        step,
    );
}

pub(crate) fn record_host_flow_with(
    events: &flux_events::EventStore,
    turn_id: i64,
    session_id: &str,
    executor: &Executor,
    ast: &DraftAst,
    phase: &str,
    step: u32,
) {
    let source = flux_lang::format::format(ast);
    let _ = events.record_plan_attempt(
        session_id,
        turn_id,
        flux_events::PlanAttempt {
            step,
            outcome: "accepted".into(),
            error: None,
            fingerprint: Some(flux_lang::runtime::sha256_hex(
                &serde_json::to_string(ast).unwrap_or_default(),
            )),
            plan_text: None,
            phase: Some(phase.into()),
            plan_source: Some(executor.context().redactor.redact(&source)),
            delta_source: None,
        },
    );
}

pub(crate) fn action_batch_ast(batch: &ActionBatch) -> DraftAst {
    DraftAst {
        body: batch
            .actions
            .iter()
            .map(|action| Node::Bind {
                name: SymbolName(format!("adaptive_{}", action.id.replace('-', "_"))),
                value: Box::new(Node::Call {
                    op: action.op.clone(),
                    args: vec![Node::Lit {
                        value: action.input.clone(),
                    }],
                }),
                ty: None,
                effect: None,
            })
            .collect(),
        ..DraftAst::default()
    }
}

pub(crate) fn action_ast(batch: &ActionBatch, action: &Action) -> DraftAst {
    let bind = format!("adaptive_{}_{}", batch.id, action.id)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    one_call_ast(&action.op, action.input.clone(), bind)
}

fn validate_call(
    spec: &ToolSpec,
    input: &Value,
    ctx: &StagedContext,
    selected: &HashSet<String>,
) -> std::result::Result<(), Vec<String>> {
    let validator = jsonschema::validator_for(&spec.input_schema).map_err(|error| {
        vec![format!(
            "registered schema for `{}` is invalid: {error}",
            spec.name
        )]
    })?;
    let schema_errors = validator
        .iter_errors(input)
        .take(4)
        .map(|error| {
            let location = error.instance_path().to_string();
            if location.is_empty() {
                error.to_string()
            } else {
                format!("at {location}: {error}")
            }
        })
        .collect::<Vec<_>>();
    if !schema_errors.is_empty() {
        return Err(schema_errors);
    }

    let ast = one_call_ast(&spec.name, input.clone(), "staged_validate".into());
    validate_plan_inner(&ast, ctx, selected).map_err(|diags| {
        diags
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    })
}

fn validate_plan_inner(
    ast: &DraftAst,
    ctx: &StagedContext,
    selected: &HashSet<String>,
) -> std::result::Result<(), Vec<flux_lang::analyze::Diagnostic>> {
    let registry = OpRegistry::new(ctx.executor.registry()).with_advertised(selected.clone());
    flux_lang::analyze::lower(ast, &registry, &HashSet::new()).map(|_| ())
}

fn tool_uses(message: &Message) -> Vec<(String, String, Value)> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

fn observe(ctx: &StagedContext, kind: &str, data: Value) {
    let observation = Observation::new(kind, EvidencePhase::Turn, data);
    ctx.executor.observe(observation.clone());
    let mut sink = SharedSink::new(ctx.sink.clone());
    sink.observation(&observation);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;
    use flux_core::{Chunk, StopReason};
    use flux_events::EventStore;
    use flux_provider::{ChunkStream, Request};
    use flux_runtime::{
        AllowApprover, ApprovalChoice, Approver, PermissionManager, Tool, ToolContext,
        ToolRegistry, ToolResult,
    };
    use flux_system::{System, Workspace};

    use super::*;

    static TEST_DIR: AtomicU64 = AtomicU64::new(0);

    /// A per-test workspace directory that removes itself on drop, so a run never leaks the
    /// `flux-staged-*` temp directories even when an assertion fails mid-test.
    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIR.fetch_add(1, Ordering::SeqCst);
            let root =
                std::env::temp_dir().join(format!("{label}-{}-{sequence}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct NoopSink;
    impl AgentSink for NoopSink {}

    #[derive(Default)]
    struct RecordingSink {
        tools: Vec<String>,
        results: Vec<(String, String, bool)>,
        observations: Vec<String>,
        events: Vec<String>,
    }

    impl AgentSink for RecordingSink {
        fn planning(&mut self, active: bool) {
            self.events.push(format!("planning:{active}"));
        }

        fn tool_call(&mut self, name: &str, _input: &Value) {
            self.tools.push(name.to_string());
            self.events.push(format!("tool:{name}"));
        }

        fn tool_result(&mut self, name: &str, result: &ToolResult) {
            self.results
                .push((name.to_string(), result.content.clone(), result.is_error));
        }

        fn observation(&mut self, observation: &Observation) {
            self.observations.push(observation.kind.clone());
            self.events
                .push(format!("observation:{}", observation.kind));
        }
    }

    struct CaptureProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    struct PendingProvider;

    #[async_trait]
    impl Provider for PendingProvider {
        fn name(&self) -> &str {
            "pending"
        }

        async fn stream(&self, _request: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    struct ErrorProvider;

    #[async_trait]
    impl Provider for ErrorProvider {
        fn name(&self) -> &str {
            "error"
        }

        async fn stream(&self, _request: Request) -> Result<ChunkStream> {
            Err(Error::Other("provider failed".into()))
        }
    }

    #[async_trait]
    impl Provider for CaptureProvider {
        fn name(&self) -> &str {
            "capture"
        }

        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            self.requests.lock().unwrap().push(request);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(
                response.into_iter().map(Ok),
            )))
        }
    }

    struct CountingTool {
        spec: ToolSpec,
        result: String,
        calls: Arc<AtomicU64>,
    }

    struct CountingDenyApprover(Arc<AtomicU64>);

    #[async_trait]
    impl Approver for CountingDenyApprover {
        async fn request(
            &self,
            _tool: &str,
            _subjects: &[String],
            _intents: &flux_spec::IntentSet,
        ) -> ApprovalChoice {
            self.0.fetch_add(1, Ordering::SeqCst);
            ApprovalChoice::Deny
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn spec(&self) -> ToolSpec {
            self.spec.clone()
        }

        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::ok(self.result.clone()))
        }
    }

    fn native_call(id: &str, name: &str, input: Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    fn prose(text: &str) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.into()),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    fn native_calls(calls: Vec<(&str, &str, Value)>) -> Vec<Chunk> {
        let mut chunks = calls
            .into_iter()
            .map(|(id, name, input)| {
                Chunk::Block(ContentBlock::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    input,
                })
            })
            .collect::<Vec<_>>();
        chunks.push(Chunk::Done {
            stop_reason: Some(StopReason::ToolUse),
        });
        chunks
    }

    fn tool_result_text(message: &Message) -> String {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                flux_core::ToolResultContent::Text { text } => Some(text.as_str()),
                flux_core::ToolResultContent::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    struct TestHarness {
        context: StagedContext,
        requests: Arc<Mutex<Vec<Request>>>,
        read_calls: Arc<AtomicU64>,
        write_calls: Arc<AtomicU64>,
        // Held so the workspace directory is cleaned up when the harness is dropped.
        _root: TempRoot,
    }

    fn staged_context(responses: Vec<Vec<Chunk>>) -> TestHarness {
        let read_calls = Arc::new(AtomicU64::new(0));
        let write_calls = Arc::new(AtomicU64::new(0));
        let read_spec = spec(
            "inspect",
            vec![Effect::Read, Effect::Filesystem],
            vec![AccessKind::Filesystem],
            None,
        );
        let read_spec = ToolSpec {
            description: "Inspect one fixture record".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"],
                "additionalProperties": false
            }),
            ..read_spec
        };
        let write_spec = ToolSpec {
            description: "Change one fixture record".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            ..spec(
                "change",
                vec![Effect::Write, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            )
        };

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingTool {
            spec: read_spec,
            result: "fixture-evidence".into(),
            calls: read_calls.clone(),
        }));
        registry.register(Arc::new(CountingTool {
            spec: write_spec,
            result: "changed".into(),
            calls: write_calls.clone(),
        }));

        let temp = TempRoot::new("flux-staged-test");
        let system = Arc::new(System::new(Workspace::new(temp.path()).unwrap()));
        let executor = Arc::new(Executor::new(
            registry,
            PermissionManager::from_rules(&["inspect".into(), "change".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        ));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(CaptureProvider {
            responses: Mutex::new(responses.into()),
            requests: requests.clone(),
        });
        let context = StagedContext {
            provider,
            model: "test-model".into(),
            executor,
            store: Arc::new(FlowStore::in_memory().unwrap()),
            session_id: "staged-test".into(),
            conversation: vec![Message::user_text("Inspect the fixture")],
            base_system: Some("Use only fixture evidence.".into()),
            sink: Arc::new(Mutex::new(NoopSink)),
            audit: None,
            advertised: HashSet::from(["inspect".into(), "change".into()]),
            groups: Vec::new(),
            opts: StageOptions::default(),
            max_native_rounds: MAX_NATIVE_ROUNDS,
            remaining_token_budget: None,
        };
        TestHarness {
            context,
            requests,
            read_calls,
            write_calls,
            _root: temp,
        }
    }

    /// Test driver for the shipped two-stage path. Production control flow lives in
    /// `agent-loop.flux`; this helper merely composes the same typed operations for focused unit
    /// tests of native requests and repair behavior.
    async fn run(context: StagedContext) -> StagedRun {
        let mut intent = detect_intent_stage(context.clone()).await;
        let intent_value = match intent.result {
            Ok(value) => value,
            Err(error) => {
                return StagedRun {
                    result: Err(error),
                    usages: intent.usages,
                };
            }
        };
        let mut exploration =
            explore_stage(context, json!({ "state": intent_value["state"].clone() })).await;
        intent.usages.append(&mut exploration.usages);
        StagedRun {
            result: exploration.result,
            usages: intent.usages,
        }
    }

    fn spec(
        name: &str,
        effects: Vec<Effect>,
        access: Vec<AccessKind>,
        group: Option<&str>,
    ) -> ToolSpec {
        let mut spec = ToolSpec::read_only(name, name, json!({"type": "object"}));
        spec.effects = effects;
        spec.access = access;
        spec.group = group.map(str::to_string);
        spec
    }

    #[tokio::test]
    async fn config_model_stage_uses_direct_schemas_and_only_gather_tools() {
        let responses = vec![
            native_call("read-1", "inspect", json!({"key": "record"})),
            native_call(
                "return-1",
                RETURN_STAGE_RESULT,
                json!({"value": {"score": 7}}),
            ),
        ];
        let TestHarness {
            context,
            requests,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);
        let definition = ModelStageDefinition {
            prompt: "Score the fixture after inspecting it.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"subject": {"type": "string"}},
                "required": ["subject"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {"score": {"type": "integer"}},
                "required": ["score"],
                "additionalProperties": false
            }),
            model: Some("stage-model".into()),
            tools: vec!["inspect".into()],
            max_tokens: 512,
            effort: Some(Effort::Low),
        };

        let run = run_model_stage(
            context,
            "score_fixture",
            definition,
            json!({"subject": "record"}),
        )
        .await;
        assert_eq!(run.result.unwrap(), json!({"score": 7}));
        assert_eq!(read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.model == "stage-model"));
        assert!(requests
            .iter()
            .all(|request| request.effort == Some(Effort::Low)));
        let returned = requests[0]
            .tools
            .iter()
            .find(|tool| tool.name == RETURN_STAGE_RESULT)
            .unwrap();
        assert_eq!(
            returned.input_schema["properties"]["value"]["required"][0],
            "score"
        );
    }

    #[tokio::test]
    async fn staged_consultations_are_balanced_and_end_before_tool_dispatch() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "answer from workspace evidence",
                    "capability_families": ["workspace.read"]
                }),
            ),
            native_call("inspect", "inspect", json!({"key": "record"})),
            prose("grounded answer"),
        ];
        let TestHarness { mut context, .. } = staged_context(responses);
        let sink = Arc::new(Mutex::new(RecordingSink::default()));
        context.sink = sink.clone();

        assert_eq!(run(context).await.result.unwrap()["kind"], "chat");

        let events = sink.lock().unwrap().events.clone();
        assert_eq!(
            events,
            vec![
                "observation:loop.phase",
                "planning:true",
                "planning:false",
                "observation:turn.intent",
                "observation:loop.phase",
                "planning:true",
                "planning:false",
                "tool:inspect",
                "observation:adaptive.call",
                "planning:true",
                "planning:false",
            ],
            "each provider wait must be visible and must stop before a gathered op starts"
        );
    }

    #[tokio::test]
    async fn staged_consultation_indicator_balances_on_error_and_cancellation() {
        for (provider, cancelled) in [
            (Arc::new(ErrorProvider) as Arc<dyn Provider>, false),
            (Arc::new(PendingProvider) as Arc<dyn Provider>, true),
        ] {
            let TestHarness { mut context, .. } = staged_context(Vec::new());
            let sink = Arc::new(Mutex::new(RecordingSink::default()));
            context.sink = sink.clone();
            context.provider = provider;

            if cancelled {
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(10), run(context))
                        .await
                        .is_err(),
                    "pending staged consultation must be cancelled by the timeout"
                );
            } else {
                assert!(run(context).await.result.is_err());
            }

            assert_eq!(
                sink.lock().unwrap().events,
                vec!["observation:loop.phase", "planning:true", "planning:false",],
                "the RAII planning bracket must clear on every exit"
            );
        }
    }

    #[test]
    fn families_are_derived_only_from_registered_discoverable_operations() {
        let specs = vec![
            spec(
                "read",
                vec![Effect::Read, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            ),
            spec(
                "slack.send",
                vec![Effect::Network],
                vec![],
                Some("plugin.slack"),
            ),
            spec("now", vec![Effect::Read], vec![], None),
            spec(
                "bash",
                vec![Effect::Process, Effect::LocalSystem],
                vec![],
                Some("shell"),
            ),
        ];
        let groups = vec![
            ToolGroup {
                name: "plugin.slack".into(),
                description: "Company chat".into(),
                tools: vec!["slack.send".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some("slack".into()),
                }],
            },
            ToolGroup {
                name: "shell".into(),
                description: "Shell".into(),
                tools: vec!["bash".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: flux_evidence::KIND_SIGNAL.into(),
                    signal: Some("shell".into()),
                }],
            },
        ];
        let advertised = HashSet::from(["read".to_string(), "now".to_string()]);
        let families = build_families(&specs, &groups, &advertised);

        assert!(families.contains_key("workspace.read"));
        assert_eq!(families["core"].specs[0].name, "now");
        assert!(families["workspace.read"]
            .specs
            .iter()
            .all(|spec| spec.name != "now"));
        assert!(families.contains_key("plugin.slack"));
        assert!(
            !families.contains_key("shell"),
            "an operator-gated family must not become semantically discoverable"
        );
        assert_eq!(families["plugin.slack"].specs[0].name, "slack.send");
    }

    #[test]
    fn provider_native_alias_is_portable_stable_and_keeps_canonical_name_in_description() {
        let operation = "plugin.with-a-very-long-namespace.operation.with.dots.and.more.characters";
        let alias = native_tool_name(operation);
        assert_eq!(alias, native_tool_name(operation));
        assert_ne!(alias, operation);
        assert!(alias.len() <= 64, "{alias}");
        assert!(alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'));

        let definition = tool_def(&spec(
            operation,
            vec![Effect::Read, Effect::Network],
            vec![AccessKind::Network],
            Some("plugin.test"),
        ));
        assert_eq!(definition.name, alias);
        assert!(definition.description.contains(operation));
    }

    #[test]
    fn gather_safety_is_contract_and_concrete_intent_based() {
        let read = spec(
            "read",
            vec![Effect::Read, Effect::Filesystem],
            vec![AccessKind::Filesystem],
            None,
        );
        assert!(gather_safe(
            &read,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new()
        ));
        assert!(!gather_safe(
            &read,
            flux_spec::StagingDisposition::Capture,
            flux_spec::IntentSet::new()
        ));
        assert!(gather_safe(
            &read,
            flux_spec::StagingDisposition::Gather,
            flux_spec::IntentSet::new()
        ));

        let network_read = spec(
            "tickets.list",
            vec![Effect::Read, Effect::Network],
            vec![AccessKind::Network],
            None,
        );
        assert!(gather_safe(
            &network_read,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new()
        ));

        let write = spec(
            "write",
            vec![Effect::Write, Effect::Filesystem],
            vec![AccessKind::Filesystem],
            None,
        );
        assert!(!gather_safe(
            &write,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new()
        ));
        assert!(!gather_safe(
            &write,
            flux_spec::StagingDisposition::Gather,
            flux_spec::IntentSet::new()
        ));

        // A pure op (no effects, no access) stays gather-safe.
        let pure = spec("compute", vec![], vec![], None);
        assert!(gather_safe(
            &pure,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new()
        ));

        // An operation that declares no effects but reaches a code-running / local-system host
        // capability is NOT gather-safe: the empty effect set must not read as inert.
        let process = spec("shell_probe", vec![], vec![AccessKind::Process], None);
        assert!(!gather_safe(
            &process,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new()
        ));
        let local_system = spec("host_probe", vec![], vec![AccessKind::LocalSystem], None);
        assert!(!gather_safe(
            &local_system,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new()
        ));
    }

    #[tokio::test]
    async fn native_requests_use_exact_selected_schema_and_feed_gather_result_back() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect the fixture",
                    "capability_families": ["workspace.read"]
                }),
            ),
            native_call("read", "inspect", json!({"key": "alpha"})),
            prose("The fixture says fixture-evidence."),
        ];
        let TestHarness {
            context: ctx,
            requests,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);

        let output = run(ctx).await.result.unwrap();
        assert_eq!(output["kind"], "chat");
        assert_eq!(read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[0]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec![DECLARE_INTENT]
        );
        assert_eq!(
            requests[1]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "inspect",
                FINALIZE_PLAN,
                REQUEST_DECISION,
                SIGNAL_CAPABILITIES,
            ]
        );
        assert_eq!(
            requests[1].tools[0].input_schema,
            json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"],
                "additionalProperties": false
            })
        );
        let evidence_feedback = requests[2]
            .messages
            .last()
            .expect("gather result message")
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => {
                    content.iter().find_map(|part| match part {
                        flux_core::ToolResultContent::Text { text } => Some(text.as_str()),
                        flux_core::ToolResultContent::Image { .. } => None,
                    })
                }
                _ => None,
            })
            .unwrap();
        assert!(evidence_feedback.contains("fixture-evidence"));
    }

    #[tokio::test]
    async fn portable_native_alias_dispatches_the_canonical_dotted_operation() {
        let operation = "plugin.records.read";
        let alias = native_tool_name(operation);
        let calls = Arc::new(AtomicU64::new(0));
        let tool_spec = ToolSpec {
            description: "Read one plugin record".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"],
                "additionalProperties": false
            }),
            ..spec(
                operation,
                vec![Effect::Read, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            )
        };
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingTool {
            spec: tool_spec,
            result: "plugin-evidence".into(),
            calls: calls.clone(),
        }));
        let temp = TempRoot::new("flux-staged-alias-test");
        let executor = Arc::new(Executor::new(
            registry,
            PermissionManager::from_rules(&[operation.into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(temp.path()).unwrap()))),
        ));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(CaptureProvider {
            responses: Mutex::new(
                vec![
                    native_call(
                        "intent",
                        DECLARE_INTENT,
                        json!({
                            "intent": "read a plugin record",
                            "capability_families": ["workspace.read"]
                        }),
                    ),
                    native_call("read", &alias, json!({"id": "record-1"})),
                    prose("The plugin record says plugin-evidence."),
                ]
                .into(),
            ),
            requests: requests.clone(),
        });
        let ctx = StagedContext {
            provider,
            model: "test-model".into(),
            executor,
            store: Arc::new(FlowStore::in_memory().unwrap()),
            session_id: "staged-alias-test".into(),
            conversation: vec![Message::user_text("Read plugin record-1")],
            base_system: None,
            sink: Arc::new(Mutex::new(NoopSink)),
            audit: None,
            advertised: HashSet::from([operation.into()]),
            groups: Vec::new(),
            opts: StageOptions::default(),
            max_native_rounds: MAX_NATIVE_ROUNDS,
            remaining_token_budget: None,
        };

        let output = run(ctx).await.result.unwrap();

        assert_eq!(output["kind"], "chat");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests[1].tools[0].name, alias);
        assert!(requests[1].tools[0].description.contains(operation));
    }

    #[tokio::test]
    async fn schema_violation_is_repaired_before_any_dispatch() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect the fixture",
                    "capability_families": ["workspace.read"]
                }),
            ),
            native_call(
                "bad-read",
                "inspect",
                json!({"key": "alpha", "invented": true}),
            ),
            native_call("good-read", "inspect", json!({"key": "alpha"})),
            prose("Grounded result."),
        ];
        let TestHarness {
            context: ctx,
            requests,
            read_calls,
            ..
        } = staged_context(responses);

        let output = run(ctx).await.result.unwrap();
        assert_eq!(output["kind"], "chat");
        assert_eq!(read_calls.load(Ordering::SeqCst), 1);
        let requests = requests.lock().unwrap();
        let repair = requests[2]
            .messages
            .last()
            .unwrap()
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => content.iter().find_map(|part| match part {
                    flux_core::ToolResultContent::Text { text } => Some(text.as_str()),
                    flux_core::ToolResultContent::Image { .. } => None,
                }),
                _ => None,
            })
            .unwrap();
        assert!(repair.contains("invented"), "repair was: {repair}");
        assert!(repair.contains("unexpected") || repair.contains("additional"));
    }

    #[tokio::test]
    async fn missing_intent_call_gets_one_actionable_repair() {
        let responses = vec![
            prose("I should inspect the fixture."),
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect the fixture",
                    "capability_families": ["workspace.read"]
                }),
            ),
            prose("No operation is needed for this test."),
        ];
        let TestHarness {
            context: ctx,
            requests,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);

        let staged = run(ctx).await;

        assert_eq!(staged.usages.len(), 3);
        assert_eq!(staged.result.unwrap()["kind"], "chat");
        assert_eq!(read_calls.load(Ordering::SeqCst), 0);
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
        let requests = requests.lock().unwrap();
        assert!(requests[1]
            .messages
            .last()
            .is_some_and(|message| message.text().contains("must call `declare_intent`")));
        assert_eq!(requests[1].tools.len(), 1);
        assert_eq!(requests[1].tools[0].name, DECLARE_INTENT);
    }

    #[tokio::test]
    async fn unknown_family_is_repaired_without_widening_visibility() {
        let responses = vec![
            native_call(
                "bad-intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect the fixture",
                    "capability_families": ["installed-but-unwired"]
                }),
            ),
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect the fixture",
                    "capability_families": ["workspace.read"]
                }),
            ),
            prose("Grounded answer."),
        ];
        let TestHarness {
            context: ctx,
            requests,
            ..
        } = staged_context(responses);

        assert_eq!(run(ctx).await.result.unwrap()["kind"], "chat");
        let requests = requests.lock().unwrap();
        let repair = tool_result_text(requests[1].messages.last().unwrap());
        assert!(repair.contains("unknown or unavailable capability family"));
        assert!(!requests[2]
            .tools
            .iter()
            .any(|tool| tool.name == "installed-but-unwired"));
    }

    #[tokio::test]
    async fn unselected_native_operation_is_rejected_without_dispatch() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect the fixture",
                    "capability_families": ["workspace.read"]
                }),
            ),
            native_call("fabricated", "change", json!({"value": "bad"})),
            prose("I did not change anything."),
        ];
        let TestHarness {
            context: ctx,
            requests,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);

        assert_eq!(run(ctx).await.result.unwrap()["kind"], "chat");
        assert_eq!(read_calls.load(Ordering::SeqCst), 0);
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
        let requests = requests.lock().unwrap();
        assert!(requests[2].messages.last().is_some_and(|message| {
            tool_result_text(message).contains("was not selected by intent")
        }));
    }

    #[tokio::test]
    async fn mixed_finalization_is_repaired_without_capturing_ambiguous_calls() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "change the fixture",
                    "capability_families": ["workspace.write"]
                }),
            ),
            native_call("first", "change", json!({"value": "first"})),
            native_calls(vec![
                (
                    "mixed-finalize",
                    FINALIZE_PLAN,
                    json!({"instructions": "Report the result."}),
                ),
                ("ambiguous", "change", json!({"value": "must-not-capture"})),
            ]),
            native_call(
                "finalize",
                FINALIZE_PLAN,
                json!({"instructions": "Report the result."}),
            ),
        ];
        let TestHarness {
            context: ctx,
            requests,
            write_calls,
            ..
        } = staged_context(responses);

        let output = run(ctx).await.result.unwrap();

        assert_eq!(output["kind"], "batch");
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
        let batch: ActionBatch = serde_json::from_value(output["batch"].clone()).unwrap();
        assert_eq!(batch.actions.len(), 1, "the mixed call was never captured");
        assert!(requests.lock().unwrap()[3]
            .messages
            .last()
            .is_some_and(|message| {
                tool_result_text(message).contains("must each be called by themselves")
            }));
    }

    #[tokio::test]
    async fn native_round_exhaustion_fails_honestly_without_execution() {
        let mut responses = vec![native_call(
            "intent",
            DECLARE_INTENT,
            json!({
                "intent": "change the fixture",
                "capability_families": ["workspace.write"]
            }),
        )];
        responses.extend((0..MAX_NATIVE_ROUNDS).map(|round| {
            native_call(
                &format!("write-{round}"),
                "change",
                json!({"value": format!("value-{round}")}),
            )
        }));
        let TestHarness {
            context: ctx,
            write_calls,
            ..
        } = staged_context(responses);

        let error = run(ctx).await.result.unwrap_err().to_string();

        assert!(
            error.contains(&format!("exhausted {MAX_NATIVE_ROUNDS} native rounds")),
            "{error}"
        );
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn mutating_native_call_is_captured_as_literal_batch_without_dispatch() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "change the fixture",
                    "capability_families": ["workspace.write"]
                }),
            ),
            native_call("write", "change", json!({"value": "new"})),
            native_call(
                "finalize",
                FINALIZE_PLAN,
                json!({"instructions": "Report the actual change result."}),
            ),
        ];
        let TestHarness {
            context: ctx,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);

        let output = run(ctx).await.result.unwrap();
        assert_eq!(output["kind"], "batch");
        assert_eq!(read_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            write_calls.load(Ordering::SeqCst),
            0,
            "proposal must be inert until batch approval and execution"
        );
        let batch: ActionBatch = serde_json::from_value(output["batch"].clone()).unwrap();
        assert_eq!(batch.actions.len(), 1);
        assert_eq!(batch.actions[0].op, "change");
        assert_eq!(batch.actions[0].input, json!({"value": "new"}));
    }

    #[tokio::test]
    async fn frozen_batch_state_carries_host_gathered_evidence() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect and change the fixture",
                    "capability_families": ["workspace.read", "workspace.write"]
                }),
            ),
            native_call("read", "inspect", json!({"key": "alpha"})),
            native_call("write", "change", json!({"value": "new"})),
            native_call(
                "finalize",
                FINALIZE_PLAN,
                json!({
                    "instructions": "Report the actual change using gathered evidence.",
                    "primer": "invented model note that must not override evidence"
                }),
            ),
        ];
        let TestHarness {
            context: ctx,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);

        let output = run(ctx).await.result.unwrap();

        assert_eq!(output["kind"], "batch");
        assert_eq!(read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
        let state = &output["state"];
        assert_eq!(state["gathered"][0]["op"], "inspect");
        assert!(state["gathered"][0]["input"]
            .as_str()
            .unwrap()
            .contains("alpha"));
        assert_eq!(state["gathered"][0]["result"], "fixture-evidence");
        assert_eq!(
            state["pending"]["instructions"],
            "Report the actual change using gathered evidence."
        );
    }

    #[tokio::test]
    async fn captured_action_reaches_batch_approval_and_denial_executes_nothing() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "change the fixture",
                    "capability_families": ["workspace.write"]
                }),
            ),
            native_call("write", "change", json!({"value": "new"})),
            native_call(
                "finalize",
                FINALIZE_PLAN,
                json!({"instructions": "Report whether the change actually happened."}),
            ),
        ];
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(CaptureProvider {
            responses: Mutex::new(responses.into()),
            requests: requests.clone(),
        });
        let write_calls = Arc::new(AtomicU64::new(0));
        let write_spec = ToolSpec {
            description: "Change one fixture record".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            ..spec(
                "change",
                vec![Effect::Write, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            )
        };
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingTool {
            spec: write_spec,
            result: "changed".into(),
            calls: write_calls.clone(),
        }));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);

        let temp = TempRoot::new("flux-staged-engine-test");
        let root = temp.path().to_path_buf();
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("capture/test-model").unwrap();
        let approval_requests = Arc::new(AtomicU64::new(0));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&[], &[]),
            Arc::new(CountingDenyApprover(approval_requests.clone())),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = crate::engine::FlowEngine::assemble(
            provider,
            executor,
            events.clone(),
            store,
            "test-model".into(),
            "Use only fixture evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            root,
        )
        .unwrap();
        let mut sink = RecordingSink::default();

        engine
            .run_turn(&session, "Change the fixture", &mut sink)
            .await
            .unwrap();

        assert_eq!(
            write_calls.load(Ordering::SeqCst),
            0,
            "the captured action must remain inert when batch approval is denied"
        );
        assert_eq!(
            approval_requests.load(Ordering::SeqCst),
            1,
            "the captured action must reach aggregate batch approval; tools={:?}, results={:?}, observations={:?}, conversation={:?}",
            sink.tools,
            sink.results,
            sink.observations,
            events.conversation(&session).unwrap()
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(events
            .conversation(&session)
            .unwrap()
            .last()
            .is_some_and(|message| message.text().contains("not approved")));
    }

    #[test]
    fn approval_receipts_are_one_shot_and_bound_to_batch_session_and_context() {
        let book = ReceiptBook::default();
        let batch = ActionBatch {
            id: "batch-1".into(),
            session_id: "session-a".into(),
            actions: vec![Action {
                id: "action-1".into(),
                op: "write".into(),
                input: json!({"path": "a.txt", "content": "x"}),
            }],
        };
        let changed = ActionBatch {
            actions: vec![Action {
                input: json!({"path": "a.txt", "content": "y"}),
                ..batch.actions[0].clone()
            }],
            ..batch.clone()
        };

        let receipt = book.issue(&batch, "session-a", "caller-policy-a", false);
        assert!(book
            .consume(&batch, &receipt, "session-a", "caller-policy-a")
            .is_ok());
        assert!(
            book.consume(&batch, &receipt, "session-a", "caller-policy-a")
                .is_err(),
            "a receipt cannot be reused"
        );

        let changed_receipt = book.issue(&batch, "session-a", "caller-policy-a", false);
        assert!(
            book.consume(&changed, &changed_receipt, "session-a", "caller-policy-a")
                .is_err(),
            "changed action bytes invalidate approval"
        );

        let context_receipt = book.issue(&batch, "session-a", "caller-policy-a", false);
        assert!(
            book.consume(&batch, &context_receipt, "session-a", "caller-policy-b")
                .is_err(),
            "caller/policy changes invalidate approval"
        );

        let session_receipt = book.issue(&batch, "session-a", "caller-policy-a", false);
        assert!(
            book.consume(&batch, &session_receipt, "session-b", "caller-policy-a")
                .is_err(),
            "a receipt cannot cross sessions"
        );
    }
}
