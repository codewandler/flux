//! The compiler front-end: turn natural language into a typed [`DraftAst`]. Prompt-and-parse (the
//! provider has no forced structured output).
//!
//! **Pure DAG:** the model has NO directly-callable ops — its only tool is `emit_plan` (+ `ask_user`).
//! Every operation, *reads included*, is a node in the emitted graph, so a turn is always an auditable
//! plan, never a free-form tool call. To gather information the model emits a plan with read nodes; the
//! runtime executes it and feeds the results back so it can plan the next step.
//!
//! - [`compile_turn`] — **the seat of the one engine**: plan a turn from the *conversation*. The model
//!   calls `emit_plan` with the execution graph, asks one clarifying question ([`AskUser`]), or answers
//!   in prose. Returns [`TurnOutput::Plan`] (the runtime executes it) or [`TurnOutput::Chat`].
//! - [`plan`] — a thin wrapper over `compile_turn` for the one-shot `--plan` surface (a single
//!   instruction; a prose-only answer is an error, since that surface wants a graph).
//! - [`compile`] — one-shot, single model call (no tools); kept for the simple path.
//!
//! All are session-aware: a [`SessionView`] lets the model reference already-created `$values` instead
//! of re-fetching, and the emitted AST may reference *any* registered op (it is the *plan*, not executed
//! here). This is the seat of "the LLM plans": the model proposes structure; the runtime owns execution.

// The compile path is a safety gate — keep it free of `unsafe` (C-17/F4 replaced the one raw-pointer
// sink reborrow with a plain safe reborrow).
#![deny(unsafe_code)]

use futures::StreamExt;

use flux_core::{Chunk, ContentBlock, Error, Message, Result, StopReason, Usage};
use flux_provider::{Provider, Request, SystemSegment, ToolDef};
use flux_spec::tool_input_schema;
use schemars::JsonSchema;

use crate::analyze::{for_each_node, lower, Diagnostic};
use crate::ast::{DraftAst, Node};
use crate::registry::OpRegistry;
use crate::state::SessionView;

/// The session-symbol set the analyzer's definedness pass checks `$var` references against
/// (L-15): the names in the current [`SessionView`], i.e. exactly the symbols the model was shown.
fn view_symbols(view: Option<&SessionView>) -> std::collections::HashSet<String> {
    view.map(|v| v.symbols.iter().map(|s| s.name.0.clone()).collect())
        .unwrap_or_default()
}

/// Validate a model-emitted plan on the production path: full structural analysis **plus** the
/// typed lowering pass (L-16/F9 — `lower` is the gate, not just `analyze_flow`), definedness
/// checked against the session's bound symbols. The `HirFlow` is discarded here; execution
/// re-derives what it needs.
fn validate_plan(
    ast: &DraftAst,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
) -> std::result::Result<(), Vec<Diagnostic>> {
    lower(ast, ops, &view_symbols(view)).map(|_| ())
}

/// Options for [`compile`] / [`plan`].
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// One-shot model attempts (initial + repairs).
    pub max_attempts: u32,
    /// Agentic planner loop steps (research / ask / emit).
    pub max_steps: u32,
    /// Token budget for each model call. The whole `emit_plan` AST must fit here, so it is generous —
    /// too small a budget truncates large plans mid-tool-call (see the `max_tokens` guard in
    /// [`compile_turn`]).
    pub max_tokens: u32,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            max_steps: 8,
            max_tokens: 16384,
        }
    }
}

/// The result of a compile: the AST the model produced, how many attempts/steps it took, and any
/// analyzer diagnostics. Non-empty `diagnostics` means the AST parsed but references unknown ops — it
/// is surfaced (compile-only shows it) rather than executed. Only the one-shot [`compile`] returns a
/// diagnostics-carrying value (its surface prints them and refuses to run); [`compile_turn`] — whose
/// plans callers execute — never does: a plan that fails analysis is repair feedback, and exhausting
/// the step budget rejects the turn with the diagnostic text (C-17/F2).
#[derive(Debug, Clone)]
pub struct Compiled {
    pub ast: DraftAst,
    pub attempts: u32,
    pub diagnostics: Vec<Diagnostic>,
    /// The model's completion signal, attached to `emit_plan` when this plan *completes* the request.
    /// When set, the engine runs the plan and then writes the final user message from the *actual*
    /// results (a grounded post-execution call) per the [`Completion`] instructions — never a
    /// pre-composed summary. `None` means "keep going" → the engine loops, and the model ends the turn
    /// by answering in prose once it has seen what it needs (the standard agent loop).
    pub complete: Option<Completion>,
    /// True when the model tagged this plan `gather: true` (design Part 1 — the phased loop): a
    /// bounded, read-only orient/gather round rather than the execution plan. Compile-time enforced
    /// (see [`OpRegistry::mutating_ops_in`] and [`GATHER_NODE_CAP`]) — a plan reaching here with
    /// `gather: true` is guaranteed effect-clean and small. Always `false` for a plan compiled via
    /// [`compile`] (the one-shot, non-phased path) or the plain-text fallback in [`compile_turn`].
    pub gather: bool,
    /// The orient/gather grounding artifact riding alongside a `gather: true` plan — `None` unless
    /// `gather` is `true` and the model supplied a usable `brief` (parsed tolerantly, like
    /// [`Completion`]).
    pub brief: Option<Brief>,
}

/// The orient/gather grounding artifact (design Part 1's `brief: {goal, needs[]}`): what the turn is
/// ultimately trying to accomplish, and the specific things still unknown. Host-carried per turn
/// (A-14) and prepended to subsequent planner feedback so a multi-round gather doesn't lose the
/// thread.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Brief {
    pub goal: String,
    pub needs: Vec<String>,
}

/// The model's turn-completion directive (the optional `complete` field of `emit_plan`). It carries
/// *instructions* for the final message — rendered **after** the plan runs, against the real results —
/// not the message text itself, so a closing summary can never promise output it hasn't seen.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Optional short human-facing context the model already knows (e.g. "Build green."), folded into
    /// the grounded summary call as a hint.
    pub primer: Option<String>,
    /// What the final message should say, e.g. "summarize what changed and why".
    pub instructions: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmitPlanInput {
    /// The Flux-Lang flow AST to execute.
    ast: DraftAst,
    /// Attach only when this plan completes the request; the runtime renders the final message after
    /// executing the plan.
    complete: Option<EmitPlanCompletionInput>,
    /// Tag this plan as a bounded, read-only orient/gather round (only where the current phase's
    /// instructions allow it — rejected in the execute phase). The compiler enforces effect-clean
    /// (no write/destructive op, composites included) and a small call-node cap; pass `brief`
    /// alongside it.
    gather: Option<bool>,
    /// Present alongside `gather: true`: the grounding artifact — what the turn is ultimately
    /// trying to accomplish, and what this gather round still needs to learn.
    brief: Option<EmitPlanBriefInput>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmitPlanCompletionInput {
    /// What the final message should say, based on the actual execution results.
    instructions: String,
    /// Optional one-line context the planner already knows.
    primer: Option<String>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmitPlanBriefInput {
    /// What the turn is ultimately trying to accomplish.
    goal: String,
    /// The specific things still unknown that this gather round (and any that follow) is trying to
    /// learn.
    #[serde(default)]
    needs: Vec<String>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmitPlanTextInput {
    /// The plan as native Flux-Lang source text: a `flow` header line at column 0, then one
    /// statement per line, indented 2 spaces per block level (never tabs).
    source: String,
    /// Attach only when this plan completes the request; the runtime renders the final message after
    /// executing the plan.
    complete: Option<EmitPlanCompletionInput>,
    /// Tag this plan as a bounded, read-only orient/gather round (only where the current phase's
    /// instructions allow it — rejected in the execute phase). The compiler enforces effect-clean
    /// (no write/destructive op, composites included) and a small call-node cap; pass `brief`
    /// alongside it.
    gather: Option<bool>,
    /// Present alongside `gather: true`: the grounding artifact — what the turn is ultimately
    /// trying to accomplish, and what this gather round still needs to learn.
    brief: Option<EmitPlanBriefInput>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct AskUserInput {
    /// The single clarifying question to ask the user.
    question: String,
}

/// Which surface `emit_plan` speaks — the **L-20 emission A/B scaffold**, selected per turn via the
/// `FLUX_EMISSION` env switch (see `docs/designs/flux-lang-emission-ab.md`).
///
/// - [`EmissionArm::Json`] (the default): the shipped strict derived [`DraftAst`] JSON schema.
///   With `FLUX_EMISSION` unset this is byte-identical to the pre-selector surface — prompt,
///   tools, and parse path all unchanged.
/// - [`EmissionArm::Text`]: the plan rides as native Flux-Lang **text** in a single `source`
///   string, taught via the native grammar (worked examples rendered by [`flux_lang::format`], so
///   they are parseable by construction) and parsed with [`flux_lang::parse`]. The decoded
///   [`DraftAst`] then flows through **exactly** the same gates as the JSON arm — surfacing
///   enforcement (A-04) and the analyze/lower validation (C-17) are arm-independent.
///
/// This is a temporary measurement scaffold: once the A/B is decided, the losing arm and this
/// selector are deleted (the project's no-fallbacks stance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmissionArm {
    /// Strict derived `DraftAst` JSON schema on `emit_plan` (`ast` param) — the control arm.
    #[default]
    Json,
    /// Native Flux-Lang text on `emit_plan` (`source` param) — the treatment arm.
    Text,
}

impl EmissionArm {
    /// Parse a selector value: unset/empty/`json` → [`Self::Json`], `text` → [`Self::Text`].
    /// Anything else is an error — a typo must not silently pick an arm mid-experiment.
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim) {
            None | Some("") => Ok(Self::Json),
            Some(v) if v.eq_ignore_ascii_case("json") => Ok(Self::Json),
            Some(v) if v.eq_ignore_ascii_case("text") => Ok(Self::Text),
            Some(v) => Err(Error::Other(format!(
                "invalid FLUX_EMISSION value `{v}` — use `json` or `text`"
            ))),
        }
    }

    /// Read the arm from the `FLUX_EMISSION` env switch (the planner's emission-surface selector).
    pub fn from_env() -> Result<Self> {
        Self::parse(std::env::var("FLUX_EMISSION").ok().as_deref())
    }
}

/// Which pass of the phased turn loop is calling [`compile_turn`] (design Part 1 —
/// `docs/designs/multipass-agent-loop.md`): selects the per-phase instruction segment appended
/// after the shared, byte-stable segment A, and gates `gather: true` emissions. The loop text and
/// host threading (brief carry, settled/budget bookkeeping) are story A-14's job — this type is
/// only the protocol carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// The turn's first planner call: a three-way contract — trivial request → prose chat (as
    /// today); simple/actionable request → the full execution plan (as today, no added latency);
    /// complex/context-hungry request → a small, read-only plan tagged `gather: true` plus a
    /// `brief: {goal, needs[]}`.
    Orient,
    /// A bounded read-only gather round: keep gathering (another `gather: true` plan) or settle
    /// (the full execution plan, or a prose answer). Still allowed to emit `gather: true`.
    Gather,
    /// The standard plan/execute/revise contract — today's behavior, byte-compatible. `gather: true`
    /// is rejected here as repair feedback: the gather budget is spent once execution starts.
    /// Default, so a phase-less call (pre-A-14 ejected loops, the one-shot `--plan`/`/plan`
    /// surfaces) gets exactly the pre-phase contract.
    #[default]
    Execute,
}

impl Phase {
    /// The wire name for this phase (the flux-lang `plan(phase: "...")` reflexive op — A-14 —
    /// carries exactly this string; kept here so the three names are defined once).
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Orient => "orient",
            Phase::Gather => "gather",
            Phase::Execute => "execute",
        }
    }

    /// Parse the `phase` argument threaded through the `plan(phase: "...")` reflexive op (A-14) —
    /// the inverse of [`Self::as_str`]. Absent, empty, or any unrecognized value defaults to
    /// [`Phase::Execute`] — the byte-compatible contract a phase-less call (an ejected/overridden
    /// pre-A-14 loop) must keep getting.
    pub fn from_wire(value: Option<&str>) -> Phase {
        match value {
            Some("orient") => Phase::Orient,
            Some("gather") => Phase::Gather,
            _ => Phase::Execute,
        }
    }
}

/// What the planner produced for a turn: an executable plan, or a plain-prose answer (a chat turn —
/// the model chose to respond rather than emit a graph). The one engine drives [`compile_turn`] every
/// turn and either executes the `Plan` or surfaces the `Chat` text as the assistant reply.
#[derive(Debug, Clone)]
pub enum TurnOutput {
    Plan(Compiled),
    Chat(String),
}

/// How the planner asks the user a clarifying question mid-plan (interactive mode). The CLI implements
/// this over stdin; `None` means no user is attached, so the `ask_user` tool is not offered.
pub trait AskUser: Send + Sync {
    /// Ask `question` and return the user's reply.
    fn ask(&self, question: &str) -> String;
}

// ---------------------------------------------------------------------------
// One-shot compile
// ---------------------------------------------------------------------------

/// Compile a natural-language instruction into a [`DraftAst`] in a single model call (prompt-and-parse,
/// with a bounded repair loop). `view`, when present, lets the model reference existing session symbols.
///
/// Cache-segmented (A-09): the instructions + catalog + grammar are ONE cached system segment,
/// byte-stable across repair attempts, so attempt 2+ re-reads it from the provider prompt cache
/// instead of re-paying the full catalog. The per-session symbols are an uncached trailing segment
/// (mirroring `compile_turn`'s A/C layout), and the instruction + each repair exchange ride as
/// ordinary messages.
pub async fn compile(
    provider: &dyn Provider,
    model: &str,
    instruction: &str,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
    opts: CompileOptions,
) -> Result<Compiled> {
    let attempts = opts.max_attempts.max(1);
    let mut segments = vec![SystemSegment {
        text: build_oneshot_system(ops),
        cache: true,
    }];
    let symbols = symbols_block(view);
    if !symbols.trim().is_empty() {
        segments.push(SystemSegment {
            text: symbols,
            cache: false,
        });
    }
    let mut messages = vec![Message::user_text(format!("Instruction: {instruction}"))];
    let mut last_err = String::new();

    for attempt in 1..=attempts {
        let req = Request {
            model: model.to_string(),
            system: None,
            system_segments: segments.clone(),
            messages: messages.clone(),
            tools: Vec::new(),
            max_tokens: opts.max_tokens,
            temperature: None,
            top_p: None,
            stop_sequences: Vec::new(),
            thinking: false,
            effort: None,
            metadata: serde_json::Map::new(),
        };
        let mut stream = provider.stream(req).await?;
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            if let Chunk::TextDelta(t) = chunk? {
                text.push_str(&t);
            }
        }
        let repair = |messages: &mut Vec<Message>, previous: &str, error: &str| {
            messages.push(Message::assistant_text(previous));
            messages.push(Message::user_text(format!(
                "Your previous output was invalid ({error}). Return a corrected AST. \
                 Output ONLY the JSON AST in a single ```json code block."
            )));
        };
        match parse_draft_ast(&text) {
            Ok(ast) => match validate_plan(&ast, ops, view) {
                Ok(()) => {
                    return Ok(Compiled {
                        ast,
                        attempts: attempt,
                        diagnostics: Vec::new(),
                        complete: None,
                        gather: false,
                        brief: None,
                    })
                }
                Err(diags) => {
                    if attempt == attempts {
                        return Ok(Compiled {
                            ast,
                            attempts: attempt,
                            diagnostics: diags,
                            complete: None,
                            gather: false,
                            brief: None,
                        });
                    }
                    last_err = join_diags(&diags);
                    repair(&mut messages, &text, &last_err);
                }
            },
            Err(e) => {
                last_err = e;
                if attempt == attempts {
                    return Err(Error::Other(format!(
                        "compile failed after {attempt} attempt(s): {last_err}"
                    )));
                }
                repair(&mut messages, &text, &last_err);
            }
        }
    }
    Err(Error::Other(format!("compile failed: {last_err}")))
}

// ---------------------------------------------------------------------------
// Agentic planner
// ---------------------------------------------------------------------------

/// Plan **one turn** of the conversation: the seat of the single engine. Seeded with the prior
/// `messages` (the conversation), the model calls **`emit_plan`** with the execution graph, [`AskUser`]
/// to clarify, or **answers in prose** (a chat turn). Pure DAG — there are no directly-callable ops, so
/// every operation lives in the emitted plan. Returns [`TurnOutput::Plan`] for a graph the runtime will
/// execute, or [`TurnOutput::Chat`] for a prose answer. `ops` is the full op catalog (the AST may use
/// any of them).
///
/// The accumulated [`Usage`] rides OUTSIDE the `Result` (C-31): a consultation that fails — budget
/// exhausted, `max_tokens` truncation, a provider error mid-loop — still spent real tokens on every
/// step it made, and dropping them undercounted `flux usage` by exactly the most wasteful turns
/// (s_360: ~8 × 37k input tokens, zero `call_usage` events). Callers account the usage first, then
/// branch on the outcome.
// Each argument is a distinct, meaningful input (provider, model, conversation, base system, catalog,
// session view, user-ask, options); bundling them would obscure rather than clarify.
#[allow(clippy::too_many_arguments)]
pub async fn compile_turn(
    provider: &dyn Provider,
    model: &str,
    conversation: &[Message],
    base_system: Option<&str>,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
    ask: Option<&dyn AskUser>,
    // Optional sink for live thinking-token streaming during the planning call. When present,
    // each ThinkingDelta chunk is forwarded via sink.thinking_delta so the surface can display
    // reasoning in real time instead of showing a silent "composing plan\u2026" indicator.
    thinking_sink: Option<&'_ mut dyn crate::AgentSink>,
    opts: CompileOptions,
    phase: Phase,
) -> (Result<TurnOutput>, Usage) {
    // The emission surface is env-selected here — the single front door — so every caller (engine,
    // loop host, CLI) picks the arm up without threading a parameter (the L-20 A/B scaffold).
    let arm = match EmissionArm::from_env() {
        Ok(arm) => arm,
        // Rejected before any provider call: nothing was spent.
        Err(e) => return (Err(e), Usage::default()),
    };
    compile_turn_with_arm(
        provider,
        model,
        conversation,
        base_system,
        ops,
        view,
        ask,
        thinking_sink,
        opts,
        arm,
        phase,
    )
    .await
}

