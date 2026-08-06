//! Typed intent routing, provider-native operation schemas, bounded evidence gathering, and inert
//! action capture for Flux-authored adaptive outer loops.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use flux_core::{ContentBlock, Error, Message, Result, ToolResultContent, Usage};
use flux_evidence::{Observation, Phase as EvidencePhase, ToolGroup, KIND_TURN_INTENT};
use flux_lang::ast::{DraftAst, Node, SymbolName};
use flux_provider::{Effort, Provider, Request, RequestTrace, SystemSegment, ToolDef};
use flux_runtime::{effective_group, AuthorizeVerdict, Executor};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::loop_host::SharedSink;
use crate::model::{consult_model, ModelCallMetrics, StageOptions};
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
const MAX_INTENT_LIST_ITEMS: usize = 8;
const MAX_INTENT_LIST_ITEM_CHARS: usize = 256;
const MAX_DELIVERABLE_CHARS: usize = 512;
const MAX_NATIVE_TOOLS: usize = 64;
const MAX_NATIVE_SCHEMA_CHARS: usize = 128_000;
/// Reserved physical group for host-owned channel facilities that must remain available after
/// functional intent narrowing. Ambient operations are visibility-only: every live executor,
/// permission, authored scope, schema budget, approval, and dispatch check still applies.
pub const ADAPTIVE_AMBIENT_GROUP: &str = "flux.ambient";
/// Per-invocation provider-round cap for an authored `[agent.stages]` model stage. Named stages are
/// a separate mechanism from the adaptive intent/explore loop, so their repair-round ceiling is a
/// fixed, self-contained bound rather than the user-tunable adaptive model-call budget (a tight or
/// generous `[agent.adaptive] max_model_calls` must not silently retarget an unrelated named stage).
const MODEL_STAGE_MAX_ROUNDS: usize = 50;
pub const DEFAULT_ADAPTIVE_MODEL_CALLS: usize = 50;

const INTENT_SYSTEM: &str = "You are Flux's intent router. Understand the user's request and call \
declare_intent exactly once. Select only the smallest capability families needed. This is routing, \
not execution: do not answer the request, call any operation, or invent a family. An empty family \
list is correct only when the answer needs no live, runtime, workspace, network, service, or other \
external fact. Never assume those facts are already known: select the matching evidence capability. \
Do not select cognition/model families merely \
to reason, calculate, summarize, cite, or write an answer: you already do those things. Select them \
only when the user explicitly asks for a separate model-backed operation.";

/// The execution contract for an operator-authored `ai_segment`.
///
/// [`EXPLORE_SYSTEM`] is the *planner's* contract: it tells the model to hand a plan back for someone
/// else to run. That framing is correct for the adaptive CLI path but wrong here, because
/// `EngineLoopHost::run_scoped_segment` consumes a finalized batch itself — `approve_batch` then
/// `execute_batch` — and loops with the execution report, so an authored segment can act, observe the
/// result, and keep going within one turn.
///
/// What it must NOT do is deny the staging step. An effectful call is not run inline: it is captured
/// into `state.proposed` with the tool result "captured as proposed action N; not executed", and only
/// `FINALIZE_PLAN` turns that batch into real execution. A first attempt at this prompt asserted that
/// effects "REALLY EXECUTE" and told the model not to hand back a plan; a live Fleet worker then made
/// 68 calls, staged its writes, never called `finalize_plan`, and finished having changed nothing —
/// worse than the planner prompt it replaced, which at least taught the ritual.
///
/// So this contract states both halves: staging is real, and finalizing executes for real. The
/// evidence discipline is copied from [`EXPLORE_SYSTEM`] verbatim; only the execution contract
/// differs.
const SEGMENT_SYSTEM: &str = "You are Flux executing an authored segment. The tools below are the \
only operations available to you and each carries its real input schema. They are the operations \
the segment's author chose for this exact assignment; if the assignment needs one that is absent, \
say so rather than working around it. Use gather tools to inspect evidence before acting; cite exact \
source identifiers from tool calls/results and never invent facts or paths. A filesystem path is \
known only when the request supplied it or a tool result listed it. If no exact relevant path is \
known, first inventory the workspace once with `glob`, set `pattern` to `*`, and omit `path`; never \
guess a likely filename. Keep a checklist of every input fact and governing rule the assignment \
needs, and read an authoritative source for each. Search hits only locate sources--read the source \
itself. Minimize provider rounds without skipping evidence: emit independent gather calls together \
in one response. Batch only independent gather calls; never batch writes or destructive work. \
A gather call runs immediately and returns its real result. An effectful call — a write, edit, \
command or commit — is STAGED instead: its tool result says `captured as proposed action N; not \
executed`. That is normal and expected, not a refusal and not a permission problem. Staged work runs \
only when you call `finalize_plan` by itself, with no other call in the same response. \
Doing so executes the whole staged batch for real against the workspace, and the execution report \
comes back to you, so you can inspect what happened and continue working in this same turn. Stage \
and finalize as many times as the assignment needs. \
You are here to carry the assignment to its stated completion, not to propose it to someone else: if \
you have staged actions and stop without calling `finalize_plan`, nothing you staged will ever \
happen and the assignment fails silently. Never report a staged action as if it had already \
happened — finalize it, read the report, then report what actually occurred with the evidence that \
proves it. If the assignment genuinely cannot be completed, stop and state precisely which fact, \
operation or decision is missing.";

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
them together with `read_many` instead of issuing sequential `read` calls. Batch only independent \
gather calls; never batch writes, destructive work, or calls that need approval. If the user supplies an \
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
    /// The immutable live-catalog generation adopted at this turn boundary.
    pub registry: Arc<flux_runtime::ToolRegistry>,
    pub store: Arc<FlowStore>,
    pub session_id: String,
    pub conversation: Vec<Message>,
    pub base_system: Option<String>,
    pub sink: Arc<Mutex<dyn AgentSink>>,
    /// Durable turn scope used to record host-built execution graphs for replay/fork. The model
    /// never supplied these graphs; the host derived them from validated native calls.
    pub audit: Option<(Arc<flux_events::EventStore>, i64)>,
    /// Operations surfaced by host evidence at the turn boundary. Semantic `turn.intent` families
    /// may grow beyond this snapshot for the current adaptive state, but never beyond the live
    /// registry/permission/`with_tools` ceiling recomputed by [`live_visible_specs`].
    pub advertised: HashSet<String>,
    /// Optional exact ceiling owned by an authored model stage such as `ai_segment`. Normal
    /// adaptive turns leave this unset; a stage that names tools explicitly sets it so semantic
    /// discovery cannot widen beyond the author's list.
    pub authored_ceiling: Option<HashSet<String>>,
    pub groups: Vec<ToolGroup>,
    pub opts: StageOptions,
    /// Remaining billed-token budget when this stage call began.
    pub remaining_token_budget: Option<u64>,
    /// Logical-run and per-stage cognition policy. Counts live in [`AdaptiveState`] so an `await`
    /// and process restart cannot reset them.
    pub adaptive_policy: AdaptiveLoopPolicy,
    /// A-94: the surface-shared mid-turn steering queue, drained at each planner-consultation
    /// round head. `None` for every caller without an interactive composer.
    pub steering: Option<Arc<crate::steering::SteeringQueue>>,
}

/// Usage stays outside the result: a provider error after usage arrived still costs tokens and must
/// be recorded by the host.
pub(crate) struct StagedRun {
    pub result: Result<Value>,
    pub usages: Vec<Usage>,
    pub model: String,
}

/// Optional overrides for one built-in adaptive model stage. Missing values inherit the agent's
/// provider-local model, effort, and token setting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentStagePolicy {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub max_tokens: Option<u32>,
    pub max_calls: Option<usize>,
}

/// Cognition policy for one logical adaptive run. The total call ceiling spans intent repair,
/// exploration, and every durable decision resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptiveLoopPolicy {
    pub max_model_calls: usize,
    pub intent: AgentStagePolicy,
    pub explore: AgentStagePolicy,
    /// Retained-history ceiling in bytes. `None` keeps [`ADAPTIVE_HISTORY_LIMIT`]. An authored
    /// `ai_segment` may raise it for a long implementation loop, where many in-budget tool results
    /// accumulate legitimately — the same way `max_rounds` overrides the model-call budget outright.
    pub max_history_bytes: Option<usize>,
}

impl Default for AdaptiveLoopPolicy {
    fn default() -> Self {
        Self {
            max_model_calls: DEFAULT_ADAPTIVE_MODEL_CALLS,
            intent: AgentStagePolicy::default(),
            explore: AgentStagePolicy::default(),
            max_history_bytes: None,
        }
    }
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
        &tool.semantic_effects(),
    )
}

