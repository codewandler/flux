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

use futures::StreamExt;

use flux_core::{Chunk, ContentBlock, Error, Message, Result, StopReason};
use flux_provider::{Provider, Request, ToolDef};

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
/// is surfaced (compile-only shows it) rather than executed.
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
pub async fn compile(
    provider: &dyn Provider,
    model: &str,
    instruction: &str,
    ops: &OpRegistry<'_>,
    view: Option<&SessionView>,
    opts: CompileOptions,
) -> Result<Compiled> {
    let attempts = opts.max_attempts.max(1);
    let base = build_oneshot_prompt(instruction, ops, view);
    let mut prompt = base.clone();
    let mut last_err = String::new();

    for attempt in 1..=attempts {
        let text = run_model(provider, model, &prompt, opts.max_tokens).await?;
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
                    prompt = repair_prompt(&base, &text, &last_err);
                }
            },
            Err(e) => {
                last_err = e;
                if attempt == attempts {
                    return Err(Error::Other(format!(
                        "compile failed after {attempt} attempt(s): {last_err}"
                    )));
                }
                prompt = repair_prompt(&base, &text, &last_err);
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
    mut thinking_sink: Option<&'_ mut dyn flux_agent::AgentSink>,
    opts: CompileOptions,
) -> Result<TurnOutput> {
    let steps = opts.max_steps.max(1);
    let interactive = ask.is_some();
    let planner = build_planner_prompt(ops, view, interactive);
    // The engine prepends its agent identity + project context + active skills; the CLI surfaces pass
    // `None` (the planner block alone, as before).
    let system = match base_system {
        Some(b) if !b.trim().is_empty() => format!("{b}\n\n{planner}"),
        _ => planner,
    };
    // Pure DAG: the model's ONLY tools are `emit_plan` (+ `ask_user`). Every op — reads included — is a
    // node in the emitted graph, so a turn is always an auditable plan, never a free-form tool call.
    let tools = planner_tools(interactive);
    let mut messages = conversation.to_vec();
    // Forward thinking-token deltas to the sink while we're in the planning phase, so both surfaces
    // (CLI: dims them on stderr; TUI: streams them into a dedicated Thinking entry) can show reasoning
    // live instead of silently waiting behind "composing plan\u2026".
    let enable_thinking = thinking_sink.is_some();

    for step in 1..=steps {
        let req = Request {
            model: model.to_string(),
            system: Some(system.clone()),
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

        // SAFETY: we reborrow through a raw pointer to break the loop-iteration
        // lifetime cycle. `stream_blocks` is `await`ed to completion before the next
        // iteration touches `thinking_sink`, so there is no actual aliasing.
        let ts: Option<&mut dyn flux_agent::AgentSink> = thinking_sink
            .as_mut()
            .map(|s| unsafe { &mut *(*s as *mut dyn flux_agent::AgentSink) });
        let (mut blocks, acc_text, stop_reason) = stream_blocks(provider, req, ts).await?;
        if blocks.is_empty() && !acc_text.trim().is_empty() {
            blocks.push(ContentBlock::Text { text: acc_text });
        }
        let assistant = Message::assistant(blocks);
        let tool_uses = collect_tool_uses(&assistant);
        if !assistant.content.is_empty() {
            messages.push(assistant.clone());
        }

        if tool_uses.is_empty() {
            // No tool call. Perhaps the model emitted the AST as plain text → a plan.
            if let Ok(ast) = parse_draft_ast(&assistant.text()) {
                if analyze_flow(&ast, ops).is_ok() {
                    return Ok(TurnOutput::Plan(Compiled {
                        ast,
                        attempts: step,
                        diagnostics: Vec::new(),
                        complete: None,
                    }));
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
                return Ok(TurnOutput::Chat(text));
            }
            if step == steps {
                return Err(Error::Other(format!(
                    "planner produced neither a plan nor an answer within {steps} steps"
                )));
            }
            continue;
        }

        // Answer every tool_use (keeps the local history valid); capture an accepted plan if any.
        let mut results = Vec::new();
        let mut done: Option<Compiled> = None;
        for (id, name, input) in tool_uses {
            match name.as_str() {
                "emit_plan" => {
                    // The model's optional completion directive (captured before `input` is moved):
                    // present ⇒ this plan completes the request, so the engine renders the final message
                    // from the results after running. Absent ⇒ the engine loops (the model answers later).
                    let complete = parse_completion(input.get("complete"));
                    let ast_val = input.get("ast").cloned().unwrap_or(input);
                    match serde_json::from_value::<DraftAst>(ast_val) {
                        Ok(ast) => {
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
                                    let msg = join_diags(&diags);
                                    if step == steps {
                                        results.push(ContentBlock::tool_result_text(
                                            id,
                                            format!("accepted with diagnostics: {msg}"),
                                            false,
                                        ));
                                        done = Some(Compiled {
                                            ast,
                                            attempts: step,
                                            diagnostics: diags,
                                            complete,
                                        });
                                    } else {
                                        results.push(ContentBlock::tool_result_text(
                                        id,
                                        format!("invalid plan: {msg}. Fix it and call emit_plan again."),
                                        true,
                                    ));
                                    }
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
            return Ok(TurnOutput::Plan(c));
        }
    }
    Err(Error::Other(format!(
        "planner did not produce a plan within {steps} steps"
    )))
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
    match compile_turn(
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
    .await?
    {
        TurnOutput::Plan(c) => Ok(c),
        TurnOutput::Chat(_) => Err(Error::Other(
            "the model answered without emitting a plan".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Model I/O
// ---------------------------------------------------------------------------

/// One single-shot text completion: stream and collect the text (mirrors `flux-agent::maybe_compact`).
async fn run_model(
    provider: &dyn Provider,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<String> {
    let req = Request::new(model.to_string(), prompt.to_string()).with_max_tokens(max_tokens);
    let mut stream = provider.stream(req).await?;
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        if let Chunk::TextDelta(t) = chunk? {
            out.push_str(&t);
        }
    }
    Ok(out)
}

/// Parse the optional `complete` field of an `emit_plan` call into a [`Completion`]. Lenient: accepts a
/// bare string (`"summarize X"` → instructions, no primer) or an object (`{primer?, instructions}`).
/// Anything without usable `instructions` ⇒ `None`, so the engine simply loops (the model answers in
/// prose later) rather than completing on a malformed signal.
fn parse_completion(value: Option<&serde_json::Value>) -> Option<Completion> {
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
/// results. The engine calls this when a plan carried a [`Completion`]: `conversation` is the working
/// log (already extended with the user's request and the fed-back `[results]`), so the model writes the
/// summary from what really happened — never a pre-composed promise. No tools are offered, so this call
/// cannot recurse into planning; it just produces prose.
pub async fn render_completion(
    provider: &dyn Provider,
    model: &str,
    conversation: &[Message],
    directive: &Completion,
    max_tokens: u32,
) -> Result<String> {
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
    let (mut blocks, acc_text, _stop) = stream_blocks(provider, req, None).await?;
    if blocks.is_empty() && !acc_text.trim().is_empty() {
        blocks.push(ContentBlock::Text { text: acc_text });
    }
    Ok(Message::assistant(blocks).text())
}

/// Stream a turn, collecting content blocks (tool_use, text), the accumulated text delta, and the
/// terminating `stop_reason`. The stop_reason matters: a `max_tokens` cutoff mid-`emit_plan` drops the
/// tool_use block (the provider never sends its `content_block_stop`), so the caller must distinguish a
/// truncated turn from a finished prose answer.
///
/// `on_thinking` receives each incremental thinking-token delta as it arrives; pass `None` when the
/// caller doesn't need live thinking output (e.g. the one-shot `compile` path).
async fn stream_blocks(
    provider: &dyn Provider,
    req: Request,
    mut on_thinking: Option<&mut dyn flux_agent::AgentSink>,
) -> Result<(Vec<ContentBlock>, String, Option<StopReason>)> {
    let mut stream = provider.stream(req).await?;
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut stop_reason = None;
    while let Some(chunk) = stream.next().await {
        match chunk? {
            Chunk::ThinkingDelta(t) => {
                if let Some(sink) = on_thinking.as_deref_mut() {
                    sink.thinking_delta(&t);
                }
            }
            Chunk::TextDelta(t) => text.push_str(&t),
            Chunk::Block(b) => blocks.push(b),
            Chunk::Done { stop_reason: r } => stop_reason = r,
            _ => {}
        }
    }
    Ok((blocks, text, stop_reason))
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

fn symbols_block(view: Option<&SessionView>) -> String {
    match view {
        Some(v) if !v.symbols.is_empty() => {
            let mut s = String::from(
                "\nExisting session symbols (reference these instead of re-fetching):\n",
            );
            for sym in &v.symbols {
                let ty = sym
                    .ty
                    .as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                s.push_str(&format!("- ${}{} = {}\n", sym.name.0, ty, sym.summary));
            }
            s
        }
        _ => String::new(),
    }
}

/// The planner grammar: top-level AST shape + node kinds auto-generated from `Node` in `ast.rs`
/// via the derived JSON schema (`crate::schema`) -- never edit by hand.
fn ast_grammar() -> String {
    format!(
        "The AST is a JSON object: {{\"name\"?:string, \"params\"?:[{{\"name\":string,\"ty\":type}}], \"returns\"?:type, \"body\":[Node,...]}}. A Node is tagged by \"kind\":\n\
{node_kinds}Prefer `each` over `repeat` for list iteration; prefer `parallel` for independent reads/calls that don't depend on each other.\n\
\nArtifact types (the `Named` types ops produce/consume — use as a `ty`/`returns` or in a `ctx`/`need`): {artifact_types}.\n\
\nExample for \"read the readme then grep it for TODO\":\n\
{{\"body\":[\n\
  {{\"kind\":\"bind\",\"name\":\"readme\",\"value\":{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"lit\",\"value\":\"README.md\"}}]}}}},\n\
  {{\"kind\":\"bind\",\"name\":\"hits\",\"value\":{{\"kind\":\"call\",\"op\":\"grep\",\"args\":[{{\"kind\":\"lit\",\"value\":\"TODO\"}}]}}}}\n\
]}}\n\
\nExample for \"read a.rs, b.rs and c.rs and summarise each\":\n\
{{\"body\":[\n\
  {{\"kind\":\"each\",\"in\":{{\"kind\":\"lit\",\"value\":[\"a.rs\",\"b.rs\",\"c.rs\"]}},\"as\":\"f\",\"body\":[\n\
    {{\"kind\":\"bind\",\"name\":\"text\",\"value\":{{\"kind\":\"call\",\"op\":\"read\",\"args\":[{{\"kind\":\"var\",\"name\":\"f\"}}]}}}}\n\
  ],\"collect\":\"all\"}}\n\
]}}",
        node_kinds = crate::schema::node_kind_catalog(),
        artifact_types = crate::prelude::PRELUDE_TYPES.join(", "),
    )
}

fn build_oneshot_prompt(instruction: &str, ops: &OpRegistry, view: Option<&SessionView>) -> String {
    format!(
        "You are Flux-Lang's compiler front-end. Convert the user's instruction into a Flux-Lang flow \
AST as JSON. Do NOT execute anything. Prefer deterministic operations; minimise model-dependent \
steps. Use ONLY operations from the catalog. Each op is shown as `name(params)`; a call's `args` are \
positional in that parameter order ([optional] params come last).\n\nOperation catalog:\n\
{catalog}{symbols}\n{grammar}\n\nOutput ONLY the JSON AST in a single ```json code block.\n\n\
Instruction: {instruction}\n",
        catalog = ops_catalog(ops),
        symbols = symbols_block(view),
        grammar = ast_grammar(),
    )
}

fn build_planner_prompt(ops: &OpRegistry, view: Option<&SessionView>, interactive: bool) -> String {
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
`for`/`while`/`if`/`&&`/`;` chains).\n\nWhen your plan edits code, fold the build/test into the SAME plan and wrap the fix in a `retry` so a compile error is repaired automatically rather than handed back to the user; before an `edit`, make sure its `old_string` actually occurs in the file (a no-op edit silently spins the loop). Decide ordinary implementation choices (a flag's default, a helper name) yourself — only stop to ask on genuinely destructive or ambiguous decisions.\n\nThe AST may use ANY operation from the catalog; prefer deterministic ops and reference \
existing session symbols instead of re-fetching. To embed a stored symbol's value INSIDE a string \
argument (e.g. a `task` prompt or a message), write `{{symbol_name}}` — the runtime substitutes the \
value at execution; to pass a symbol's value as a whole argument, use it directly as a `var` node. Each \
op is shown as `name(params)`; a call's `args` are positional in that parameter order ([optional] \
params come last).\n\nOperation catalog (for the AST):\n{catalog}{symbols}\n{grammar}\n",
        catalog = ops_catalog(ops),
        symbols = symbols_block(view),
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
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "ast": { "type": "object", "description": "The flow AST (DraftAst) as JSON" },
                "complete": {
                    "type": "object",
                    "description": "Attach ONLY when this plan completes the request. Instructions for the \
                                    final message, rendered from the real results after the plan runs.",
                    "properties": {
                        "instructions": {
                            "type": "string",
                            "description": "What the final message should say, e.g. \"summarize what changed and why\""
                        },
                        "primer": {
                            "type": "string",
                            "description": "Optional one-line context you already know (e.g. \"Build green.\")"
                        }
                    },
                    "required": ["instructions"]
                }
            },
            "required": ["ast"]
        }),
    });
    if interactive {
        tools.push(ToolDef {
            name: "ask_user".to_string(),
            description: "Ask the user one clarifying question; returns their reply.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "question": { "type": "string" } },
                "required": ["question"]
            }),
        });
    }
    tools
}

fn repair_prompt(base: &str, previous: &str, error: &str) -> String {
    format!(
        "{base}\nYour previous output was invalid ({error}). Previous output:\n{previous}\n\n\
Return a corrected AST. Output ONLY the JSON AST in a single ```json code block.\n"
    )
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

    #[test]
    fn planner_advertises_only_emit_plan_and_ask_user() {
        // Pure DAG: the model has NO directly-callable ops — only `emit_plan` (+ `ask_user`).
        let names: Vec<String> = planner_tools(false).into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["emit_plan"]);
        let interactive: Vec<String> = planner_tools(true).into_iter().map(|t| t.name).collect();
        assert_eq!(interactive, vec!["emit_plan", "ask_user"]);
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
        let with_write = r#"{"ast":{"body":[{"kind":"call","op":"write","args":[{"kind":"lit","value":"out.txt"}]}]}}"#;
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
        let out = compile_turn(
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
        let p = mock(vec![tool_call(
            "emit_plan",
            serde_json::from_str(VALID_AST).unwrap(),
        )]);
        let out = compile_turn(
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
        let good = "```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"read\",\"args\":[]}]}\n```";
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
