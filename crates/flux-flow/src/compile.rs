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

use crate::analyze::{analyze_flow, Diagnostic};
use crate::ast::DraftAst;
use crate::registry::OpRegistry;
use crate::state::SessionView;

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
struct AskUserInput {
    /// The single clarifying question to ask the user.
    question: String,
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
            Ok(ast) => match analyze_flow(&ast, ops) {
                Ok(()) => {
                    return Ok(Compiled {
                        ast,
                        attempts: attempt,
                        diagnostics: Vec::new(),
                        complete: None,
                    })
                }
                Err(diags) => {
                    if attempt == attempts {
                        return Ok(Compiled {
                            ast,
                            attempts: attempt,
                            diagnostics: diags,
                            complete: None,
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
    mut thinking_sink: Option<&'_ mut dyn crate::AgentSink>,
    opts: CompileOptions,
) -> Result<(TurnOutput, Usage)> {
    let steps = opts.max_steps.max(1);
    let interactive = ask.is_some();
    let segments = assemble_system_segments(base_system, ops, view, interactive);
    // Pure DAG: the model's ONLY tools are `emit_plan` (+ `ask_user`). Every op — reads included — is a
    // node in the emitted graph, so a turn is always an auditable plan, never a free-form tool call.
    let tools = planner_tools(interactive);
    let mut messages = conversation.to_vec();
    // Forward thinking-token deltas to the sink while we're in the planning phase, so both surfaces
    // (CLI: dims them on stderr; TUI: streams them into a dedicated Thinking entry) can show reasoning
    // live instead of silently waiting behind "composing plan\u2026".
    let enable_thinking = thinking_sink.is_some();

    // Token usage for this whole planner consultation: summed across the repair/tool-result steps
    // below (each is a separate provider call), with the input/cache side reflecting the final step.
    let mut usage = Usage::default();
    // The most recent rejection fed back to the model (hidden ops, analyzer diagnostics, duplicate
    // emit_plan). When the step budget runs out, the turn is rejected WITH this text (C-17/F2) —
    // never "accepted with diagnostics" for a caller to execute blind.
    let mut last_reject = String::new();
    for step in 1..=steps {
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
        let (mut blocks, acc_text, stop_reason, call_usage) =
            stream_blocks(provider, req, ts).await?;
        usage.accumulate(&call_usage);
        if blocks.is_empty() && !acc_text.trim().is_empty() {
            blocks.push(ContentBlock::Text { text: acc_text });
        }
        let assistant = Message::assistant(blocks);
        let tool_uses = collect_tool_uses(&assistant);
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
                if !ast.body.is_empty() && analyze_flow(&ast, ops).is_ok() {
                    // Surfacing enforcement (A-04) applies to the text fallback too (C-17/F1): a
                    // prose-emitted plan is still a model-emitted plan, so a registered-but-hidden
                    // op (e.g. `bash` with the `shell` group off) is rejected here exactly like the
                    // `emit_plan` branch rejects it — repair feedback, and never an accepted plan,
                    // not even on the last step. Without this gate, emitting the plan as plain text
                    // instead of a tool call bypassed the check entirely.
                    let hidden = ops.hidden_ops_in(&ast.body);
                    if hidden.is_empty() {
                        return Ok((
                            TurnOutput::Plan(Compiled {
                                ast,
                                attempts: step,
                                diagnostics: Vec::new(),
                                complete: None,
                            }),
                            usage,
                        ));
                    }
                    // No tool_use id to answer here — the rejection rides as a plain user message.
                    last_reject = hidden_ops_rejection(&hidden);
                    messages.push(Message::user_text(last_reject.clone()));
                    continue;
                }
            }
            // A `max_tokens` cutoff drops the in-flight `emit_plan` block — the provider never sends its
            // `content_block_stop`, so only the model's preamble text survives. Don't mistake that
            // truncation for a finished prose answer (which would silently end the turn with no work
            // done); surface it so the user can raise the budget or narrow the request.
            if stop_reason == Some(StopReason::MaxTokens) {
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
                return Ok((TurnOutput::Chat(text), usage));
            }
            if step == steps {
                return Err(Error::Other(format!(
                    "planner produced neither a plan nor an answer within {steps} steps"
                )));
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
            match name.as_str() {
                "emit_plan" if emit_plan_calls > 1 => {
                    last_reject = format!(
                        "invalid: you called emit_plan {emit_plan_calls} times in one message — a \
                         turn takes exactly ONE plan. Merge the steps into a single plan and call \
                         emit_plan once."
                    );
                    results.push(ContentBlock::tool_result_text(id, last_reject.clone(), true));
                }
                "emit_plan" => {
                    // The model's optional completion directive (captured before `input` is moved):
                    // present ⇒ this plan completes the request, so the engine renders the final message
                    // from the results after running. Absent ⇒ the engine loops (the model answers later).
                    let complete = parse_completion(input.get("complete"));
                    let ast_val = input.get("ast").cloned().unwrap_or(input);
                    match serde_json::from_value::<DraftAst>(ast_val) {
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
                            match analyze_flow(&ast, ops) {
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
                                    });
                                }
                                Err(diags) => {
                                    // Always repair feedback — never "accepted with diagnostics"
                                    // (C-17/F2): every `compile_turn` caller executes the plan it
                                    // gets back, so a diagnostics-carrying plan must not escape,
                                    // not even on the last step. If the budget runs out, the turn
                                    // is rejected with this text (below).
                                    let msg = join_diags(&diags);
                                    last_reject =
                                        format!("invalid plan: {msg}. Fix it and call emit_plan again.");
                                    results.push(ContentBlock::tool_result_text(
                                        id,
                                        last_reject.clone(),
                                        true,
                                    ));
                                }
                            }
                        }
                        Err(e) => results.push(ContentBlock::tool_result_text(
                            id,
                            format!("emit_plan: invalid AST JSON: {e}"),
                            true,
                        )),
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
                other => results.push(ContentBlock::tool_result_text(
                    id,
                    format!(
                        "`{other}` is not callable — you have no direct tools. Put it in a plan node \
                         and call `emit_plan` instead."
                    ),
                    true,
                )),
            }
        }
        messages.push(Message::user(results));
        if let Some(c) = done {
            return Ok((TurnOutput::Plan(c), usage));
        }
    }
    // Reject the turn with the last rejection's text (C-17/F2): the diagnostics are surfaced to the
    // caller as the turn's error instead of riding out on an "accepted" plan.
    Err(Error::Other(if last_reject.is_empty() {
        format!("planner did not produce a plan within {steps} steps")
    } else {
        format!(
            "planner did not produce a valid plan within {steps} step(s) — \
             the last plan was rejected: {last_reject}"
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
    )
    .await?;
    match out {
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
    let (mut blocks, acc_text, _stop, usage) = stream_blocks(provider, req, None).await?;
    if blocks.is_empty() && !acc_text.trim().is_empty() {
        blocks.push(ContentBlock::Text { text: acc_text });
    }
    Ok((Message::assistant(blocks).text(), usage))
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
async fn stream_blocks<'a, 'b>(
    provider: &dyn Provider,
    req: Request,
    mut on_thinking: Option<&'a mut (dyn crate::AgentSink + 'b)>,
) -> Result<(Vec<ContentBlock>, String, Option<StopReason>, Usage)> {
    let mut stream = provider.stream(req).await?;
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut stop_reason = None;
    // Providers emit usage cumulatively (the codec carries the input/cache counts from `message_start`
    // forward onto the final `message_delta`), so the last chunk holds the complete picture — last wins.
    let mut usage = Usage::default();
    while let Some(chunk) = stream.next().await {
        match chunk? {
            Chunk::ThinkingDelta(t) => {
                if let Some(sink) = on_thinking.as_deref_mut() {
                    sink.thinking_delta(&t);
                }
            }
            Chunk::TextDelta(t) => text.push_str(&t),
            Chunk::Block(b) => blocks.push(b),
            Chunk::Usage(u) => usage = u,
            Chunk::Done { stop_reason: r } => stop_reason = r,
            _ => {}
        }
    }
    Ok((blocks, text, stop_reason, usage))
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
/// Hard character backstop for the rendered symbols block (a few huge summaries can blow the
/// budget long before the line cap).
const SYMBOLS_CHAR_CAP: usize = 10_000;

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
    let (line_cap, char_cap) = match cap {
        Some(0) => (usize::MAX, usize::MAX),
        Some(n) => (n, SYMBOLS_CHAR_CAP),
        None => (SYMBOLS_LINE_CAP, SYMBOLS_CHAR_CAP),
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
    let mut kept = 0usize;
    let mut omitted = 0usize;
    let mut chars = 0usize;
    for sym in pinned.chain(visible) {
        let ty = sym
            .ty
            .as_deref()
            .map(|t| format!(": {t}"))
            .unwrap_or_default();
        let line = format!("- ${}{} = {}\n", sym.name.0, ty, sym.summary);
        if kept >= line_cap || chars.saturating_add(line.len()) > char_cap {
            omitted += 1;
            continue;
        }
        chars += line.len();
        kept += 1;
        s.push_str(&line);
    }
    if omitted > 0 {
        s.push_str(&format!(
            "… {omitted} older symbol(s) omitted (still referencable as $name)\n"
        ));
    }
    s
}

/// The planner grammar: top-level AST shape + node kinds auto-generated from `Node` in `ast.rs`
/// via the derived JSON schema (`crate::schema`) -- never edit by hand.
fn ast_grammar() -> String {
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
///       workspace/groups, the bulk of the prompt.
///   B (cached) — agent identity + project context + active skills (the engine's base system):
///       stable within a turn, drifts across turns/invocations (git status, skills) — its own
///       breakpoint keeps a B-only change from invalidating A's segment.
///   C (uncached) — the per-iteration session symbols: they change every plan round, so they live
///       AFTER the last breakpoint where they can't invalidate anything.
fn assemble_system_segments(
    base_system: Option<&str>,
    ops: &OpRegistry,
    view: Option<&SessionView>,
    interactive: bool,
) -> Vec<SystemSegment> {
    let mut segments = vec![SystemSegment {
        text: build_planner_prompt(ops, interactive),
        cache: true,
    }];
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

/// The **static** planner block (segment A): instructions + sorted op catalog + grammar. Contains
/// nothing per-turn — the session symbols render separately (an uncached trailing segment in
/// `compile_turn`) so this block stays byte-stable and prompt-cacheable across calls (A-03).
fn build_planner_prompt(ops: &OpRegistry, interactive: bool) -> String {
    let ask_line = if interactive {
        " and `ask_user` (ask ONE clarifying question only if the request is genuinely ambiguous)"
    } else {
        ""
    };
    format!(
        "You are Flux-Lang's planning agent. For the user's request, either call `emit_plan` with ONE \
execution plan (a Flux-Lang flow AST) that accomplishes it, or — if the request needs no operations or is \
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
can plan the next step. Put the WHOLE task in one plan rather than many tiny plans.\n\nIMPORTANT — \
express control flow as Flux-Lang nodes, NOT inside shell commands, so the plan stays auditable: use a \
`repeat` node for loops and a `when` node for branches — e.g. run the tests three times with \
`repeat max 3 {{ cargo_test() }}`, never a shell `for` loop. The generic `bash` op is OFF by default \
— prefer the dedicated ops; when `bash` IS enabled, keep each call to ONE discrete command (no \
`for`/`while`/`if`/`&&`/`;` chains).\n\nWhen the user asks to create, add, define, or register an operation, first check whether it can be expressed as a Flux-Lang composite op using existing operations. If yes, use `op.register` instead of editing Rust; default to `session` scope unless the user explicitly asks for project/global persistence. Only add a native Rust tool when the operation needs a new host capability, new IO primitive, or permanent built-in behavior.\n\nWhen your plan edits code, fold the build/test into the SAME plan and wrap the fix in a `retry` so a compile error is repaired automatically rather than handed back to the user; before an `edit`, make sure its `old_string` actually occurs in the file (a no-op edit silently spins the loop). Decide ordinary implementation choices (a flag's default, a helper name) yourself — only stop to ask on genuinely destructive or ambiguous decisions.\n\nThe AST may use ANY operation from the catalog; prefer deterministic ops and reference \
existing session symbols instead of re-fetching. To embed a stored symbol's value INSIDE a string \
argument (e.g. a `task` prompt or a message), write `{{symbol_name}}` — the runtime substitutes the \
value at execution; to pass a symbol's value as a whole argument, use it directly as a `var` node. Each \
op is shown as `name({{params}})` — call a multi-param op with a single object argument naming each parameter (e.g. `write({{path, content}})`); a sole-required-param op accepts a bare value (e.g. `read(\"README.md\")`). [optional] params may be omitted from the object.\n\nOperation catalog (for the AST):\n{catalog}\n{grammar}\n",
        catalog = ops_catalog(ops),
        grammar = ast_grammar(),
    )
}

/// The only tools the planner can call: the synthetic `emit_plan` (and `ask_user` when interactive).
/// There are NO directly-callable ops — every operation (reads included) is a node in the emitted AST,
/// so a turn is always an auditable plan (pure DAG).
fn planner_tools(interactive: bool) -> Vec<ToolDef> {
    let mut tools: Vec<ToolDef> = Vec::new();
    tools.push(ToolDef {
        name: "emit_plan".to_string(),
        description: "Emit the Flux-Lang flow AST to run (your only way to act). Pass the AST as `ast`. \
                      If this plan completes the request, also pass `complete` — `instructions` for your \
                      final message (the runtime writes it from the actual results and ends the turn), \
                      NOT the message itself. Omit `complete` if you must see the results before you can \
                      answer, or to keep working; then answer in prose once done."
            .to_string(),
        input_schema: tool_input_schema::<EmitPlanInput>(),
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
    use std::sync::Mutex;

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
        let huge = "x".repeat(SYMBOLS_CHAR_CAP + 1);
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
        let names: Vec<String> = planner_tools(false).into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["emit_plan"]);
        let interactive: Vec<String> = planner_tools(true).into_iter().map(|t| t.name).collect();
        assert_eq!(interactive, vec!["emit_plan", "ask_user"]);
    }

    #[test]
    fn planner_emit_plan_schema_is_the_real_ast_schema() {
        let tools = planner_tools(false);
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
        let tools = planner_tools(true);
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

    // -- A-03: cache-first system segmentation ----------------------------------------------------

    #[test]
    fn system_segments_keep_the_static_prefix_stable_across_symbol_changes() {
        // The whole point of the layout: per-turn symbols must not perturb the cached segments.
        // Segments A (planner) and B (base system) are byte-identical whether or not symbols exist;
        // the symbols ride in a trailing UNCACHED segment.
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
        let without = assemble_system_segments(Some("identity"), &ops, None, false);
        let with = assemble_system_segments(Some("identity"), &ops, Some(&view), false);

        assert_eq!(without.len(), 2, "planner + base, no symbols segment");
        assert_eq!(with.len(), 3, "planner + base + symbols");
        assert_eq!(
            with[0].text, without[0].text,
            "segment A must be byte-stable"
        );
        assert_eq!(
            with[1].text, without[1].text,
            "segment B must be byte-stable"
        );
        assert!(
            with[0].cache && with[1].cache,
            "static segments carry breakpoints"
        );
        assert!(!with[2].cache, "the symbols segment must be uncached");
        assert!(with[2].text.contains("$notes"));
        assert!(
            !with[0].text.contains("$notes") && !with[1].text.contains("$notes"),
            "symbols must not leak into the cached segments"
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
        )
        .await
        .unwrap();
        match out {
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
        )
        .await
        .unwrap();
        match out {
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
        let err = compile_turn(
            &p,
            "mock",
            &[Message::user_text("implement all the nodes")],
            None,
            &ops,
            None,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap_err();
        let msg = format!("{err}");
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
        )
        .await
        .unwrap();
        assert!(matches!(out, TurnOutput::Plan(_)));
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