/// [`compile_turn`] with the emission arm made explicit (the L-20 A/B harness entry point — a
/// measurement runner drives both arms in one process without racing on the `FLUX_EMISSION` env).
/// Production callers use [`compile_turn`], which reads the arm from the environment. Same
/// usage-outside-the-`Result` shape (C-31).
#[allow(clippy::too_many_arguments)]
pub async fn compile_turn_with_arm(
    provider: &dyn Provider,
    model: &str,
    conversation: &[Message],
    base_system: Option<&str>,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
    ask: Option<&dyn AskUser>,
    thinking_sink: Option<&'_ mut dyn crate::AgentSink>,
    opts: CompileOptions,
    arm: EmissionArm,
    phase: Phase,
) -> (Result<TurnOutput>, Usage) {
    // Usage lives OUTSIDE the loop's `Result` (C-31): the inner fn accumulates into it through
    // every early return — error paths included — so a failed consultation's spend survives.
    let mut usage = Usage::default();
    let out = compile_turn_inner(
        provider,
        model,
        conversation,
        base_system,
        ops,
        view,
        ask,
        thinking_sink,
        opts,
        arm,
        phase,
        &mut usage,
    )
    .await;
    (out, usage)
}

/// The planner loop proper. `usage` is an out-parameter so the accumulated spend reaches the caller
/// on EVERY exit — `Ok`, budget exhaustion, truncation, or a provider error surfaced via `?`.
#[allow(clippy::too_many_arguments)]
async fn compile_turn_inner(
    provider: &dyn Provider,
    model: &str,
    conversation: &[Message],
    base_system: Option<&str>,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
    ask: Option<&dyn AskUser>,
    mut thinking_sink: Option<&'_ mut dyn crate::AgentSink>,
    opts: CompileOptions,
    arm: EmissionArm,
    phase: Phase,
    usage: &mut Usage,
) -> Result<TurnOutput> {
    let steps = opts.max_steps.max(1);
    let interactive = ask.is_some();
    let segments = assemble_system_segments(base_system, ops, view, interactive, arm, phase);
    // Pure DAG: the model's ONLY tools are `emit_plan` (+ `ask_user`). Every op — reads included — is a
    // node in the emitted graph, so a turn is always an auditable plan, never a free-form tool call.
    let tools = planner_tools(interactive, arm);
    let mut messages = conversation.to_vec();
    // Forward thinking-token deltas to the sink while we're in the planning phase, so both surfaces
    // (CLI: dims them on stderr; TUI: streams them into a dedicated Thinking entry) can show reasoning
    // live instead of silently waiting behind "composing plan\u2026".
    let enable_thinking = thinking_sink.is_some();

    // The most recent rejection fed back to the model (hidden ops, analyzer diagnostics, duplicate
    // emit_plan). When the step budget runs out, the turn is rejected WITH this text (C-17/F2) —
    // never "accepted with diagnostics" for a caller to execute blind.
    let mut last_reject = String::new();
    for step in 1..=steps {
        // A-38: snapshot `last_reject` before this step can touch it, so `step_reject` below can
        // tell "this step rejected something" apart from "last_reject still holds an earlier
        // step's text" — one clone, taken only when tracing is actually active.
        let step_reject_snapshot = planner_trace_enabled().then(|| last_reject.clone());
        let req = Request {
            model: model.to_string(),
            system: None,
            system_segments: segments.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            max_tokens: opts.max_tokens,
            temperature: None,
            top_p: None,
            stop_sequences: Vec::new(),
            thinking: enable_thinking,
            effort: None,
            metadata: serde_json::Map::new(),
        };

        // Per-iteration reborrow of the sink (C-17/F4): `as_deref_mut` borrows `thinking_sink` only
        // for this statement — the `stream_blocks` future is awaited to completion and dropped
        // before the next iteration reborrows, so no `unsafe` is needed. (`stream_blocks` keeps its
        // reference and trait-object lifetimes independent for exactly this reborrow.)
        let ts = thinking_sink.as_deref_mut();
        let (result, call_usage) = stream_blocks(provider, req, ts).await;
        usage.accumulate(&call_usage);
        let (mut blocks, acc_text, stop_reason, diagnostic) = match result {
            Ok(v) => v,
            // A-33: a decode-class error (malformed/truncated provider bytes) is provider/model
            // -originated, never a transport/availability failure — a fresh identical call is the
            // correct retry, and it costs exactly one step of the existing budget, never the whole
            // turn. No message is pushed: there is no coherent assistant turn to answer, so the next
            // iteration re-sends the same conversation unchanged.
            Err(e) if is_stream_decode(&e) => {
                last_reject =
                    format!("the provider stream broke while decoding the model's output: {e}");
                trace_step(
                    step,
                    steps,
                    None,
                    &[],
                    step_reject(&step_reject_snapshot, &last_reject),
                    None,
                );
                continue;
            }
            // Transport/availability errors (`Api`, `Http`, a transport-flavored `Provider`) keep
            // propagating exactly as before — swallowing a real outage would misreport it as budget
            // exhaustion.
            Err(e) => return Err(e),
        };
        if blocks.is_empty() && !acc_text.trim().is_empty() {
            blocks.push(ContentBlock::Text { text: acc_text });
        }
        let assistant = Message::assistant(blocks);
        let tool_uses = collect_tool_uses(&assistant);
        // A-38: snapshot the tool names for this step's trace before `tool_uses` is consumed
        // below (the emit_plan/ask_user processing loop moves out of it) — stays empty when
        // tracing is off, so nothing allocates on the hot path.
        let step_tool_names: Vec<String> = if planner_trace_enabled() {
            tool_uses.iter().map(|(_, name, _)| name.clone()).collect()
        } else {
            Vec::new()
        };
        if !assistant.content.is_empty() {
            messages.push(assistant.clone());
        }

        if tool_uses.is_empty() {
            // No tool call. Perhaps the model emitted the AST as plain text → a plan. Require a
            // non-empty `body`: `DraftAst`'s fields all default (no `deny_unknown_fields`), so an
            // UNRELATED JSON object embedded in ordinary prose (e.g. a reviewer's structured-JSON
            // finding, `{"fingerprint": …, "reviewer": "security"}`) parses "successfully" as a
            // trivially-empty, trivially-analyzed plan — misclassifying a legitimate prose/JSON
            // answer as a no-op `Plan` instead of `Chat`. A genuinely empty plan achieves nothing a
            // model would intentionally emit outside `emit_plan`, so require at least one node.
            if let Ok(ast) = parse_draft_ast(&assistant.text()) {
                if !ast.body.is_empty() && validate_plan(&ast, ops, view).is_ok() {
                    // Surfacing enforcement (A-04) applies to the text fallback too (C-17/F1): a
                    // prose-emitted plan is still a model-emitted plan, so a registered-but-hidden
                    // op (e.g. `bash` with the `shell` group off) is rejected here exactly like the
                    // `emit_plan` branch rejects it — repair feedback, and never an accepted plan,
                    // not even on the last step. Without this gate, emitting the plan as plain text
                    // instead of a tool call bypassed the check entirely.
                    let hidden = ops.hidden_ops_in(&ast.body);
                    if hidden.is_empty() {
                        trace_step(
                            step,
                            steps,
                            stop_reason,
                            &step_tool_names,
                            step_reject(&step_reject_snapshot, &last_reject),
                            diagnostic.as_ref(),
                        );
                        return Ok(TurnOutput::Plan(Compiled {
                            ast,
                            attempts: step,
                            diagnostics: Vec::new(),
                            complete: None,
                            // The text fallback has no wrapping JSON object to carry `gather`/
                            // `brief` from (it's a bare AST parsed out of prose) — same as
                            // `complete` above, neither rides through this path.
                            gather: false,
                            brief: None,
                        }));
                    }
                    // No tool_use id to answer here — the rejection rides as a plain user message.
                    last_reject = hidden_ops_rejection(&hidden);
                    messages.push(Message::user_text(last_reject.clone()));
                    trace_step(
                        step,
                        steps,
                        stop_reason,
                        &step_tool_names,
                        step_reject(&step_reject_snapshot, &last_reject),
                        diagnostic.as_ref(),
                    );
                    continue;
                }
            }
            // A `max_tokens` cutoff drops the in-flight `emit_plan` block — the provider never sends its
            // `content_block_stop`, so only the model's preamble text survives. Don't mistake that
            // truncation for a finished prose answer (which would silently end the turn with no work
            // done); surface it so the user can raise the budget or narrow the request.
            if stop_reason == Some(StopReason::MaxTokens) {
                trace_step(
                    step,
                    steps,
                    stop_reason,
                    &step_tool_names,
                    step_reject(&step_reject_snapshot, &last_reject),
                    diagnostic.as_ref(),
                );
                return Err(Error::Other(format!(
                    "planner output was truncated at max_tokens ({}) before it finished the plan — \
                     raise --max-tokens or split the request into smaller steps",
                    opts.max_tokens
                )));
            }
            // Otherwise prose is a chat answer (the engine surfaces it; the turn ends). A *truly empty*
            // turn (no blocks, no text) wasn't pushed, so just retry on the next step.
            let text = assistant.text();
            if !text.trim().is_empty() {
                trace_step(
                    step,
                    steps,
                    stop_reason,
                    &step_tool_names,
                    step_reject(&step_reject_snapshot, &last_reject),
                    diagnostic.as_ref(),
                );
                return Ok(TurnOutput::Chat(text));
            }
            // A-33: the stream completed without error but a codec tolerated and skipped
            // unparseable frames (`Chunk::StreamDiagnostic`) AND the turn produced nothing usable —
            // name the cause so an exhausted budget doesn't just say "no plan" with zero diagnostic
            // value. A turn that produced something real despite the drops took one of the earlier
            // return paths instead and never reaches here.
            // A-38: snapshot the diagnostic for the trace before it's (possibly) consumed below —
            // stays `None` when tracing is off.
            let diag_for_trace = planner_trace_enabled()
                .then(|| diagnostic.clone())
                .flatten();
            if let Some(diag) = diagnostic {
                last_reject = format!(
                    "the provider stream tolerated {} dropped frame(s) and produced no usable \
                     output: {}",
                    diag.dropped_frames, diag.detail
                );
            }
            trace_step(
                step,
                steps,
                stop_reason,
                &step_tool_names,
                step_reject(&step_reject_snapshot, &last_reject),
                diag_for_trace.as_ref(),
            );
            if step == steps {
                return Err(Error::Other(if last_reject.is_empty() {
                    format!("planner produced neither a plan nor an answer within {steps} steps")
                } else {
                    format!(
                        "planner did not produce a valid plan within {steps} step(s) — \
                         the last attempt was rejected: {last_reject}"
                    )
                }));
            }
            continue;
        }

        // Answer every tool_use (keeps the local history valid); capture an accepted plan if any.
        // A message carrying MORE than one `emit_plan` is rejected outright (C-17/F3): a turn takes
        // exactly one plan, and silently letting the last call win would execute a plan the model
        // may not have meant as final.
        let emit_plan_calls = tool_uses
            .iter()
            .filter(|(_, name, _)| name == "emit_plan")
            .count();
        let mut results = Vec::new();
        let mut done: Option<Compiled> = None;
        for (id, name, input) in tool_uses {
            // A-32: the wire codec could not parse this call's arguments even after repair — the
            // model emitted malformed or truncated JSON, and the codec carried the failure as a
            // sentinel instead of killing the provider stream (s_368: deepseek truncated a 19KB
            // emit_plan mid-list and the turn died after seven accepted rounds). A rejection like
            // any other (A-31): the model re-emits, and an exhausted budget names this cause.
            // Checked before anything reads the input — a sentinel carries no real fields, and
            // without this gate it would decode as an empty (accepted!) plan.
            if let Some(perr) = input
                .get(flux_core::ARGS_PARSE_ERROR_KEY)
                .and_then(|v| v.as_str())
            {
                last_reject = format!(
                    "your `{name}` arguments were not valid JSON ({perr}) — re-emit the complete \
                     arguments as one well-formed JSON object and call `{name}` again."
                );
                results.push(ContentBlock::tool_result_text(
                    id,
                    last_reject.clone(),
                    true,
                ));
                continue;
            }
            match name.as_str() {
                "emit_plan" if emit_plan_calls > 1 => {
                    last_reject = format!(
                        "invalid: you called emit_plan {emit_plan_calls} times in one message — a \
                         turn takes exactly ONE plan. Merge the steps into a single plan and call \
                         emit_plan once."
                    );
                    results.push(ContentBlock::tool_result_text(
                        id,
                        last_reject.clone(),
                        true,
                    ));
                }
                "emit_plan" => {
                    // The model's optional completion directive (captured before `input` is moved):
                    // present ⇒ this plan completes the request, so the engine renders the final message
                    // from the results after running. Absent ⇒ the engine loops (the model answers later).
                    let complete = parse_completion(input.get("complete"));
                    // The orient/gather tag + grounding artifact (design Part 1), parsed the same
                    // tolerant way regardless of arm — like `complete` above, both live on the raw
                    // JSON `input`, not inside either arm's `ast`/`source` payload.
                    let gather = parse_gather(input.get("gather"));
                    let brief = gather.then(|| parse_brief(input.get("brief"))).flatten();
                    // Per-arm decode (L-20): both arms land on the SAME `DraftAst` and flow through
                    // the identical gates below — surfacing enforcement (A-04) and analyze/lower
                    // (C-17) hold regardless of which surface carried the plan.
                    let decoded: std::result::Result<DraftAst, String> = match arm {
                        EmissionArm::Json => {
                            let ast_val = input.get("ast").cloned().unwrap_or(input);
                            // Stringified-JSON tolerance (A-30): OpenAI-wire-trained models
                            // habitually double-encode nested tool args — `ast` arrives as a JSON
                            // *string* containing the plan (qwen3.7, s_360/s_361) and the repair
                            // feedback never converges on the re-encode. Unwrap that one encoding
                            // layer here; a string that is not valid JSON falls through unchanged,
                            // keeping the strict decode's informative error. Encoding-level only:
                            // the decoded plan traverses the identical gates below.
                            let ast_val = match ast_val {
                                serde_json::Value::String(s) => {
                                    serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
                                }
                                v => v,
                            };
                            serde_json::from_value::<DraftAst>(ast_val)
                                .map_err(|e| format!("emit_plan: invalid AST JSON: {e}"))
                        }
                        EmissionArm::Text => match input.get("source").and_then(|v| v.as_str()) {
                            Some(src) => flux_lang::parse::parse(src).map_err(|e| {
                                format!(
                                    "emit_plan: invalid Flux-Lang source ({e}). Fix the source \
                                     text — a `flow` header at column 0, statements indented 2 \
                                     spaces per level, no tabs — and call emit_plan again."
                                )
                            }),
                            None => Err("emit_plan: missing `source` — pass the plan as native \
                                         Flux-Lang text in the `source` string parameter."
                                .to_string()),
                        },
                    };
                    match decoded {
                        Ok(ast) => {
                            // Surfacing enforcement (A-04): a model-emitted plan may only call ops
                            // advertised this turn. A registered-but-hidden op (e.g. `bash` with the
                            // `shell` group off) is rejected unconditionally — including on the last
                            // repair step, where ordinary diagnostics would be tolerated: a gated op
                            // must never execute because the model ran out of repair budget.
                            let hidden = ops.hidden_ops_in(&ast.body);
                            if !hidden.is_empty() {
                                last_reject = hidden_ops_rejection(&hidden);
                                results.push(ContentBlock::tool_result_text(
                                    id,
                                    last_reject.clone(),
                                    true,
                                ));
                                continue;
                            }
                            // Gather is enforced, not trusted (design Part 1): a `gather: true` plan
                            // is only ever granted in the orient/gather phases, and even there must
                            // be effect-clean and small — the SAME repair-feedback shape as the
                            // hidden-op gate above, never a silent downgrade or a hard turn error.
                            if gather {
                                if phase == Phase::Execute {
                                    last_reject = GATHER_REJECTED_IN_EXECUTE_PHASE.to_string();
                                    results.push(ContentBlock::tool_result_text(
                                        id,
                                        last_reject.clone(),
                                        true,
                                    ));
                                    continue;
                                }
                                if let Some(msg) = gather_violation(&ast, ops) {
                                    last_reject = msg;
                                    results.push(ContentBlock::tool_result_text(
                                        id,
                                        last_reject.clone(),
                                        true,
                                    ));
                                    continue;
                                }
                            }
                            match validate_plan(&ast, ops, view) {
                                Ok(()) => {
                                    results.push(ContentBlock::tool_result_text(
                                        id,
                                        "plan accepted".to_string(),
                                        false,
                                    ));
                                    done = Some(Compiled {
                                        ast,
                                        attempts: step,
                                        diagnostics: Vec::new(),
                                        complete,
                                        gather,
                                        brief,
                                    });
                                }
                                Err(diags) => {
                                    // Always repair feedback — never "accepted with diagnostics"
                                    // (C-17/F2): every `compile_turn` caller executes the plan it
                                    // gets back, so a diagnostics-carrying plan must not escape,
                                    // not even on the last step. If the budget runs out, the turn
                                    // is rejected with this text (below).
                                    let msg = join_diags(&diags);
                                    last_reject = format!(
                                        "invalid plan: {msg}. Fix it and call emit_plan again."
                                    );
                                    results.push(ContentBlock::tool_result_text(
                                        id,
                                        last_reject.clone(),
                                        true,
                                    ));
                                }
                            }
                        }
                        // A decode failure is repair feedback like any other rejection — record it
                        // (A-31) so the exhausted-budget error names the actual cause instead of
                        // the bare "did not produce a plan" that made s_360 undiagnosable.
                        Err(msg) => {
                            last_reject = msg.clone();
                            results.push(ContentBlock::tool_result_text(id, msg, true));
                        }
                    }
                }
                "ask_user" => {
                    let q = input
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no question)");
                    let answer = ask
                        .map(|a| a.ask(q))
                        .unwrap_or_else(|| "(no user)".to_string());
                    results.push(ContentBlock::tool_result_text(id, answer, false));
                }
                // Pure DAG: nothing but `emit_plan`/`ask_user` is advertised, so any other tool name is
                // a model error — there is no direct tool execution. Steer it back to `emit_plan`.
                // The steering is a rejection too (A-31): a model that only hallucinates direct
                // tools must exhaust the budget with this text as the reported cause.
                other => {
                    last_reject = format!(
                        "`{other}` is not callable — you have no direct tools. Put it in a plan node \
                         and call `emit_plan` instead."
                    );
                    results.push(ContentBlock::tool_result_text(
                        id,
                        last_reject.clone(),
                        true,
                    ));
                }
            }
        }
        messages.push(Message::user(results));
        trace_step(
            step,
            steps,
            stop_reason,
            &step_tool_names,
            step_reject(&step_reject_snapshot, &last_reject),
            diagnostic.as_ref(),
        );
        if let Some(c) = done {
            return Ok(TurnOutput::Plan(c));
        }
    }
    // Reject the turn with the last rejection's text (C-17/F2): the diagnostics are surfaced to the
    // caller as the turn's error instead of riding out on an "accepted" plan. "Attempt", not
    // "plan" (A-31): the recorded rejection may be a decode failure or a hallucinated direct tool
    // call — feedback the model saw without ever landing a plan.
    Err(Error::Other(if last_reject.is_empty() {
        format!("planner did not produce a plan within {steps} steps")
    } else {
        format!(
            "planner did not produce a valid plan within {steps} step(s) — \
             the last attempt was rejected: {last_reject}"
        )
    }))
}