#[derive(Clone)]
struct Family {
    name: String,
    description: String,
    specs: Vec<ToolSpec>,
    /// Virtual families have no authored semantic manifest, so their member names are the routing
    /// contract and must be listed exhaustively. Authored groups may use their description plus a
    /// compact sample because the group itself is the semantic contract.
    exhaustive_members: bool,
    /// Manifest-declared routing-only hints. They select visibility, never authority, and remain
    /// compact because operation schemas are not included until the family is selected.
    routing_signals: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct IntentDeclaration {
    intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effect_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deliverable: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    uncertainties: Vec<String>,
    families: Vec<String>,
    /// The families came from an operator-authored `ai_segment` ceiling rather than from the intent
    /// router's model call, so [`MAX_FAMILIES`] — a bound on what the *model* may select — does not
    /// apply. Defaults to `false`, so a state serialized by an older runtime stays capped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    scoped: bool,
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

struct AdmittedNativeCall {
    index: usize,
    step: usize,
    call_id: String,
    operation: String,
    input: Value,
    ast: DraftAst,
    parallel_safe: bool,
}

enum NativeBatchSlot {
    Execute(AdmittedNativeCall),
    Fixed(ContentBlock),
}

struct ExecutedNativeCall {
    index: usize,
    step: usize,
    call_id: String,
    operation: String,
    input: Value,
    result: std::result::Result<String, String>,
}

enum NativeBatchResult {
    Executed(ExecutedNativeCall),
    Fixed(ContentBlock),
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
    Routing {
        candidates: Vec<String>,
    },
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
    #[serde(default)]
    intent_calls: usize,
    #[serde(default)]
    explore_calls: usize,
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

const ADAPTIVE_TOOL_RESULT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdaptiveToolResultBudget {
    actual_bytes: usize,
    sha256: String,
}

fn bound_adaptive_tool_result(result: String) -> (String, Option<AdaptiveToolResultBudget>) {
    let actual_bytes = result.len();
    if actual_bytes <= ADAPTIVE_TOOL_RESULT_LIMIT {
        return (result, None);
    }

    let sha256 = digest(&result);
    let summary = json!({
        "type": "tool_result_omitted",
        "reason": "tool_result_too_large",
        "actual_bytes": actual_bytes,
        "limit_bytes": ADAPTIVE_TOOL_RESULT_LIMIT,
        "sha256": &sha256,
    })
    .to_string();
    (
        summary,
        Some(AdaptiveToolResultBudget {
            actual_bytes,
            sha256,
        }),
    )
}

const ADAPTIVE_HISTORY_LIMIT: usize = 512 * 1024;
const ADAPTIVE_REQUEST_LIMIT: usize = 1024 * 1024;
/// Trailing messages an authored-segment elision never touches, so the model always retains the
/// exchange it is mid-way through. Matches the engine compactor's `keep = 2` for the same reason.
const SEGMENT_HISTORY_KEEP_RECENT: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentHistoryElision {
    before_bytes: usize,
    after_bytes: usize,
    elided_results: usize,
}

/// Replace the oldest retained tool-result payloads with payload-free receipts until the retained
/// history fits `limit`, and report what was dropped.
///
/// This is [`bound_adaptive_tool_result`]'s shape applied one level up: an oversized *single* result
/// is already replaced by a `tool_result_omitted` receipt at gather time, but a long authored segment
/// can exceed the history ceiling through the sheer *number* of in-budget results. Eliding the oldest
/// ones keeps the loop running with its recent context intact instead of destroying a turn whose work
/// may already be committed. Only tool results are touched — the model's own turns, the goal, and the
/// most recent [`SEGMENT_HISTORY_KEEP_RECENT`] messages are preserved verbatim, so the conversation
/// stays valid for every provider.
fn elide_segment_history(messages: &mut [Message], limit: usize) -> Option<SegmentHistoryElision> {
    let before_bytes = adaptive_history_bytes(&messages);
    if before_bytes <= limit {
        return None;
    }
    let elidable = messages.len().saturating_sub(SEGMENT_HISTORY_KEEP_RECENT);
    let mut elided_results = 0usize;
    for index in 0..elidable {
        for block in messages[index].content.iter_mut() {
            let ContentBlock::ToolResult {
                content, is_error, ..
            } = block
            else {
                continue;
            };
            // An error result is the loop's own diagnostic and is small; keep it legible.
            if *is_error {
                continue;
            }
            let mut replaced = false;
            for entry in content.iter_mut() {
                let ToolResultContent::Text { text } = entry else {
                    continue;
                };
                // Already a receipt — re-eliding would only grow the digest chain.
                if text.starts_with(r#"{"type":"tool_result_omitted""#) {
                    continue;
                }
                *text = json!({
                    "type": "tool_result_omitted",
                    "reason": "segment_history_budget",
                    "actual_bytes": text.len(),
                    "limit_bytes": limit,
                    "sha256": digest(text),
                })
                .to_string();
                replaced = true;
            }
            if replaced {
                elided_results += 1;
            }
        }
        if adaptive_history_bytes(&messages) <= limit {
            break;
        }
    }
    if elided_results == 0 {
        return None;
    }
    Some(SegmentHistoryElision {
        before_bytes,
        after_bytes: adaptive_history_bytes(&messages),
        elided_results,
    })
}

/// The report an authored segment returns when its history cannot be brought under budget. The turn
/// ends, but it ends as a *result* carrying what was established — not as an error that discards it.
fn segment_history_summary(state: &AdaptiveState, history_bytes: usize, limit: usize) -> String {
    let operations = state
        .gathered
        .iter()
        .map(|evidence| evidence.op.as_str())
        .collect::<Vec<_>>();
    format!(
        "The authored segment ended early: retained history reached {history_bytes} bytes against a \
         {limit}-byte ceiling and could not be reduced further. {} gather \
         operation(s) completed{}{}. Any side effect already performed — including a commit — stands \
         and must be verified directly rather than inferred from this turn.",
        operations.len(),
        if operations.is_empty() {
            String::new()
        } else {
            format!(": {}", operations.join(", "))
        },
        if state.last_error.is_empty() {
            String::new()
        } else {
            format!("; last issue: {}", state.last_error)
        }
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdaptiveRequestBytes {
    system_bytes: usize,
    message_bytes: usize,
    tool_bytes: usize,
    total_bytes: usize,
}

fn serialized_bytes<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value)
        .map(|serialized| serialized.len())
        .unwrap_or(usize::MAX)
}

fn adaptive_history_bytes<T: serde::Serialize + ?Sized>(messages: &T) -> usize {
    serialized_bytes(messages)
}

fn adaptive_request_bytes<M, T>(system_text: &str, messages: &M, tools: &T) -> AdaptiveRequestBytes
where
    M: serde::Serialize + ?Sized,
    T: serde::Serialize + ?Sized,
{
    let system_bytes = system_text.len();
    let message_bytes = serialized_bytes(messages);
    let tool_bytes = serialized_bytes(tools);
    let total_bytes = system_bytes
        .saturating_add(message_bytes)
        .saturating_add(tool_bytes);
    AdaptiveRequestBytes {
        system_bytes,
        message_bytes,
        tool_bytes,
        total_bytes,
    }
}

fn adaptive_budget_status(actual_bytes: usize, limit_bytes: usize) -> Option<&'static str> {
    if actual_bytes > limit_bytes {
        Some("exceeded")
    } else if actual_bytes >= limit_bytes.saturating_mul(4) / 5 {
        Some("approaching")
    } else {
        None
    }
}

fn adaptive_budget_diagnostic(
    budget: &'static str,
    actual_bytes: usize,
    limit_bytes: usize,
    request: Option<AdaptiveRequestBytes>,
) -> Option<Value> {
    let status = adaptive_budget_status(actual_bytes, limit_bytes)?;
    let mut diagnostic = json!({
        "budget": budget,
        "status": status,
        "actual_bytes": actual_bytes,
        "limit_bytes": limit_bytes,
    });
    if let Some(request) = request {
        let object = diagnostic
            .as_object_mut()
            .expect("budget diagnostic is an object");
        object.insert("system_bytes".into(), request.system_bytes.into());
        object.insert("message_bytes".into(), request.message_bytes.into());
        object.insert("tool_bytes".into(), request.tool_bytes.into());
    }
    Some(diagnostic)
}

fn adaptive_budget_refusal(budget: &str, actual_bytes: usize, limit_bytes: usize) -> String {
    format!(
        "adaptive {budget} budget exceeded: actual_bytes={actual_bytes} limit_bytes={limit_bytes}"
    )
}

/// Run only the intent stage and return a durable state artifact for [`explore_stage`].
pub(crate) async fn detect_intent_stage(ctx: StagedContext) -> StagedRun {
    let mut usages = Vec::new();
    let model = stage_model(&ctx, &ctx.adaptive_policy.intent);
    let result = detect_intent_inner(&ctx, &mut usages).await;
    StagedRun {
        result,
        usages,
        model,
    }
}

/// Seed a bounded, capability-scoped `ai_segment` without an extra intent-model call. The authored
/// segment already names its exact tool ceiling, so every discoverable family inside that ceiling is
/// selected deterministically and the normal exploration ledger starts from `goal`.
pub(crate) fn scoped_segment_state(ctx: &StagedContext, goal: &str) -> Result<Value> {
    let specs = live_visible_specs(ctx);
    let families = build_families(&specs, &ctx.groups, &ctx.advertised);
    let ambient = ambient_specs(&specs, &ctx.groups);
    if families.is_empty() {
        return Err(Error::Other(
            "ai_segment has no registered operation inside its capability scope".into(),
        ));
    }
    let declaration = IntentDeclaration {
        intent: goal.to_string(),
        families: families.keys().cloned().collect(),
        scoped: true,
        ..Default::default()
    };
    let selected = selected_specs(&declaration, &families, &ambient)?;
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
        intent_calls: 0,
        explore_calls: 0,
    };
    adaptive_result(
        "intent",
        &state,
        json!({
            "intent": declaration.intent,
            "task_kind": declaration.task_kind,
            "effect_mode": declaration.effect_mode,
            "deliverable": declaration.deliverable,
            "constraints": declaration.constraints,
            "uncertainties": declaration.uncertainties,
            "families": declaration.families,
            "operations": selected_names,
        }),
    )
}

/// Names the exploration stage claims for its own control tools. A registered operation sharing one
/// would collide with the control tool in the provider request and be misrouted, so reserve them.
fn ensure_control_names_free(ctx: &StagedContext) -> Result<()> {
    for reserved in [FINALIZE_PLAN, REQUEST_DECISION, SIGNAL_CAPABILITIES] {
        if ctx.registry.get(reserved).is_some() {
            return Err(Error::Other(format!(
                "adaptive planning cannot reserve `{reserved}` because an operation already uses that name"
            )));
        }
    }
    Ok(())
}

async fn detect_intent_inner(ctx: &StagedContext, usages: &mut Vec<Usage>) -> Result<Value> {
    ensure_control_names_free(ctx)?;
    let specs = live_visible_specs(ctx);
    let families = build_families(&specs, &ctx.groups, &ctx.advertised);
    let ambient = ambient_specs(&specs, &ctx.groups);
    if families.is_empty() {
        return Err(Error::Other(
            "adaptive planning has no registered capability families".into(),
        ));
    }

    observe(ctx, "loop.phase", json!({"phase": "intent"}));
    let routed = matched_routing_families(ctx, &families);
    if routed.len() > 1 {
        let candidates = routed
            .iter()
            .map(|matched| matched.group.clone())
            .collect::<Vec<_>>();
        let intent = latest_user_input(ctx);
        let state = AdaptiveState {
            version: 1,
            declaration: IntentDeclaration {
                intent: intent.clone(),
                families: Vec::new(),
                ..Default::default()
            },
            selected: ambient.iter().map(|spec| spec.name.clone()).collect(),
            messages: ctx.conversation.clone(),
            proposed: Vec::new(),
            gathered: Vec::new(),
            native_step: 0,
            last_error: String::new(),
            pending: Some(PendingResponse::Routing {
                candidates: candidates.clone(),
            }),
            intent_calls: 0,
            explore_calls: 0,
        };
        observe(
            ctx,
            "turn.routing",
            json!({
                "status": "ambiguous",
                "families": &candidates,
                "signals": routed.iter().flat_map(|matched| &matched.signals).collect::<Vec<_>>(),
            }),
        );
        return adaptive_result(
            "decision",
            &state,
            json!({"question": routing_question(&candidates, &families, false)}),
        );
    }

    let mut declaration = declare_intent(ctx, &families, usages).await?;
    if let Some(routed) = routed.first() {
        if !declaration.families.contains(&routed.group) {
            declaration
                .families
                .truncate(MAX_FAMILIES.saturating_sub(1));
            declaration.families.push(routed.group.clone());
        }
        observe(
            ctx,
            "turn.routing",
            json!({
                "status": "matched",
                "family": &routed.group,
                "signals": &routed.signals,
            }),
        );
    }
    let selected = selected_specs(&declaration, &families, &ambient)?;
    let selected_names = selected
        .iter()
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    observe(
        ctx,
        "turn.intent",
        json!({
            "intent": declaration.intent,
            "task_kind": declaration.task_kind,
            "effect_mode": declaration.effect_mode,
            "deliverable": declaration.deliverable,
            "constraints": declaration.constraints,
            "uncertainties": declaration.uncertainties,
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
        intent_calls: usages.len(),
        explore_calls: 0,
    };
    Ok(json!({
        "kind": "intent",
        "intent": declaration.intent,
        "task_kind": declaration.task_kind,
        "effect_mode": declaration.effect_mode,
        "deliverable": declaration.deliverable,
        "constraints": declaration.constraints,
        "uncertainties": declaration.uncertainties,
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
    let model = stage_model(&ctx, &ctx.adaptive_policy.explore);
    let result = explore_stage_inner(&ctx, input, &mut usages).await;
    StagedRun {
        result,
        usages,
        model,
    }
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
    let model = definition
        .model
        .clone()
        .unwrap_or_else(|| ctx.model.clone());
    let result = run_model_stage_inner(&ctx, name, &definition, input, &mut usages).await;
    StagedRun {
        result,
        usages,
        model,
    }
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
        let tool = ctx.registry.get(operation).ok_or_else(|| {
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
    // The advertised tool set is loop-invariant, so build it once and clone per repair round instead
    // of re-running every `tool_def` conversion each round.
    let stage_tools: Vec<ToolDef> = by_native
        .values()
        .map(tool_def)
        .chain(std::iter::once(return_tool))
        .collect();
    let round_limit = MODEL_STAGE_MAX_ROUNDS;
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
        req.tools = stage_tools.clone();
        let stage_label = format!("stage.{name}");
        correlate_request(ctx, &mut req, &stage_label, round);
        let request_model = req.model.clone();

        let (streamed, usage, metrics) =
            consult_model(ctx.provider.as_ref(), ctx.sink.clone(), req).await;
        observe_model_call(
            ctx,
            ModelCallObservation {
                stage: &stage_label,
                round,
                repair_attempt: round.saturating_sub(1),
                model: &request_model,
                usage: &usage,
                metrics: &metrics,
                ok: streamed.is_ok(),
            },
        );
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

        let mut slots = Vec::with_capacity(calls.len());
        for (index, (id, native, call_input)) in calls.into_iter().enumerate() {
            let Some(spec) = by_native.get(&native) else {
                last_error = format!("model stage `{name}` called unavailable tool `{native}`");
                record_batch_admission(
                    ctx,
                    "native.batch_call",
                    json!({
                        "stage": stage_label,
                        "round": round,
                        "call_index": index,
                        "call_id": id,
                        "native_operation": native,
                        "disposition": "rejected",
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    last_error.clone(),
                    true,
                )));
                continue;
            };
            let operation = spec.name.clone();
            let Some(tool) = ctx.registry.get(&operation) else {
                last_error = format!("model stage `{name}` tool `{operation}` disappeared");
                record_batch_admission(
                    ctx,
                    "native.batch_call",
                    json!({
                        "stage": stage_label,
                        "round": round,
                        "call_index": index,
                        "call_id": id,
                        "operation": operation,
                        "disposition": "rejected",
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    last_error.clone(),
                    true,
                )));
                continue;
            };
            if let Err(diagnostics) = validate_call(spec, &call_input, ctx, &selected) {
                last_error = format!("invalid `{operation}` input: {}", diagnostics.join("; "));
                record_batch_admission(
                    ctx,
                    "native.batch_call",
                    json!({
                        "stage": stage_label,
                        "round": round,
                        "call_index": index,
                        "call_id": id,
                        "operation": operation,
                        "disposition": "rejected",
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    last_error.clone(),
                    true,
                )));
                continue;
            }
            if !gather_safe(
                spec,
                tool.staging_disposition(),
                tool.intents(&call_input),
                &tool.semantic_effects(),
            ) {
                last_error = format!(
                    "model stage `{name}` call to `{operation}` is not gather-safe for these arguments"
                );
                record_batch_admission(
                    ctx,
                    "native.batch_call",
                    json!({
                        "stage": stage_label,
                        "round": round,
                        "call_index": index,
                        "call_id": id,
                        "operation": operation,
                        "disposition": "rejected",
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    last_error.clone(),
                    true,
                )));
                continue;
            }
            let parallel_safe = native_call_parallel_safe(ctx, spec, tool.as_ref(), &call_input);
            let ast = one_call_ast(
                &operation,
                call_input.clone(),
                format!("model_stage_{round}_{index}"),
            );
            record_host_flow(ctx, &ast, &format!("stage:{name}"), round as u32);
            record_batch_admission(
                ctx,
                "native.batch_call",
                json!({
                    "stage": stage_label,
                    "round": round,
                    "call_index": index,
                    "call_id": id,
                    "operation": operation,
                    "disposition": if parallel_safe { "parallel_gather" } else { "ordered_gather" },
                }),
            );
            slots.push(NativeBatchSlot::Execute(AdmittedNativeCall {
                index,
                step: index,
                call_id: id,
                operation,
                input: call_input,
                ast,
                parallel_safe,
            }));
        }
        for result in execute_native_call_batch(ctx, slots).await {
            match result {
                NativeBatchResult::Fixed(block) => results.push(block),
                NativeBatchResult::Executed(call) => match call.result {
                    Ok(result) => {
                        results.push(ContentBlock::tool_result_text(call.call_id, result, false))
                    }
                    Err(error) => {
                        last_error = format!("`{}` gather call failed: {error}", call.operation);
                        results.push(ContentBlock::tool_result_text(
                            call.call_id,
                            last_error.clone(),
                            true,
                        ));
                    }
                },
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
            Some(PendingResponse::Routing { candidates }) => {
                let specs = live_visible_specs(ctx);
                let families = build_families(&specs, &ctx.groups, &ctx.advertised);
                let ambient = ambient_specs(&specs, &ctx.groups);
                let live_candidates = candidates
                    .iter()
                    .filter(|candidate| families.contains_key(candidate.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                if live_candidates.is_empty() {
                    return Err(Error::Other(
                        "explore: every integration offered by the routing decision is no longer wired and visible"
                            .into(),
                    ));
                }
                let Some(chosen) =
                    resolve_routing_choice(&decision, &live_candidates, &ctx.groups, &families)
                else {
                    state.pending = Some(PendingResponse::Routing {
                        candidates: live_candidates.clone(),
                    });
                    return adaptive_result(
                        "decision",
                        &state,
                        json!({"question": routing_question(&live_candidates, &families, true)}),
                    );
                };
                state.declaration.families = vec![chosen.clone()];
                state.selected = selected_specs(&state.declaration, &families, &ambient)?
                    .into_iter()
                    .map(|spec| spec.name)
                    .collect();
                state.messages.push(Message::user_text(format!(
                    "The user selected integration family `{chosen}`: {decision}"
                )));
                observe(
                    ctx,
                    "turn.intent",
                    json!({
                        "intent": &state.declaration.intent,
                        "families": &state.declaration.families,
                        "operations": &state.selected,
                        "routing_decision": true,
                    }),
                );
            }
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

    observe(
        ctx,
        "loop.phase",
        json!({"phase": if ctx.authored_ceiling.is_some() { "segment" } else { "explore" }}),
    );
    adaptive_explore(ctx, state, usages).await
}

fn latest_user_input(ctx: &StagedContext) -> String {
    ctx.conversation
        .iter()
        .rev()
        .find(|message| message.role == flux_core::Role::User)
        .map(Message::text)
        .unwrap_or_default()
}

fn matched_routing_families(
    ctx: &StagedContext,
    families: &BTreeMap<String, Family>,
) -> Vec<flux_evidence::IntentGroupMatch> {
    flux_evidence::matching_turn_intent_groups(&ctx.groups, &latest_user_input(ctx))
        .into_iter()
        .filter(|matched| families.contains_key(&matched.group))
        .collect()
}

fn routing_question(
    candidates: &[String],
    families: &BTreeMap<String, Family>,
    retry: bool,
) -> DecisionRequest {
    let options = candidates
        .iter()
        .map(|candidate| {
            families
                .get(candidate)
                .map(|family| format!("{} — {}", family.name, family.description))
                .unwrap_or_else(|| candidate.clone())
        })
        .collect();
    DecisionRequest {
        prompt: if retry {
            "That did not identify one of the live integration families. Which integration should Flux use?"
                .into()
        } else {
            "The request matches more than one live integration. Which integration should Flux use?"
                .into()
        },
        options,
    }
}

fn resolve_routing_choice(
    decision: &str,
    candidates: &[String],
    groups: &[ToolGroup],
    families: &BTreeMap<String, Family>,
) -> Option<String> {
    let trimmed = decision.trim();
    if let Ok(index) = trimmed.parse::<usize>() {
        if (1..=candidates.len()).contains(&index) {
            return Some(candidates[index - 1].clone());
        }
    }
    if let Some(exact) = candidates
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(trimmed))
    {
        return Some(exact.clone());
    }
    let matched = flux_evidence::matching_turn_intent_groups(groups, trimmed)
        .into_iter()
        .map(|matched| matched.group)
        .filter(|group| candidates.contains(group) && families.contains_key(group))
        .collect::<std::collections::BTreeSet<_>>();
    (matched.len() == 1).then(|| matched.into_iter().next().unwrap())
}

/// A-94: drain the surface-shared steering queue and inject the drained texts into the adaptive
/// conversation before the next planner consultation. The block is appended to a trailing user
/// message when one exists (a gather/report `tool_result` round) so the conversation never grows
/// a consecutive-user pair; otherwise it becomes its own user message. Consumed steering is
/// recorded as a durable `turn.steering` observation — deliberately NOT an `EventKind::Message`,
/// which would break the session log's strict user → assistant alternation.
fn inject_steering(ctx: &StagedContext, state: &mut AdaptiveState) {
    let Some(queue) = &ctx.steering else {
        return;
    };
    let texts = queue.drain();
    if texts.is_empty() {
        return;
    }
    observe(ctx, "turn.steering", json!({ "messages": &texts }));
    let block = steering_block(&texts);
    match state.messages.last_mut() {
        Some(last) if last.role == flux_core::Role::User => {
            last.content.push(ContentBlock::text(block));
        }
        _ => state.messages.push(Message::user_text(block)),
    }
}

/// Render drained steering texts as one attributed block, preserving submission order.
fn steering_block(texts: &[String]) -> String {
    let mut block = String::from(
        "<user-steering>\nThe user sent this guidance while the turn was executing. Honor it from \
         this point on; it refines the original request, and results above were produced before \
         it arrived.\n",
    );
    for text in texts {
        block.push_str("- ");
        block.push_str(text);
        block.push('\n');
    }
    block.push_str("</user-steering>");
    block
}

async fn adaptive_explore(
    ctx: &StagedContext,
    mut state: AdaptiveState,
    usages: &mut Vec<Usage>,
) -> Result<Value> {
    // The `ai_segment` entry point seeds exploration without going through `detect_intent_inner`, so
    // re-assert the control-tool reservation here to cover every path into the exploration stage.
    ensure_control_names_free(ctx)?;
    let round_limit = model_call_limit(ctx);
    // `specs`/`families` derive only from the immutable `ctx` (registry snapshot, groups, advertised).
    // A capability signal mutates `state`, never these, so the full family ceiling is loop-invariant:
    // compute it once instead of rebuilding a sorted spec clone + family map on every round.
    let specs = live_visible_specs(ctx);
    let families = build_families(&specs, &ctx.groups, &ctx.advertised);
    let ambient = ambient_specs(&specs, &ctx.groups);
    let authored_segment = ctx.authored_ceiling.is_some();
    let model_stage = if authored_segment {
        "segment"
    } else {
        "explore"
    };
    let history_limit = ctx
        .adaptive_policy
        .max_history_bytes
        .unwrap_or(ADAPTIVE_HISTORY_LIMIT);
    for _round in 1..=round_limit {
        ensure_stage_budget(ctx, usages)?;
        ensure_model_call_budget(ctx, state.intent_calls, state.explore_calls, "explore")?;
        inject_steering(ctx, &mut state);
        let selected = selected_specs_for_state(&state, &families, &ambient, ctx)?;
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
        let mut req = adaptive_request(
            ctx,
            &ctx.adaptive_policy.explore,
            state.messages.clone(),
            if authored_segment {
                ctx.opts.max_tokens
            } else {
                ctx.opts.max_tokens.min(8_192)
            },
        );
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
        let mut history_bytes = adaptive_history_bytes(&req.messages);
        let system_text = req.system_text();
        let mut request_bytes = adaptive_request_bytes(
            system_text.as_deref().unwrap_or_default(),
            &req.messages,
            &req.tools,
        );
        if let Some(diagnostic) = adaptive_budget_diagnostic(
            if authored_segment {
                "segment_history"
            } else {
                "adaptive_history"
            },
            history_bytes,
            history_limit,
            None,
        ) {
            observe(ctx, "turn.budget", diagnostic);
        }
        // An authored segment reaching the ceiling is a retention problem, not a fatal one: the loop
        // may already have committed. Shed the oldest tool-result payloads and keep going.
        if authored_segment && history_bytes > history_limit {
            if let Some(elision) = elide_segment_history(&mut state.messages, history_limit) {
                observe(
                    ctx,
                    "turn.budget",
                    json!({
                        "budget": "segment_history",
                        "status": "elided",
                        "actual_bytes": elision.before_bytes,
                        "limit_bytes": history_limit,
                        "resulting_bytes": elision.after_bytes,
                        "elided_results": elision.elided_results,
                    }),
                );
                req.messages = state.messages.clone();
                history_bytes = adaptive_history_bytes(&req.messages);
                request_bytes = adaptive_request_bytes(
                    system_text.as_deref().unwrap_or_default(),
                    &req.messages,
                    &req.tools,
                );
            }
        }
        if history_bytes > history_limit {
            // Elision could not free enough. End the segment as a *result* carrying the evidence
            // ledger, not as an error that discards it along with any commit already made.
            if authored_segment {
                observe(
                    ctx,
                    "turn.budget",
                    json!({
                        "budget": "segment_history",
                        "status": "terminated",
                        "actual_bytes": history_bytes,
                        "limit_bytes": history_limit,
                    }),
                );
                return adaptive_result(
                    "chat",
                    &state,
                    json!({"text": segment_history_summary(&state, history_bytes, history_limit)}),
                );
            }
            return Err(Error::Other(adaptive_budget_refusal(
                "history",
                history_bytes,
                history_limit,
            )));
        }
        if let Some(diagnostic) = adaptive_budget_diagnostic(
            if authored_segment {
                "segment_request"
            } else {
                "adaptive_request"
            },
            request_bytes.total_bytes,
            ADAPTIVE_REQUEST_LIMIT,
            Some(request_bytes),
        ) {
            observe(ctx, "turn.budget", diagnostic);
        }
        if request_bytes.total_bytes > ADAPTIVE_REQUEST_LIMIT {
            return Err(Error::Other(if authored_segment {
                format!(
                    "authored segment request budget exceeded: actual_bytes={} limit_bytes={ADAPTIVE_REQUEST_LIMIT}",
                    request_bytes.total_bytes
                )
            } else {
                adaptive_budget_refusal(
                    "request",
                    request_bytes.total_bytes,
                    ADAPTIVE_REQUEST_LIMIT,
                )
            }));
        }
        let repair_attempt = state.explore_calls;
        correlate_request(ctx, &mut req, model_stage, state.explore_calls + 1);
        let request_model = req.model.clone();
        let (result, usage, metrics) =
            consult_model(ctx.provider.as_ref(), ctx.sink.clone(), req).await;
        state.explore_calls += 1;
        observe_model_call(
            ctx,
            ModelCallObservation {
                stage: model_stage,
                round: state.explore_calls,
                repair_attempt,
                model: &request_model,
                usage: &usage,
                metrics: &metrics,
                ok: result.is_ok(),
            },
        );
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
                "{} is incomplete: {}.",
                if authored_segment {
                    "The authored model segment"
                } else {
                    "Adaptive exploration"
                },
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
            let added = apply_capability_signal(&mut state, input, &families, &ambient)?;
            observe(
                ctx,
                "turn.capability_signal",
                json!({
                    "families": &state.declaration.families,
                    "new_operations": &added,
                    "reason": input.get("reason").and_then(Value::as_str).unwrap_or_default(),
                }),
            );
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
        let mut slots = Vec::with_capacity(calls.len());
        for (call_index, (id, name, input)) in calls.into_iter().enumerate() {
            state.native_step += 1;
            let step = state.native_step;
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
                    json!({
                        "native_operation": name,
                        "disposition": "rejected",
                        "step": step,
                        "call_index": call_index,
                        "call_id": id,
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    state.last_error.clone(),
                    true,
                )));
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
                observe(
                    ctx,
                    "adaptive.call",
                    json!({
                        "operation": operation,
                        "disposition": "rejected",
                        "step": step,
                        "call_index": call_index,
                        "call_id": id,
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    state.last_error.clone(),
                    true,
                )));
                continue;
            }
            if let Err(diags) = validate_call(spec, &input, ctx, &selected_names) {
                state.last_error = format!("invalid `{operation}` input: {}", diags.join("; "));
                observe(
                    ctx,
                    "adaptive.call",
                    json!({
                        "operation": operation,
                        "disposition": "rejected",
                        "step": step,
                        "call_index": call_index,
                        "call_id": id,
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    state.last_error.clone(),
                    true,
                )));
                continue;
            }

            let tool = ctx.registry.get(&operation).ok_or_else(|| {
                Error::Other(format!(
                    "selected operation `{operation}` disappeared from the registry"
                ))
            })?;
            if gather_safe(
                spec,
                tool.staging_disposition(),
                tool.intents(&input),
                &tool.semantic_effects(),
            ) {
                let parallel_safe = native_call_parallel_safe(ctx, spec, tool.as_ref(), &input);
                let ast =
                    one_call_ast(&operation, input.clone(), format!("adaptive_gather_{step}"));
                record_host_flow(ctx, &ast, "gather", step as u32);
                record_batch_admission(
                    ctx,
                    "native.batch_call",
                    json!({
                        "operation": operation,
                        "disposition": "gather",
                        "schedule": if parallel_safe { "parallel" } else { "ordered" },
                        "step": step,
                        "call_index": call_index,
                        "call_id": id,
                    }),
                );
                slots.push(NativeBatchSlot::Execute(AdmittedNativeCall {
                    index: call_index,
                    step,
                    call_id: id,
                    operation,
                    input,
                    ast,
                    parallel_safe,
                }));
            } else {
                state.proposed.push(ProposedCall {
                    op: operation.clone(),
                    input,
                });
                observe(
                    ctx,
                    "adaptive.call",
                    json!({
                        "operation": operation,
                        "disposition": "captured",
                        "step": step,
                        "call_index": call_index,
                        "call_id": id,
                    }),
                );
                slots.push(NativeBatchSlot::Fixed(ContentBlock::tool_result_text(
                    id,
                    format!(
                        "captured as proposed action {}; not executed. Continue gathering or call `{FINALIZE_PLAN}` by itself when the batch is complete.",
                        state.proposed.len()
                    ),
                    false,
                )));
            }
        }
        for result in execute_native_call_batch(ctx, slots).await {
            match result {
                NativeBatchResult::Fixed(block) => results.push(block),
                NativeBatchResult::Executed(call) => match call.result {
                    Ok(result) => {
                        let redactor = &ctx.executor.context().redactor;
                        observe(
                            ctx,
                            "adaptive.call",
                            json!({
                                "operation": &call.operation,
                                "disposition": "gather",
                                "step": call.step,
                                "call_index": call.index,
                                "call_id": &call.call_id,
                            }),
                        );
                        // Redact before hashing or retaining the result so omission metadata cannot
                        // become a stable fingerprint of an unredacted secret.
                        let result = redactor.redact(&result);
                        let (result, budget) = bound_adaptive_tool_result(result);
                        if let Some(budget) = budget {
                            observe(
                                ctx,
                                "turn.budget",
                                json!({
                                    "operation": &call.operation,
                                    "budget": "tool_result",
                                    "status": "exceeded",
                                    "actual_bytes": budget.actual_bytes,
                                    "limit_bytes": ADAPTIVE_TOOL_RESULT_LIMIT,
                                    "sha256": budget.sha256,
                                }),
                            );
                        }
                        state.gathered.push(GatheredEvidence {
                            op: call.operation,
                            input: redactor.redact(&call.input.to_string()),
                            result: result.clone(),
                        });
                        results.push(ContentBlock::tool_result_text(call.call_id, result, false));
                    }
                    Err(error) => {
                        state.last_error =
                            format!("`{}` gather call failed: {error}", call.operation);
                        observe(
                            ctx,
                            "adaptive.call_failed",
                            json!({
                                "operation": call.operation,
                                "call_index": call.index,
                                "call_id": call.call_id,
                            }),
                        );
                        results.push(ContentBlock::tool_result_text(
                            call.call_id,
                            state.last_error.clone(),
                            true,
                        ));
                    }
                },
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
            || effective_group(spec, groups) == Some(ADAPTIVE_AMBIENT_GROUP)
            || effective_group(spec, groups) == Some(flux_runtime::REFLECT_GROUP)
        {
            continue;
        }

        let physical = effective_group(spec, groups);
        let (name, description, discoverable, exhaustive_members, routing_signals) = match physical
        {
            Some(name) => {
                let manifest = groups.iter().find(|g| g.name == name);
                let semantic = manifest
                    .is_some_and(|g| g.surface_when.iter().any(|m| m.kind == KIND_TURN_INTENT));
                let active = advertised.contains(&spec.name);
                let description = manifest
                    .map(|g| g.description.clone())
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| format!("Registered `{name}` operations."));
                let mut routing_signals = manifest
                    .into_iter()
                    .flat_map(|group| &group.surface_when)
                    .filter(|matcher| matcher.kind == KIND_TURN_INTENT)
                    .filter_map(|matcher| matcher.signal.clone())
                    .collect::<Vec<_>>();
                routing_signals.sort();
                routing_signals.dedup();
                (
                    name.to_string(),
                    description,
                    semantic || active,
                    false,
                    routing_signals,
                )
            }
            None => {
                let name = virtual_family(spec);
                (
                    name.to_string(),
                    virtual_description(name).to_string(),
                    advertised.contains(&spec.name),
                    true,
                    Vec::new(),
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
            exhaustive_members,
            routing_signals,
        });
        family.specs.push(spec.clone());
    }
    for family in out.values_mut() {
        family.specs.sort_by(|a, b| a.name.cmp(&b.name));
    }
    out
}

/// Ambient specs are rebuilt from the same live, permission-filtered registry snapshot as normal
/// families. They never enter the router's family index, but they count in every selected native
/// operation/schema union because the provider receives them on every exploration round.
fn ambient_specs(specs: &[ToolSpec], groups: &[ToolGroup]) -> Vec<ToolSpec> {
    let mut ambient = specs
        .iter()
        .filter(|spec| effective_group(spec, groups) == Some(ADAPTIVE_AMBIENT_GROUP))
        .cloned()
        .collect::<Vec<_>>();
    ambient.sort_by(|a, b| a.name.cmp(&b.name));
    ambient
}

fn virtual_family(spec: &ToolSpec) -> &'static str {
    if spec.effects.contains(&Effect::Write) {
        "workspace.write"
    } else if spec.effects.contains(&Effect::Process)
        || spec.effects.contains(&Effect::LocalSystem)
        || spec.access.contains(&AccessKind::Process)
        || spec.access.contains(&AccessKind::Connection)
        || spec.access.contains(&AccessKind::LocalSystem)
    {
        "process"
    } else if spec.access.contains(&AccessKind::Provider) {
        "model"
    } else if (spec.effects.contains(&Effect::Network) && spec.effects.contains(&Effect::Read))
        || spec.access.contains(&AccessKind::Datasource)
    {
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
        let member_limit = if family.exhaustive_members {
            usize::MAX
        } else {
            8
        };
        let sample = family
            .specs
            .iter()
            .take(member_limit)
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let member_label = if family.exhaustive_members {
            "operations"
        } else {
            "e.g."
        };
        let routing = if family.routing_signals.is_empty() {
            String::new()
        } else {
            format!(
                " Routing hints: {}.",
                family
                    .routing_signals
                    .iter()
                    .take(8)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        out.push_str(&format!(
            "- {} ({} operation{}; {member_label} {}): {}{routing}\n",
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
        description: "Declare the user's task contract and the smallest registered capability families needed. This signal guides planning and narrows visibility; it grants no authority.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "intent": {
                    "type": "string",
                    "description": "A concise description of the request in capability-neutral language."
                },
                "task_kind": {
                    "type": "string",
                    "enum": ["conversation", "answer", "investigate", "review", "change", "operate", "mixed"],
                    "description": "The request's top-level task shape."
                },
                "effect_mode": {
                    "type": "string",
                    "enum": ["none", "read_only", "propose_only", "apply_changes", "perform_operation"],
                    "description": "The effects the user asked Flux to perform. Descriptive only; never authority."
                },
                "deliverable": {
                    "type": "string",
                    "maxLength": MAX_DELIVERABLE_CHARS,
                    "description": "What the final response or completed work must deliver."
                },
                "constraints": {
                    "type": "array",
                    "items": {"type": "string", "maxLength": MAX_INTENT_LIST_ITEM_CHARS},
                    "maxItems": MAX_INTENT_LIST_ITEMS,
                    "description": "Explicit user constraints; empty when none."
                },
                "uncertainties": {
                    "type": "array",
                    "items": {"type": "string", "maxLength": MAX_INTENT_LIST_ITEM_CHARS},
                    "maxItems": MAX_INTENT_LIST_ITEMS,
                    "description": "Material ambiguities requiring evidence or a user decision; empty when none."
                },
                "capability_families": {
                    "type": "array",
                    "items": {"type": "string", "enum": names},
                    "maxItems": MAX_FAMILIES,
                    "description": "The smallest set of registered families needed; empty for pure conversation."
                }
            },
            "required": [
                "intent", "task_kind", "effect_mode", "deliverable", "constraints",
                "uncertainties", "capability_families"
            ],
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

    for attempt in 0..MAX_INTENT_ATTEMPTS {
        ensure_stage_budget(ctx, usages)?;
        ensure_model_call_budget(ctx, usages.len(), 0, "intent")?;
        let mut req = adaptive_request(ctx, &ctx.adaptive_policy.intent, messages.clone(), 1_024);
        req.system_segments = intent_segments(ctx, family_index(families));
        req.tools = vec![intent_tool(families)];
        correlate_request(ctx, &mut req, "intent", attempt + 1);
        let request_model = req.model.clone();
        let (result, usage, metrics) =
            consult_model(ctx.provider.as_ref(), ctx.sink.clone(), req).await;
        observe_model_call(
            ctx,
            ModelCallObservation {
                stage: "intent",
                round: attempt + 1,
                repair_attempt: attempt,
                model: &request_model,
                usage: &usage,
                metrics: &metrics,
                ok: result.is_ok(),
            },
        );
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

        // This is a host-owned routing control signal, not a registered runtime operation: the
        // protocol requires exactly one declaration and deliberately rejects batches. Keep it out
        // of native-call scheduling and the Executor dispatch path.
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
    let known = [
        "intent",
        "task_kind",
        "effect_mode",
        "deliverable",
        "constraints",
        "uncertainties",
        "capability_families",
    ];
    let unknown = obj
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(format!("unknown field(s): {}", unknown.join(", ")));
    }
    let intent = required_intent_string(obj, "intent", None)?;
    let task_kind = match obj.get("task_kind") {
        Some(_) => required_intent_enum(
            obj,
            "task_kind",
            &[
                "conversation",
                "answer",
                "investigate",
                "review",
                "change",
                "operate",
                "mixed",
            ],
        )?,
        None => "mixed".into(),
    };
    let effect_mode = match obj.get("effect_mode") {
        Some(_) => required_intent_enum(
            obj,
            "effect_mode",
            &[
                "none",
                "read_only",
                "propose_only",
                "apply_changes",
                "perform_operation",
            ],
        )?,
        None => "read_only".into(),
    };
    let deliverable = match obj.get("deliverable") {
        Some(_) => required_intent_string(obj, "deliverable", Some(MAX_DELIVERABLE_CHARS))?,
        None => intent.clone(),
    };
    let constraints = match obj.get("constraints") {
        Some(_) => required_intent_list(obj, "constraints")?,
        None => Vec::new(),
    };
    let uncertainties = match obj.get("uncertainties") {
        Some(_) => required_intent_list(obj, "uncertainties")?,
        None => Vec::new(),
    };

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
        task_kind: Some(task_kind),
        effect_mode: Some(effect_mode),
        deliverable: Some(deliverable),
        constraints,
        uncertainties,
        families: selected,
        // A model-declared routing selection; the router cap applies.
        scoped: false,
    })
}

fn required_intent_string(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    max_chars: Option<usize>,
) -> std::result::Result<String, String> {
    let value = obj
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("`{field}` must be a non-empty string"))?;
    if max_chars.is_some_and(|max| value.chars().count() > max) {
        return Err(format!(
            "`{field}` exceeds the {}-character maximum",
            max_chars.expect("checked as some")
        ));
    }
    Ok(value.to_string())
}

fn required_intent_enum(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> std::result::Result<String, String> {
    let value = obj
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{field}` must be a string"))?;
    if !allowed.contains(&value) {
        return Err(format!("`{field}` must be one of: {}", allowed.join(", ")));
    }
    Ok(value.to_string())
}

fn required_intent_list(
    obj: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<Vec<String>, String> {
    let values = obj
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{field}` must be an array"))?;
    if values.len() > MAX_INTENT_LIST_ITEMS {
        return Err(format!(
            "`{field}` has {} items; the maximum is {MAX_INTENT_LIST_ITEMS}",
            values.len()
        ));
    }
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("every `{field}` item must be a non-empty string"))?;
            if value.chars().count() > MAX_INTENT_LIST_ITEM_CHARS {
                return Err(format!(
                    "a `{field}` item exceeds the {MAX_INTENT_LIST_ITEM_CHARS}-character maximum"
                ));
            }
            Ok(value.to_string())
        })
        .collect()
}

fn selected_specs(
    declaration: &IntentDeclaration,
    families: &BTreeMap<String, Family>,
    ambient: &[ToolSpec],
) -> Result<Vec<ToolSpec>> {
    // `MAX_FAMILIES` bounds what the intent router's model call may select. An authored ceiling was
    // fixed by the operator before any model ran, so narrowing it would silently drop capability the
    // author named explicitly. The operation/schema budgets below still bound both paths.
    if !declaration.scoped {
        let distinct_family_count = declaration
            .families
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>()
            .len();
        if distinct_family_count > MAX_FAMILIES {
            return Err(Error::Other(format!(
                "adaptive capability declaration selected {distinct_family_count} distinct families; the maximum is {MAX_FAMILIES}"
            )));
        }
    }
    let mut selected: BTreeMap<String, ToolSpec> = ambient
        .iter()
        .map(|spec| (spec.name.clone(), spec.clone()))
        .collect();
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
    ambient: &[ToolSpec],
    ctx: &StagedContext,
) -> Result<Vec<ToolSpec>> {
    // Re-expand the accumulated family signals from the LIVE registry on every stage call. This
    // keeps wiring/policy/tool-subset changes fail-closed instead of trusting serialized operation
    // names from an earlier turn.
    let unavailable_families = state
        .declaration
        .families
        .iter()
        .filter(|name| !families.contains_key(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unavailable_families.is_empty() {
        return Err(stale_capability_state_error(
            state,
            families,
            ctx,
            Some(&unavailable_families),
        ));
    }
    let selected = selected_specs(&state.declaration, families, ambient)?;
    let names = selected
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<HashSet<_>>();
    if state
        .selected
        .iter()
        .any(|name| !names.contains(name.as_str()))
    {
        return Err(stale_capability_state_error(state, families, ctx, None));
    }
    Ok(selected)
}

/// Snapshot the hard operation ceiling live at this stage boundary. The registry already reflects
/// the agent's configured tool subset; `operation_visible` adds bare-deny and active `with_tools`
/// enforcement. Semantic signals may expand turn-local visibility only inside this set.
fn live_visible_specs(ctx: &StagedContext) -> Vec<ToolSpec> {
    ctx.registry
        .specs()
        .into_iter()
        .filter(|spec| {
            ctx.executor.operation_visible(&spec.name)
                && ctx
                    .authored_ceiling
                    .as_ref()
                    .is_none_or(|ceiling| ceiling.contains(&spec.name))
        })
        .collect()
}

fn stale_capability_state_error(
    state: &AdaptiveState,
    families: &BTreeMap<String, Family>,
    ctx: &StagedContext,
    unavailable_families: Option<&[String]>,
) -> Error {
    let mut selected_live = state
        .declaration
        .families
        .iter()
        .filter_map(|name| families.get(name))
        .flat_map(|family| family.specs.iter().map(|spec| spec.name.clone()))
        .collect::<HashSet<_>>();
    selected_live.extend(
        ambient_specs(&live_visible_specs(ctx), &ctx.groups)
            .into_iter()
            .map(|spec| spec.name),
    );
    let mut unavailable = state
        .selected
        .iter()
        .filter(|name| !selected_live.contains(name.as_str()))
        .map(|name| format!("`{name}` ({})", operation_unavailable_reason(ctx, name)))
        .collect::<Vec<_>>();
    unavailable.sort();
    let family_detail = unavailable_families
        .filter(|families| !families.is_empty())
        .map(|families| format!("; unavailable families: {}", families.join(", ")))
        .unwrap_or_default();
    Error::Other(format!(
        "adaptive capability state is no longer valid{family_detail}; unavailable operations: {}",
        if unavailable.is_empty() {
            "(none recorded)".into()
        } else {
            unavailable.join(", ")
        }
    ))
}

fn operation_unavailable_reason(ctx: &StagedContext, name: &str) -> &'static str {
    if ctx.registry.get(name).is_none() {
        return "not registered";
    }
    if ctx
        .authored_ceiling
        .as_ref()
        .is_some_and(|ceiling| !ceiling.contains(name))
    {
        return "outside the authored stage tool ceiling";
    }
    if ctx
        .executor
        .active_cap_scope()
        .is_some_and(|scope| !scope.iter().any(|allowed| allowed == name))
    {
        return "outside the active with_tools scope";
    }
    if !ctx.executor.operation_visible(name) {
        return "denied by operation permissions";
    }
    "no longer wired to a selected discoverable family"
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
        description: format!(
            "Signal additional registered capability families justified by facts discovered during exploration. This only changes tool visibility; it neither calls an operation nor grants authority. The accumulated active set may contain at most {MAX_FAMILIES} families. Call by itself."
        ),
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
    ambient: &[ToolSpec],
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
    if new_families.len() > MAX_FAMILIES {
        return Err(Error::Other(format!(
            "`{SIGNAL_CAPABILITIES}` would select {} accumulated families; the maximum is {MAX_FAMILIES}",
            new_families.len()
        )));
    }
    // A-95: a signal that widens nothing must change nothing. The model re-signalling a family it
    // already holds used to still rewrite the declaration — appending to `intent` and so rewriting
    // the trailing system segment — which churns the prompt prefix for no capability gain. On the
    // Anthropic wire that segment rides after the last breakpoint so the damage is bounded, but on
    // the Responses wire it is flattened into `instructions` at the very FRONT of the cached prefix
    // (C-137), where it invalidates everything behind it. Either way it buys nothing: no new
    // operation is surfaced, and `intent` grows without bound across a signal-heavy turn.
    if new_families == state.declaration.families {
        return Ok(Vec::new());
    }
    let declaration = IntentDeclaration {
        intent: format!("{}; additional signal: {reason}", state.declaration.intent),
        families: new_families,
        ..state.declaration.clone()
    };
    let selected = selected_specs(&declaration, families, ambient)?;
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
        // `selected` was rebuilt from `live_visible_specs` in this native round. Requiring the
        // immutable turn-start surface as well would incorrectly discard a semantic family that
        // exploration added later in this same turn.
        if !selected.contains(&action.op) {
            return Err(Error::Other(format!(
                "action `{}` is outside the intent-selected capability ceiling",
                action.op
            )));
        }
        let spec = ctx
            .registry
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
    // C-134: every staged call re-sends the turn's growing transcript, so the conversation tail is
    // where the cache pays. Codecs without a breakpoint notion ignore the flag.
    //
    // `FLUX_CACHE_TAIL=off` is the kill switch (mirrors `FLUX_OP_CACHE=off`): it is the A/B lever
    // that measures what the tail breakpoint is worth, and the escape hatch if a provider ever
    // rejects the extra breakpoint.
    req.cache_tail = !cache_tail_disabled();
    req
}

/// Whether `FLUX_CACHE_TAIL=off` has switched the conversation-tail breakpoint off (C-134).
fn cache_tail_disabled() -> bool {
    std::env::var("FLUX_CACHE_TAIL").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

fn correlate_request(ctx: &StagedContext, request: &mut Request, stage: &str, round: usize) {
    request.trace = Some(RequestTrace {
        session_id: ctx.session_id.clone(),
        turn_id: ctx.audit.as_ref().map(|(_, turn_id)| *turn_id).unwrap_or(0),
        stage: stage.to_string(),
        round,
    });
}

struct ModelCallObservation<'a> {
    stage: &'a str,
    round: usize,
    repair_attempt: usize,
    model: &'a str,
    usage: &'a Usage,
    metrics: &'a ModelCallMetrics,
    ok: bool,
}

fn observe_model_call(ctx: &StagedContext, call: ModelCallObservation<'_>) {
    let ModelCallObservation {
        stage,
        round,
        repair_attempt,
        model,
        usage,
        metrics,
        ok,
    } = call;
    observe(
        ctx,
        "model.call",
        json!({
            "session_id": ctx.session_id,
            "turn_id": ctx.audit.as_ref().map(|(_, turn_id)| *turn_id),
            "stage": stage,
            "round": round,
            "repair_attempt": repair_attempt,
            "provider": ctx.provider.name(),
            "model": model,
            "ok": ok,
            "duration_us": metrics.duration_us,
            "ttft_us": metrics.ttft_us,
            "chunks": metrics.chunks,
            // C-181: connect-phase recovery this call paid for. Present (as 0) on every call so a
            // consumer never has to tell "no retries" apart from "an older log".
            "retries": metrics.retries,
            "oauth_refreshes": metrics.oauth_refreshes,
            "transport_fallbacks": metrics.transport_fallbacks,
            "system_bytes": metrics.system_bytes,
            "message_bytes": metrics.message_bytes,
            "message_count": metrics.message_count,
            "tool_result_count": metrics.tool_result_count,
            "tool_result_bytes": metrics.tool_result_bytes,
            "tool_use_bytes": metrics.tool_use_bytes,
            "text_bytes": metrics.text_bytes,
            "operations": metrics.operations,
            "schema_bytes": metrics.schema_bytes,
            "usage": usage,
        }),
    );
}

fn stage_model(ctx: &StagedContext, policy: &AgentStagePolicy) -> String {
    policy.model.clone().unwrap_or_else(|| ctx.model.clone())
}

fn adaptive_request(
    ctx: &StagedContext,
    policy: &AgentStagePolicy,
    messages: Vec<Message>,
    default_max_tokens: u32,
) -> Request {
    let mut req = base_request(
        ctx,
        messages,
        policy.max_tokens.unwrap_or(default_max_tokens),
    );
    req.model = stage_model(ctx, policy);
    req.effort = policy.effort.or(ctx.opts.effort);
    req
}

fn ensure_model_call_budget(
    ctx: &StagedContext,
    intent_calls: usize,
    explore_calls: usize,
    stage: &str,
) -> Result<()> {
    let total = intent_calls.saturating_add(explore_calls);
    if total >= ctx.adaptive_policy.max_model_calls {
        return Err(Error::Other(format!(
            "adaptive model-call budget exhausted before `{stage}` ({total}/{} calls used in this logical run)",
            ctx.adaptive_policy.max_model_calls
        )));
    }
    let (used, cap) = match stage {
        "intent" => (intent_calls, ctx.adaptive_policy.intent.max_calls),
        "explore" => (explore_calls, ctx.adaptive_policy.explore.max_calls),
        _ => (total, None),
    };
    if let Some(cap) = cap {
        if used >= cap {
            return Err(Error::Other(format!(
                "adaptive `{stage}` model-call cap exhausted ({used}/{cap} calls used in this logical run)"
            )));
        }
    }
    Ok(())
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

/// The adaptive loop's shared logical model-call budget, used by `adaptive_explore` as a round safety
/// bound above [`ensure_model_call_budget`] (which is the real, resume-aware ceiling). Named
/// `[agent.stages]` stages use their own [`MODEL_STAGE_MAX_ROUNDS`] instead, so this stays scoped to
/// the adaptive intent/explore run.
fn model_call_limit(ctx: &StagedContext) -> usize {
    ctx.adaptive_policy.max_model_calls
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

fn intent_contract_json(declaration: &IntentDeclaration) -> Value {
    json!({
        "intent": declaration.intent,
        "task_kind": declaration.task_kind,
        "effect_mode": declaration.effect_mode,
        "deliverable": declaration.deliverable,
        "constraints": declaration.constraints,
        "uncertainties": declaration.uncertainties,
        "families": declaration.families,
    })
}

fn explore_segments(ctx: &StagedContext, declaration: &IntentDeclaration) -> Vec<SystemSegment> {
    // An authored segment executes; the adaptive planner proposes. Same signal `adaptive_explore`
    // uses for `authored_segment`.
    let mut segments = vec![SystemSegment {
        text: if ctx.authored_ceiling.is_some() {
            SEGMENT_SYSTEM.into()
        } else {
            EXPLORE_SYSTEM.to_string()
        },
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
            "Accepted intent contract: {}\nSelected capability families: {}",
            intent_contract_json(declaration),
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

/// Whether this invocation may run during pre-approval evidence gathering.
///
/// `semantic_effects` is `flux_runtime::Tool::semantic_effects` for the tool being classified. It is
/// not optional context (C-210): an operation declares consequence through two channels, and the
/// effect set is only one of them. `web.fetch` wired with a record sink persists a durable
/// `web.page` datasource record and says so as the semantic effect `write_db` — which lowers to
/// `flow.write_db`, one of exactly two authorities the default policy floor grants *without*
/// approval. Deciding what may run before a human looks while blind to that channel is the gap this
/// argument closes.
///
/// The spec-shape half stays the exact negation of `flux_spec::is_consequence_bearing_with_effects`,
/// which is the correspondence C-191's metadata invariants rest on. Change one, change both.
fn gather_safe(
    spec: &ToolSpec,
    disposition: flux_spec::StagingDisposition,
    intents: flux_spec::IntentSet,
    semantic_effects: &[String],
) -> bool {
    if disposition == flux_spec::StagingDisposition::Capture {
        return false;
    }
    if spec.risk != Risk::Low || intents.is_mutating() || intents.is_destructive() {
        return false;
    }
    // A self-declared consequential effect (`write_db`, `model`, `send_external`, `delete`, `money`)
    // disqualifies the call whatever the effect set says.
    if flux_spec::declares_consequential_effect(semantic_effects) {
        return false;
    }
    if spec.effects.is_empty() {
        // No declared effects does not mean inert: an operation that reaches a code-running or
        // local-system host capability can still act during exploration. Trust an empty effect set
        // as gather-safe only when the access set is equally inert (a pure op declares neither).
        return !spec.access.iter().any(|access| {
            matches!(
                access,
                AccessKind::Process | AccessKind::Connection | AccessKind::LocalSystem
            )
        });
    }
    let allowed = spec
        .effects
        .iter()
        .all(|effect| matches!(effect, Effect::Read | Effect::Filesystem | Effect::Network));
    let network_is_read =
        !spec.effects.contains(&Effect::Network) || spec.effects.contains(&Effect::Read);
    allowed && network_is_read
}

/// The stricter admission needed before two native calls may overlap.
///
/// `gather_safe` decides whether a call may execute before an action batch is approved. Parallel
/// admission additionally requires an idempotent operation and an authorize-only verdict proving
/// that the concrete call will not enter the approval path. Hooks are an ordering barrier because
/// authorize-only deliberately does not run them; an active cassette is a barrier because its tape
/// is ordered. Because the remaining admitted effect set is pure or read-only, two such calls have
/// no write/write or read/write conflict; calls whose declarations cannot prove that property stay
/// on the ordered path.
fn native_call_parallel_safe(
    ctx: &StagedContext,
    spec: &ToolSpec,
    tool: &dyn flux_runtime::Tool,
    input: &Value,
) -> bool {
    !ctx.executor.has_pre_tool_hooks()
        && ctx.store.cassette().is_none()
        && spec.idempotency == Idempotency::Idempotent
        && gather_safe(
            spec,
            tool.staging_disposition(),
            tool.intents(input),
            &tool.semantic_effects(),
        )
        && matches!(
            ctx.executor.authorize(&spec.name, input),
            AuthorizeVerdict::Allow
        )
}

async fn execute_admitted_native_call(
    ctx: &StagedContext,
    call: AdmittedNativeCall,
) -> NativeBatchResult {
    let outcome = {
        let mut sink = SharedSink::new(ctx.sink.clone());
        execute_flow_with_composites(
            ctx.store.as_ref(),
            ctx.executor.as_ref(),
            &ctx.session_id,
            &call.ast,
            &[],
            &mut sink,
        )
        .await
        .map(|outcome| outcome.result)
        .map_err(|error| error.to_string())
    };
    NativeBatchResult::Executed(ExecutedNativeCall {
        index: call.index,
        step: call.step,
        call_id: call.call_id,
        operation: call.operation,
        input: call.input,
        result: outcome,
    })
}

async fn flush_parallel_native_calls(
    ctx: &StagedContext,
    pending: &mut Vec<AdmittedNativeCall>,
) -> Vec<NativeBatchResult> {
    futures::future::join_all(
        std::mem::take(pending)
            .into_iter()
            .map(|call| execute_admitted_native_call(ctx, call)),
    )
    .await
}

/// Execute one provider-emitted native-call batch while preserving its result slots.
///
/// Consecutive calls that passed [`native_call_parallel_safe`] are polled together. Every fixed
/// refusal/capture and every approval-sensitive or otherwise unproven call is an ordering barrier.
/// `join_all` retains input order and does not spawn detached tasks: sibling failures remain local,
/// and dropping this future for turn cancellation drops every in-flight call.
async fn execute_native_call_batch(
    ctx: &StagedContext,
    slots: Vec<NativeBatchSlot>,
) -> Vec<NativeBatchResult> {
    let mut results = Vec::with_capacity(slots.len());
    let mut pending = Vec::new();
    for slot in slots {
        match slot {
            NativeBatchSlot::Execute(call) if call.parallel_safe => pending.push(call),
            NativeBatchSlot::Execute(call) => {
                results.extend(flush_parallel_native_calls(ctx, &mut pending).await);
                results.push(execute_admitted_native_call(ctx, call).await);
            }
            NativeBatchSlot::Fixed(block) => {
                results.extend(flush_parallel_native_calls(ctx, &mut pending).await);
                results.push(NativeBatchResult::Fixed(block));
            }
        }
    }
    results.extend(flush_parallel_native_calls(ctx, &mut pending).await);
    results
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
    let registry = OpRegistry::new(&ctx.registry).with_advertised(selected.clone());
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

/// Persist deterministic batch admission through the turn audit seam without pretending scheduler
/// admission is live progress. Tool start/result callbacks still surface in real execution order
/// through [`SharedSink`].
fn record_batch_admission(ctx: &StagedContext, kind: &str, data: Value) {
    let Some((events, turn_id)) = &ctx.audit else {
        return;
    };
    let observation = Observation::new(kind, EvidencePhase::Turn, data);
    let _ = events.record_observation(&ctx.session_id, *turn_id, &observation);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use flux_core::{Chunk, DispatchId, StopReason};
    use flux_events::EventStore;
    use flux_provider::{ChunkStream, Request};
    use flux_runtime::{
        AllowApprover, ApprovalChoice, Approver, HookOutcome, PermissionManager, PreToolHook,
        ResourceLimits, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::Idempotency;
    use flux_system::{System, Workspace};

    use super::*;

    static TEST_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn adaptive_tool_result_is_bounded_without_retaining_payload() {
        let payload_marker = "oversized-sensitive-tool-result";
        let oversized =
            payload_marker.repeat((ADAPTIVE_TOOL_RESULT_LIMIT / payload_marker.len()) + 2);
        let actual_bytes = oversized.len();
        let expected_digest = digest(&oversized);

        let (bounded, budget) = bound_adaptive_tool_result(oversized.clone());
        let budget = budget.expect("oversized result should report its budget");
        assert!(!bounded.contains(payload_marker));
        assert!(!bounded.contains(&oversized));
        assert!(bounded.len() < 256);
        assert!(bounded.contains(&actual_bytes.to_string()));
        assert!(bounded.contains(&ADAPTIVE_TOOL_RESULT_LIMIT.to_string()));
        assert!(bounded.contains(&expected_digest));
        assert_eq!(budget.actual_bytes, actual_bytes);
        assert_eq!(budget.sha256, expected_digest);

        let in_budget = "small tool result".to_string();
        let (unchanged, budget) = bound_adaptive_tool_result(in_budget.clone());
        assert_eq!(unchanged, in_budget);
        assert_eq!(budget, None);
    }

    #[test]
    fn adaptive_history_and_request_budgets_are_hard_and_payload_free() {
        let at_history_limit = "x".repeat(ADAPTIVE_HISTORY_LIMIT - 2);
        assert_eq!(
            adaptive_history_bytes(&at_history_limit),
            ADAPTIVE_HISTORY_LIMIT
        );
        assert_eq!(
            adaptive_budget_status(ADAPTIVE_HISTORY_LIMIT, ADAPTIVE_HISTORY_LIMIT),
            Some("approaching")
        );
        assert_eq!(
            adaptive_budget_status(ADAPTIVE_HISTORY_LIMIT + 1, ADAPTIVE_HISTORY_LIMIT),
            Some("exceeded")
        );

        let marker = "sensitive-input-marker";
        let messages = vec![marker];
        let tools = vec![json!({"name": "bounded"})];
        let request = adaptive_request_bytes("system", &messages, &tools);
        assert_eq!(request.system_bytes, "system".len());
        assert_eq!(
            request.message_bytes,
            serde_json::to_vec(&messages).unwrap().len()
        );
        assert_eq!(
            request.tool_bytes,
            serde_json::to_vec(&tools).unwrap().len()
        );
        assert_eq!(
            request.total_bytes,
            request
                .system_bytes
                .saturating_add(request.message_bytes)
                .saturating_add(request.tool_bytes)
        );
        assert_eq!(
            adaptive_budget_status(ADAPTIVE_REQUEST_LIMIT, ADAPTIVE_REQUEST_LIMIT),
            Some("approaching")
        );
        assert_eq!(
            adaptive_budget_status(ADAPTIVE_REQUEST_LIMIT + 1, ADAPTIVE_REQUEST_LIMIT),
            Some("exceeded")
        );

        let diagnostic = adaptive_budget_diagnostic(
            "adaptive_request",
            ADAPTIVE_REQUEST_LIMIT + 1,
            ADAPTIVE_REQUEST_LIMIT,
            Some(request),
        )
        .unwrap()
        .to_string();
        let refusal = adaptive_budget_refusal(
            "request",
            ADAPTIVE_REQUEST_LIMIT + 1,
            ADAPTIVE_REQUEST_LIMIT,
        );
        assert!(!diagnostic.contains(marker));
        assert!(!refusal.contains(marker));
        assert!(diagnostic.contains("system_bytes"));
        assert!(diagnostic.contains("message_bytes"));
        assert!(diagnostic.contains("tool_bytes"));

        let roots = ["flux", "connectors", "exchange"];
        let raw_marker = "continued-fleet-raw-result-marker";
        let raw_result = format!("{raw_marker}:{}", "x".repeat(201 * 1024));
        let raw_bytes = raw_result.len().saturating_mul(6);
        let mut continued_messages = vec![Message::user_text(
            "Continue the Fleet inspection across all configured repository roots.",
        )];

        for repair_attempt in 1..=6 {
            let root = roots[(repair_attempt - 1) % roots.len()];
            let tool_use_id = format!("continued-read-{repair_attempt}");
            continued_messages.push(Message::assistant(vec![ContentBlock::ToolUse {
                id: tool_use_id.clone(),
                name: "read_root".into(),
                input: json!({"root": root}),
            }]));
            let (bounded_result, omitted) = bound_adaptive_tool_result(raw_result.clone());
            assert!(omitted.is_some());
            continued_messages.push(Message::user(vec![ContentBlock::tool_result_text(
                tool_use_id,
                bounded_result,
                false,
            )]));
            continued_messages.push(Message::user_text(format!(
                "Repair attempt {repair_attempt}: continue the Fleet inspection for root {root} using only the bounded result summary."
            )));
        }

        let tools = (0..28)
            .map(|index| {
                json!({
                    "name": format!("continued_tool_{index}"),
                    "description": "Inspect one configured Fleet root.",
                    "input_schema": {
                        "type": "object",
                        "properties": {"root": {"type": "string"}},
                        "required": ["root"]
                    }
                })
            })
            .collect::<Vec<_>>();
        let seventh_request = adaptive_request_bytes(EXPLORE_SYSTEM, &continued_messages, &tools);
        let serialized_messages = serde_json::to_string(&continued_messages).unwrap();
        let seventh_round_diagnostic = adaptive_budget_diagnostic(
            "seventh_explore_request",
            seventh_request.total_bytes,
            seventh_request.total_bytes,
            Some(seventh_request),
        )
        .unwrap()
        .to_string();

        assert!(raw_bytes > 1_231_783);
        assert!(serialized_messages.len() < ADAPTIVE_HISTORY_LIMIT);
        assert!(serialized_messages.len() < raw_bytes / 10);
        assert!(seventh_request.total_bytes < ADAPTIVE_REQUEST_LIMIT);
        assert_eq!(tools.len(), 28);
        assert!(!serialized_messages.contains(raw_marker));
        assert!(!seventh_round_diagnostic.contains(raw_marker));
    }

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

    /// C-531: records every tool call/result the sink stream carries, shared with the test through
    /// `Arc`s so the recording survives the sink being moved into a [`StagedContext`].
    #[derive(Default, Clone)]
    struct PairingSink {
        calls: Arc<Mutex<Vec<(DispatchId, String, Value)>>>,
        results: Arc<Mutex<Vec<(DispatchId, String, String)>>>,
    }

    impl AgentSink for PairingSink {
        fn tool_call(&mut self, dispatch: DispatchId, name: &str, input: &Value) {
            self.calls
                .lock()
                .unwrap()
                .push((dispatch, name.to_string(), input.clone()));
        }

        fn tool_result(&mut self, dispatch: DispatchId, name: &str, result: &ToolResult) {
            self.results
                .lock()
                .unwrap()
                .push((dispatch, name.to_string(), result.content.clone()));
        }
    }

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

        fn tool_call(&mut self, _dispatch: DispatchId, name: &str, _input: &Value) {
            self.tools.push(name.to_string());
            self.events.push(format!("tool:{name}"));
        }

        fn tool_result(&mut self, _dispatch: DispatchId, name: &str, result: &ToolResult) {
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

    /// A provider that reports one connect-phase retry — exactly as `NativeProvider` does before
    /// backing off — and then answers normally. Lets the C-181 stage-side reporter be driven
    /// without standing up a flaky socket.
    struct RetryingProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
        fail: bool,
    }

    #[async_trait]
    impl Provider for RetryingProvider {
        fn name(&self) -> &str {
            "retrying"
        }

        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            // Through the constructor, as an external provider must (`RetryEvent` is
            // `#[non_exhaustive]`) — this test doubles as the downstream-usage proof.
            flux_provider::report_retry(flux_provider::RetryEvent::new(
                "retrying",
                request.model.clone(),
                1,
                6,
                std::time::Duration::from_millis(500),
                flux_provider::RetryReason::Status(429),
            ));
            if self.fail {
                return Err(Error::Other("exhausted".into()));
            }
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

    /// A-94: pushes steering messages into the shared queue immediately after serving the request
    /// at index `after` (0-based) — simulating a user typing while that round's tool calls run.
    struct SteerAfterProvider {
        inner: Arc<dyn Provider>,
        queue: Arc<crate::steering::SteeringQueue>,
        after: usize,
        texts: Mutex<Vec<String>>,
        served: AtomicU64,
    }

    #[async_trait]
    impl Provider for SteerAfterProvider {
        fn name(&self) -> &str {
            self.inner.name()
        }

        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            let stream = self.inner.stream(request).await;
            let index = self.served.fetch_add(1, Ordering::SeqCst) as usize;
            if index == self.after {
                for text in self.texts.lock().unwrap().drain(..) {
                    self.queue.push(text);
                }
            }
            stream
        }
    }

    struct CountingTool {
        spec: ToolSpec,
        result: String,
        calls: Arc<AtomicU64>,
    }

    struct NativeBatchBarrier {
        entered: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
        entered_notify: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }

    impl Default for NativeBatchBarrier {
        fn default() -> Self {
            Self {
                entered: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                entered_notify: tokio::sync::Notify::new(),
                release: tokio::sync::Semaphore::new(0),
            }
        }
    }

    impl NativeBatchBarrier {
        async fn wait_for_entered(&self, expected: usize) {
            loop {
                let notified = self.entered_notify.notified();
                if self.entered.load(Ordering::SeqCst) >= expected {
                    return;
                }
                notified.await;
            }
        }

        fn release(&self, permits: usize) {
            self.release.add_permits(permits);
        }
    }

    struct ActiveNativeCall<'a>(&'a AtomicUsize);

    impl Drop for ActiveNativeCall<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct BlockingGatherTool {
        spec: ToolSpec,
        barrier: Arc<NativeBatchBarrier>,
    }

    struct CountingDenyApprover(Arc<AtomicU64>);

    struct CountingContinueHook(Arc<AtomicUsize>);

    impl PreToolHook for CountingContinueHook {
        fn pre_tool(&self, _tool: &str, _input: &Value) -> HookOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            HookOutcome::Continue
        }
    }

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

    #[async_trait]
    impl Tool for BlockingGatherTool {
        fn spec(&self) -> ToolSpec {
            self.spec.clone()
        }

        async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
            self.barrier.entered.fetch_add(1, Ordering::SeqCst);
            let active = self.barrier.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _active = ActiveNativeCall(&self.barrier.active);
            self.barrier.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.entered_notify.notify_waiters();
            let permit = self
                .barrier
                .release
                .acquire()
                .await
                .map_err(|error| Error::Other(error.to_string()))?;
            permit.forget();
            if params["fail"].as_bool().unwrap_or(false) {
                return Err(Error::Other("fixture gather failure".into()));
            }
            Ok(ToolResult::ok(
                params["key"].as_str().unwrap_or_default().to_string(),
            ))
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
        let registry = executor.active_registry_snapshot();
        let context = StagedContext {
            provider,
            model: "test-model".into(),
            executor,
            registry,
            store: Arc::new(FlowStore::in_memory().unwrap()),
            session_id: "staged-test".into(),
            conversation: vec![Message::user_text("Inspect the fixture")],
            base_system: Some("Use only fixture evidence.".into()),
            sink: Arc::new(Mutex::new(NoopSink)),
            audit: None,
            advertised: HashSet::from(["inspect".into(), "change".into()]),
            authored_ceiling: None,
            groups: Vec::new(),
            opts: StageOptions::default(),
            remaining_token_budget: None,
            adaptive_policy: AdaptiveLoopPolicy::default(),
            steering: None,
        };
        TestHarness {
            context,
            requests,
            read_calls,
            write_calls,
            _root: temp,
        }
    }

    fn blocking_model_stage_context(
        responses: Vec<Vec<Chunk>>,
    ) -> (
        StagedContext,
        Arc<Mutex<Vec<Request>>>,
        Arc<NativeBatchBarrier>,
        TempRoot,
    ) {
        blocking_model_stage_context_with(responses, true, ResourceLimits::new(), Vec::new())
    }

    fn blocking_model_stage_context_with(
        responses: Vec<Vec<Chunk>>,
        allow_without_approval: bool,
        limits: ResourceLimits,
        hooks: Vec<Arc<dyn PreToolHook>>,
    ) -> (
        StagedContext,
        Arc<Mutex<Vec<Request>>>,
        Arc<NativeBatchBarrier>,
        TempRoot,
    ) {
        let operation = "blocking_inspect";
        let tool_spec = ToolSpec {
            description: "Inspect one fixture while the test controls completion".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "fail": {"type": "boolean"}
                },
                "required": ["key"],
                "additionalProperties": false
            }),
            ..spec(
                operation,
                vec![Effect::Read, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            )
        };
        let barrier = Arc::new(NativeBatchBarrier::default());
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(BlockingGatherTool {
            spec: tool_spec,
            barrier: barrier.clone(),
        }));
        let temp = TempRoot::new("flux-native-batch-test");
        let allow_rules = if allow_without_approval {
            vec![operation.into()]
        } else {
            Vec::new()
        };
        let executor = Arc::new(
            Executor::new(
                registry,
                PermissionManager::from_rules(&allow_rules, &[]),
                Arc::new(AllowApprover),
                ToolContext::new(Arc::new(System::new(Workspace::new(temp.path()).unwrap()))),
            )
            .with_hooks(hooks)
            .with_resource_limits(limits),
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(CaptureProvider {
            responses: Mutex::new(responses.into()),
            requests: requests.clone(),
        });
        let registry = executor.active_registry_snapshot();
        let context = StagedContext {
            provider,
            model: "test-model".into(),
            executor,
            registry,
            store: Arc::new(FlowStore::in_memory().unwrap()),
            session_id: "native-batch-test".into(),
            conversation: vec![Message::user_text("Inspect both fixture records")],
            base_system: None,
            sink: Arc::new(Mutex::new(NoopSink)),
            audit: None,
            advertised: HashSet::from([operation.into()]),
            authored_ceiling: None,
            groups: Vec::new(),
            opts: StageOptions::default(),
            remaining_token_budget: None,
            adaptive_policy: AdaptiveLoopPolicy::default(),
            steering: None,
        };
        (context, requests, barrier, temp)
    }

    /// C-531: like [`NativeBatchBarrier`], but each call blocks on the gate for **its own key**, so
    /// the test dictates the completion order of two concurrent same-name calls instead of
    /// releasing whichever waiter happens to reach a shared semaphore first.
    #[derive(Default)]
    struct KeyedBarrier {
        entered: AtomicUsize,
        entered_notify: tokio::sync::Notify,
        gates: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    }

    impl KeyedBarrier {
        fn gate(&self, key: &str) -> Arc<tokio::sync::Semaphore> {
            self.gates
                .lock()
                .unwrap()
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(0)))
                .clone()
        }

        async fn wait_for_entered(&self, expected: usize) {
            loop {
                let notified = self.entered_notify.notified();
                if self.entered.load(Ordering::SeqCst) >= expected {
                    return;
                }
                notified.await;
            }
        }

        fn release(&self, key: &str) {
            self.gate(key).add_permits(1);
        }
    }

    struct KeyedBlockingReadTool {
        spec: ToolSpec,
        barrier: Arc<KeyedBarrier>,
    }

    #[async_trait]
    impl Tool for KeyedBlockingReadTool {
        fn spec(&self) -> ToolSpec {
            self.spec.clone()
        }

        async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
            let key = params["key"].as_str().unwrap_or_default().to_string();
            let gate = self.barrier.gate(&key);
            self.barrier.entered.fetch_add(1, Ordering::SeqCst);
            self.barrier.entered_notify.notify_waiters();
            let permit = gate
                .acquire()
                .await
                .map_err(|error| Error::Other(error.to_string()))?;
            permit.forget();
            Ok(ToolResult::ok(key))
        }
    }

    /// C-531: a context whose `read` tool blocks per input key until the test releases that key,
    /// with a [`PairingSink`] recording every tool call/result event the surfaces would see.
    fn keyed_read_batch_context(
        responses: Vec<Vec<Chunk>>,
    ) -> (StagedContext, PairingSink, Arc<KeyedBarrier>, TempRoot) {
        let tool_spec = ToolSpec {
            description: "Read one fixture record while the test controls completion order".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"],
                "additionalProperties": false
            }),
            ..spec(
                "read",
                vec![Effect::Read, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            )
        };
        let barrier = Arc::new(KeyedBarrier::default());
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(KeyedBlockingReadTool {
            spec: tool_spec,
            barrier: barrier.clone(),
        }));
        let temp = TempRoot::new("flux-dispatch-id-test");
        let executor = Arc::new(Executor::new(
            registry,
            PermissionManager::from_rules(&["read".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(temp.path()).unwrap()))),
        ));
        let provider: Arc<dyn Provider> = Arc::new(CaptureProvider {
            responses: Mutex::new(responses.into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let registry = executor.active_registry_snapshot();
        let sink = PairingSink::default();
        let context = StagedContext {
            provider,
            model: "test-model".into(),
            executor,
            registry,
            store: Arc::new(FlowStore::in_memory().unwrap()),
            session_id: "dispatch-id-test".into(),
            conversation: vec![Message::user_text("Read both fixture records")],
            base_system: None,
            sink: Arc::new(Mutex::new(sink.clone())),
            audit: None,
            advertised: HashSet::from(["read".into()]),
            authored_ceiling: None,
            groups: Vec::new(),
            opts: StageOptions::default(),
            remaining_token_budget: None,
            adaptive_policy: AdaptiveLoopPolicy::default(),
            steering: None,
        };
        (context, sink, barrier, temp)
    }

    fn keyed_read_stage_definition() -> ModelStageDefinition {
        ModelStageDefinition {
            prompt: "Read every requested record before returning.".into(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({
                "type": "object",
                "properties": {"complete": {"type": "boolean"}},
                "required": ["complete"],
                "additionalProperties": false
            }),
            model: None,
            tools: vec!["read".into()],
            max_tokens: 256,
            effort: None,
        }
    }

    fn blocking_model_stage_definition() -> ModelStageDefinition {
        ModelStageDefinition {
            prompt: "Inspect every requested record before returning.".into(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({
                "type": "object",
                "properties": {"complete": {"type": "boolean"}},
                "required": ["complete"],
                "additionalProperties": false
            }),
            model: None,
            tools: vec!["blocking_inspect".into()],
            max_tokens: 256,
            effort: None,
        }
    }

    fn ordered_tool_results(message: &Message) -> Vec<(String, bool, String)> {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some((
                    tool_use_id.clone(),
                    *is_error,
                    content
                        .iter()
                        .filter_map(|part| match part {
                            flux_core::ToolResultContent::Text { text } => Some(text.as_str()),
                            flux_core::ToolResultContent::Image { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )),
                _ => None,
            })
            .collect()
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
                    model: intent.model,
                };
            }
        };
        let mut exploration =
            explore_stage(context, json!({ "state": intent_value["state"].clone() })).await;
        intent.usages.append(&mut exploration.usages);
        StagedRun {
            result: exploration.result,
            usages: intent.usages,
            model: exploration.model,
        }
    }

    #[test]
    fn adaptive_model_call_default_is_50() {
        assert_eq!(DEFAULT_ADAPTIVE_MODEL_CALLS, 50);
        assert_eq!(AdaptiveLoopPolicy::default().max_model_calls, 50);
    }

    /// A-95's named failing-first test: a capability signal that widens nothing must leave the
    /// declaration — and therefore the advertised tool set and the rendered system segments —
    /// byte-identical. Re-signalling a held family used to append to `intent` on every call, which
    /// rewrites the trailing system segment and (on the Responses wire, where segments are flattened
    /// into `instructions`) cold-writes the whole prompt prefix for zero capability gain.
    #[test]
    fn a_noop_capability_signal_does_not_churn_the_prompt() {
        let ops = vec![spec(
            "db.query",
            vec![],
            vec![AccessKind::Connection],
            Some("db"),
        )];
        // A family is discoverable only when its ops are advertised (or its group manifest carries a
        // turn-intent matcher); this fixture takes the advertised route.
        let advertised: HashSet<String> = ["db.query".to_string()].into_iter().collect();
        let families = build_families(&ops, &[], &advertised);
        assert!(
            families.contains_key("db"),
            "fixture family resolves: {:?}",
            families.keys()
        );

        let mut state = AdaptiveState {
            version: 1,
            declaration: IntentDeclaration {
                intent: "inspect the database".into(),
                families: vec!["db".into()],
                ..Default::default()
            },
            selected: vec!["db.query".into()],
            messages: Vec::new(),
            proposed: Vec::new(),
            gathered: Vec::new(),
            native_step: 0,
            last_error: String::new(),
            pending: None,
            intent_calls: 0,
            explore_calls: 0,
        };
        let before = state.declaration.clone();

        // Signal a family the state already holds.
        let added = apply_capability_signal(
            &mut state,
            &json!({"capability_families": ["db"], "reason": "still need the database"}),
            &families,
            &[],
        )
        .expect("a redundant signal is accepted, not an error");

        assert!(added.is_empty(), "nothing new was surfaced: {added:?}");
        assert_eq!(
            state.declaration.families, before.families,
            "the family set is unchanged"
        );
        assert_eq!(
            state.declaration.intent, before.intent,
            "`intent` must NOT grow — it renders into the trailing system segment"
        );
        assert_eq!(state.selected, vec!["db.query".to_string()]);
    }

    /// A-95: absent a widening signal the advertised tool set must be byte-identical round to round.
    /// Tools render BEFORE system on the Anthropic wire, so any membership or ordering wobble here
    /// cold-writes every system breakpoint too. This pins the property the rest of the epic's
    /// caching depends on.
    #[test]
    fn the_advertised_tool_set_is_byte_stable_across_rounds() {
        let ops = vec![
            spec("db.query", vec![], vec![AccessKind::Connection], Some("db")),
            spec("db.exec", vec![], vec![AccessKind::Connection], Some("db")),
            spec("fs.read", vec![], vec![AccessKind::Filesystem], Some("fs")),
        ];
        let advertised: HashSet<String> = ops.iter().map(|o| o.name.clone()).collect();
        let families = build_families(&ops, &[], &advertised);
        let declaration = IntentDeclaration {
            intent: "look at the database".into(),
            families: vec!["db".into()],
            ..Default::default()
        };

        let render = || {
            selected_specs(&declaration, &families, &[])
                .expect("selection succeeds")
                .iter()
                .map(tool_def)
                .map(|t| serde_json::to_string(&t).expect("tool serializes"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            render(),
            render(),
            "the same declaration must render the same tools"
        );
        // …and it is the family's ops, name-sorted, with nothing from the unselected family.
        let names = selected_specs(&declaration, &families, &[])
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["db.exec".to_string(), "db.query".to_string()]);
    }

    /// A-95: a signal that genuinely widens still widens — the no-op guard must not disable
    /// capability discovery, which is the one way this change could do real harm.
    #[test]
    fn a_widening_capability_signal_still_surfaces_new_operations() {
        let ops = vec![
            spec("db.query", vec![], vec![AccessKind::Connection], Some("db")),
            spec("net.get", vec![], vec![AccessKind::Network], Some("net")),
        ];
        let advertised: HashSet<String> = ["db.query".to_string(), "net.get".to_string()]
            .into_iter()
            .collect();
        let families = build_families(&ops, &[], &advertised);
        let mut state = AdaptiveState {
            version: 1,
            declaration: IntentDeclaration {
                intent: "inspect the database".into(),
                families: vec!["db".into()],
                ..Default::default()
            },
            selected: vec!["db.query".into()],
            messages: Vec::new(),
            proposed: Vec::new(),
            gathered: Vec::new(),
            native_step: 0,
            last_error: String::new(),
            pending: None,
            intent_calls: 0,
            explore_calls: 0,
        };

        let added = apply_capability_signal(
            &mut state,
            &json!({"capability_families": ["net"], "reason": "the record points at a URL"}),
            &families,
            &[],
        )
        .expect("a widening signal is accepted");

        assert_eq!(added, vec!["net.get".to_string()], "the new op is surfaced");
        assert_eq!(
            state.declaration.families,
            vec!["db".to_string(), "net".to_string()]
        );
        assert!(
            state.declaration.intent.contains("additional signal"),
            "a real widening still records why: {}",
            state.declaration.intent
        );
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

    /// C-528 failing-first: one provider response carries two independent reads. The tool blocks
    /// each execution until the test releases it, so concurrency is observed structurally rather
    /// than inferred from elapsed time.
    #[tokio::test]
    async fn model_stage_native_batch_overlaps_independent_gather_calls() {
        let responses = vec![
            native_calls(vec![
                ("read-first", "blocking_inspect", json!({"key": "first"})),
                ("read-second", "blocking_inspect", json!({"key": "second"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let (mut context, requests, barrier, _root) = blocking_model_stage_context(responses);
        let events = Arc::new(EventStore::in_memory().unwrap());
        let turn_id = events
            .begin_turn(
                &context.session_id,
                "Inspect both fixture records",
                "test-model",
            )
            .unwrap();
        context.audit = Some((events.clone(), turn_id));
        let session_id = context.session_id.clone();
        let definition = blocking_model_stage_definition();

        let stage = run_model_stage(context, "batch_probe", definition, json!({}));
        let controller = async {
            let overlapped = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                barrier.wait_for_entered(2),
            )
            .await
            .is_ok();
            if overlapped {
                barrier.release(2);
            } else {
                barrier.release(1);
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    barrier.wait_for_entered(2),
                )
                .await
                .expect("the serial baseline must eventually start the second call");
                barrier.release(1);
            }
            overlapped
        };
        let (stage, overlapped) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        assert!(
            overlapped,
            "both independent reads must become active before either is released"
        );
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 2);

        let requests = requests.lock().unwrap();
        let result_message = requests[1].messages.last().expect("batch result message");
        let result_ids = ordered_tool_results(result_message)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(result_ids, ["read-first", "read-second"]);
        let admitted = events
            .observations(&session_id)
            .unwrap()
            .into_iter()
            .filter(|observation| observation.kind == "native.batch_call")
            .collect::<Vec<_>>();
        assert_eq!(admitted.len(), 2);
        assert_eq!(admitted[0].data["call_index"], 0);
        assert_eq!(admitted[0].data["call_id"], "read-first");
        assert_eq!(admitted[1].data["call_index"], 1);
        assert_eq!(admitted[1].data["call_id"], "read-second");
        assert!(admitted
            .iter()
            .all(|observation| observation.data["disposition"] == "parallel_gather"));
    }

    #[tokio::test]
    async fn adaptive_exploration_uses_the_shared_concurrent_native_batch() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect both fixture records",
                    "capability_families": ["workspace.read"]
                }),
            ),
            native_calls(vec![
                (
                    "adaptive-first",
                    "blocking_inspect",
                    json!({"key": "first"}),
                ),
                (
                    "adaptive-second",
                    "blocking_inspect",
                    json!({"key": "second"}),
                ),
            ]),
            prose("Both records were inspected."),
        ];
        let (context, requests, barrier, _root) = blocking_model_stage_context(responses);

        let exploration = run(context);
        let controller = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                barrier.wait_for_entered(2),
            )
            .await
            .expect("adaptive exploration must start both independent calls");
            barrier.release(2);
        };
        let (exploration, ()) = tokio::join!(exploration, controller);

        assert_eq!(exploration.result.unwrap()["kind"], "chat");
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 2);
        let requests = requests.lock().unwrap();
        let results = ordered_tool_results(
            requests[2]
                .messages
                .last()
                .expect("adaptive batch result message"),
        );
        assert_eq!(results[0].0, "adaptive-first");
        assert_eq!(results[1].0, "adaptive-second");
    }

    /// C-531 failing-first: one provider response carries two independent, same-name `read` calls.
    /// `flush_parallel_native_calls` polls them together and the test releases them OUT OF ORDER,
    /// so the second call's result reaches the sink before the first call's. Arrival order is the
    /// only pairing an id-less sink stream offers, and here it cross-attaches: every surface that
    /// matches a result to a card by name/order shows the wrong body under the wrong header.
    #[tokio::test]
    async fn concurrent_same_name_results_pair_with_their_own_call() {
        let responses = vec![
            native_calls(vec![
                ("read-a", "read", json!({"key": "a"})),
                ("read-b", "read", json!({"key": "b"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let (context, sink, barrier, _root) = keyed_read_batch_context(responses);

        let stage = run_model_stage(
            context,
            "keyed_reads",
            keyed_read_stage_definition(),
            json!({}),
        );
        let recorded = sink.clone();
        let controller = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                barrier.wait_for_entered(2),
            )
            .await
            .expect("both same-name reads must be in flight together");
            // Release the SECOND call first and let it settle, so the two results reach the sink
            // in the opposite order from their calls.
            barrier.release("b");
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if recorded.results.lock().unwrap().len() == 1 {
                        return;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the second read must complete first");
            barrier.release("a");
        };
        let (stage, ()) = tokio::join!(stage, controller);
        assert_eq!(stage.result.unwrap(), json!({"complete": true}));

        let calls = sink.calls.lock().unwrap().clone();
        let results = sink.results.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "both calls must surface: {calls:?}");
        assert_eq!(results.len(), 2, "both results must surface: {results:?}");
        assert_eq!(calls[0].2["key"], "a", "calls surface in issue order");
        assert_eq!(calls[1].2["key"], "b");
        // The results arrive in the OPPOSITE order, which is exactly what makes arrival-order
        // pairing unsound — and what the dispatch id has to survive.
        assert_eq!(results[0].2, "b", "the second read completed first");
        assert_eq!(results[1].2, "a");
        assert_ne!(calls[0].0, calls[1].0, "each call mints its own id");

        // The tool echoes its own input key, so a correctly paired stream reads a→"a", b→"b".
        let paired: Vec<(String, String)> = results
            .iter()
            .map(|(dispatch, _, content)| {
                let call = calls
                    .iter()
                    .find(|(id, _, _)| id == dispatch)
                    .unwrap_or_else(|| {
                        panic!("every result carries its own call's dispatch id: {calls:?}")
                    });
                (
                    call.2["key"].as_str().unwrap_or_default().to_string(),
                    content.clone(),
                )
            })
            .collect();
        assert_eq!(
            paired,
            vec![("b".into(), "b".into()), ("a".into(), "a".into())],
            "each result must pair with its own call: {calls:?} / {results:?}"
        );
    }

    #[tokio::test]
    async fn native_batch_obeys_the_runtime_concurrency_ceiling() {
        let responses = vec![
            native_calls(vec![
                ("read-1", "blocking_inspect", json!({"key": "one"})),
                ("read-2", "blocking_inspect", json!({"key": "two"})),
                ("read-3", "blocking_inspect", json!({"key": "three"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let limits = ResourceLimits::new()
            .with_max_concurrent_tool_calls(2)
            .with_tool_call_queue_timeout(std::time::Duration::from_secs(1));
        let (context, _requests, barrier, _root) =
            blocking_model_stage_context_with(responses, true, limits, Vec::new());

        let stage = run_model_stage(
            context,
            "bounded_batch",
            blocking_model_stage_definition(),
            json!({}),
        );
        let controller = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                barrier.wait_for_entered(2),
            )
            .await
            .expect("two calls must occupy the two runtime slots");
            assert_eq!(barrier.active.load(Ordering::SeqCst), 2);
            barrier.release(1);
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                barrier.wait_for_entered(3),
            )
            .await
            .expect("the queued third call must start after a slot is released");
            barrier.release(2);
        };
        let (stage, ()) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        assert_eq!(barrier.entered.load(Ordering::SeqCst), 3);
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn native_batch_queue_timeout_is_an_ordered_actionable_result() {
        let responses = vec![
            native_calls(vec![
                ("held", "blocking_inspect", json!({"key": "held"})),
                ("timed-out", "blocking_inspect", json!({"key": "queued"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let limits = ResourceLimits::new()
            .with_max_concurrent_tool_calls(1)
            .with_tool_call_queue_timeout(std::time::Duration::from_millis(25));
        let (context, requests, barrier, _root) =
            blocking_model_stage_context_with(responses, true, limits, Vec::new());

        let stage = run_model_stage(
            context,
            "timed_batch",
            blocking_model_stage_definition(),
            json!({}),
        );
        let controller = async {
            barrier.wait_for_entered(1).await;
            tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            barrier.release(1);
        };
        let (stage, ()) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        assert_eq!(barrier.entered.load(Ordering::SeqCst), 1);
        let requests = requests.lock().unwrap();
        let results = ordered_tool_results(
            requests[1]
                .messages
                .last()
                .expect("ordered queue-timeout result message"),
        );
        assert_eq!(results[0], ("held".into(), false, "held".into()));
        assert_eq!(results[1].0, "timed-out");
        assert!(results[1].1);
        assert!(results[1].2.contains("max_concurrent_tool_calls"));
        assert!(results[1].2.contains("Retry once a call completes"));
    }

    #[tokio::test]
    async fn approval_sensitive_native_gather_calls_remain_ordered() {
        let responses = vec![
            native_calls(vec![
                ("approval-1", "blocking_inspect", json!({"key": "one"})),
                ("approval-2", "blocking_inspect", json!({"key": "two"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let (context, _requests, barrier, _root) =
            blocking_model_stage_context_with(responses, false, ResourceLimits::new(), Vec::new());

        let stage = run_model_stage(
            context,
            "approval_batch",
            blocking_model_stage_definition(),
            json!({}),
        );
        let controller = async {
            barrier.wait_for_entered(1).await;
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    barrier.wait_for_entered(2),
                )
                .await
                .is_err(),
                "an approval-sensitive sibling must not overlap the active call"
            );
            barrier.release(1);
            barrier.wait_for_entered(2).await;
            barrier.release(1);
        };
        let (stage, ()) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pre_tool_hooks_are_a_conservative_native_batch_barrier() {
        let responses = vec![
            native_calls(vec![
                ("hook-1", "blocking_inspect", json!({"key": "one"})),
                ("hook-2", "blocking_inspect", json!({"key": "two"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let hooks: Vec<Arc<dyn PreToolHook>> =
            vec![Arc::new(CountingContinueHook(hook_calls.clone()))];
        let (context, _requests, barrier, _root) =
            blocking_model_stage_context_with(responses, true, ResourceLimits::new(), hooks);

        let stage = run_model_stage(
            context,
            "hook_batch",
            blocking_model_stage_definition(),
            json!({}),
        );
        let controller = async {
            barrier.wait_for_entered(1).await;
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    barrier.wait_for_entered(2),
                )
                .await
                .is_err(),
                "authorize-only cannot prove a hook-rewritten sibling remains independent"
            );
            barrier.release(1);
            barrier.wait_for_entered(2).await;
            barrier.release(1);
        };
        let (stage, ()) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(hook_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn active_cassette_is_a_conservative_native_batch_barrier() {
        let responses = vec![
            native_calls(vec![
                ("cassette-1", "blocking_inspect", json!({"key": "one"})),
                ("cassette-2", "blocking_inspect", json!({"key": "two"})),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let (context, _requests, barrier, _root) = blocking_model_stage_context(responses);
        let events = Arc::new(EventStore::in_memory().unwrap());
        context
            .store
            .set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Record(
                crate::cassette::RecordScope::new(events, &context.session_id),
            ))));

        let stage = run_model_stage(
            context,
            "cassette_batch",
            blocking_model_stage_definition(),
            json!({}),
        );
        let controller = async {
            barrier.wait_for_entered(1).await;
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    barrier.wait_for_entered(2),
                )
                .await
                .is_err(),
                "ordered cassette cells must not race concurrent native calls"
            );
            barrier.release(1);
            barrier.wait_for_entered(2).await;
            barrier.release(1);
        };
        let (stage, ()) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn native_batch_failure_keeps_successful_siblings_and_provider_order() {
        let responses = vec![
            native_calls(vec![
                (
                    "failed-first",
                    "blocking_inspect",
                    json!({"key": "first", "fail": true}),
                ),
                (
                    "successful-second",
                    "blocking_inspect",
                    json!({"key": "second"}),
                ),
            ]),
            native_call(
                "return",
                RETURN_STAGE_RESULT,
                json!({"value": {"complete": true}}),
            ),
        ];
        let (context, requests, barrier, _root) = blocking_model_stage_context(responses);

        let stage = run_model_stage(
            context,
            "failure_batch",
            blocking_model_stage_definition(),
            json!({}),
        );
        let controller = async {
            barrier.wait_for_entered(2).await;
            barrier.release(2);
        };
        let (stage, ()) = tokio::join!(stage, controller);

        assert_eq!(stage.result.unwrap(), json!({"complete": true}));
        let requests = requests.lock().unwrap();
        let results = ordered_tool_results(
            requests[1]
                .messages
                .last()
                .expect("mixed batch result message"),
        );
        assert_eq!(results[0].0, "failed-first");
        assert!(results[0].1);
        assert!(results[0].2.contains("fixture gather failure"));
        assert_eq!(
            results[1],
            ("successful-second".into(), false, "second".into())
        );
    }

    #[tokio::test]
    async fn cancelled_native_batch_leaves_no_detached_tool_execution() {
        let responses = vec![native_calls(vec![
            ("cancel-1", "blocking_inspect", json!({"key": "one"})),
            ("cancel-2", "blocking_inspect", json!({"key": "two"})),
        ])];
        let (context, _requests, barrier, _root) = blocking_model_stage_context(responses);

        {
            let stage = run_model_stage(
                context,
                "cancelled_batch",
                blocking_model_stage_definition(),
                json!({}),
            );
            tokio::pin!(stage);
            tokio::select! {
                () = barrier.wait_for_entered(2) => {}
                result = &mut stage => panic!("batch returned before cancellation: {:?}", result.result),
            }
            assert_eq!(barrier.active.load(Ordering::SeqCst), 2);
        }

        assert_eq!(
            barrier.active.load(Ordering::SeqCst),
            0,
            "dropping the owning turn future must drop every in-flight sibling"
        );
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
                "observation:model.call",
                "observation:turn.intent",
                "observation:loop.phase",
                "planning:true",
                "planning:false",
                "observation:model.call",
                "tool:inspect",
                "observation:adaptive.call",
                "planning:true",
                "planning:false",
                "observation:model.call",
            ],
            "each provider wait must be visible and must stop before a gathered op starts"
        );
    }

    /// A-90 failing first: a production-shaped router selects only the functional reporting
    /// family. Channel facilities are not another intent choice, but they remain available to the
    /// exploration model. The old family-only expansion exposed `flux.ambient` to intent and then
    /// dropped both facilities when the model correctly selected only `reporting`.
    #[tokio::test]
    async fn ambient_operations_survive_single_family_intent_routing() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "read the reporting fixture",
                    "capability_families": ["reporting"]
                }),
            ),
            prose("grounded answer"),
        ];
        let TestHarness {
            mut context,
            requests,
            ..
        } = staged_context(responses);
        context.groups = vec![
            ToolGroup {
                name: "reporting".into(),
                description: "Read reporting data".into(),
                tools: vec!["inspect".into()],
                surface_when: Vec::new(),
            },
            ToolGroup {
                name: "flux.ambient".into(),
                description: "Host channel facilities".into(),
                tools: vec!["change".into()],
                surface_when: Vec::new(),
            },
        ];

        assert_eq!(run(context).await.result.unwrap()["kind"], "chat");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let intent_families = requests[0].tools[0].input_schema["properties"]
            ["capability_families"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(intent_families, &vec![json!("reporting")]);
        assert!(requests[1].tools.iter().any(|tool| tool.name == "inspect"));
        assert!(
            requests[1].tools.iter().any(|tool| tool.name == "change"),
            "the ambient operation remains visible without selecting its group"
        );
        let signal = requests[1]
            .tools
            .iter()
            .find(|tool| tool.name == SIGNAL_CAPABILITIES)
            .unwrap();
        assert_eq!(
            signal.input_schema["properties"]["capability_families"]["items"]["enum"],
            json!(["reporting"]),
            "ambient is not a semantic capability family"
        );
    }

    #[test]
    fn ambient_operations_share_native_budgets_and_live_authority_ceilings() {
        let functional = (0..63)
            .map(|index| {
                spec(
                    &format!("reporting.read-{index}"),
                    vec![Effect::Read],
                    vec![],
                    Some("reporting"),
                )
            })
            .collect::<Vec<_>>();
        let families = BTreeMap::from([(
            "reporting".into(),
            Family {
                name: "reporting".into(),
                description: "Reporting reads".into(),
                specs: functional,
                exhaustive_members: false,
                routing_signals: Vec::new(),
            },
        )]);
        let ambient = vec![
            spec(
                "ui.present",
                vec![Effect::Read],
                vec![],
                Some(ADAPTIVE_AMBIENT_GROUP),
            ),
            spec(
                "ui.status",
                vec![Effect::Read],
                vec![],
                Some(ADAPTIVE_AMBIENT_GROUP),
            ),
        ];
        let declaration = IntentDeclaration {
            intent: "read reporting data".into(),
            families: vec!["reporting".into()],
            ..Default::default()
        };
        let error = selected_specs(&declaration, &families, &ambient)
            .unwrap_err()
            .to_string();
        assert!(error.contains("65 operations"), "{error}");

        let TestHarness {
            context: original, ..
        } = staged_context(Vec::new());
        let ambient_group = ToolGroup {
            name: ADAPTIVE_AMBIENT_GROUP.into(),
            description: "Host channel facilities".into(),
            tools: vec!["change".into()],
            surface_when: Vec::new(),
        };

        let denied_executor = Arc::new(Executor::new(
            original.executor.registry().clone(),
            PermissionManager::from_rules(&["change".into()], &["change".into()]),
            Arc::new(AllowApprover),
            original.executor.context().clone(),
        ));
        let mut denied = original.clone();
        denied.executor = denied_executor;
        denied.groups = vec![ambient_group.clone()];
        assert!(
            ambient_specs(&live_visible_specs(&denied), &denied.groups).is_empty(),
            "ambient visibility must not re-grant a denied operation"
        );

        let mut scoped = original.clone();
        scoped.groups = vec![ambient_group.clone()];
        let _scope = scoped.executor.push_cap_scope(&["inspect".into()]);
        assert!(
            ambient_specs(&live_visible_specs(&scoped), &scoped.groups).is_empty(),
            "ambient visibility must stay inside an active with_tools ceiling"
        );

        let mut authored = original;
        authored.groups = vec![ambient_group];
        authored.authored_ceiling = Some(HashSet::from(["inspect".into()]));
        assert!(
            ambient_specs(&live_visible_specs(&authored), &authored.groups).is_empty(),
            "ambient visibility must stay inside an authored model-stage ceiling"
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

            let expected = if cancelled {
                vec!["observation:loop.phase", "planning:true", "planning:false"]
            } else {
                vec![
                    "observation:loop.phase",
                    "planning:true",
                    "planning:false",
                    "observation:model.call",
                ]
            };
            assert_eq!(
                sink.lock().unwrap().events,
                expected,
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

    /// A-149: the production Node manifest must carry a greenfield request all the way through the
    /// staged family index and ordinary evidence surfacing without activating the shell escape
    /// hatch. This deliberately uses the real built-in registry and groups rather than a reduced
    /// fixture that could drift from production assembly.
    #[test]
    fn greenfield_node_intent_reaches_dedicated_tools_without_shell_fallback() {
        let mut registry = ToolRegistry::new();
        flux_tools::try_register_builtins(&mut registry).unwrap();
        let specs = registry.specs();
        let groups = flux_tools::groups::builtin_groups();

        let families = build_families(&specs, &groups, &HashSet::new());
        let node = &families["node"];
        assert_eq!(
            node.specs
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            vec!["node_run", "npm"]
        );
        assert!(!families.contains_key("shell"));
        let index = family_index(&families);
        assert!(index.contains("- node (2 operations; e.g. node_run, npm):"));
        assert!(index.contains(
            "Routing hints: javascript, node.js, npm, package.json, react, typescript, vue, vuex."
        ));

        let observations = flux_evidence::turn_intent_observations(
            &groups,
            "Create a greenfield Vue app and run its npm tests",
        );
        let active = flux_evidence::resolve_active_groups(&groups, &observations);
        assert!(active.contains("node"));
        assert!(!active.contains("shell"));
        let advertised = registry
            .active_specs(&groups, &active)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<HashSet<_>>();
        assert!(advertised.contains("npm"));
        assert!(advertised.contains("node_run"));
        assert!(!advertised.contains("bash"));
        assert!(!advertised.contains("proc.run"));
    }

    #[test]
    fn virtual_family_index_never_hides_a_registered_operation() {
        let specs = (0..12)
            .map(|index| {
                spec(
                    &format!("runtime_fact_{index:02}"),
                    vec![Effect::Read],
                    vec![],
                    None,
                )
            })
            .collect::<Vec<_>>();
        let advertised = specs
            .iter()
            .map(|spec| spec.name.clone())
            .collect::<HashSet<_>>();
        let families = build_families(&specs, &[], &advertised);
        let index = family_index(&families);

        for spec in specs {
            assert!(
                index.contains(&spec.name),
                "ungrouped operation `{}` was hidden from intent routing",
                spec.name
            );
        }
    }

    fn routed_group(name: &str, tool: &str, signals: &[&str]) -> ToolGroup {
        ToolGroup {
            name: name.into(),
            description: format!("{name} integration"),
            tools: vec![tool.into()],
            surface_when: signals
                .iter()
                .map(|signal| flux_evidence::SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some((*signal).into()),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn one_strong_routing_match_cannot_be_dropped_by_the_intent_model() {
        let TestHarness {
            mut context,
            requests,
            ..
        } = staged_context(vec![native_call(
            "intent",
            DECLARE_INTENT,
            json!({
                "intent": "post a fixture update",
                "capability_families": []
            }),
        )]);
        context.conversation = vec![Message::user_text("post this to company chat")];
        context.groups = vec![routed_group(
            "plugin.fixture-chat",
            "change",
            &["fixture chat", "company chat", "chat.example.com"],
        )];

        let result = detect_intent_stage(context).await.result.unwrap();
        assert_eq!(result["kind"], "intent");
        assert_eq!(result["families"], json!(["plugin.fixture-chat"]));
        assert_eq!(result["operations"], json!(["change"]));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ambiguous_routing_asks_before_exposing_an_integration_schema() {
        let TestHarness {
            mut context,
            requests,
            ..
        } = staged_context(vec![prose("used the selected integration")]);
        context.conversation = vec![Message::user_text("post this to chat")];
        context.groups = vec![
            routed_group("plugin.fixture-read", "inspect", &["chat"]),
            routed_group("plugin.fixture-write", "change", &["chat"]),
        ];

        let decision = detect_intent_stage(context.clone()).await.result.unwrap();
        assert_eq!(decision["kind"], "decision");
        assert_eq!(decision["question"]["options"].as_array().unwrap().len(), 2);
        assert!(
            requests.lock().unwrap().is_empty(),
            "an exact ambiguity is resolved before a provider request or integration schema"
        );

        let resumed = explore_stage(
            context,
            json!({"state": decision["state"].clone(), "decision": "1"}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(resumed["kind"], "chat");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.iter().any(|tool| tool.name == "inspect"));
        assert!(!requests[0].tools.iter().any(|tool| tool.name == "change"));
    }

    #[tokio::test]
    async fn routing_never_offers_an_unwired_group() {
        let TestHarness {
            mut context,
            requests,
            ..
        } = staged_context(vec![native_call(
            "intent",
            DECLARE_INTENT,
            json!({"intent": "answer conversationally", "capability_families": []}),
        )]);
        context.conversation = vec![Message::user_text("post this to ghost chat")];
        context.groups = vec![routed_group("plugin.ghost", "ghost.send", &["ghost chat"])];

        let result = detect_intent_stage(context).await.result.unwrap();
        assert_eq!(result["kind"], "intent");
        assert_eq!(result["families"], json!([]));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn logical_model_call_budget_survives_a_decision_resume() {
        let TestHarness {
            mut context,
            requests,
            ..
        } = staged_context(vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({"intent": "choose safely", "capability_families": []}),
            ),
            native_call(
                "decision",
                REQUEST_DECISION,
                json!({"prompt": "Which path?", "options": ["a", "b"]}),
            ),
            prose("this response must never be requested"),
        ]);
        context.adaptive_policy.max_model_calls = 2;

        let intent = detect_intent_stage(context.clone()).await.result.unwrap();
        let decision = explore_stage(context.clone(), json!({"state": intent["state"].clone()}))
            .await
            .result
            .unwrap();
        assert_eq!(decision["kind"], "decision");

        let error = explore_stage(
            context,
            json!({"state": decision["state"].clone(), "decision": "a"}),
        )
        .await
        .result
        .unwrap_err()
        .to_string();
        assert!(error.contains("model-call budget exhausted"), "{error}");
        assert!(error.contains("2/2"), "{error}");
        assert_eq!(
            requests.lock().unwrap().len(),
            2,
            "resume must stop before a third provider request"
        );
    }

    #[tokio::test]
    async fn built_in_stage_policy_overrides_model_effort_and_tokens_independently() {
        let TestHarness {
            mut context,
            requests,
            ..
        } = staged_context(vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({"intent": "answer", "capability_families": []}),
            ),
            prose("done"),
        ]);
        context.adaptive_policy.intent = AgentStagePolicy {
            model: Some("fast-router".into()),
            effort: Some(Effort::Low),
            max_tokens: Some(333),
            max_calls: Some(1),
        };
        context.adaptive_policy.explore = AgentStagePolicy {
            model: Some("deep-explorer".into()),
            effort: Some(Effort::High),
            max_tokens: Some(777),
            max_calls: Some(2),
        };
        let executor = context.executor.clone();

        assert_eq!(run(context).await.result.unwrap()["kind"], "chat");
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].model, "fast-router");
        assert_eq!(requests[0].effort, Some(Effort::Low));
        assert_eq!(requests[0].max_tokens, 333);
        assert_eq!(requests[1].model, "deep-explorer");
        assert_eq!(requests[1].effort, Some(Effort::High));
        assert_eq!(requests[1].max_tokens, 777);
        assert_eq!(requests[0].trace.as_ref().unwrap().stage, "intent");
        assert_eq!(requests[1].trace.as_ref().unwrap().stage, "explore");
        let evidence = executor.evidence();
        let calls = evidence.by_kind("model.call").collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].data["stage"], "intent");
        assert_eq!(calls[0].data["operations"], 1);
        assert!(calls[0].data["duration_us"].is_number());
        assert!(calls[0].data["ttft_us"].is_number());
        assert_eq!(calls[1].data["stage"], "explore");
    }

    /// C-181 failing first: a connect retry reaches the live surface as a `model.retry` observation
    /// *while the call is still open* (between `planning:true` and `planning:false`), and its tally
    /// lands on the call's `model.call` observation. Pre-fix the retry only reached `tracing`, which
    /// no surface subscribes to, so neither ever appeared.
    #[tokio::test]
    async fn a_connect_retry_reaches_the_surface_live_and_is_tallied_on_the_call() {
        let TestHarness { mut context, .. } = staged_context(Vec::new());
        context.provider = Arc::new(RetryingProvider {
            responses: Mutex::new(VecDeque::from(vec![native_call(
                "intent",
                DECLARE_INTENT,
                json!({"intent": "answer", "capability_families": []}),
            )])),
            fail: false,
        });
        let sink = Arc::new(Mutex::new(RecordingSink::default()));
        context.sink = sink.clone();
        let executor = context.executor.clone();

        detect_intent_stage(context).await.result.unwrap();

        let events = sink.lock().unwrap().events.clone();
        let retry = events
            .iter()
            .position(|e| e == "observation:model.retry")
            .expect("the retry must reach the live sink");
        let open = events.iter().position(|e| e == "planning:true").unwrap();
        let close = events.iter().position(|e| e == "planning:false").unwrap();
        assert!(
            open < retry && retry < close,
            "the retry must surface while the call is still open, got {events:?}"
        );

        let evidence = executor.evidence();
        let calls = evidence.by_kind("model.call").collect::<Vec<_>>();
        assert_eq!(calls[0].data["retries"], 1);
        assert_eq!(calls[0].data["oauth_refreshes"], 0);
        assert_eq!(calls[0].data["transport_fallbacks"], 0);
    }

    /// The tally must survive the failure path too: when the retry budget is exhausted no stream
    /// ever exists, so the count cannot ride the stream — it has to come off the observer.
    #[tokio::test]
    async fn retries_are_tallied_even_when_the_call_ultimately_fails() {
        let TestHarness { mut context, .. } = staged_context(Vec::new());
        context.provider = Arc::new(RetryingProvider {
            responses: Mutex::new(VecDeque::new()),
            fail: true,
        });
        let executor = context.executor.clone();

        assert!(detect_intent_stage(context).await.result.is_err());

        let evidence = executor.evidence();
        let calls = evidence.by_kind("model.call").collect::<Vec<_>>();
        assert_eq!(calls[0].data["ok"], false);
        assert_eq!(calls[0].data["retries"], 1);
    }

    /// The `model.retry` observation carries only the reason's short label — never the underlying
    /// transport error string, which can embed an endpoint URL.
    #[tokio::test]
    async fn a_retry_observation_carries_a_label_not_the_raw_transport_error() {
        let TestHarness { mut context, .. } = staged_context(Vec::new());
        context.provider = Arc::new(RetryingProvider {
            responses: Mutex::new(VecDeque::from(vec![native_call(
                "intent",
                DECLARE_INTENT,
                json!({"intent": "answer", "capability_families": []}),
            )])),
            fail: false,
        });
        let executor = context.executor.clone();

        detect_intent_stage(context).await.result.unwrap();

        let evidence = executor.evidence();
        let retries = evidence.by_kind("model.retry").collect::<Vec<_>>();
        assert!(
            retries.is_empty(),
            "each retry is a live signal only; the durable tally rides `model.call`"
        );
    }

    #[test]
    fn semantic_families_cannot_escape_permission_or_with_tools_ceiling() {
        let TestHarness {
            context: original, ..
        } = staged_context(Vec::new());
        let semantic_group = ToolGroup {
            name: "plugin.fixture".into(),
            description: "Fixture integration".into(),
            tools: vec!["change".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: KIND_TURN_INTENT.into(),
                signal: Some("fixture".into()),
            }],
        };

        let denied_executor = Arc::new(Executor::new(
            original.executor.registry().clone(),
            PermissionManager::from_rules(&["change".into()], &["change".into()]),
            Arc::new(AllowApprover),
            original.executor.context().clone(),
        ));
        let mut denied = original.clone();
        denied.executor = denied_executor;
        denied.groups = vec![semantic_group.clone()];
        let specs = live_visible_specs(&denied);
        let families = build_families(&specs, &denied.groups, &denied.advertised);
        assert!(
            !families.contains_key("plugin.fixture"),
            "a turn-intent family must not re-grant a bare-denied operation"
        );

        let mut scoped = original;
        scoped.groups = vec![semantic_group];
        let _scope = scoped.executor.push_cap_scope(&["inspect".into()]);
        let specs = live_visible_specs(&scoped);
        let families = build_families(&specs, &scoped.groups, &scoped.advertised);
        assert!(
            !families.contains_key("plugin.fixture"),
            "a turn-intent family must not widen an active with_tools ceiling"
        );

        let TestHarness {
            context: mut authored,
            ..
        } = staged_context(Vec::new());
        authored.groups = vec![ToolGroup {
            name: "plugin.fixture".into(),
            description: "Fixture integration".into(),
            tools: vec!["change".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: KIND_TURN_INTENT.into(),
                signal: Some("fixture".into()),
            }],
        }];
        authored.authored_ceiling = Some(HashSet::from(["inspect".into()]));
        let specs = live_visible_specs(&authored);
        let families = build_families(&specs, &authored.groups, &authored.advertised);
        assert!(
            !families.contains_key("plugin.fixture"),
            "semantic discovery must not widen an ai_segment's authored tool list"
        );
    }

    #[test]
    fn stale_capability_state_names_each_unavailable_operation_and_reason() {
        let TestHarness { context, .. } = staged_context(Vec::new());
        let specs = live_visible_specs(&context);
        let families = build_families(&specs, &context.groups, &context.advertised);
        let state = AdaptiveState {
            version: 1,
            declaration: IntentDeclaration {
                intent: "inspect stale evidence".into(),
                families: vec!["workspace.read".into()],
                ..Default::default()
            },
            selected: vec!["inspect".into(), "removed.inspect".into()],
            messages: vec![Message::user_text("inspect")],
            proposed: Vec::new(),
            gathered: Vec::new(),
            native_step: 0,
            last_error: String::new(),
            pending: None,
            intent_calls: 0,
            explore_calls: 0,
        };

        let error = selected_specs_for_state(&state, &families, &[], &context)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`removed.inspect`"), "{error}");
        assert!(error.contains("not registered"), "{error}");
        assert!(
            !error.contains("`inspect` ("),
            "live operations must not be blamed: {error}"
        );
    }

    #[tokio::test]
    async fn semantic_capability_signal_expands_beyond_initial_surface_within_live_ceiling() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "post the current Bitcoin price to Slack",
                    "capability_families": ["plugin.slack"]
                }),
            ),
            native_call(
                "expand",
                SIGNAL_CAPABILITIES,
                json!({
                    "capability_families": ["plugin.websearch"],
                    "reason": "the current price requires a live public source"
                }),
            ),
            native_call("price", "inspect", json!({"key": "current-bitcoin-price"})),
            prose("The current fixture price is grounded."),
        ];
        let TestHarness {
            mut context,
            requests,
            read_calls,
            write_calls,
            ..
        } = staged_context(responses);
        context.conversation = vec![Message::user_text(
            "Post the current Bitcoin price to my Slack DM",
        )];
        context.advertised = HashSet::from(["change".into()]);
        context.groups = vec![
            ToolGroup {
                name: "plugin.slack".into(),
                description: "Company chat".into(),
                tools: vec!["change".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some("slack".into()),
                }],
            },
            ToolGroup {
                name: "plugin.websearch".into(),
                description: "Public web search".into(),
                tools: vec!["inspect".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some("websearch".into()),
                }],
            },
        ];

        let output = run(context).await.result.unwrap();
        assert_eq!(output["kind"], "chat");
        assert_eq!(read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(!requests[1].tools.iter().any(|tool| tool.name == "inspect"));
        let expanded = requests[2]
            .tools
            .iter()
            .find(|tool| tool.name == "inspect")
            .expect("the accepted semantic signal must surface web search on the next round");
        assert!(
            requests[2].tools.iter().any(|tool| tool.name == "change"),
            "previously surfaced evidence must remain available for the whole adaptive turn"
        );
        assert_eq!(
            expanded.input_schema,
            json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"],
                "additionalProperties": false
            })
        );
    }

    /// A-83 (failing first): later signals may widen visibility, but the durable union must retain
    /// the same four-family ceiling as the initial declaration. Reject before expanding schemas or
    /// mutating the resumable state so a fifth small family cannot defer failure to a later round.
    #[test]
    fn semantic_capability_signal_rejects_fifth_cumulative_family_before_expansion() {
        let families = (0..5)
            .map(|index| {
                let family_name = format!("plugin.fixture-{index}");
                let operation = format!("fixture_{index}.inspect");
                (
                    family_name.clone(),
                    Family {
                        name: family_name,
                        description: format!("Fixture family {index}"),
                        specs: vec![spec(&operation, vec![Effect::Read], Vec::new(), None)],
                        exhaustive_members: false,
                        routing_signals: Vec::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let initial_families = (0..4)
            .map(|index| format!("plugin.fixture-{index}"))
            .collect::<Vec<_>>();
        let initial_selected = (0..4)
            .map(|index| format!("fixture_{index}.inspect"))
            .collect::<Vec<_>>();
        let mut state = AdaptiveState {
            version: 1,
            declaration: IntentDeclaration {
                intent: "inspect four fixture families".into(),
                families: initial_families.clone(),
                ..Default::default()
            },
            selected: initial_selected.clone(),
            messages: vec![Message::user_text("inspect the fixtures")],
            proposed: Vec::new(),
            gathered: Vec::new(),
            native_step: 0,
            last_error: String::new(),
            pending: None,
            intent_calls: 1,
            explore_calls: 1,
        };

        let error = apply_capability_signal(
            &mut state,
            &json!({
                "capability_families": ["plugin.fixture-4"],
                "reason": "later evidence named one more integration"
            }),
            &families,
            &[],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("5"), "error was: {error}");
        assert!(error.contains("maximum is 4"), "error was: {error}");
        assert_eq!(state.declaration.families, initial_families);
        assert_eq!(state.selected, initial_selected);
    }

    /// C-597 (failing first): an authored segment executes its effects, so it must not inherit the
    /// planner's capture contract. A live Fleet worker read that contract and complied — 35
    /// read-only calls, no effect attempted, and a reply that it "captures actions for approval
    /// instead of executing them" while holding write/edit/bash/git_commit in its ceiling.
    #[test]
    fn an_authored_segment_is_told_its_effects_execute_not_that_they_are_captured() {
        let TestHarness { mut context, .. } = staged_context(Vec::new());
        let declaration = IntentDeclaration::default();

        let adaptive = explore_segments(&context, &declaration);
        let adaptive_text = adaptive[0].text.clone();

        context.authored_ceiling = Some(HashSet::from(["inspect".to_string()]));
        let authored = explore_segments(&context, &declaration);
        let authored_text = authored[0].text.clone();

        assert_ne!(
            authored_text, adaptive_text,
            "an authored segment gets its own contract"
        );
        // The planner's deferral language must be absent — it is what suppressed the effects.
        for planner_only in [
            "may capture an action instead of executing it",
            "call finalize_plan",
            "Never claim a captured action already happened",
        ] {
            assert!(
                !authored_text.contains(planner_only),
                "authored segment must not be told `{planner_only}`"
            );
        }
        // The contract must teach the staging ritual, because effectful calls really are captured
        // and only `finalize_plan` executes them. A prompt that merely asserts "effects execute"
        // makes the model stage writes and stop, changing nothing (observed on Fleet wave-275).
        assert!(
            authored_text.contains(FINALIZE_PLAN),
            "the authored contract must name `{FINALIZE_PLAN}`, or staged work never runs"
        );
        assert!(
            authored_text.contains("STAGED"),
            "the authored contract must say effectful calls are staged, not run inline"
        );
        assert!(
            authored_text.contains("captured as proposed action"),
            "the contract should quote the exact tool result the model will see"
        );
        // Evidence discipline is preserved, not traded away for execution.
        for kept in [
            "never invent facts or paths",
            "Search hits only locate sources",
        ] {
            assert!(authored_text.contains(kept), "`{kept}` must survive");
        }
        // The adaptive planner is untouched.
        assert!(adaptive_text.contains("may capture an action instead of executing it"));
    }

    /// C-595 (failing first): an authored segment that crosses the retained-history ceiling must
    /// shed the oldest tool-result payloads and keep running. Fleet wave-257 lost `flux/C-562` to
    /// the old behavior — the loop committed its complete deliverable, the next round measured
    /// 526 544 bytes against the 524 288-byte ceiling, and the whole turn was destroyed 0.43% over.
    #[test]
    fn an_authored_segment_over_budget_elides_old_results_instead_of_failing() {
        let big = "x".repeat(4096);
        let mut messages = vec![Message::user_text("goal")];
        for index in 0..8 {
            messages.push(Message::user(vec![ContentBlock::tool_result_text(
                format!("call-{index}"),
                big.clone(),
                false,
            )]));
        }
        // A ceiling that the untouched history exceeds but an elided history fits.
        let limit = adaptive_history_bytes(&messages) / 2;

        let elision = elide_segment_history(&mut messages, limit)
            .expect("an over-budget history reports what it shed");

        assert!(elision.elided_results > 0, "something was actually elided");
        assert!(
            elision.after_bytes < elision.before_bytes,
            "elision shrank the history: {} -> {}",
            elision.before_bytes,
            elision.after_bytes
        );
        assert!(
            adaptive_history_bytes(&messages) <= limit,
            "history was brought under the ceiling"
        );
        // The trailing exchange the model is mid-way through is never touched.
        let tail = &messages[messages.len() - SEGMENT_HISTORY_KEEP_RECENT..];
        for message in tail {
            for block in &message.content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    for entry in content {
                        let ToolResultContent::Text { text } = entry else {
                            continue;
                        };
                        assert_eq!(text, &big, "the most recent results stay verbatim");
                    }
                }
            }
        }
    }

    /// A history already inside its ceiling is left byte-for-byte alone — elision is a relief valve,
    /// not a routine rewrite.
    #[test]
    fn an_authored_segment_within_budget_is_never_rewritten() {
        let mut messages = vec![
            Message::user_text("goal"),
            Message::user(vec![ContentBlock::tool_result_text(
                "call-0", "small", false,
            )]),
        ];
        let before = messages.clone();

        assert_eq!(elide_segment_history(&mut messages, 1024 * 1024), None);
        assert_eq!(messages, before, "an in-budget history is untouched");
    }

    /// An operator-authored `ai_segment` names its exact tool ceiling, so `scoped_segment_state`
    /// selects every family inside that ceiling deterministically — there is no model choice to
    /// narrow. `MAX_FAMILIES` bounds what the *intent router* may select; applying it to an authored
    /// ceiling makes a legitimate five-family loop (read + write + git + shell + one more)
    /// unrunnable, which is how the Fleet story-implementation loop failed every worker. The
    /// operation/schema budgets still apply and remain the real ceiling.
    #[test]
    fn scoped_authored_ceiling_admits_more_families_than_the_intent_router_cap() {
        let families = (0..6)
            .map(|index| {
                let family_name = format!("plugin.fixture-{index}");
                let operation = format!("fixture_{index}.inspect");
                (
                    family_name.clone(),
                    Family {
                        name: family_name,
                        description: format!("Fixture family {index}"),
                        specs: vec![spec(&operation, vec![Effect::Read], Vec::new(), None)],
                        exhaustive_members: false,
                        routing_signals: Vec::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        // The same six families are rejected for a model-declared (router) selection...
        let routed = IntentDeclaration {
            intent: "inspect six fixture families".into(),
            families: families.keys().cloned().collect(),
            ..Default::default()
        };
        let error = selected_specs(&routed, &families, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("6 distinct families"), "error was: {error}");
        assert!(error.contains("maximum is 4"), "error was: {error}");

        // ...and admitted when the author already fixed the ceiling.
        let authored = IntentDeclaration {
            scoped: true,
            ..routed
        };
        let selected = selected_specs(&authored, &families, &[])
            .expect("an authored ceiling is not narrowed by the intent-router family cap");
        assert_eq!(selected.len(), 6, "every authored family expands");
    }

    /// The authored-ceiling exemption is scoped to the family cap only. The operation/schema budget
    /// is an independent host limit and must still reject an oversized authored ceiling.
    #[test]
    fn scoped_authored_ceiling_still_obeys_the_operation_budget() {
        let families = (0..MAX_NATIVE_TOOLS + 1)
            .map(|index| {
                let family_name = format!("plugin.fixture-{index}");
                let operation = format!("fixture_{index}.inspect");
                (
                    family_name.clone(),
                    Family {
                        name: family_name,
                        description: format!("Fixture family {index}"),
                        specs: vec![spec(&operation, vec![Effect::Read], Vec::new(), None)],
                        exhaustive_members: false,
                        routing_signals: Vec::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let authored = IntentDeclaration {
            intent: "an oversized authored ceiling".into(),
            families: families.keys().cloned().collect(),
            scoped: true,
            ..Default::default()
        };

        let error = selected_specs(&authored, &families, &[])
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("schema budget"),
            "the operation budget still applies; error was: {error}"
        );
    }

    /// A-84 (failing first): the durable state boundary must not trust a family union produced by
    /// an older runtime. Five one-operation families stay below the independent operation/schema
    /// budgets, so only the family invariant can reject this deserialized resume before expansion.
    #[test]
    fn resumed_adaptive_state_rejects_fifth_family_before_catalog_expansion() {
        let families = (0..5)
            .map(|index| {
                let family_name = format!("plugin.fixture-{index}");
                let operation = format!("fixture_{index}.inspect");
                (
                    family_name.clone(),
                    Family {
                        name: family_name,
                        description: format!("Fixture family {index}"),
                        specs: vec![spec(&operation, vec![Effect::Read], Vec::new(), None)],
                        exhaustive_members: false,
                        routing_signals: Vec::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let serialized = json!({
            "version": 1,
            "declaration": {
                "intent": "inspect five fixture families",
                "families": (0..5)
                    .map(|index| format!("plugin.fixture-{index}"))
                    .collect::<Vec<_>>()
            },
            "selected": (0..5)
                .map(|index| format!("fixture_{index}.inspect"))
                .collect::<Vec<_>>(),
            "messages": [Message::user_text("inspect the fixtures")],
            "proposed": [],
            "gathered": [],
            "native_step": 0,
            "last_error": "",
            "pending": null,
            "intent_calls": 1,
            "explore_calls": 1
        });
        let state: AdaptiveState = serde_json::from_value(serialized).unwrap();
        let TestHarness { context, .. } = staged_context(Vec::new());

        let error = selected_specs_for_state(&state, &families, &[], &context)
            .unwrap_err()
            .to_string();

        assert!(error.contains("5 distinct families"), "error was: {error}");
        assert!(error.contains("maximum is 4"), "error was: {error}");
    }

    #[test]
    fn resumed_adaptive_state_accepts_four_distinct_families_with_duplicate_names() {
        let families = (0..4)
            .map(|index| {
                let family_name = format!("plugin.fixture-{index}");
                let operation = format!("fixture_{index}.inspect");
                (
                    family_name.clone(),
                    Family {
                        name: family_name,
                        description: format!("Fixture family {index}"),
                        specs: vec![spec(&operation, vec![Effect::Read], Vec::new(), None)],
                        exhaustive_members: false,
                        routing_signals: Vec::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let serialized = json!({
            "version": 1,
            "declaration": {
                "intent": "inspect four fixture families",
                "families": [
                    "plugin.fixture-0",
                    "plugin.fixture-1",
                    "plugin.fixture-2",
                    "plugin.fixture-3",
                    "plugin.fixture-0"
                ]
            },
            "selected": (0..4)
                .map(|index| format!("fixture_{index}.inspect"))
                .collect::<Vec<_>>(),
            "messages": [Message::user_text("inspect the fixtures")],
            "proposed": [],
            "gathered": [],
            "native_step": 0,
            "last_error": "",
            "pending": null,
            "intent_calls": 1,
            "explore_calls": 1
        });
        let state: AdaptiveState = serde_json::from_value(serialized).unwrap();
        let TestHarness { context, .. } = staged_context(Vec::new());

        let selected = selected_specs_for_state(&state, &families, &[], &context).unwrap();

        assert_eq!(selected.len(), 4);
        assert_eq!(selected[0].name, "fixture_0.inspect");
        assert_eq!(selected[3].name, "fixture_3.inspect");
    }

    #[tokio::test]
    async fn semantic_capability_signal_can_propose_an_action_beyond_initial_surface() {
        let responses = vec![
            native_call(
                "intent",
                DECLARE_INTENT,
                json!({
                    "intent": "inspect a fact and then update the matching service",
                    "capability_families": ["plugin.lookup"]
                }),
            ),
            native_call(
                "expand",
                SIGNAL_CAPABILITIES,
                json!({
                    "capability_families": ["plugin.action"],
                    "reason": "the inspected fact identifies the required service action"
                }),
            ),
            native_call("write", "change", json!({"value": "new"})),
            native_call(
                "finalize",
                FINALIZE_PLAN,
                json!({"instructions": "Report the guarded action result."}),
            ),
        ];
        let TestHarness {
            mut context,
            write_calls,
            ..
        } = staged_context(responses);
        context.advertised = HashSet::from(["inspect".into()]);
        context.groups = vec![
            ToolGroup {
                name: "plugin.lookup".into(),
                description: "Lookup service facts".into(),
                tools: vec!["inspect".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some("lookup".into()),
                }],
            },
            ToolGroup {
                name: "plugin.action".into(),
                description: "Act on a service".into(),
                tools: vec!["change".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some("action".into()),
                }],
            },
        ];

        let output = run(context).await.result.unwrap();
        assert_eq!(output["kind"], "batch");
        assert_eq!(output["batch"]["actions"][0]["op"], "change");
        assert_eq!(
            write_calls.load(Ordering::SeqCst),
            0,
            "the newly surfaced action must remain inert until approval"
        );
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
            flux_spec::IntentSet::new(),
            &[]
        ));
        assert!(!gather_safe(
            &read,
            flux_spec::StagingDisposition::Capture,
            flux_spec::IntentSet::new(),
            &[]
        ));
        assert!(gather_safe(
            &read,
            flux_spec::StagingDisposition::Gather,
            flux_spec::IntentSet::new(),
            &[]
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
            flux_spec::IntentSet::new(),
            &[]
        ));

        // C-528 live connector-metadata constraint: generated connector specs commonly declare
        // `Network` without the truthful `Read`/`Write` half today. Low risk and idempotency cannot
        // turn that incomplete metadata into gather authority; the connector contract must first
        // say both `Network` and `Read` explicitly.
        let mut network_only = spec(
            "connector.get",
            vec![Effect::Network],
            vec![AccessKind::Network],
            None,
        );
        network_only.risk = Risk::Low;
        network_only.idempotency = Idempotency::Idempotent;
        assert!(!gather_safe(
            &network_only,
            flux_spec::StagingDisposition::Gather,
            flux_spec::IntentSet::new(),
            &[]
        ));

        let mut fresh_read = spec("clock", vec![Effect::Read], vec![], None);
        fresh_read.idempotency = Idempotency::NonIdempotent;
        assert!(
            gather_safe(
                &fresh_read,
                flux_spec::StagingDisposition::Infer,
                flux_spec::IntentSet::new(),
                &[]
            ),
            "freshness/cacheability must not turn a side-effect-free read into an action"
        );

        let write = spec(
            "write",
            vec![Effect::Write, Effect::Filesystem],
            vec![AccessKind::Filesystem],
            None,
        );
        assert!(!gather_safe(
            &write,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new(),
            &[]
        ));
        assert!(!gather_safe(
            &write,
            flux_spec::StagingDisposition::Gather,
            flux_spec::IntentSet::new(),
            &[]
        ));

        // A pure op (no effects, no access) stays gather-safe.
        let pure = spec("compute", vec![], vec![], None);
        assert!(gather_safe(
            &pure,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new(),
            &[]
        ));

        // An operation that declares no effects but reaches a code-running / local-system host
        // capability is NOT gather-safe: the empty effect set must not read as inert.
        let process = spec("shell_probe", vec![], vec![AccessKind::Process], None);
        assert!(!gather_safe(
            &process,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new(),
            &[]
        ));
        let local_system = spec("host_probe", vec![], vec![AccessKind::LocalSystem], None);
        assert!(!gather_safe(
            &local_system,
            flux_spec::StagingDisposition::Infer,
            flux_spec::IntentSet::new(),
            &[]
        ));
    }

    /// C-210: an op may declare consequence through the semantic-effect tags instead of the effect
    /// set, and gather-safety has to read that channel too. Every spec below is `[Read, Network]` at
    /// `Risk::Low` — gather-safe on its effect set alone — so the tag is the only thing deciding.
    #[test]
    fn a_declared_consequential_semantic_effect_is_not_gather_safe() {
        let fetch = spec(
            "site.fetch",
            vec![Effect::Read, Effect::Network],
            vec![AccessKind::Network],
            None,
        );
        let gather = |tags: &[&str]| {
            let owned: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
            gather_safe(
                &fetch,
                flux_spec::StagingDisposition::Infer,
                flux_spec::IntentSet::new(),
                &owned,
            )
        };

        assert!(gather(&[]), "the effect set alone is gather-safe");

        // The two tags the default policy floor grants *without* approval, which is why the
        // classifier — not the gate — has to be the thing that stops them.
        assert!(
            !gather(&["write_db"]),
            "a durable datasource write must not run before a human sees the plan"
        );
        assert!(
            !gather(&["model"]),
            "a billable model call must not be spent during pre-approval exploration"
        );

        // The rest are stopped at the gate today by the shipped policy, but gather-safety must not
        // depend on a default the operator is expected to edit.
        for tag in ["send_external", "delete", "money", "write_file"] {
            assert!(!gather(&[tag]), "`{tag}` declares a consequence");
        }

        // Inert tags leave the classification alone, and an unknown tag lowers to nothing at all.
        for tag in ["read", "pure", "human_visible", "network", "not_a_real_tag"] {
            assert!(
                gather(&[tag]),
                "`{tag}` reaches nothing that outlives the call"
            );
        }

        // One consequential tag in a list is enough.
        assert!(!gather(&["read", "write_db"]));

        // `Capture` and a mutating intent still dominate, tags or no tags.
        assert!(!gather_safe(
            &fetch,
            flux_spec::StagingDisposition::Capture,
            flux_spec::IntentSet::new(),
            &[]
        ));
    }

    /// The C-191 correspondence, restated for the tag channel: `gather_safe`'s spec-shape branch is
    /// the exact negation of `is_consequence_bearing_with_effects`. C-208 rested a whole catalog
    /// census on that, so it is pinned rather than assumed.
    #[test]
    fn gather_safety_stays_the_exact_negation_of_the_consequence_classifier() {
        let cases: Vec<(ToolSpec, Vec<String>)> = vec![
            (
                spec(
                    "read",
                    vec![Effect::Read, Effect::Filesystem],
                    vec![AccessKind::Filesystem],
                    None,
                ),
                vec![],
            ),
            (
                spec(
                    "fetch",
                    vec![Effect::Read, Effect::Network],
                    vec![AccessKind::Network],
                    None,
                ),
                vec!["write_db".to_string()],
            ),
            (
                spec(
                    "egress",
                    vec![Effect::Network],
                    vec![AccessKind::Network],
                    None,
                ),
                vec![],
            ),
            (
                spec(
                    "write",
                    vec![Effect::Write, Effect::Filesystem],
                    vec![AccessKind::Filesystem],
                    None,
                ),
                vec![],
            ),
            (spec("compute", vec![], vec![], None), vec![]),
            (
                spec("shell_probe", vec![], vec![AccessKind::Process], None),
                vec![],
            ),
            (
                spec("consult", vec![Effect::Read], vec![], None),
                vec!["model".to_string()],
            ),
        ];

        for (spec, tags) in cases {
            assert_eq!(
                gather_safe(
                    &spec,
                    flux_spec::StagingDisposition::Infer,
                    flux_spec::IntentSet::new(),
                    &tags,
                ),
                !flux_spec::is_consequence_bearing_with_effects(&spec, &tags),
                "`{}` disagrees between gather-safety and the consequence classifier",
                spec.name
            );
        }
    }

    #[tokio::test]
    async fn steering_queued_mid_turn_is_injected_at_the_next_consultation_in_order() {
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
            context: mut ctx,
            requests,
            ..
        } = staged_context(responses);
        let queue = Arc::new(crate::steering::SteeringQueue::default());
        // The user "types" both messages while the round-1 gather call is executing.
        ctx.provider = Arc::new(SteerAfterProvider {
            inner: ctx.provider.clone(),
            queue: queue.clone(),
            after: 1,
            texts: Mutex::new(vec![
                "focus only on alpha".into(),
                "answer in one sentence".into(),
            ]),
            served: AtomicU64::new(0),
        });
        ctx.steering = Some(queue.clone());

        let output = run(ctx).await.result.unwrap();
        assert_eq!(output["kind"], "chat");
        assert!(queue.is_empty(), "drained at the next consultation");

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            !requests[1]
                .messages
                .iter()
                .any(|message| message.text().contains("user-steering")),
            "steering queued mid-round must not reach the consultation already in flight"
        );
        let last = requests[2].messages.last().expect("gather result message");
        assert_eq!(last.role, flux_core::Role::User);
        let text = last.text();
        assert!(
            text.contains("<user-steering>"),
            "steering is attributed, got: {text}"
        );
        let first = text.find("focus only on alpha").expect("first message");
        let second = text.find("answer in one sentence").expect("second message");
        assert!(first < second, "messages inject in submission order");
        assert!(
            last.content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. })),
            "steering merges into the trailing tool_result user message — never a consecutive-user pair"
        );
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
        let registry = executor.active_registry_snapshot();
        let ctx = StagedContext {
            provider,
            model: "test-model".into(),
            executor,
            registry,
            store: Arc::new(FlowStore::in_memory().unwrap()),
            session_id: "staged-alias-test".into(),
            conversation: vec![Message::user_text("Read plugin record-1")],
            base_system: None,
            sink: Arc::new(Mutex::new(NoopSink)),
            audit: None,
            advertised: HashSet::from([operation.into()]),
            authored_ceiling: None,
            groups: Vec::new(),
            opts: StageOptions::default(),
            remaining_token_budget: None,
            adaptive_policy: AdaptiveLoopPolicy::default(),
            steering: None,
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

    #[test]
    fn returned_arguments_validate_against_the_original_registered_schema() {
        let TestHarness { context, .. } = staged_context(Vec::new());
        let spec = ToolSpec {
            description: "Inspect a bounded set of records".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "records": {"type": "array", "maxItems": 1}
                },
                "required": ["records"],
                "additionalProperties": false
            }),
            ..spec(
                "inspect",
                vec![Effect::Read, Effect::Filesystem],
                vec![AccessKind::Filesystem],
                None,
            )
        };
        let original = spec.input_schema.clone();

        let errors = validate_call(
            &spec,
            &json!({"records": [{"id": 1}, {"id": 2}]}),
            &context,
            &HashSet::from(["inspect".into()]),
        )
        .unwrap_err();

        assert!(
            errors
                .iter()
                .any(|error| error.contains("more than 1 item")),
            "errors were: {errors:?}"
        );
        assert!(spec.input_schema["properties"]["records"]
            .get("items")
            .is_none());
        assert_eq!(spec.input_schema, original);
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
    async fn logical_call_budget_allows_50_calls_and_refuses_the_51st_request() {
        let mut responses = vec![native_call(
            "intent",
            DECLARE_INTENT,
            json!({
                "intent": "change the fixture",
                "capability_families": ["workspace.write"]
            }),
        )];
        responses.extend((0..50).map(|round| {
            native_call(
                &format!("write-{round}"),
                "change",
                json!({"value": format!("value-{round}")}),
            )
        }));
        let TestHarness {
            context: mut ctx,
            requests,
            write_calls,
            ..
        } = staged_context(responses);
        ctx.adaptive_policy.max_model_calls = 50;

        let error = run(ctx).await.result.unwrap_err().to_string();

        assert!(
            error.contains("adaptive model-call budget exhausted before `explore` (50/50"),
            "{error}"
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            50,
            "one intent plus 49 exploration calls are allowed; no 51st request is sent"
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
