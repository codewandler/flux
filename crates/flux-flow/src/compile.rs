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

use flux_core::{Chunk, ContentBlock, Error, Message, Result};
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
    /// Token budget for each model call.
    pub max_tokens: u32,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            max_steps: 8,
            max_tokens: 4096,
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
                    })
                }
                Err(diags) => {
                    if attempt == attempts {
                        return Ok(Compiled {
                            ast,
                            attempts: attempt,
                            diagnostics: diags,
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
            thinking: false,
            effort: None,
            metadata: serde_json::Map::new(),
        };

        let (mut blocks, acc_text) = stream_blocks(provider, req).await?;
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
                    }));
                }
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
    match compile_turn(provider, model, &conversation, None, ops, view, ask, opts).await? {
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

/// Stream a turn, collecting content blocks (tool_use, text) and the accumulated text delta.
async fn stream_blocks(
    provider: &dyn Provider,
    req: Request,
) -> Result<(Vec<ContentBlock>, String)> {
    let mut stream = provider.stream(req).await?;
    let mut blocks = Vec::new();
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk? {
            Chunk::TextDelta(t) => text.push_str(&t),
            Chunk::Block(b) => blocks.push(b),
            _ => {}
        }
    }
    Ok((blocks, text))
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
const AST_GRAMMAR: &str = r#"The AST is a JSON object: {"name"?:string, "params"?:[{"name":string,"ty":type}], "returns"?:type, "body":[Node,...]}. A Node is tagged by "kind":
- {"kind":"call","op":"<op>","args":[Node,...]}
- {"kind":"bind","name":"<sym>","value":Node,"effect"?:"<effect>"}
- {"kind":"when","cond":Node,"then":[Node,...],"otherwise":[Node,...]}
- {"kind":"repeat","max":<int>,"until"?:Node,"body":[Node,...]}
- {"kind":"await","binding"?:"<sym>","source":"<source>"}
- {"kind":"return","value":Node}
- {"kind":"var","name":"<sym>"}
- {"kind":"lit","value":<any JSON>}

Example for "read the readme then grep it for TODO":
{"body":[
  {"kind":"bind","name":"readme","value":{"kind":"call","op":"read","args":[{"kind":"lit","value":"README.md"}]}},
  {"kind":"bind","name":"hits","value":{"kind":"call","op":"grep","args":[{"kind":"lit","value":"TODO"}]}}
]}"#;

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
        grammar = AST_GRAMMAR,
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
plan, and do NOT repeat work already done).\n\nYou have NO directly-callable tools except `emit_plan`\
{ask_line} — you cannot run `read`/`grep`/`bash`/etc. yourself. To gather information, put `read`/`grep`/\
`glob` as NODES in a plan and emit it; the runtime executes the plan and gives you the results, so you \
can plan the next step. Put the WHOLE task in one plan rather than many tiny plans.\n\nIMPORTANT — \
express control flow as Flux-Lang nodes, NOT inside shell commands, so the plan stays auditable: use a \
`repeat` node for loops and a `when` node for branches. Do NOT write shell loops/conditionals (`for`, \
`while`, `if`, `&&`, `;`) inside a `bash` command — a `bash` op is ONE discrete command. E.g. \"print X \
three times\" is `repeat max 3 {{ bash(\"echo X\") }}`, never `bash(\"for i in 1 2 3; do echo X; \
done\")`.\n\nThe AST may use ANY operation from the catalog; prefer deterministic ops and reference \
existing session symbols instead of re-fetching. Each op is shown as `name(params)`; a call's `args` are \
positional in that parameter order ([optional] params come last).\n\nOperation catalog (for the AST):\n\
{catalog}{symbols}\n{grammar}\n",
        catalog = ops_catalog(ops),
        symbols = symbols_block(view),
        grammar = AST_GRAMMAR,
    )
}

/// The only tools the planner can call: the synthetic `emit_plan` (and `ask_user` when interactive).
/// There are NO directly-callable ops — every operation (reads included) is a node in the emitted AST,
/// so a turn is always an auditable plan (pure DAG).
fn planner_tools(interactive: bool) -> Vec<ToolDef> {
    let mut tools: Vec<ToolDef> = Vec::new();
    tools.push(ToolDef {
        name: "emit_plan".to_string(),
        description: "Emit the Flux-Lang flow AST to run (your only way to act). Pass the AST as `ast`."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "ast": { "type": "object", "description": "The flow AST (DraftAst) as JSON" } },
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
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert!(matches!(out, TurnOutput::Plan(_)));
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