/// Compile a single natural-language instruction into a [`DraftAst`] (the one-shot `--plan` surface).
/// A thin wrapper over [`compile_turn`]: a one-message conversation, where a prose-only answer (no plan)
/// is an error since that surface explicitly wants a graph.
// One meaningful argument per parameter, mirroring `compile_turn`.
#[allow(clippy::too_many_arguments)]
pub async fn plan(
    provider: &dyn Provider,
    model: &str,
    instruction: &str,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
    ask: Option<&dyn AskUser>,
    opts: CompileOptions,
) -> Result<Compiled> {
    let conversation = [Message::user_text(instruction)];
    // The one-shot `--plan` surface isn't phase-aware yet (design Part 1: unchanged in MVP, A-18
    // brings gather to it later) — always the execute/default contract.
    // This surface has no per-call usage ledger (the CLI prints the plan and exits), so the
    // C-31 usage side-channel is deliberately unused here — not silently lost in a `?`.
    let (out, _usage) = compile_turn(
        provider,
        model,
        &conversation,
        None,
        ops,
        view,
        ask,
        None,
        opts,
        Phase::Execute,
    )
    .await;
    match out? {
        TurnOutput::Plan(c) => Ok(c),
        TurnOutput::Chat(_) => Err(Error::Other(
            "the model answered without emitting a plan".to_string(),
        )),
    }
}

/// Parse the optional `complete` field of an `emit_plan` call into a [`Completion`]. Lenient: accepts a
/// bare string (`"summarize X"` → instructions, no primer) or an object (`{primer?, instructions}`).
/// Anything without usable `instructions` ⇒ `None`, so the engine simply loops (the model answers in
/// prose later) rather than completing on a malformed signal. `pub(crate)`: the loop host re-parses
/// the directive off the plan *value* it received (the same shape `plan` serialized it into).
pub(crate) fn parse_completion(value: Option<&serde_json::Value>) -> Option<Completion> {
    let value = value?;
    let nonempty = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    match value {
        serde_json::Value::String(s) => nonempty(s).map(|instructions| Completion {
            primer: None,
            instructions,
        }),
        serde_json::Value::Object(map) => {
            let instructions = map
                .get("instructions")
                .and_then(|v| v.as_str())
                .and_then(nonempty)?;
            let primer = map
                .get("primer")
                .and_then(|v| v.as_str())
                .and_then(nonempty);
            Some(Completion {
                primer,
                instructions,
            })
        }
        _ => None,
    }
}

/// Parse the optional `gather` field of an `emit_plan` call: a tolerant boolean read (`true`/`false`,
/// or the strings `"true"`/`"false"`, case-insensitive). Anything else — absent, malformed, or a
/// truthy-looking-but-not-boolean value — defaults to `false`: a gather round is an *opt-in*
/// stricter contract, so a malformed flag must never be silently upgraded to `true` and get gated
/// as if the model asked for it.
pub(crate) fn parse_gather(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Parse the optional `brief` field of an `emit_plan` call into a [`Brief`] — tolerant like
/// [`parse_completion`]: requires a non-empty `goal`; a missing/malformed `needs` defaults to empty
/// rather than discarding the whole brief (a goal with no listed needs is still useful grounding).
pub(crate) fn parse_brief(value: Option<&serde_json::Value>) -> Option<Brief> {
    let goal = value
        .and_then(|v| v.as_object())
        .and_then(|obj| obj.get("goal"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let needs = value
        .and_then(|v| v.as_object())
        .and_then(|obj| obj.get("needs"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Brief { goal, needs })
}

/// Render the turn's final user-facing message **after** the plan has run, grounded in its actual
/// results. The loop host calls this when a completion-carrying plan ran to success (A-06):
/// `conversation` is the working log (already extended with the user's request and the fed-back
/// `[results]`), so the model writes the summary from what really happened — never a pre-composed
/// promise. No tools and no op catalog are offered, so this call cannot recurse into planning; it
/// just produces prose. Returns the rendered text plus the call's token usage so the turn totals
/// stay honest.
pub async fn render_completion(
    provider: &dyn Provider,
    model: &str,
    conversation: &[Message],
    directive: &Completion,
    max_tokens: u32,
) -> Result<(String, Usage)> {
    let primer = directive
        .primer
        .as_deref()
        .map(|p| format!(" Context you already know: {p}."))
        .unwrap_or_default();
    let system = format!(
        "The plan has run and its results are in the conversation above. Write the final message to \
         the user now, grounded in those actual results — do not predict or invent outcomes, and do \
         not narrate the runtime mechanics.{primer}\n\nWrite the message per these instructions: \
         {instructions}\n\nRespond with the message text only — no tool calls, no preamble.",
        instructions = directive.instructions,
    );
    let req = Request {
        model: model.to_string(),
        system: Some(system),
        system_segments: Vec::new(),
        messages: conversation.to_vec(),
        tools: Vec::new(),
        max_tokens,
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        thinking: false,
        effort: None,
        metadata: serde_json::Map::new(),
    };
    let (result, usage) = stream_blocks(provider, req, None).await;
    let (mut blocks, acc_text, _stop, _diagnostic) = result?;
    if blocks.is_empty() && !acc_text.trim().is_empty() {
        blocks.push(ContentBlock::Text { text: acc_text });
    }
    Ok((Message::assistant(blocks).text(), usage))
}

/// A codec tolerated and skipped unparseable frames mid-stream (informational, not a failure — the
/// [`Chunk::StreamDiagnostic`] carried out of a call). Folded into `last_reject` by
/// [`compile_turn_inner`] only when the turn that carried it produced nothing usable; a turn that
/// produced real content despite the drops proceeds normally.
#[derive(Debug, Clone)]
struct StreamDiagnostic {
    dropped_frames: u32,
    detail: String,
}

/// A-33: classifies an error observed while consuming a provider's chunk stream as
/// model/provider-bytes-originated — safe to retry as a fresh identical call, priced against the
/// existing step budget — vs. everything else (transport/availability, which keeps its own
/// connection-level retry and must propagate). Scoped to this stream-consuming context ONLY: a
/// `Serde` error anywhere else in the codebase is not necessarily provider-originated.
fn is_stream_decode(e: &Error) -> bool {
    matches!(e, Error::StreamDecode(_) | Error::Serde(_))
}

// -- A-38: env-gated planner trace ("FLUX_PLANNER_TRACE=1") --------------------------------------
//
// The parse-resilience residual (`docs/designs/parse-resilience.md`), promoted in the
// stream-resilience epic once the A-33 backstop made planner failures *quieter* — retries instead
// of crashes — which is exactly what makes forensics harder without a trace: a s_360/s_368-class
// session used to just die loudly; now it can silently retry a few times and still land a plan,
// burning steps and tokens with nothing on stderr to show why. `FLUX_PLANNER_TRACE=1` writes one
// line per planner step: step index, stop reason, tool names, reject/decode text (A-31's
// `last_reject`), and dropped-frame diagnostics (A-33's `StreamDiagnostic`) — so the next
// occurrence needs zero ad-hoc instrumentation.
//
// Purely observational: everything here READS values `compile_turn_inner` already computed —
// `last_reject`'s semantics and the decode-classification logic are exactly A-33's, untouched.

/// One planner step's trace record — the structured payload behind a `FLUX_PLANNER_TRACE=1`
/// line. Test-observable (mirrors the C-19 `fallback_note_sink` pattern in
/// `flux-provider/src/lib.rs`): production formats this to stderr; a test installs a sink (see
/// `tests::install_trace_sink`) and asserts on these fields directly instead of scraping text.
#[derive(Debug, Clone, PartialEq)]
struct TraceRecord {
    step: u32,
    max_steps: u32,
    stop_reason: Option<StopReason>,
    tool_names: Vec<String>,
    reject: Option<String>,
    dropped_frames: Option<u32>,
    diagnostic_detail: Option<String>,
}

impl TraceRecord {
    /// Render the stderr line — one record, one line, `key=value` pairs so it stays greppable.
    fn to_line(&self) -> String {
        let stop_reason = self
            .stop_reason
            .map(|r| format!("{r:?}"))
            .unwrap_or_else(|| "-".to_string());
        let tools = if self.tool_names.is_empty() {
            "-".to_string()
        } else {
            self.tool_names.join(",")
        };
        let reject = self.reject.as_deref().unwrap_or("-");
        let dropped_frames = self
            .dropped_frames
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".to_string());
        let diagnostic = self.diagnostic_detail.as_deref().unwrap_or("-");
        format!(
            "flux: planner_trace step={step}/{max_steps} stop_reason={stop_reason} \
             tools={tools} reject={reject:?} dropped_frames={dropped_frames} \
             diagnostic={diagnostic:?}",
            step = self.step,
            max_steps = self.max_steps,
        )
    }
}

/// Test-only sink for [`TraceRecord`] — a thread-local rather than a struct field, since this
/// loop lives in free functions with no long-lived object to hang a field off (the shape
/// `NativeProvider::fallback_note_sink` uses for the same C-19 test-observable pattern one crate
/// over). `#[tokio::test]` runs on the default current-thread executor, so a sink installed here
/// stays visible across the `.await` points inside the call under test.
#[cfg(test)]
type TraceSink = std::sync::Arc<dyn Fn(&TraceRecord) + Send + Sync>;

#[cfg(test)]
thread_local! {
    static TRACE_SINK: std::cell::RefCell<Option<TraceSink>> = const { std::cell::RefCell::new(None) };
}

/// Whether the trace is active on this call path: a test sink installed on this thread, or the
/// real `FLUX_PLANNER_TRACE=1` env switch (read once and cached). Every trace call site checks
/// this FIRST and returns immediately when it's `false` — no record is built, nothing is cloned
/// or allocated — so tracing costs nothing on the hot path when disabled.
fn planner_trace_enabled() -> bool {
    #[cfg(test)]
    {
        if TRACE_SINK.with(|s| s.borrow().is_some()) {
            return true;
        }
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FLUX_PLANNER_TRACE").as_deref() == Ok("1"))
}

/// Deliver a trace record: the test sink when one is installed, else a stderr line.
fn emit_trace(record: &TraceRecord) {
    #[cfg(test)]
    {
        let delivered = TRACE_SINK.with(|s| -> bool {
            if let Some(sink) = s.borrow().as_ref() {
                sink(record);
                true
            } else {
                false
            }
        });
        if delivered {
            return;
        }
    }
    eprintln!("{}", record.to_line());
}

/// The reject text to attribute to THIS step — `None` unless `last_reject` was freshly set since
/// the step began (compared against the snapshot [`compile_turn_inner`] takes at the top of each
/// iteration), so a step that rejected nothing never echoes a stale rejection carried over from
/// an earlier step. Purely a read of `last_reject`; never writes it.
fn step_reject<'a>(snapshot: &Option<String>, last_reject: &'a str) -> Option<&'a str> {
    let before = snapshot.as_deref()?;
    (before != last_reject).then_some(last_reject)
}

/// Build and emit ONE [`TraceRecord`] for this planner step — a no-op (no allocation, no
/// `to_vec`/`to_string` calls) unless [`planner_trace_enabled`] is true.
fn trace_step(
    step: u32,
    max_steps: u32,
    stop_reason: Option<StopReason>,
    tool_names: &[String],
    reject: Option<&str>,
    diagnostic: Option<&StreamDiagnostic>,
) {
    if !planner_trace_enabled() {
        return;
    }
    emit_trace(&TraceRecord {
        step,
        max_steps,
        stop_reason,
        tool_names: tool_names.to_vec(),
        reject: reject.map(str::to_string),
        dropped_frames: diagnostic.map(|d| d.dropped_frames),
        diagnostic_detail: diagnostic.map(|d| d.detail.clone()),
    });
}

/// Stream a turn, collecting content blocks (tool_use, text), the accumulated text delta, and the
/// terminating `stop_reason`. The stop_reason matters: a `max_tokens` cutoff mid-`emit_plan` drops the
/// tool_use block (the provider never sends its `content_block_stop`), so the caller must distinguish a
/// truncated turn from a finished prose answer.
///
/// `on_thinking` receives each incremental thinking-token delta as it arrives; pass `None` when the
/// caller doesn't need live thinking output (e.g. the one-shot `compile` path).
///
/// The sink's reference lifetime and trait-object bound are deliberately independent (`+ 'b`):
/// `&mut` is invariant, so unifying them (the `&mut dyn AgentSink` default) would force a caller's
/// per-iteration reborrow to last as long as the original sink borrow — the lifetime cycle the old
/// `unsafe` raw-pointer reborrow in [`compile_turn`] existed to break (C-17/F4).
///
/// Usage rides OUTSIDE the `Result` (A-33, mirroring C-31's `compile_turn`/`compile_turn_with_arm`
/// shape one level down): a mid-stream decode error still spent real tokens on everything the call
/// streamed before it broke, and dropping that on the `?`-propagated error path is exactly the C-31
/// bug recurring here. Callers account the usage first, then branch on the outcome.
async fn stream_blocks<'a, 'b>(
    provider: &dyn Provider,
    req: Request,
    mut on_thinking: Option<&'a mut (dyn crate::AgentSink + 'b)>,
) -> (
    Result<(
        Vec<ContentBlock>,
        String,
        Option<StopReason>,
        Option<StreamDiagnostic>,
    )>,
    Usage,
) {
    let mut usage = Usage::default();
    let mut stream = match provider.stream(req).await {
        Ok(s) => s,
        Err(e) => return (Err(e), usage),
    };
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut stop_reason = None;
    let mut diagnostic = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(Chunk::ThinkingDelta(t)) => {
                if let Some(sink) = on_thinking.as_deref_mut() {
                    sink.thinking_delta(&t);
                }
            }
            Ok(Chunk::TextDelta(t)) => text.push_str(&t),
            Ok(Chunk::Block(b)) => blocks.push(b),
            // Providers emit usage cumulatively (the codec carries the input/cache counts from
            // `message_start` forward onto the final `message_delta`), so the last chunk holds the
            // complete picture — last wins.
            Ok(Chunk::Usage(u)) => usage = u,
            Ok(Chunk::Done { stop_reason: r }) => stop_reason = r,
            Ok(Chunk::StreamDiagnostic {
                dropped_frames,
                detail,
            }) => {
                diagnostic = Some(StreamDiagnostic {
                    dropped_frames,
                    detail,
                });
            }
            Ok(_) => {}
            // A-33: whatever usage arrived on earlier chunks of THIS call rides out alongside the
            // error — the caller accounts it before deciding whether the failure is a retryable
            // decode error or a fatal one.
            Err(e) => return (Err(e), usage),
        }
    }
    (Ok((blocks, text, stop_reason, diagnostic)), usage)
}

/// Extract `(id, name, input)` for every tool_use block in a message.
fn collect_tool_uses(msg: &Message) -> Vec<(String, String, serde_json::Value)> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

fn parse_draft_ast(text: &str) -> std::result::Result<DraftAst, String> {
    let json =
        extract_json(text).ok_or_else(|| "no JSON object found in model output".to_string())?;
    serde_json::from_str::<DraftAst>(&json).map_err(|e| format!("invalid AST JSON: {e}"))
}

/// Extract the AST JSON from model output: prefer a fenced ```json block, else the first balanced
/// `{ … }`.
fn extract_json(text: &str) -> Option<String> {
    for fence in ["```json", "```"] {
        if let Some(start) = text.find(fence) {
            let rest = &text[start + fence.len()..];
            if let Some(end) = rest.find("```") {
                let inner = rest[..end].trim();
                if inner.starts_with('{') {
                    return Some(inner.to_string());
                }
            }
        }
    }
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in text.as_bytes()[start..].iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..start + i + 1].to_string());
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn join_diags(diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The repair feedback for a plan calling registered-but-hidden ops (A-04). Shared by the
/// `emit_plan` branch and the plain-text plan fallback (C-17/F1), so both paths gate identically.
fn hidden_ops_rejection(hidden: &[String]) -> String {
    format!(
        "invalid plan: operation(s) `{}` exist but are not enabled \
         in this workspace (their tool group is not active — e.g. \
         `bash` requires `enable_shell = true` in .flux/config.toml \
         or FLUX_ENABLE_BASH=1). Re-plan using only operations from \
         the catalog, then call emit_plan again.",
        hidden.join("`, `")
    )
}

// -- A-13: gather-plan enforcement (design Part 1 — "enforced, not trusted") -------------------

/// Cap on call nodes in a `gather: true` plan (design Part 1 — "~12 call nodes"): keeps a gather
/// round small enough that it can't smuggle the whole task in disguised as "just gathering". A
/// composite call counts as ONE node — the cap bounds what the model emits in *this* plan, not a
/// composite's own expansion (whose effects `OpRegistry::mutating_ops_in` already vouches for).
const GATHER_NODE_CAP: usize = 12;

/// The repair feedback for a `gather: true` emission arriving in the execute phase (design Part 1):
/// the gather budget is spent by the time execute runs, so no further gather rounds are granted — a
/// rejection, not a silent downgrade to an ordinary plan, so the model corrects and re-emits rather
/// than the tag being quietly ignored.
const GATHER_REJECTED_IN_EXECUTE_PHASE: &str =
    "invalid: `gather: true` is not allowed in the execute phase — the gather budget is already \
     spent this turn. Call emit_plan with the ordinary execution plan (no `gather` tag) instead.";

/// Count every `Node::Call` in `body`, recursing into nested bodies (`each`/`repeat`/`parallel`/
/// `when`/…) — the size measure a `gather: true` plan is capped against ([`GATHER_NODE_CAP`]).
fn count_call_nodes(body: &[Node]) -> usize {
    let mut n = 0usize;
    for_each_node(body, &mut |node| {
        if matches!(node, Node::Call { .. }) {
            n += 1;
        }
    });
    n
}

/// Validate a `gather: true` plan against its enforced contract (design Part 1 — "enforced, not
/// trusted"): effect-clean (no write/destructive op — composites included, via the registry's own
/// validated effect metadata, see [`OpRegistry::mutating_ops_in`]) and capped at
/// [`GATHER_NODE_CAP`] call nodes. `None` means the plan passes; `Some(msg)` is repair feedback in
/// the same shape as the hidden-op rejection (C-17 style) — the model corrects and re-emits rather
/// than the plan silently running or the turn failing outright.
fn gather_violation(ast: &DraftAst, ops: &OpRegistry) -> Option<String> {
    let mutating = ops.mutating_ops_in(&ast.body);
    if !mutating.is_empty() {
        return Some(format!(
            "invalid gather plan: operation(s) `{}` are not read-only — a `gather: true` plan may \
             only call effect-clean operations (no write/destructive effects). Remove them, or drop \
             `gather: true` and emit the full execution plan instead, then call emit_plan again.",
            mutating.join("`, `")
        ));
    }
    let calls = count_call_nodes(&ast.body);
    if calls > GATHER_NODE_CAP {
        return Some(format!(
            "invalid gather plan: {calls} call node(s) exceeds the gather cap of {GATHER_NODE_CAP} \
             — a `gather: true` plan must stay small. Trim it to the essentials, or drop \
             `gather: true` and emit the full execution plan instead, then call emit_plan again."
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Prompts & tools
// ---------------------------------------------------------------------------

/// The Node grammar + a worked example (literal JSON; no format escaping).
fn ops_catalog(ops: &OpRegistry) -> String {
    let mut s = String::new();
    for sig in ops.signatures() {
        let effects = sig
            .effects
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join(",");
        s.push_str(&format!(
            "- {}({}) : {} [effects: {}; risk: {:?}]\n",
            sig.name,
            sig.param_signature(),
            sig.description,
            effects,
            sig.risk
        ));
    }
    s
}

/// Default line cap for the rendered symbols block (segment C). The block is uncached and re-sent
/// on EVERY planner call, so it must stay bounded even in a long session (A-07) — the cap applies
/// only to this rendering; `FlowStore::view` itself stays uncapped (it also serves symbol
/// resolution and context budgeting).
const SYMBOLS_LINE_CAP: usize = 64;
/// Hard **byte** backstop for the rendered symbols block, header + marker included (a few huge
/// summaries can blow the budget long before the line cap). This is a byte budget — it is measured
/// against `line.len()` / `block.len()`, not `chars().count()` — so the name and the bound agree
/// (A-24; the old "10k chars" claim was untrue for multibyte summaries).
const SYMBOLS_BYTE_CAP: usize = 10_000;

fn symbols_block(view: Option<&SessionView>) -> String {
    let cap = std::env::var("FLUX_SYMBOLS_CAP")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok());
    symbols_block_bounded(view, cap)
}

/// Render the symbols block bounded to `cap` lines (`None` → the default cap; `Some(0)` →
/// unbounded, the `FLUX_SYMBOLS_CAP=0` escape hatch). Pinned symbols always rank ahead of visible
/// ones, and the store's newest-updated-first order is preserved within each tier, so the freshest
/// working set survives. An overflowing line is dropped-and-continued (L-08 precedent) — one
/// oversized summary must not evict everything after it. Omitted symbols stay referencable (the
/// cap is on the *digest*, not the store), which the trailing marker states.
fn symbols_block_bounded(view: Option<&SessionView>, cap: Option<usize>) -> String {
    let v = match view {
        Some(v) if !v.symbols.is_empty() => v,
        _ => return String::new(),
    };
    let (line_cap, byte_cap) = match cap {
        Some(0) => (usize::MAX, usize::MAX),
        Some(n) => (n, SYMBOLS_BYTE_CAP),
        None => (SYMBOLS_LINE_CAP, SYMBOLS_BYTE_CAP),
    };
    let pinned = v
        .symbols
        .iter()
        .filter(|s| s.visibility == flux_lang::ast::Visibility::Pinned);
    let visible = v
        .symbols
        .iter()
        .filter(|s| s.visibility != flux_lang::ast::Visibility::Pinned);
    let mut s =
        String::from("\nExisting session symbols (reference these instead of re-fetching):\n");
    // Count the header AND reserve the worst-case omission marker up front so the finished block
    // (header + kept lines + marker) can't exceed `byte_cap` — both were omitted from the old tally,
    // which ran ~110 B over (A-24). Worst case: every symbol omitted, so reserve the marker for the
    // full count.
    let marker_reserve = symbols_omission_marker(v.symbols.len()).len();
    let mut used = s.len().saturating_add(marker_reserve);
    let mut kept = 0usize;
    let mut omitted = 0usize;
    for sym in pinned.chain(visible) {
        let ty = sym
            .ty
            .as_deref()
            .map(|t| format!(": {t}"))
            .unwrap_or_default();
        let line = format!("- ${}{} = {}\n", sym.name.0, ty, sym.summary);
        if kept >= line_cap || used.saturating_add(line.len()) > byte_cap {
            omitted += 1;
            continue;
        }
        used += line.len();
        kept += 1;
        s.push_str(&line);
    }
    if omitted > 0 {
        s.push_str(&symbols_omission_marker(omitted));
    }
    s
}

/// The trailing marker naming how many session symbols were dropped from the digest (they stay
/// referencable as `$name`). Factored out so [`symbols_block_bounded`] can reserve its length up
/// front and keep `block.len() <= SYMBOLS_BYTE_CAP` (A-24).
fn symbols_omission_marker(omitted: usize) -> String {
    format!("… {omitted} older symbol(s) omitted (still referencable as $name)\n")
}

/// The planner grammar: top-level AST shape + node kinds auto-generated from `Node` in `ast.rs`
/// via the derived JSON schema (`crate::schema`) -- never edit by hand. Memoized (see
/// [`build_ast_grammar`]) — it's a compile-time constant rebuilt on every planner call otherwise.
fn ast_grammar() -> String {
    static CELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CELL.get_or_init(build_ast_grammar).clone()
}

fn build_ast_grammar() -> String {
    format!(
        "The AST is a JSON object: {{\"name\"?:string, \"params\"?:[{{\"name\":string,\"ty\":type}}], \"returns\"?:type, \"body\":[Node,...]}}. A Node is tagged by \"kind\":\n\
{node_kinds}Independent reads/calls over a KNOWN set run CONCURRENTLY with `parallel` (each branch binds its result to its name) — `each` runs strictly in order, so keep it for steps that depend on each other, and for iterating a DYNAMIC list value (`parallel` branches are static). Prefer `each` over `repeat` for list iteration.\n\
\nArtifact types (the `Named` types ops produce/consume — use as a `ty`/`returns` or in a `ctx`/`need`): {artifact_types}.\n\
\nExample for \"read the readme then grep it for TODO\" (step 2 depends on step 1 — sequential):\n\
{{\"body\":[\n\
  {{\"kind\":\"bind\",\"name\":\"readme\",\"value\":{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"lit\",\"value\":\"README.md\"}}]}}}},\n\
  {{\"kind\":\"bind\",\"name\":\"hits\",\"value\":{{\"kind\":\"call\",\"op\":\"grep\",\"args\":[{{\"kind\":\"lit\",\"value\":\"TODO\"}}]}}}}\n\
]}}\n\
\nExample for \"read a.rs, b.rs and c.rs and summarise each\" (independent reads — concurrent):\n\
{{\"body\":[\n\
  {{\"kind\":\"parallel\",\"branches\":[\n\
    {{\"name\":\"a\",\"body\":[{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"lit\",\"value\":\"a.rs\"}}]}}]}},\n\
    {{\"name\":\"b\",\"body\":[{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"lit\",\"value\":\"b.rs\"}}]}}]}},\n\
    {{\"name\":\"c\",\"body\":[{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"lit\",\"value\":\"c.rs\"}}]}}]}}\n\
  ]}}\n\
]}}\n\
\nExample for \"read every file in $files\" (a dynamic list from an earlier step — `each` iterates it in order):\n\
{{\"body\":[\n\
  {{\"kind\":\"each\",\"in\":{{\"kind\":\"var\",\"name\":\"files\"}},\"as\":\"f\",\"body\":[\n\
    {{\"kind\":\"bind\",\"name\":\"text\",\"value\":{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"var\",\"name\":\"f\"}}]}}}}\n\
  ],\"collect\":\"all\"}}\n\
]}}",
        node_kinds = crate::schema::node_kind_catalog(),
        artifact_types = crate::prelude::PRELUDE_TYPES.join(", "),
    )
}

/// The `text`-arm grammar (L-20): teaches the SAME plan language through the **native text**
/// surface. Node semantics still come from the schema-derived catalog (the SSOT), and the worked
/// examples are [`ast_grammar`]'s own examples re-rendered through [`flux_lang::format::format`] —
/// so both arms teach byte-equivalent plans, and every text example is parseable by construction
/// (the format/parse round-trip invariant). The `expect`s run over a compile-time-constant string
/// and are guarded by `text_grammar_examples_parse_and_match_the_json_arm`. Memoized (see
/// [`build_text_grammar`]) — otherwise every planner call re-parses and re-formats each example.
fn text_grammar() -> String {
    static CELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CELL.get_or_init(build_text_grammar).clone()
}

fn build_text_grammar() -> String {
    let mut examples = String::new();
    for chunk in ast_grammar().split("\nExample for ").skip(1) {
        let intro = chunk.lines().next().unwrap_or_default();
        let json = extract_json(chunk).expect("grammar example carries a JSON AST");
        let ast: DraftAst =
            serde_json::from_str(&json).expect("grammar example parses as a DraftAst");
        examples.push_str(&format!(
            "\nExample for {intro}\n{}",
            flux_lang::format::format(&ast)
        ));
    }
    format!(
        "A plan is native Flux-Lang TEXT: a `flow` header at column 0 (optionally `flow name(param: Type, ...) -> Type`), then one statement per line, indented 2 spaces per block level (never tabs).\n\
Statements:\n\
- `$x = <expr>` — bind a result to `$x` (optional type annotation: `$x: String = <expr>`).\n\
- `do <op> <arg>, ...` — call an op and discard its result.\n\
- `when <cond>` / `else` and `unless <cond>` — branch (bodies indented one level).\n\
- `each $item in <expr> [-> $collected]` — iterate a list value IN ORDER (`-> $c` collects the per-item results).\n\
- `repeat <max> [-> $collected]` — bounded loop; an optional FIRST body line `until <cond>` exits early.\n\
- `parallel` with indented `branch $name` arms — run independent branches CONCURRENTLY; each branch binds its result to its `$name`.\n\
- `retry <max> [backoff <word>] [delay <ms>] [-> $bind]` — re-run the body on failure.\n\
- `assert <cond>[, \"message\"]` — fail the plan when the condition is falsy.\n\
- `return <expr>` — the flow's result.\n\
- `@json {{\"kind\":...}}` — single-line compact-JSON escape for any node with no text spelling.\n\
Expressions: `$var` (a bound symbol), `$var.field.path` (field access), JSON literals (`\"s\"`, `3`, `[\"a\"]`, `{{\"k\":\"v\"}}`), `op(<arg>, ...)` (inline call), `fmt(\"...{{var}}...\")` (string interpolation), and value templates with dynamic leaves: `{{ key: <expr>, ... }}` / `[ <expr>, ... ]`.\n\
Call a multi-param op with ONE object argument naming each parameter — `write({{\"path\":\"out.txt\",\"content\":\"hi\"}})`, or `write({{ path: \"out.txt\", content: $body }})` when a value is dynamic.\n\
\nNode semantics (each maps to a statement/expression above; anything else spells as `@json`):\n\
{node_kinds}Independent reads/calls over a KNOWN set run CONCURRENTLY with `parallel` (each branch binds its result to its name) — `each` runs strictly in order, so keep it for steps that depend on each other, and for iterating a DYNAMIC list value (`parallel` branches are static). Prefer `each` over `repeat` for list iteration.\n\
\nArtifact types (the `Named` types ops produce/consume — use as a param/return type): {artifact_types}.\n\
{examples}",
        node_kinds = crate::schema::node_kind_catalog(),
        artifact_types = crate::prelude::PRELUDE_TYPES.join(", "),
    )
}

/// The one-shot compiler's **static** system block (segment A): instructions + sorted op catalog +
/// grammar. Contains nothing per-session or per-attempt — symbols render as a separate uncached
/// segment and the instruction/repair exchanges ride as messages — so this block stays byte-stable
/// and prompt-cacheable across the repair loop (A-09).
fn build_oneshot_system(ops: &OpRegistry) -> String {
    format!(
        "You are Flux-Lang's compiler front-end. Convert the user's instruction into a Flux-Lang flow \
AST as JSON. Do NOT execute anything. Prefer deterministic operations; minimise model-dependent \
steps. Use ONLY operations from the catalog. Each op is shown as `name({{params}})` — call a multi-param op with a single object argument naming each parameter (e.g. `write({{path, content}})`); a sole-required-param op accepts a bare value (e.g. `read(\"README.md\")`). [optional] params may be omitted from the object.\n\nOperation catalog:\n\
{catalog}\n{grammar}\n\nOutput ONLY the JSON AST in a single ```json code block.\n",
        catalog = ops_catalog(ops),
        grammar = ast_grammar(),
    )
}

/// The planner's cache-first system layout (A-03), most-stable material first so provider prompt
/// caching hits:
///   A (cached) — planner instructions + sorted op catalog + grammar: byte-stable per
///       workspace/groups AND across every [`Phase`] (A-13) — the bulk of the prompt.
///   phase (cached) — the per-phase contract ([`phase_contract`]): a SEPARATE, byte-stable segment
///       appended immediately after A, so every phase shares A's cache prefix (A-13/A-03) — folding
///       phase text into segment A itself would invalidate that shared prefix on every phase change.
///   B (cached) — agent identity + project context + active skills (the engine's base system):
///       stable within a turn, drifts across turns/invocations (git status, skills) — its own
///       breakpoint keeps a B-only change from invalidating A's (or the phase segment's) cache.
///   C (uncached) — the per-iteration session symbols: they change every plan round, so they live
///       AFTER the last breakpoint where they can't invalidate anything.
fn assemble_system_segments(
    base_system: Option<&str>,
    ops: &OpRegistry,
    view: Option<&SessionView>,
    interactive: bool,
    arm: EmissionArm,
    phase: Phase,
) -> Vec<SystemSegment> {
    let mut segments = vec![
        SystemSegment {
            text: build_planner_prompt(ops, interactive, arm),
            cache: true,
        },
        SystemSegment {
            text: phase_contract(phase),
            cache: true,
        },
    ];
    if let Some(b) = base_system {
        if !b.trim().is_empty() {
            segments.push(SystemSegment {
                text: b.to_string(),
                cache: true,
            });
        }
    }
    let symbols = symbols_block(view);
    if !symbols.trim().is_empty() {
        segments.push(SystemSegment {
            text: symbols,
            cache: false,
        });
    }
    segments
}

/// The per-phase instruction segment (design Part 1, normative — `docs/designs/multipass-agent-loop.md`):
/// appended as its OWN byte-stable cached segment directly after segment A ([`build_planner_prompt`])
/// so every phase call shares A's cache prefix — segment A itself never varies by phase (asserted by
/// `phase_segments_do_not_perturb_segment_a`). [`Phase::Execute`] carries the "whole task in one
/// execution plan" guidance that used to live inside segment A (rescoped here, design Part 1) — a
/// phase-less call defaults to `Execute`, so its overall planner prompt (A + this segment) says
/// exactly what the single pre-phase segment used to say.
fn phase_contract(phase: Phase) -> String {
    match phase {
        Phase::Orient => "\
This is the turn's FIRST planning call (the orient pass). Choose exactly ONE of three responses:\n\
1. Trivial request (needs no operations, or is already answered by the conversation) — answer \
directly in prose; do not emit a plan.\n\
2. Simple, already-actionable request (you already know enough to do it in one shot) — call \
emit_plan with the FULL execution plan that accomplishes it, exactly as you would today. Put the \
WHOLE task in one execution plan rather than many tiny plans.\n\
3. Complex or context-hungry request (you need to see the repo/files/state before you can plan the \
real work) — call emit_plan with a SMALL, read-only plan (no writes, no destructive operations, at \
most 12 call nodes) tagged `gather: true`, plus a `brief` object: `{\"goal\": \"<what this turn is \
ultimately trying to accomplish>\", \"needs\": [\"<the specific things you still need to learn>\", \
...]}`. Only gather what you need to decide the real plan — you have a bounded number of these \
rounds before you must settle on a prose answer or an execution plan.\n"
            .to_string(),
        Phase::Gather => "\
This is a gather round: you already emitted a `gather: true` plan and its read-only results are in \
the conversation above. Either keep gathering — call emit_plan again with another small, read-only \
plan tagged `gather: true` (same constraints: no writes, no destructive operations, at most 12 call \
nodes) if you still need more context, carrying an updated `brief`; or settle — call emit_plan with \
the FULL execution plan now that you have enough context (put the WHOLE task in one execution plan \
rather than many tiny plans), or answer directly in prose if nothing further needs doing. You have a \
bounded number of gather rounds left, so settle as soon as you have what you need.\n"
            .to_string(),
        Phase::Execute => "\
Put the WHOLE task in one execution plan rather than many tiny plans. Any gather budget this turn \
had is already spent — do NOT tag a plan `gather: true` here; call emit_plan with the ordinary, \
complete execution plan (or the next corrective step) instead.\n"
            .to_string(),
    }
}

/// The **static** planner block (segment A): instructions + sorted op catalog + grammar. Contains
/// nothing per-turn — the session symbols render separately (an uncached trailing segment in
/// `compile_turn`) so this block stays byte-stable and prompt-cacheable across calls (A-03).
///
/// The `arm` selects the emission surface (L-20): `Json` interpolates to the exact pre-selector
/// prompt (byte-identical — the A/B control); `Text` swaps the plan-shape wording and teaches the
/// native-text grammar instead.
fn build_planner_prompt(ops: &OpRegistry, interactive: bool, arm: EmissionArm) -> String {
    let ask_line = if interactive {
        " and `ask_user` (ask ONE clarifying question only if the request is genuinely ambiguous)"
    } else {
        ""
    };
    let (plan_form, ast_word, grammar) = match arm {
        EmissionArm::Json => ("a Flux-Lang flow AST", "AST", ast_grammar()),
        EmissionArm::Text => ("native Flux-Lang source text", "plan", text_grammar()),
    };
    format!(
        "You are Flux-Lang's planning agent. For the user's request, either call `emit_plan` with ONE \
execution plan ({plan_form}) that accomplishes it, or — if the request needs no operations or is \
ALREADY satisfied by results shown earlier in the conversation — answer directly in prose (do NOT emit a \
plan, and do NOT repeat work already done).\n\nWhen a plan COMPLETES the request, attach `complete` to \
`emit_plan` — NOT the finished message, but `instructions` for it (e.g. \"summarize what changed and \
why\") plus an optional one-line `primer` of context you already know. The runtime runs the plan and \
THEN writes your final message to the user from the ACTUAL results per your `instructions`, and the \
turn ends. Never pre-write the closing summary yourself — you have not seen the results yet, so a \
summary you compose now can only promise output you cannot have. Omit `complete` whenever you need to \
SEE the results before you can answer or to keep working — you'll get the results back and can plan \
again or answer directly in prose (answering in prose ends the turn).\n\nYou have NO directly-callable tools except `emit_plan`\
{ask_line} — you cannot run `read`/`grep`/`bash`/etc. yourself. To gather information, put `read`/`grep`/\
`glob` as NODES in a plan and emit it; the runtime executes the plan and gives you the results, so you \
can plan the next step.\n\nIMPORTANT — \
express control flow as Flux-Lang nodes, NOT inside shell commands, so the plan stays auditable: use a \
`repeat` node for loops and a `when` node for branches — e.g. run the tests three times with \
`repeat max 3 {{ cargo_test() }}`, never a shell `for` loop. The generic `bash` op is OFF by default \
— prefer the dedicated ops; when `bash` IS enabled, keep each call to ONE discrete command (no \
`for`/`while`/`if`/`&&`/`;` chains).\n\nWhen the user asks to create, add, define, or register an operation, first check whether it can be expressed as a Flux-Lang composite op using existing operations. If yes, use `op.register` instead of editing Rust; default to `session` scope unless the user explicitly asks for project/global persistence. Only add a native Rust tool when the operation needs a new host capability, new IO primitive, or permanent built-in behavior.\n\nWhen your plan edits code, fold the build/test into the SAME plan and wrap the fix in a `retry` so a compile error is repaired automatically rather than handed back to the user; before an `edit`, make sure its `old_string` actually occurs in the file (a no-op edit silently spins the loop). Decide ordinary implementation choices (a flag's default, a helper name) yourself — only stop to ask on genuinely destructive or ambiguous decisions.\n\nThe {ast_word} may use ANY operation from the catalog; prefer deterministic ops and reference \
existing session symbols instead of re-fetching. To embed a stored symbol's value INSIDE a string \
argument (e.g. a `task` prompt or a message), write `{{symbol_name}}` — the runtime substitutes the \
value at execution; to pass a symbol's value as a whole argument, use it directly as a `var` node. Each \
op is shown as `name({{params}})` — call a multi-param op with a single object argument naming each parameter (e.g. `write({{path, content}})`); a sole-required-param op accepts a bare value (e.g. `read(\"README.md\")`). [optional] params may be omitted from the object.\n\nOperation catalog (for the {ast_word}):\n{catalog}\n{grammar}\n",
        catalog = ops_catalog(ops),
    )
}

/// The only tools the planner can call: the synthetic `emit_plan` (and `ask_user` when interactive).
/// There are NO directly-callable ops — every operation (reads included) is a node in the emitted AST,
/// so a turn is always an auditable plan (pure DAG). The `arm` picks `emit_plan`'s surface (L-20):
/// the strict derived `DraftAst` schema on `ast` (json, the unchanged default) or a native
/// Flux-Lang `source` string (text).
fn planner_tools(interactive: bool, arm: EmissionArm) -> Vec<ToolDef> {
    let mut tools: Vec<ToolDef> = Vec::new();
    tools.push(match arm {
        EmissionArm::Json => ToolDef {
            name: "emit_plan".to_string(),
            description: "Emit the Flux-Lang flow AST to run (your only way to act). Pass the AST as `ast`. \
                      If this plan completes the request, also pass `complete` — `instructions` for your \
                      final message (the runtime writes it from the actual results and ends the turn), \
                      NOT the message itself. Omit `complete` if you must see the results before you can \
                      answer, or to keep working; then answer in prose once done. When the current \
                      phase's instructions allow it, tag `gather: true` (with a `brief: {goal, needs[]}`) \
                      for a small, read-only round instead — see the phase instructions for when this \
                      applies and its limits."
                .to_string(),
            input_schema: tool_input_schema::<EmitPlanInput>(),
        },
        EmissionArm::Text => ToolDef {
            name: "emit_plan".to_string(),
            description: "Emit the Flux-Lang plan to run (your only way to act). Pass the plan as `source` — \
                      native Flux-Lang TEXT per the grammar: a `flow` header line at column 0, then \
                      statements indented 2 spaces per block level (never tabs). \
                      If this plan completes the request, also pass `complete` — `instructions` for your \
                      final message (the runtime writes it from the actual results and ends the turn), \
                      NOT the message itself. Omit `complete` if you must see the results before you can \
                      answer, or to keep working; then answer in prose once done. When the current \
                      phase's instructions allow it, tag `gather: true` (with a `brief: {goal, needs[]}`) \
                      for a small, read-only round instead — see the phase instructions for when this \
                      applies and its limits."
                .to_string(),
            input_schema: tool_input_schema::<EmitPlanTextInput>(),
        },
    });
    if interactive {
        tools.push(ToolDef {
            name: "ask_user".to_string(),
            description: "Ask the user one clarifying question; returns their reply.".to_string(),
            input_schema: tool_input_schema::<AskUserInput>(),
        });
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use flux_provider::ChunkStream;
    use flux_runtime::ToolRegistry;

    /// A provider that replays canned chunk sequences, one per `stream()` call.
    struct Mock {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
    }
    #[async_trait]
    impl Provider for Mock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn tool_call(name: &str, input: serde_json::Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: format!("{name}_1"),
                name: name.to_string(),
                input,
            }),
            Chunk::Done {
                stop_reason: Some(flux_core::StopReason::ToolUse),
            },
        ]
    }

    /// Like [`tool_call`] but with a `Usage` chunk, so a test can assert usage rides back out.
    fn tool_call_with_usage(name: &str, input: serde_json::Value, usage: Usage) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: format!("{name}_1"),
                name: name.to_string(),
                input,
            }),
            Chunk::Usage(usage),
            Chunk::Done {
                stop_reason: Some(flux_core::StopReason::ToolUse),
            },
        ]
    }

    fn mock(responses: Vec<Vec<Chunk>>) -> Mock {
        Mock {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }

    /// A-33: like [`Mock`], but a call's replayed sequence may include an `Err` mid-stream — the
    /// shape [`Mock`] can't express (it always wraps chunks in `Ok`). Used to exercise the
    /// stream-decode backstop without touching any real codec.
    struct MockChunks {
        responses: Mutex<VecDeque<Vec<Result<Chunk>>>>,
    }
    #[async_trait]
    impl Provider for MockChunks {
        fn name(&self) -> &str {
            "mock-chunks"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    fn mock_chunks(responses: Vec<Vec<Result<Chunk>>>) -> MockChunks {
        MockChunks {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }

    fn full_registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        flux_tools::register_builtins(&mut r);
        r
    }

    struct StubAsk {
        asked: Mutex<Vec<String>>,
        reply: String,
    }
    impl AskUser for StubAsk {
        fn ask(&self, q: &str) -> String {
            self.asked.lock().unwrap().push(q.to_string());
            self.reply.clone()
        }
    }

    const VALID_AST: &str =
        r#"{"ast":{"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}}"#;

    fn sym(name: &str, summary: &str, vis: flux_lang::ast::Visibility) -> crate::state::SymbolView {
        crate::state::SymbolView {
            name: flux_lang::ast::SymbolName(name.to_string()),
            ty: None,
            summary: summary.to_string(),
            visibility: vis,
        }
    }

    /// A-07: the rendered symbols digest is bounded — pinned symbols always survive, the newest
    /// visible ones fill the rest, and the marker names how many were omitted (they stay
    /// referencable; only the digest is capped).
    #[test]
    fn symbols_block_caps_lines_and_keeps_pinned_newest_first() {
        use flux_lang::ast::Visibility;
        // View order is newest-updated first. Put the pinned symbol LAST (oldest) — it must still
        // survive the cap ahead of newer visible symbols.
        let mut symbols: Vec<crate::state::SymbolView> = (0..10)
            .map(|i| sym(&format!("s{i}"), "v", Visibility::Visible))
            .collect();
        symbols.push(sym("keystone", "pinned", Visibility::Pinned));
        let view = SessionView { symbols };

        let block = symbols_block_bounded(Some(&view), Some(4));
        assert!(
            block.contains("$keystone"),
            "pinned survives the cap: {block}"
        );
        assert!(block.contains("$s0"), "newest visible survives: {block}");
        assert!(!block.contains("$s9"), "oldest visible is dropped: {block}");
        assert!(
            block.find("$keystone").unwrap() < block.find("$s0").unwrap(),
            "pinned ranks ahead of visible: {block}"
        );
        assert!(
            block.contains("7 older symbol(s) omitted"),
            "marker counts the omissions: {block}"
        );
        assert!(block.contains("still referencable"), "{block}");
    }

    /// A-07: `FLUX_SYMBOLS_CAP=0` disables the bound entirely.
    #[test]
    fn symbols_block_cap_zero_disables() {
        use flux_lang::ast::Visibility;
        let symbols: Vec<crate::state::SymbolView> = (0..SYMBOLS_LINE_CAP + 10)
            .map(|i| sym(&format!("s{i}"), "v", Visibility::Visible))
            .collect();
        let view = SessionView { symbols };
        let block = symbols_block_bounded(Some(&view), Some(0));
        assert!(
            !block.contains("omitted"),
            "cap 0 renders everything: {block}"
        );
        assert!(block.contains(&format!("$s{}", SYMBOLS_LINE_CAP + 9)));
    }

    /// A-07: one oversized summary is dropped-and-continued (L-08 precedent) — it must not evict
    /// the symbols after it.
    #[test]
    fn symbols_block_char_backstop_drops_oversized_and_continues() {
        use flux_lang::ast::Visibility;
        let huge = "x".repeat(SYMBOLS_BYTE_CAP + 1);
        let view = SessionView {
            symbols: vec![
                sym("small_before", "v", Visibility::Visible),
                sym("huge", &huge, Visibility::Visible),
                sym("small_after", "v", Visibility::Visible),
            ],
        };
        let block = symbols_block_bounded(Some(&view), None);
        assert!(block.contains("$small_before"));
        assert!(
            !block.contains(&huge),
            "oversized summary dropped: len {}",
            block.len()
        );
        assert!(
            block.contains("$small_after"),
            "drop-and-continue: the symbol after the oversized one survives: {block}"
        );
        assert!(block.contains("1 older symbol(s) omitted"));
    }

    /// A-24: the rendered symbols digest — header + kept lines + omission marker — never exceeds the
    /// byte cap (the header and marker were previously left out of the tally, running ~110 B over).
    #[test]
    fn symbols_block_stays_within_cap() {
        use flux_lang::ast::Visibility;
        // Many small symbols so the BYTE cap (not the line cap) is what bites: raise the line cap
        // well out of the way and pile on enough bytes to overflow SYMBOLS_BYTE_CAP several times.
        let symbols: Vec<crate::state::SymbolView> = (0..4000)
            .map(|i| {
                sym(
                    &format!("symbol_{i}"),
                    "a summary that adds up over time",
                    Visibility::Visible,
                )
            })
            .collect();
        let view = SessionView { symbols };
        let block = symbols_block_bounded(Some(&view), Some(1_000_000));
        assert!(
            block.len() <= SYMBOLS_BYTE_CAP,
            "symbols block must stay within its byte cap: len {} > cap {SYMBOLS_BYTE_CAP}",
            block.len()
        );
        // We deliberately overflowed, so the clip is still visible.
        assert!(
            block.contains("older symbol(s) omitted"),
            "a visible clip marker is present: {block}"
        );
    }

    /// A-09: every worked example in the grammar must parse as a real `DraftAst` (guards the prompt
    /// against AST drift forever), and the independent-reads example must teach `parallel` —
    /// `each` is strictly serial, so the old `each`-based example steered every model toward
    /// serialized reads.
    #[test]
    fn grammar_examples_parse_and_use_parallel_for_independent_reads() {
        use flux_lang::ast::Node;
        let grammar = ast_grammar();
        let examples: Vec<(String, DraftAst)> = grammar
            .split("Example for")
            .skip(1)
            .map(|chunk| {
                let json = extract_json(chunk).expect("worked example carries a JSON AST");
                let ast: DraftAst = serde_json::from_str(&json).unwrap_or_else(|e| {
                    panic!("worked example must parse as a DraftAst ({e}): {json}")
                });
                (chunk.to_string(), ast)
            })
            .collect();
        assert!(
            examples.len() >= 3,
            "expected the sequential, parallel and dynamic-list examples, got {}",
            examples.len()
        );
        let (_, parallel_example) = examples
            .iter()
            .find(|(intro, _)| intro.contains("summarise each"))
            .expect("the independent-reads example");
        assert!(
            parallel_example
                .body
                .iter()
                .any(|n| matches!(n, Node::Parallel { .. })),
            "independent reads must be taught as a `parallel` node, not a serial `each`"
        );
    }

    /// A-09: the one-shot repair loop must re-send the instructions+catalog+grammar as a byte-stable
    /// CACHED system segment (attempt 2 hits the provider prompt cache), with the repair exchange
    /// riding as messages — not as a re-inflated, uncached flat prompt.
    #[tokio::test]
    async fn oneshot_repair_reuses_a_byte_stable_cached_segment() {
        struct Capture {
            responses: Mutex<VecDeque<Vec<Chunk>>>,
            requests: Mutex<Vec<Request>>,
        }
        #[async_trait]
        impl Provider for Capture {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, req: Request) -> Result<ChunkStream> {
                self.requests.lock().unwrap().push(req);
                let chunks = self
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_default();
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }
        let text = |t: &str| {
            vec![
                Chunk::TextDelta(t.to_string()),
                Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                },
            ]
        };
        let provider = Capture {
            responses: Mutex::new(
                vec![
                    text("not a json object at all"),
                    text("```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"read\",\"args\":[{\"kind\":\"lit\",\"value\":\"x\"}]}]}\n```"),
                ]
                .into_iter()
                .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        };
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let compiled = compile(
            &provider,
            "m",
            "read x",
            &ops,
            None,
            CompileOptions::default(),
        )
        .await
        .expect("second attempt repairs");
        assert_eq!(compiled.attempts, 2);

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        // Segment A is cached and byte-identical across attempts — that is what makes the repair
        // round a cache HIT instead of a full re-pay of the catalog+grammar.
        assert!(!requests[0].system_segments.is_empty(), "segmented request");
        assert!(requests[0].system_segments[0].cache, "segment A is cached");
        assert_eq!(
            requests[0].system_segments, requests[1].system_segments,
            "system segments must be byte-stable across repair attempts"
        );
        // The repair context rides as messages: instruction, then previous output + repair note.
        assert_eq!(requests[0].messages.len(), 1);
        assert_eq!(requests[1].messages.len(), 3);
        assert!(requests[1].messages[2].text().contains("invalid"));
    }

    #[test]
    fn planner_advertises_only_emit_plan_and_ask_user() {
        // Pure DAG: the model has NO directly-callable ops — only `emit_plan` (+ `ask_user`).
        let names: Vec<String> = planner_tools(false, EmissionArm::Json)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["emit_plan"]);
        let interactive: Vec<String> = planner_tools(true, EmissionArm::Json)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(interactive, vec!["emit_plan", "ask_user"]);
    }

    #[test]
    fn planner_emit_plan_schema_is_the_real_ast_schema() {
        let tools = planner_tools(false, EmissionArm::Json);
        let emit = tools
            .iter()
            .find(|t| t.name == "emit_plan")
            .expect("emit_plan tool");
        assert_eq!(emit.input_schema["type"], "object");
        assert_eq!(emit.input_schema["required"], json!(["ast"]));
        assert!(emit.input_schema.get("$schema").is_none());
        assert!(emit.input_schema.get("title").is_none());

        let defs = emit
            .input_schema
            .get("definitions")
            .or_else(|| emit.input_schema.get("$defs"))
            .expect("emit_plan schema carries derived definitions");
        assert!(
            defs.get("DraftAst").is_some(),
            "schema must reference the DraftAst definition"
        );
        assert!(
            defs.get("Node").is_some(),
            "schema must include the Flux-Lang node union"
        );
        assert!(
            emit.input_schema.to_string().contains("\"call\""),
            "schema should expose concrete node kinds, not a placeholder object"
        );
    }

    #[test]
    fn planner_ask_user_schema_is_derived() {
        let tools = planner_tools(true, EmissionArm::Json);
        let ask = tools
            .iter()
            .find(|t| t.name == "ask_user")
            .expect("ask_user tool");
        assert_eq!(ask.input_schema["type"], "object");
        assert_eq!(ask.input_schema["required"], json!(["question"]));
        assert_eq!(ask.input_schema["properties"]["question"]["type"], "string");
        assert!(ask.input_schema.get("$schema").is_none());
        assert!(ask.input_schema.get("title").is_none());
    }

    // -- L-20: emission-arm selector (json control vs native-text treatment) ---------------------

    /// Drive one planner turn with an explicit arm (the hermetic A/B harness path). Fixed at the
    /// execute/default phase — these tests exercise arm behavior, not phase behavior.
    async fn turn_with_arm(
        p: &Mock,
        ops: &OpRegistry<'_>,
        arm: EmissionArm,
        opts: CompileOptions,
    ) -> Result<(TurnOutput, Usage)> {
        turn_with_phase(p, ops, arm, Phase::Execute, opts).await
    }

    /// Like [`turn_with_arm`], with the phase made explicit — the A-13 test entry point.
    /// Re-folds the C-31 `(Result, Usage)` shape into a `Result` tuple so the many outcome-focused
    /// tests stay untouched; usage-on-error tests call `compile_turn_with_arm` directly.
    async fn turn_with_phase(
        p: &Mock,
        ops: &OpRegistry<'_>,
        arm: EmissionArm,
        phase: Phase,
        opts: CompileOptions,
    ) -> Result<(TurnOutput, Usage)> {
        let (out, usage) = compile_turn_with_arm(
            p,
            "mock",
            &[Message::user_text("do it")],
            None,
            ops,
            None,
            None,
            None,
            opts,
            arm,
            phase,
        )
        .await;
        out.map(|o| (o, usage))
    }

    #[test]
    fn emission_arm_defaults_to_json_and_rejects_junk() {
        // The selector contract: unset/empty/`json` → Json (the shipped behavior), `text` → Text,
        // anything else is a hard error (no silent fallback mid-experiment).
        assert_eq!(EmissionArm::parse(None).unwrap(), EmissionArm::Json);
        assert_eq!(EmissionArm::parse(Some("")).unwrap(), EmissionArm::Json);
        assert_eq!(EmissionArm::parse(Some("json")).unwrap(), EmissionArm::Json);
        assert_eq!(EmissionArm::parse(Some("JSON")).unwrap(), EmissionArm::Json);
        assert_eq!(EmissionArm::parse(Some("text")).unwrap(), EmissionArm::Text);
        assert_eq!(
            EmissionArm::parse(Some(" Text ")).unwrap(),
            EmissionArm::Text
        );
        assert!(EmissionArm::parse(Some("yaml")).is_err());
        // With FLUX_EMISSION unset (the normal test env), the env read lands on the default arm.
        if std::env::var("FLUX_EMISSION").is_err() {
            assert_eq!(EmissionArm::from_env().unwrap(), EmissionArm::Json);
        }
    }

    #[test]
    fn json_arm_surface_is_byte_identical_when_unset() {
        // The default (env unset) arm must reproduce the pre-selector surface exactly: the strict
        // `ast` schema on emit_plan and the JSON grammar in the prompt — no trace of the text arm.
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let default_arm = EmissionArm::default();
        assert_eq!(default_arm, EmissionArm::Json);
        let prompt = build_planner_prompt(&ops, false, EmissionArm::Json);
        assert_eq!(
            prompt,
            build_planner_prompt(&ops, false, default_arm),
            "default arm builds the json prompt"
        );
        assert!(
            prompt.contains("(a Flux-Lang flow AST)") && prompt.contains("\"kind\":\"bind\""),
            "json prompt keeps the original wording and JSON worked examples"
        );
        assert!(
            !prompt.contains("native Flux-Lang source text") && !prompt.contains("`flow` header"),
            "no text-arm material leaks into the json prompt"
        );
        let tools = planner_tools(false, EmissionArm::Json);
        assert_eq!(tools[0].input_schema["required"], json!(["ast"]));
    }

    #[test]
    fn text_arm_advertises_a_source_string() {
        let tools = planner_tools(false, EmissionArm::Text);
        let emit = tools
            .iter()
            .find(|t| t.name == "emit_plan")
            .expect("emit_plan");
        assert_eq!(emit.input_schema["required"], json!(["source"]));
        assert_eq!(emit.input_schema["properties"]["source"]["type"], "string");
        assert!(emit.description.contains("native Flux-Lang TEXT"));
        // The prompt teaches the native grammar, not the JSON one.
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let prompt = build_planner_prompt(&ops, false, EmissionArm::Text);
        assert!(prompt.contains("native Flux-Lang source text"));
        assert!(prompt.contains("`flow` header at column 0"));
        assert!(
            !prompt.contains("{\"kind\":\"bind\""),
            "the JSON worked examples must not ride along in the text arm"
        );
    }

    /// The text grammar's worked examples are the JSON grammar's examples re-rendered through
    /// `format` — assert they appear, round-trip through `parse` to the SAME `DraftAst`s, and thus
    /// Memoization guard: the cached `ast_grammar`/`text_grammar` return exactly what a fresh
    /// (uncached) build produces, and successive calls are byte-stable. A caching bug that returned
    /// stale/empty content would break the cache-stable planner prefix (A-03) silently otherwise.
    #[test]
    fn memoized_grammars_match_the_fresh_build() {
        assert_eq!(ast_grammar(), build_ast_grammar());
        assert_eq!(text_grammar(), build_text_grammar());
        assert_eq!(ast_grammar(), ast_grammar());
        assert_eq!(text_grammar(), text_grammar());
        assert!(!ast_grammar().is_empty());
    }

    /// can never drift from the json arm (in-sync-by-construction, L-20).
    #[test]
    fn text_grammar_examples_parse_and_match_the_json_arm() {
        let text = text_grammar();
        let mut checked = 0usize;
        for chunk in ast_grammar().split("\nExample for ").skip(1) {
            let json = extract_json(chunk).expect("json example");
            let ast: DraftAst = serde_json::from_str(&json).expect("json example parses");
            let native = flux_lang::format::format(&ast);
            assert!(
                text.contains(&native),
                "text grammar carries the formatted twin of the json example: {native}"
            );
            assert_eq!(
                flux_lang::parse::parse(&native).expect("text example parses"),
                ast,
                "round-trip: the text example is the same plan as the json example"
            );
            checked += 1;
        }
        assert!(
            checked >= 3,
            "expected the three worked examples, got {checked}"
        );
    }

    /// L-20 acceptance: both arms produce a runnable plan for the same fixture task via a scripted
    /// provider — and the plans are identical `DraftAst`s (the arm changes the wire, not the plan).
    #[tokio::test]
    async fn both_arms_produce_the_same_runnable_plan_for_a_fixture_task() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);

        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::from_str(VALID_AST).unwrap(),
        )]);
        let (json_out, _) = turn_with_arm(&p, &ops, EmissionArm::Json, CompileOptions::default())
            .await
            .expect("json arm plans");

        let p = mock(vec![tool_call(
            "emit_plan",
            json!({"source": "flow\n  do read \"x\""}),
        )]);
        let (text_out, _) = turn_with_arm(&p, &ops, EmissionArm::Text, CompileOptions::default())
            .await
            .expect("text arm plans");

        match (json_out, text_out) {
            (TurnOutput::Plan(j), TurnOutput::Plan(t)) => {
                assert_eq!(j.ast, t.ast, "both surfaces carry the same plan");
                assert_eq!(j.attempts, 1);
                assert_eq!(t.attempts, 1);
            }
            other => panic!("both arms must produce a plan, got {other:?}"),
        }
    }

    /// A malformed `source` is repair feedback (the same loop as bad JSON): the model fixes the
    /// text and the corrected plan is accepted on the next step.
    #[tokio::test]
    async fn text_arm_parse_failure_is_repaired() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![
            // Missing the `flow` header — flux_lang::parse rejects it.
            tool_call("emit_plan", json!({"source": "do read \"x\""})),
            tool_call("emit_plan", json!({"source": "flow\n  do read \"x\""})),
        ]);
        let (out, _) = turn_with_arm(&p, &ops, EmissionArm::Text, CompileOptions::default())
            .await
            .unwrap();
        match out {
            TurnOutput::Plan(c) => {
                assert_eq!(c.attempts, 2, "the bad source must cost a repair round");
                assert!(
                    matches!(&c.ast.body[0], crate::ast::Node::Call { op, .. } if op == "read")
                );
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// A text-arm `emit_plan` that ignores the surface and passes `ast` instead of `source` gets
    /// steering feedback, then recovers.
    #[tokio::test]
    async fn text_arm_missing_source_is_repaired() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
            tool_call("emit_plan", json!({"source": "flow\n  do read \"x\""})),
        ]);
        let (out, _) = turn_with_arm(&p, &ops, EmissionArm::Text, CompileOptions::default())
            .await
            .unwrap();
        match out {
            TurnOutput::Plan(c) => assert_eq!(c.attempts, 2),
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// The C-17/A-04 gates hold in the text arm: a registered-but-hidden op (`bash`, shell group
    /// off) arriving as native text is rejected — even on the final step (no gate skipping).
    #[tokio::test]
    async fn text_arm_hidden_op_is_rejected_even_on_the_final_step() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        let p = mock(vec![tool_call(
            "emit_plan",
            json!({"source": "flow\n  do bash \"rm x\""}),
        )]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let err = turn_with_arm(&p, &ops, EmissionArm::Text, opts)
            .await
            .expect_err("a hidden-op text plan must never be accepted");
        assert!(
            format!("{err}").contains("not enabled"),
            "the rejection carries the gate's reason: {err}"
        );
    }

    /// The analyze/lower gate holds in the text arm: an unknown op in native text is repair
    /// feedback, never an accepted plan (C-17/F2 parity with the json arm).
    #[tokio::test]
    async fn text_arm_unknown_op_is_never_accepted() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![
            tool_call("emit_plan", json!({"source": "flow\n  do nope.op"})),
            tool_call("emit_plan", json!({"source": "flow\n  do nope.op"})),
        ]);
        let opts = CompileOptions {
            max_steps: 2,
            ..Default::default()
        };
        let err = turn_with_arm(&p, &ops, EmissionArm::Text, opts)
            .await
            .expect_err("an unknown-op plan must not be accepted when the budget runs out");
        assert!(
            format!("{err}").contains("unknown operation"),
            "the rejection carries the diagnostic text: {err}"
        );
    }

    #[tokio::test]
    async fn plan_rejects_a_bare_tool_call_then_emits() {
        // Pure DAG: the model has no directly-callable ops. If it tries to call one (here `read`), it
        // is told it has no direct tools; it then emits the op as a plan node instead.
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![
            tool_call("read", json!({ "path": "Cargo.toml" })),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.attempts, 2);
        assert_eq!(out.ast.body.len(), 1);
    }

    // -- parse resilience (A-30/A-31): stringified-ast tolerance + faithful failure surfacing ----

    /// A-30: the qwen-shaped double encoding (s_360/s_361) — `ast` arrives as a JSON *string*
    /// containing a valid plan. The decode unwraps that one encoding layer and accepts the plan
    /// exactly like its object twin, costing no repair round.
    #[tokio::test]
    async fn json_arm_accepts_a_string_encoded_ast() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let inner = r#"{"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}"#;
        let p = mock(vec![tool_call("emit_plan", json!({ "ast": inner }))]);
        let (out, _) = turn_with_arm(&p, &ops, EmissionArm::Json, CompileOptions::default())
            .await
            .expect("a string-encoded valid plan must be accepted");
        match out {
            TurnOutput::Plan(c) => {
                assert_eq!(c.attempts, 1, "a pure encoding quirk costs no repair round");
                let object_twin: DraftAst = serde_json::from_str(inner).unwrap();
                assert_eq!(c.ast, object_twin, "same plan as the object form");
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// A-30: tolerance is encoding-level ONLY — a string-encoded plan naming a hidden op is
    /// rejected exactly like its object twin (the A-04 gate sees the same `DraftAst`), even on
    /// the final step.
    #[tokio::test]
    async fn json_arm_string_encoded_hidden_op_is_still_rejected() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        let inner =
            r#"{"body":[{"kind":"call","op":"bash","args":[{"kind":"lit","value":"ls"}]}]}"#;
        let p = mock(vec![tool_call("emit_plan", json!({ "ast": inner }))]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let err = turn_with_arm(&p, &ops, EmissionArm::Json, opts)
            .await
            .expect_err("a hidden-op plan must never be accepted, string-encoded or not");
        assert!(
            format!("{err}").contains("not enabled"),
            "the rejection carries the gate's reason, not a decode error: {err}"
        );
    }

    /// A-30 guard + A-31: a string that is NOT valid JSON keeps the strict decode error, and the
    /// exhausted-budget turn error carries that cause — not the bare "did not produce a plan
    /// within N steps" that made s_360 undiagnosable.
    #[tokio::test]
    async fn json_arm_garbage_string_ast_surfaces_the_decode_error() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![tool_call(
            "emit_plan",
            json!({ "ast": "not json at all" }),
        )]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let err = turn_with_arm(&p, &ops, EmissionArm::Json, opts)
            .await
            .expect_err("garbage must never be silently accepted");
        assert!(
            format!("{err}").contains("invalid AST JSON"),
            "the turn error names the decode failure: {err}"
        );
    }

    /// C-31: a consultation that exhausts its budget still hands back the accumulated usage —
    /// the spend side-channel is failure-independent. s_360's failed turn made ~8 provider calls
    /// and surfaced none of them to accounting.
    #[tokio::test]
    async fn failed_consultation_still_returns_accumulated_usage() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let round = |input_tokens| Usage {
            input_tokens,
            output_tokens: 10,
            ..Default::default()
        };
        let p = mock(vec![
            tool_call_with_usage("emit_plan", json!({ "ast": 42 }), round(100)),
            tool_call_with_usage("emit_plan", json!({ "ast": 42 }), round(120)),
        ]);
        let opts = CompileOptions {
            max_steps: 2,
            ..Default::default()
        };
        let (out, usage) = compile_turn_with_arm(
            &p,
            "mock",
            &[Message::user_text("do it")],
            None,
            &ops,
            None,
            None,
            None,
            opts,
            EmissionArm::Json,
            Phase::Execute,
        )
        .await;
        let err = out.expect_err("an undecodable plan on every step must fail the turn");
        assert!(format!("{err}").contains("invalid AST JSON"), "{err}");
        // `Usage::accumulate` semantics: outputs sum across steps, input reflects the final step.
        assert_eq!(usage.output_tokens, 20, "both steps' outputs are counted");
        assert_eq!(usage.input_tokens, 120, "input reflects the final step");
    }

    /// A-31: a model that only hallucinates direct tool calls (never `emit_plan`) exhausts the
    /// budget with the steering feedback as the reported cause.
    #[tokio::test]
    async fn exhausted_budget_reports_the_not_callable_feedback() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![
            tool_call("read_file", json!({ "path": "x" })),
            tool_call("read_file", json!({ "path": "x" })),
        ]);
        let opts = CompileOptions {
            max_steps: 2,
            ..Default::default()
        };
        let err = turn_with_arm(&p, &ops, EmissionArm::Json, opts)
            .await
            .expect_err("hallucinated tools must not produce a plan");
        assert!(
            format!("{err}").contains("not callable"),
            "the turn error names the steering feedback: {err}"
        );
    }

    /// A-32 (s_368): a wire codec that could not parse the model's tool-call arguments even after
    /// repair carries the failure as a sentinel input instead of killing the stream — a truncated
    /// 19KB emit_plan blob used to be fatal to the turn (after seven accepted rounds). The planner
    /// must turn the sentinel into repair feedback, and the next attempt can still be accepted.
    #[tokio::test]
    async fn sentinel_args_are_rejected_and_the_turn_survives() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let sentinel = json!({
            (flux_core::ARGS_PARSE_ERROR_KEY): "EOF while parsing a list at line 1 column 19189",
            (flux_core::ARGS_RAW_PREFIX_KEY): "{\"ast\":{\"body\":[",
        });
        let good = json!({
            "ast": {"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}
        });
        let p = mock(vec![
            tool_call("emit_plan", sentinel),
            tool_call("emit_plan", good),
        ]);
        let (out, _) = turn_with_arm(&p, &ops, EmissionArm::Json, CompileOptions::default())
            .await
            .expect("the turn must survive a codec parse-error sentinel");
        match out {
            TurnOutput::Plan(c) => {
                assert_eq!(c.attempts, 2, "one repair round, then accepted");
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// A-32 + A-31: when every step carries the sentinel, the exhausted-budget error names the
    /// underlying parse failure — not the bare "did not produce a plan within N steps".
    #[tokio::test]
    async fn exhausted_budget_reports_the_args_parse_error() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let sentinel = json!({
            (flux_core::ARGS_PARSE_ERROR_KEY): "EOF while parsing a list at line 1 column 19189",
        });
        let p = mock(vec![tool_call("emit_plan", sentinel)]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let err = turn_with_arm(&p, &ops, EmissionArm::Json, opts)
            .await
            .expect_err("sentinel args must never be accepted as a plan");
        assert!(
            format!("{err}").contains("EOF while parsing a list"),
            "the turn error names the parse failure: {err}"
        );
    }

    // -- A-33: stream-decode backstop ---------------------------------------------------------------

    /// A-33: a mid-stream decode failure (malformed/truncated provider bytes) must cost one
    /// planner step, never the whole turn — the next identical call is the correct retry.
    #[tokio::test]
    async fn stream_decode_error_becomes_a_planner_retry_and_the_turn_survives() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let good = json!({
            "ast": {"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}
        });
        let p = mock_chunks(vec![
            vec![Err(Error::StreamDecode("boom".into()))],
            tool_call("emit_plan", good).into_iter().map(Ok).collect(),
        ]);
        let (out, _usage) = compile_turn_with_arm(
            &p,
            "mock",
            &[Message::user_text("do it")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            EmissionArm::Json,
            Phase::Execute,
        )
        .await;
        match out.expect("the turn must survive a mid-stream decode error") {
            TurnOutput::Plan(c) => {
                assert_eq!(
                    c.attempts, 2,
                    "one retry after the decode error, then accepted"
                );
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// A-33: when every step in the budget hits the decode error, the exhausted-budget error names
    /// the stream-decode failure as the cause — not the bare "did not produce a plan within N
    /// steps".
    #[tokio::test]
    async fn exhausted_budget_reports_the_stream_decode_error() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock_chunks(vec![vec![Err(Error::StreamDecode(
            "unexpected end of JSON input".into(),
        ))]]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let (out, _usage) = compile_turn_with_arm(
            &p,
            "mock",
            &[Message::user_text("do it")],
            None,
            &ops,
            None,
            None,
            None,
            opts,
            EmissionArm::Json,
            Phase::Execute,
        )
        .await;
        let err = out.expect_err("a decode error on every step must never be accepted as a plan");
        assert!(
            format!("{err}").contains("provider stream broke while decoding")
                && format!("{err}").contains("unexpected end of JSON input"),
            "the turn error names the stream-decode failure: {err}"
        );
    }

    /// A-33 (mirrors C-31's `failed_consultation_still_returns_accumulated_usage` one level down):
    /// a mid-stream decode error discards the call's blocks, but the usage that call already
    /// streamed before it broke must still reach the turn's accumulated total.
    #[tokio::test]
    async fn usage_survives_a_mid_stream_decode_error() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let good = json!({
            "ast": {"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}
        });
        let p = mock_chunks(vec![
            vec![
                Ok(Chunk::Usage(Usage {
                    input_tokens: 500,
                    output_tokens: 37,
                    ..Default::default()
                })),
                Err(Error::StreamDecode("boom".into())),
            ],
            tool_call_with_usage(
                "emit_plan",
                good,
                Usage {
                    input_tokens: 600,
                    output_tokens: 12,
                    ..Default::default()
                },
            )
            .into_iter()
            .map(Ok)
            .collect(),
        ]);
        let (out, usage) = compile_turn_with_arm(
            &p,
            "mock",
            &[Message::user_text("do it")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            EmissionArm::Json,
            Phase::Execute,
        )
        .await;
        out.expect("the turn must survive a mid-stream decode error");
        // `Usage::accumulate` sums output tokens across calls — the failed call's 37 must show up
        // alongside the accepted call's 12, proving the discarded call's spend wasn't dropped.
        assert_eq!(
            usage.output_tokens, 49,
            "the failed call's output tokens are counted alongside the accepted call's: {usage:?}"
        );
    }

    /// A-33: a stream that completes without error, but whose codec tolerated and skipped
    /// unparseable frames (`Chunk::StreamDiagnostic`) and produced no usable content, still names
    /// the diagnostic as the rejection cause when the budget runs out — not a bare "no plan"
    /// message with zero forensic value.
    #[tokio::test]
    async fn empty_turn_with_stream_diagnostic_sets_last_reject() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock_chunks(vec![vec![
            Ok(Chunk::StreamDiagnostic {
                dropped_frames: 3,
                detail: "unparseable data: frame at offset 118".to_string(),
            }),
            Ok(Chunk::Done {
                stop_reason: Some(flux_core::StopReason::EndTurn),
            }),
        ]]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let (out, _usage) = compile_turn_with_arm(
            &p,
            "mock",
            &[Message::user_text("do it")],
            None,
            &ops,
            None,
            None,
            None,
            opts,
            EmissionArm::Json,
            Phase::Execute,
        )
        .await;
        let err = out.expect_err("an empty turn must never be accepted as a plan");
        assert!(
            format!("{err}").contains("dropped frame")
                && format!("{err}").contains("unparseable data: frame at offset 118"),
            "the turn error names the stream diagnostic: {err}"
        );
    }

    // -- A-38: env-gated planner trace ---------------------------------------------------------------

    /// Install a trace sink for the guard's lifetime — the C-19 `fallback_note_sink`
    /// test-observable pattern (`flux-provider/src/lib.rs`) adapted to a thread-local since this
    /// loop has no long-lived object to hang a field off. `#[tokio::test]` defaults to the
    /// current-thread executor, so the thread-local set here is visible across every `.await`
    /// point inside the call under test.
    struct TraceSinkGuard;
    impl Drop for TraceSinkGuard {
        fn drop(&mut self) {
            TRACE_SINK.with(|s| *s.borrow_mut() = None);
        }
    }
    fn install_trace_sink(sink: TraceSink) -> TraceSinkGuard {
        TRACE_SINK.with(|s| *s.borrow_mut() = Some(sink));
        TraceSinkGuard
    }

    /// Failing-first (A-38): with the trace sink installed — the test-observable equivalent of
    /// `FLUX_PLANNER_TRACE=1` — a rejected step's record carries the step index, the stop
    /// reason, the tool name(s) called, and the reject text; the accepted step that follows must
    /// NOT echo the earlier step's stale rejection.
    #[tokio::test]
    async fn planner_trace_records_step_stop_reason_and_reject_when_enabled() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        // Step 1: emit_plan references an op that doesn't exist at all → `validate_plan` rejects
        // it with an "unknown operation" diagnostic. Step 2: emit_plan calls a real op → accepted.
        let bad = json!({
            "ast": {"body":[{"kind":"call","op":"totally.unknown","args":[]}]}
        });
        let good = json!({
            "ast": {"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}
        });
        let p = mock(vec![
            tool_call("emit_plan", bad),
            tool_call("emit_plan", good),
        ]);

        let records: Arc<Mutex<Vec<TraceRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_records = records.clone();
        let guard = install_trace_sink(Arc::new(move |r: &TraceRecord| {
            sink_records.lock().unwrap().push(r.clone());
        }));

        let (out, _usage) = compile_turn_with_arm(
            &p,
            "mock",
            &[Message::user_text("do it")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            EmissionArm::Json,
            Phase::Execute,
        )
        .await;
        drop(guard);
        out.expect("the turn accepts the plan on the second step");

        let records = records.lock().unwrap();
        assert_eq!(
            records.len(),
            2,
            "one trace record per planner step: {records:?}"
        );

        let rejected = &records[0];
        assert_eq!(rejected.step, 1);
        assert_eq!(rejected.max_steps, CompileOptions::default().max_steps);
        assert_eq!(rejected.stop_reason, Some(flux_core::StopReason::ToolUse));
        assert_eq!(rejected.tool_names, vec!["emit_plan".to_string()]);
        assert!(
            rejected
                .reject
                .as_deref()
                .is_some_and(|r| r.contains("totally.unknown")),
            "the rejected step's trace names the unknown op: {rejected:?}"
        );

        let accepted = &records[1];
        assert_eq!(accepted.step, 2);
        assert_eq!(accepted.tool_names, vec!["emit_plan".to_string()]);
        assert!(
            accepted.reject.is_none(),
            "the accepted step must not echo the earlier step's stale rejection: {accepted:?}"
        );
    }

    // -- A-03: cache-first system segmentation ----------------------------------------------------

    #[test]
    fn system_segments_keep_the_static_prefix_stable_across_symbol_changes() {
        // The whole point of the layout: per-turn symbols must not perturb the cached segments.
        // Segments A (planner), the phase segment, and B (base system) are byte-identical whether
        // or not symbols exist; the symbols ride in a trailing UNCACHED segment.
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let view = SessionView {
            symbols: vec![crate::state::SymbolView {
                name: crate::ast::SymbolName("notes".into()),
                ty: None,
                summary: "the gathered notes".into(),
                visibility: crate::ast::Visibility::Visible,
            }],
        };
        let without = assemble_system_segments(
            Some("identity"),
            &ops,
            None,
            false,
            EmissionArm::Json,
            Phase::Execute,
        );
        let with = assemble_system_segments(
            Some("identity"),
            &ops,
            Some(&view),
            false,
            EmissionArm::Json,
            Phase::Execute,
        );

        assert_eq!(
            without.len(),
            3,
            "planner + phase + base, no symbols segment"
        );
        assert_eq!(with.len(), 4, "planner + phase + base + symbols");
        assert_eq!(
            with[0].text, without[0].text,
            "segment A must be byte-stable"
        );
        assert_eq!(
            with[1].text, without[1].text,
            "the phase segment must be byte-stable"
        );
        assert_eq!(
            with[2].text, without[2].text,
            "segment B must be byte-stable"
        );
        assert!(
            with[0].cache && with[1].cache && with[2].cache,
            "static segments carry breakpoints"
        );
        assert!(!with[3].cache, "the symbols segment must be uncached");
        assert!(with[3].text.contains("$notes"));
        assert!(
            !with[0].text.contains("$notes")
                && !with[1].text.contains("$notes")
                && !with[2].text.contains("$notes"),
            "symbols must not leak into the cached segments"
        );
    }

    /// A-13/A-03: segment A (the shared instructions+catalog+grammar) must be byte-identical no
    /// matter which phase is calling — only the per-phase segment (appended right after it) may
    /// vary, so every phase shares A's cache prefix. This is the story's named acceptance test.
    #[test]
    fn phase_segments_do_not_perturb_segment_a() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let orient =
            assemble_system_segments(None, &ops, None, false, EmissionArm::Json, Phase::Orient);
        let gather =
            assemble_system_segments(None, &ops, None, false, EmissionArm::Json, Phase::Gather);
        let execute =
            assemble_system_segments(None, &ops, None, false, EmissionArm::Json, Phase::Execute);

        assert_eq!(
            orient[0].text, gather[0].text,
            "segment A: orient vs gather"
        );
        assert_eq!(
            orient[0].text, execute[0].text,
            "segment A: orient vs execute"
        );
        assert!(
            orient[0].cache && gather[0].cache && execute[0].cache,
            "segment A stays a cached breakpoint in every phase"
        );

        // The phase segment itself must differ (it carries the actual per-phase contract) and still
        // be its own cached, byte-stable segment.
        assert_ne!(
            orient[1].text, execute[1].text,
            "orient and execute phase segments differ"
        );
        assert_ne!(
            gather[1].text, execute[1].text,
            "gather and execute phase segments differ"
        );
        assert!(
            orient[1].cache && gather[1].cache && execute[1].cache,
            "the phase segment is cached too"
        );

        // The rescoped "whole task in one plan" instruction now lives in the phase segment, not
        // segment A — it must never appear in segment A (else editing it would perturb the shared
        // cached prefix on every phase change).
        assert!(!orient[0].text.contains("Put the WHOLE task"));
        assert!(!gather[0].text.contains("Put the WHOLE task"));
        assert!(!execute[0].text.contains("Put the WHOLE task"));
        assert!(execute[1]
            .text
            .contains("Put the WHOLE task in one execution plan"));
    }

    /// A-13: a phase-less call defaults to `Execute`; its overall planner prompt (segment A + the
    /// execute phase segment) must say exactly what the single pre-phase segment used to say — just
    /// split across two cached segments instead of one.
    #[test]
    fn execute_phase_is_byte_compatible_with_the_pre_phase_prompt() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let segments =
            assemble_system_segments(None, &ops, None, false, EmissionArm::Json, Phase::Execute);
        let combined = format!("{}{}", segments[0].text, segments[1].text);
        assert!(combined
            .contains("Put the WHOLE task in one execution plan rather than many tiny plans."));
        assert!(combined.contains("You have NO directly-callable tools except `emit_plan`"));
        assert!(
            !segments[0].text.contains("gather: true"),
            "segment A must stay phase-agnostic: {}",
            segments[0].text
        );
    }

    // -- A-04: surfacing enforcement (hidden ops in model-emitted plans) -------------------------

    /// The advertised set = everything except `bash` — the shape a turn has when the `shell` group
    /// is off (its op registered for pre-authored flows, hidden from the model).
    fn ops_without_bash(reg: &ToolRegistry) -> OpRegistry<'_> {
        let advertised: std::collections::HashSet<String> = OpRegistry::new(reg)
            .op_names()
            .into_iter()
            .filter(|n| n != "bash")
            .collect();
        OpRegistry::new(reg).with_advertised(advertised)
    }

    const BASH_AST: &str =
        r#"{"ast":{"body":[{"kind":"call","op":"bash","args":[{"kind":"lit","value":"rm x"}]}]}}"#;

    #[tokio::test]
    async fn hidden_op_plan_is_rejected_and_repaired() {
        // A model-emitted plan calling a registered-but-not-advertised op (`bash`, shell group off)
        // is rejected with the "not enabled" diagnostic — it must never reach dispatch. The model
        // then recovers with a plan using surfaced ops.
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        let p = mock(vec![
            tool_call("emit_plan", serde_json::from_str(BASH_AST).unwrap()),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "delete x",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.attempts, 2, "the bash plan must cost a repair round");
        assert_eq!(out.ast.body.len(), 1);
        assert!(
            matches!(&out.ast.body[0], crate::ast::Node::Call { op, .. } if op == "read"),
            "the accepted plan is the repaired one"
        );
    }

    #[tokio::test]
    async fn hidden_op_plan_is_rejected_even_on_the_final_repair_step() {
        // Ordinary diagnostics are tolerated on the last step ("accepted with diagnostics"); a
        // hidden op must NOT be — safety can't depend on the repair budget running out.
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::from_str(BASH_AST).unwrap(),
        )]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let res = plan(&p, "mock", "delete x", &ops, None, None, opts).await;
        assert!(
            res.is_err(),
            "a hidden-op plan must never be accepted, even on the last step"
        );
    }

    /// C-17 (F1): the SAME gate applies when the model emits the plan as plain text instead of
    /// calling `emit_plan` — a gated op must never reach dispatch through the text fallback either.
    /// Mirrors `hidden_op_plan_is_rejected_and_repaired` above.
    #[tokio::test]
    async fn hidden_op_text_fallback_plan_is_rejected_and_repaired() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        // A prose-JSON plan (no tool call) naming the hidden op — parses, analyzes clean (bash IS
        // registered), and before C-17 sailed straight through as an executable TurnOutput::Plan.
        let text_plan = "```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"bash\",\"args\":[{\"kind\":\"lit\",\"value\":\"rm x\"}]}]}\n```";
        let p = mock(vec![
            text_chunk(text_plan),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "delete x",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.attempts, 2, "the text plan must cost a repair round");
        assert!(
            matches!(&out.ast.body[0], crate::ast::Node::Call { op, .. } if op == "read"),
            "the accepted plan is the repaired one, not the hidden-op text plan"
        );
    }

    /// C-17 (F1): like the tool-call path, the text fallback rejects a hidden op even when the step
    /// budget is exhausted — and the turn's rejection names why.
    #[tokio::test]
    async fn hidden_op_text_fallback_is_rejected_even_on_the_final_step() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        let text_plan = "```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"bash\",\"args\":[{\"kind\":\"lit\",\"value\":\"rm x\"}]}]}\n```";
        let p = mock(vec![text_chunk(text_plan)]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let err = plan(&p, "mock", "delete x", &ops, None, None, opts)
            .await
            .expect_err("a hidden-op text plan must never be accepted, even on the last step");
        assert!(
            format!("{err}").contains("not enabled"),
            "the rejection carries the gate's reason: {err}"
        );
    }

    /// C-17 (F2): `compile_turn` must NEVER return an accepted plan carrying diagnostics — every
    /// turn caller executes what it gets back, and the `Compiled` contract says diagnostics are
    /// surfaced rather than executed. Exhausting the repair budget on an unknown-op plan rejects
    /// the turn with the diagnostic text instead of "accepting with diagnostics".
    #[tokio::test]
    async fn unknown_op_plan_is_never_accepted_with_diagnostics() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let invalid = r#"{"ast":{"body":[{"kind":"call","op":"nope.op","args":[]}]}}"#;
        let p = mock(vec![
            tool_call("emit_plan", serde_json::from_str(invalid).unwrap()),
            tool_call("emit_plan", serde_json::from_str(invalid).unwrap()),
        ]);
        let opts = CompileOptions {
            max_steps: 2,
            ..Default::default()
        };
        let err = plan(&p, "mock", "do it", &ops, None, None, opts)
            .await
            .expect_err("an unknown-op plan must not be accepted when the budget runs out");
        assert!(
            format!("{err}").contains("unknown operation"),
            "the rejection carries the diagnostic text: {err}"
        );
    }

    /// C-17 (F3): two `emit_plan` calls in one assistant message are rejected with repair feedback
    /// — never silently last-wins. The model recovers by emitting ONE plan on the next round.
    #[tokio::test]
    async fn duplicate_emit_plan_calls_are_rejected_not_last_wins() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let plan_a = json!({"ast": {"body": [{"kind":"call","op":"glob","args":[{"kind":"lit","value":"a"}]}]}});
        let plan_b = json!({"ast": {"body": [{"kind":"call","op":"grep","args":[{"kind":"lit","value":"b"}]}]}});
        let double = vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "ep_1".into(),
                name: "emit_plan".into(),
                input: plan_a,
            }),
            Chunk::Block(ContentBlock::ToolUse {
                id: "ep_2".into(),
                name: "emit_plan".into(),
                input: plan_b,
            }),
            Chunk::Done {
                stop_reason: Some(flux_core::StopReason::ToolUse),
            },
        ];
        let p = mock(vec![
            double,
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            out.attempts, 2,
            "the duplicate message must cost a repair round"
        );
        assert!(
            matches!(&out.ast.body[0], crate::ast::Node::Call { op, .. } if op == "read"),
            "the accepted plan is the single re-emitted one — neither duplicate wins"
        );
    }

    #[test]
    fn hidden_ops_in_reports_only_registered_unadvertised_calls() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg);
        let ast: DraftAst = serde_json::from_str(
            r#"{"body":[
                {"kind":"call","op":"bash","args":[{"kind":"lit","value":"ls"}]},
                {"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]},
                {"kind":"call","op":"totally.unknown","args":[]}
            ]}"#,
        )
        .unwrap();
        // `bash` is registered but hidden → reported. `read` is advertised → not reported.
        // `totally.unknown` is not registered → analyze_flow's unknown-op diagnostic owns it.
        assert_eq!(ops.hidden_ops_in(&ast.body), vec!["bash".to_string()]);
        // An unrestricted registry (pre-authored paths) reports nothing.
        assert!(OpRegistry::new(&reg).hidden_ops_in(&ast.body).is_empty());
    }

    /// L-30: a turn/session-scoped composite (`op.register`) that wraps a hidden op. Its own
    /// declared `effects`/`risk` cover the body correctly (so `analyze_composites` accepts
    /// registration — that check runs against the FULL registry, not this turn's advertised set),
    /// but the composite's body calls `bash`, which is hidden this turn.
    fn bash_composite() -> flux_lang::program::CompositeOpDecl {
        use flux_lang::program::CompositeOpMeta;
        flux_lang::program::CompositeOpDecl {
            name: "wrap_bash".into(),
            body: DraftAst {
                body: vec![Node::Call {
                    op: "bash".into(),
                    args: vec![Node::Lit {
                        value: json!("rm x"),
                    }],
                }],
                ..Default::default()
            },
            meta: CompositeOpMeta {
                effects: vec![flux_spec::Effect::Process, flux_spec::Effect::LocalSystem],
                risk: flux_spec::Risk::High,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn hidden_ops_in_expands_composite_bodies_transitively() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg).with_owned_composites(vec![bash_composite()]);
        let ast: DraftAst =
            serde_json::from_str(r#"{"body":[{"kind":"call","op":"wrap_bash","args":[]}]}"#)
                .unwrap();
        // The composite call itself is never reported (composites aren't gated by advertisement),
        // but the hidden `bash` op inside its body IS surfaced — a turn-scoped composite can't
        // launder a hidden op past the gate by wrapping it.
        assert_eq!(ops.hidden_ops_in(&ast.body), vec!["bash".to_string()]);
    }

    /// The story's named failing-first test (L-30): mirrors `hidden_op_plan_is_rejected_and_repaired`
    /// above, but the hidden op is reached through a registered composite rather than named directly.
    /// Before the fix, `compile_turn` accepted this plan outright — `hidden_ops_in` exempted every
    /// composite call without ever looking at its body.
    #[tokio::test]
    async fn hidden_op_behind_a_composite_is_rejected_and_repaired() {
        let reg = full_registry();
        let ops = ops_without_bash(&reg).with_owned_composites(vec![bash_composite()]);
        let wrap_bash_ast = r#"{"ast":{"body":[{"kind":"call","op":"wrap_bash","args":[]}]}}"#;
        let p = mock(vec![
            tool_call("emit_plan", serde_json::from_str(wrap_bash_ast).unwrap()),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "delete x",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            out.attempts, 2,
            "the composite-wrapped bash plan must cost a repair round"
        );
        assert!(
            matches!(&out.ast.body[0], crate::ast::Node::Call { op, .. } if op == "read"),
            "the accepted plan is the repaired one, not the composite hiding bash"
        );
    }

    // -- A-13: gather-plan enforcement (design Part 1 — "enforced, not trusted") -------------------

    fn write_composite() -> flux_lang::program::CompositeOpDecl {
        use flux_lang::program::CompositeOpMeta;
        flux_lang::program::CompositeOpDecl {
            name: "wrap_write".into(),
            body: DraftAst {
                body: vec![Node::Call {
                    op: "write".into(),
                    args: vec![Node::Lit {
                        value: json!({"path": "out.txt", "content": "x"}),
                    }],
                }],
                ..Default::default()
            },
            meta: CompositeOpMeta {
                // A composite's OWN declared risk/effects must already cover its body transitively
                // (enforced by `analyze_composites` at registration) — this is what
                // `mutating_ops_in` reads, so it catches the write without expanding the body itself.
                effects: vec![flux_spec::Effect::Write, flux_spec::Effect::Filesystem],
                risk: flux_spec::Risk::Medium,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn mutating_ops_in_flags_write_effect_and_composite_transitively() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg).with_owned_composites(vec![write_composite()]);
        let ast: DraftAst = serde_json::from_str(
            r#"{"body":[
                {"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]},
                {"kind":"call","op":"wrap_write","args":[]}
            ]}"#,
        )
        .unwrap();
        // `read` is effect-clean; `wrap_write` (a composite) is flagged via its OWN declared
        // effects/risk — no separate body expansion needed.
        assert_eq!(
            ops.mutating_ops_in(&ast.body),
            vec!["wrap_write".to_string()]
        );

        let clean: DraftAst = serde_json::from_str(
            r#"{"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]}"#,
        )
        .unwrap();
        assert!(ops.mutating_ops_in(&clean.body).is_empty());

        // `git_status` et al. carry `Effect::Process` (no `Write`) at `Risk::Low` — genuinely
        // read-only gather-phase ops must not be misclassified as mutating.
        let git_status: DraftAst =
            serde_json::from_str(r#"{"body":[{"kind":"call","op":"git_status","args":[]}]}"#)
                .unwrap();
        assert!(OpRegistry::new(&reg)
            .mutating_ops_in(&git_status.body)
            .is_empty());
    }

    /// The story's named failing-first test: a `gather: true` plan calling a write op is repair
    /// feedback (C-17-style), never a hard turn error and never silently executed.
    #[tokio::test]
    async fn mutating_gather_plan_is_repair_feedback() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let mutating_gather = json!({
            "ast": {"body": [{"kind":"call","op":"write","args":[{"kind":"lit","value":{"path":"out.txt","content":"x"}}]}]},
            "gather": true,
            "brief": {"goal": "figure out what to change", "needs": ["current file contents"]},
        });
        let clean_gather = json!({
            "ast": {"body": [{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]},
            "gather": true,
            "brief": {"goal": "figure out what to change", "needs": ["current file contents"]},
        });
        let p = mock(vec![
            tool_call("emit_plan", mutating_gather),
            tool_call("emit_plan", clean_gather),
        ]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .expect("a mutating gather plan is repair feedback, not a hard error");
        match out {
            TurnOutput::Plan(c) => {
                assert_eq!(
                    c.attempts, 2,
                    "the mutating gather plan must cost a repair round"
                );
                assert!(c.gather, "the repaired plan is still tagged gather");
                assert_eq!(
                    c.brief.as_ref().map(|b| b.goal.as_str()),
                    Some("figure out what to change")
                );
                assert!(
                    matches!(&c.ast.body[0], Node::Call { op, .. } if op == "read"),
                    "the accepted plan is the effect-clean repair"
                );
            }
            TurnOutput::Chat(t) => panic!("expected a gather plan, got chat: {t}"),
        }
    }

    /// A `gather: true` plan calling a composite whose OWN declared effects/risk cover a hidden
    /// write is rejected exactly the same way — proving the enforcement is transitive through the
    /// registry's own composite metadata, not just literal top-level calls.
    #[tokio::test]
    async fn gather_plan_calling_a_mutating_composite_is_rejected_transitively() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg).with_owned_composites(vec![write_composite()]);
        let mutating_gather = json!({
            "ast": {"body": [{"kind":"call","op":"wrap_write","args":[]}]},
            "gather": true,
        });
        let clean_gather = json!({
            "ast": {"body": [{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]},
            "gather": true,
        });
        let p = mock(vec![
            tool_call("emit_plan", mutating_gather),
            tool_call("emit_plan", clean_gather),
        ]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .expect("a gather plan hiding a write behind a composite is repair feedback");
        match out {
            TurnOutput::Plan(c) => assert_eq!(
                c.attempts, 2,
                "the composite-hidden write must cost a repair round"
            ),
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// The story's named failing-first test: a `gather: true` plan over the node cap is repair
    /// feedback.
    #[tokio::test]
    async fn oversize_gather_plan_is_rejected() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let many_calls: Vec<serde_json::Value> = (0..GATHER_NODE_CAP + 1)
            .map(|i| json!({"kind":"call","op":"read","args":[{"kind":"lit","value":format!("f{i}.txt")}]}))
            .collect();
        let oversize = json!({ "ast": {"body": many_calls}, "gather": true });
        let small = json!({
            "ast": {"body": [{"kind":"call","op":"read","args":[{"kind":"lit","value":"f0.txt"}]}]},
            "gather": true,
        });
        let p = mock(vec![
            tool_call("emit_plan", oversize),
            tool_call("emit_plan", small),
        ]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .expect("an oversize gather plan is repair feedback, not a hard error");
        match out {
            TurnOutput::Plan(c) => assert_eq!(
                c.attempts, 2,
                "the oversize gather plan must cost a repair round"
            ),
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// The story's named failing-first test: `gather: true` in the execute phase is rejected as
    /// repair feedback — the gather budget is spent once execution starts.
    #[tokio::test]
    async fn gather_tag_rejected_in_execute_phase() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let gather_plan = json!({
            "ast": {"body": [{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]},
            "gather": true,
        });
        let full_plan: serde_json::Value = serde_json::from_str(VALID_AST).unwrap();
        let p = mock(vec![
            tool_call("emit_plan", gather_plan),
            tool_call("emit_plan", full_plan),
        ]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Execute,
            CompileOptions::default(),
        )
        .await
        .expect("gather:true in the execute phase is repair feedback, not a hard error");
        match out {
            TurnOutput::Plan(c) => {
                assert_eq!(
                    c.attempts, 2,
                    "the gather-tagged plan in the execute phase must cost a repair round"
                );
                assert!(!c.gather, "the accepted plan is the ungathered repair");
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// Like the hidden-op gate (C-17), `gather: true` in the execute phase must never be accepted
    /// even when the repair budget runs out.
    #[tokio::test]
    async fn gather_tag_in_execute_phase_is_rejected_even_on_the_final_step() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let gather_plan = json!({
            "ast": {"body": [{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]},
            "gather": true,
        });
        let p = mock(vec![tool_call("emit_plan", gather_plan)]);
        let opts = CompileOptions {
            max_steps: 1,
            ..Default::default()
        };
        let err = turn_with_phase(&p, &ops, EmissionArm::Json, Phase::Execute, opts)
            .await
            .expect_err(
                "gather:true in the execute phase must never be accepted, even on the last step",
            );
        assert!(
            format!("{err}").contains("gather budget"),
            "the rejection carries the gate's reason: {err}"
        );
    }

    /// A small, effect-clean `gather: true` plan (with a `brief`) is accepted in the orient phase —
    /// the positive path alongside the rejection tests above.
    #[tokio::test]
    async fn clean_gather_plan_is_accepted_in_orient_phase() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let gather_plan = json!({
            "ast": {"body": [{"kind":"call","op":"read","args":[{"kind":"lit","value":"x"}]}]},
            "gather": true,
            "brief": {"goal": "understand the repo layout", "needs": ["where the config lives"]},
        });
        let p = mock(vec![tool_call("emit_plan", gather_plan)]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .expect("a small, effect-clean gather plan is accepted");
        match out {
            TurnOutput::Plan(c) => {
                assert_eq!(c.attempts, 1, "no repair round needed");
                assert!(c.gather);
                let brief = c.brief.expect("brief rides through");
                assert_eq!(brief.goal, "understand the repo layout");
                assert_eq!(brief.needs, vec!["where the config lives".to_string()]);
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    /// In the gather phase, another `gather: true` round is still allowed (the budget isn't spent
    /// until execute).
    #[tokio::test]
    async fn clean_gather_plan_is_accepted_in_gather_phase() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let gather_plan = json!({
            "ast": {"body": [{"kind":"call","op":"grep","args":[{"kind":"lit","value":"TODO"}]}]},
            "gather": true,
        });
        let p = mock(vec![tool_call("emit_plan", gather_plan)]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Gather,
            CompileOptions::default(),
        )
        .await
        .expect("gather:true is still allowed in the gather phase");
        match out {
            TurnOutput::Plan(c) => assert!(c.gather),
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
    }

    #[tokio::test]
    async fn plan_asks_the_user() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let ask = StubAsk {
            asked: Mutex::new(Vec::new()),
            reply: "the readme".to_string(),
        };
        let p = mock(vec![
            tool_call("ask_user", json!({"question": "which file?"})),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let _ = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            Some(&ask),
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(ask.asked.lock().unwrap().len(), 1);
        assert_eq!(ask.asked.lock().unwrap()[0], "which file?");
    }

    #[tokio::test]
    async fn plan_repairs_an_invalid_emit() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let invalid = r#"{"ast":{"body":[{"kind":"call","op":"nope.op","args":[]}]}}"#;
        let p = mock(vec![
            tool_call("emit_plan", serde_json::from_str(invalid).unwrap()),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.attempts, 2);
        assert!(out.diagnostics.is_empty());
    }

    #[tokio::test]
    async fn plan_accepts_side_effecting_ops_in_the_graph() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        // The plan may include `write` (a side-effecting op) — it's the plan, not executed here.
        // `write` has two required params, so it must be called with a named object argument.
        let with_write = r#"{"ast":{"body":[{"kind":"call","op":"write","args":[{"kind":"lit","value":{"path":"out.txt","content":"x"}}]}]}}"#;
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::from_str(with_write).unwrap(),
        )]);
        let out = plan(
            &p,
            "mock",
            "write a file",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.ast.body.len(), 1);
    }

    #[tokio::test]
    async fn plan_recovers_from_an_empty_turn() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        // An empty model turn (no blocks, no text) must not corrupt the local history; the planner
        // skips the nudge and retries on the next step.
        let p = mock(vec![
            vec![],
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.attempts, 2);
    }

    #[tokio::test]
    async fn compile_turn_returns_chat_for_prose() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        // Prose with no `emit_plan` and no tool calls = a chat answer, not an error.
        let p = mock(vec![text_chunk("Here's an explanation — no plan needed.")]);
        let (out, _usage) = compile_turn(
            &p,
            "mock",
            &[Message::user_text("explain the safety model")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            Phase::Execute,
        )
        .await;
        match out.unwrap() {
            TurnOutput::Chat(t) => assert!(t.contains("explanation")),
            TurnOutput::Plan(_) => panic!("expected a chat answer, got a plan"),
        }
    }

    #[tokio::test]
    async fn compile_turn_does_not_mistake_structured_json_prose_for_an_empty_plan() {
        // Regression (found while building L-10's strict-review flow): `DraftAst`'s fields all
        // default (no `deny_unknown_fields`), so ANY balanced `{ … }` embedded in ordinary prose
        // parses "successfully" as a trivially-empty plan. A sub-agent instructed to reply with pure
        // JSON (e.g. a code-reviewer role returning `[{"fingerprint": …, "reviewer": "security"}]`)
        // would have that first finding object misdetected as an empty `Plan` instead of `Chat` — the
        // turn then executes a no-op plan and the loop stalls instead of surfacing the JSON answer.
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let findings_json = r#"[{"fingerprint":"dup-1","severity":"high","category":"security","file":"a.rs","line":10,"title":"t","evidence":"e","recommendation":"r","confidence":0.9,"reviewer":"security"}]"#;
        let p = mock(vec![text_chunk(findings_json)]);
        let (out, _usage) = compile_turn(
            &p,
            "mock",
            &[Message::user_text("review this and reply with ONLY JSON")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            Phase::Execute,
        )
        .await;
        match out.unwrap() {
            TurnOutput::Chat(t) => assert_eq!(t, findings_json),
            TurnOutput::Plan(_) => panic!(
                "a JSON-array reply with an embedded balanced-brace object must not be \
                 misclassified as a plan"
            ),
        }
    }

    #[tokio::test]
    async fn compile_turn_errors_on_max_tokens_truncation() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        // Regression: a large `emit_plan` cut off by `max_tokens` yields only the model's preamble text
        // (the tool_use block never gets its `content_block_stop`, so the provider drops it) plus a
        // `Done { MaxTokens }`. This must surface as an error — NOT a silent chat answer that ends the
        // turn with the preamble and no work done.
        let truncated = vec![
            Chunk::TextDelta(
                "Now I have everything I need. Let me implement it all in one go.".into(),
            ),
            Chunk::Done {
                stop_reason: Some(flux_core::StopReason::MaxTokens),
            },
        ];
        let p = mock(vec![truncated]);
        let (out, _usage) = compile_turn(
            &p,
            "mock",
            &[Message::user_text("implement all the nodes")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            Phase::Execute,
        )
        .await;
        let msg = format!("{}", out.unwrap_err());
        assert!(
            msg.contains("truncated") && msg.contains("max_tokens"),
            "expected a max_tokens truncation error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn compile_turn_returns_plan_for_emit() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let p = mock(vec![tool_call_with_usage(
            "emit_plan",
            serde_json::from_str(VALID_AST).unwrap(),
            Usage {
                input_tokens: 120,
                output_tokens: 40,
                cache_read_input_tokens: 80,
                ..Default::default()
            },
        )]);
        let (out, usage) = compile_turn(
            &p,
            "mock",
            &[Message::user_text("read x")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
            Phase::Execute,
        )
        .await;
        assert!(matches!(out.unwrap(), TurnOutput::Plan(_)));
        // The planner call's token usage rides back out alongside the plan.
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(usage.cache_read_input_tokens, 80);
        assert_eq!(usage.context_tokens(), 200);
    }

    #[tokio::test]
    async fn emit_plan_captures_optional_complete() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let ast = serde_json::json!({
            "body": [{ "kind": "call", "op": "read", "args": [{ "kind": "lit", "value": "x" }] }]
        });

        // Object form: `{primer, instructions}` → captured on the Compiled.
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::json!({
                "ast": ast,
                "complete": { "primer": "build green", "instructions": "summarize what changed" }
            }),
        )]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        let c = out.complete.expect("object complete captured");
        assert_eq!(c.instructions, "summarize what changed");
        assert_eq!(c.primer.as_deref(), Some("build green"));

        // Bare-string form → instructions only, no primer (leniency).
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::json!({ "ast": ast, "complete": "all done" }),
        )]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        let c = out.complete.expect("string complete captured");
        assert_eq!(c.instructions, "all done");
        assert_eq!(c.primer, None);

        // Absent → None (the engine loops to let the model answer in prose).
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::from_str(VALID_AST).unwrap(),
        )]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert!(out.complete.is_none());
    }

    /// A-13: `brief` parses tolerantly alongside `gather: true`, mirroring `complete`'s leniency —
    /// and rides through onto `Compiled.brief` only when `gather` is actually set.
    #[tokio::test]
    async fn emit_plan_captures_optional_brief() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let ast = serde_json::json!({
            "body": [{ "kind": "call", "op": "read", "args": [{ "kind": "lit", "value": "x" }] }]
        });

        // Full brief: goal + needs → captured on the Compiled.
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::json!({
                "ast": ast,
                "gather": true,
                "brief": { "goal": "understand the failure", "needs": ["the stack trace", "the last commit"] }
            }),
        )]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        let c = match out {
            TurnOutput::Plan(c) => c,
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        };
        let brief = c.brief.expect("brief captured");
        assert_eq!(brief.goal, "understand the failure");
        assert_eq!(
            brief.needs,
            vec!["the stack trace".to_string(), "the last commit".to_string()]
        );

        // Goal only, no `needs` → defaults to empty (leniency, like `complete`'s bare-string form).
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::json!({ "ast": ast, "gather": true, "brief": { "goal": "just the goal" } }),
        )]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        let c = match out {
            TurnOutput::Plan(c) => c,
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        };
        assert_eq!(c.brief.unwrap().needs, Vec::<String>::new());

        // `gather` absent → `brief` never rides through even if the model sent one (meaningless
        // without a gather round).
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::json!({ "ast": ast, "brief": { "goal": "ignored" } }),
        )]);
        let (out, _) = turn_with_phase(
            &p,
            &ops,
            EmissionArm::Json,
            Phase::Orient,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        let c = match out {
            TurnOutput::Plan(c) => c,
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        };
        assert!(!c.gather);
        assert!(c.brief.is_none());
    }

    // ---- one-shot compile (with the view param) ----

    fn text_chunk(s: &str) -> Vec<Chunk> {
        vec![Chunk::TextDelta(s.to_string())]
    }

    #[tokio::test]
    async fn oneshot_compiles_and_repairs() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let good =
            "```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"read\",\"args\":[{\"kind\":\"lit\",\"value\":\"README.md\"}]}]}\n```";
        let p = mock(vec![text_chunk("no json"), text_chunk(good)]);
        let out = compile(&p, "mock", "read it", &ops, None, CompileOptions::default())
            .await
            .unwrap();
        assert_eq!(out.attempts, 2);
        assert!(out.diagnostics.is_empty());
    }

    #[tokio::test]
    async fn oneshot_unknown_op_yields_diagnostics() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let bad = "```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"nope\",\"args\":[]}]}\n```";
        let p = mock(vec![text_chunk(bad), text_chunk(bad)]);
        let out = compile(
            &p,
            "mock",
            "do magic",
            &ops,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert!(!out.diagnostics.is_empty());
    }
}
