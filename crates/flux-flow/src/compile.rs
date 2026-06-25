//! The compiler front-end: turn a natural-language instruction into a typed [`DraftAst`].
//!
//! Two entry points share the prompt-and-parse approach (the provider has no forced structured
//! output):
//! - [`compile`] — **one-shot**: a single model call. Cheap; plans from the instruction (and the
//!   session symbol view) alone.
//! - [`plan`] — **agentic**: a bounded loop where the model has agency. It may call read-only research
//!   tools (`read`/`grep`/`glob`, dispatched through the safety envelope) to gather context, ask the
//!   user a clarifying question ([`AskUser`]), and emits the final AST via a synthetic `emit_plan`
//!   tool. One-shot is the degenerate case (it emits immediately). The emitted AST may reference *any*
//!   registered op (it is the *plan*, not executed here); only read-only tools execute during planning.
//!
//! Both are session-aware: a [`SessionView`] lets the model reference already-created `$values` instead
//! of re-fetching. This is the seat of "the LLM plans": the model proposes structure; the runtime owns
//! execution.

use futures::StreamExt;

use flux_core::{Chunk, ContentBlock, Error, Message, Result};
use flux_provider::{Provider, Request, ToolDef};
use flux_runtime::Executor;
use flux_spec::{Effect, Risk, ToolSpec};

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

/// How the planner asks the user a clarifying question mid-plan (interactive mode). The CLI implements
/// this over stdin; `None` means no user is attached, so the `ask_user` tool is not offered.
pub trait AskUser: Send + Sync {
    /// Ask `question` and return the user's reply.
    fn ask(&self, question: &str) -> String;
}

/// True if a tool is safe for the planner to *execute* while planning: read-only and low-risk
/// (effects ⊆ {Read, Filesystem}, no Write/Process/Network/Browser/LocalSystem). Over the builtins
/// this is `read`, `grep`, `glob`.
pub fn is_safe(spec: &ToolSpec) -> bool {
    spec.risk == Risk::Low
        && spec
            .effects
            .iter()
            .all(|e| matches!(e, Effect::Read | Effect::Filesystem))
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

/// Compile a natural-language instruction into a [`DraftAst`] with an agentic planner loop: the model
/// may call the read-only `research` tools to gather context, [`AskUser`] to clarify, and emits the
/// final AST via the synthetic `emit_plan` tool. `ops` is the full op catalog (the AST may use any of
/// them); `research` is a safety-gated executor scoped to read-only tools (the only ones run here).
// Each argument is a distinct, meaningful input (provider, model, catalog, research executor, session
// view, user-ask, options); bundling them would obscure rather than clarify.
#[allow(clippy::too_many_arguments)]
pub async fn plan(
    provider: &dyn Provider,
    model: &str,
    instruction: &str,
    ops: &OpRegistry<'_>,
    research: &Executor,
    view: Option<&SessionView>,
    ask: Option<&dyn AskUser>,
    opts: CompileOptions,
) -> Result<Compiled> {
    let steps = opts.max_steps.max(1);
    let interactive = ask.is_some();
    let system = build_planner_prompt(ops, view, interactive);
    let tools = planner_tools(research, interactive);
    let mut messages = vec![Message::user_text(instruction)];

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
            // No tool call — perhaps the model emitted the AST as plain text. Try it; else nudge.
            if let Ok(ast) = parse_draft_ast(&assistant.text()) {
                if analyze_flow(&ast, ops).is_ok() {
                    return Ok(Compiled {
                        ast,
                        attempts: step,
                        diagnostics: Vec::new(),
                    });
                }
            }
            if step == steps {
                return Err(Error::Other(format!(
                    "planner produced no plan within {steps} steps"
                )));
            }
            // Nudge only if the assistant turn was non-empty (so it was pushed). A nudge after a prior
            // `user(results)` with no assistant in between would be an invalid user-after-user
            // sequence; an empty turn just retries on the next step.
            if !assistant.content.is_empty() {
                messages.push(Message::user_text(
                    "Call `emit_plan` with the final AST when you are ready.",
                ));
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
                // Research tool: dispatch through the safe envelope. Unknown / non-safe names are
                // refused by `dispatch` (they aren't in the scoped registry).
                _ => {
                    let r = research.dispatch(&name, input).await;
                    results.push(ContentBlock::tool_result_text(id, r.content, r.is_error));
                }
            }
        }
        messages.push(Message::user(results));
        if let Some(c) = done {
            return Ok(c);
        }
    }
    Err(Error::Other(format!(
        "planner did not produce a plan within {steps} steps"
    )))
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
            "- {} : {} [effects: {}; risk: {:?}]\n",
            sig.name, sig.description, effects, sig.risk
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
steps. Use ONLY operations from the catalog.\n\nOperation catalog:\n{catalog}{symbols}\n{grammar}\n\n\
Output ONLY the JSON AST in a single ```json code block.\n\nInstruction: {instruction}\n",
        catalog = ops_catalog(ops),
        symbols = symbols_block(view),
        grammar = AST_GRAMMAR,
    )
}

fn build_planner_prompt(ops: &OpRegistry, view: Option<&SessionView>, interactive: bool) -> String {
    let ask_line = if interactive {
        " You may call `ask_user` to ask the user ONE clarifying question if the instruction is genuinely ambiguous."
    } else {
        ""
    };
    format!(
        "You are Flux-Lang's planning agent. Produce a Flux-Lang flow AST (the execution plan) for the \
user's instruction.\n\nYou have read-only research tools — `read`, `grep`, `glob` — to gather context. \
Use them ONLY if you need information you do not already have (e.g. to find file paths or inspect \
code); do not over-research.{ask_line} When ready, call `emit_plan` with the final AST as its `ast` \
argument. The AST may use ANY operation from the catalog (it is the plan, not executed now); prefer \
deterministic ops and reference existing session symbols instead of re-fetching.\n\n\
Operation catalog (for the AST):\n{catalog}{symbols}\n{grammar}\n",
        catalog = ops_catalog(ops),
        symbols = symbols_block(view),
        grammar = AST_GRAMMAR,
    )
}

/// The tools advertised to the planner: the read-only research tools + the synthetic `emit_plan` (and
/// `ask_user` when interactive). Non-safe ops are NOT advertised — they can appear in the emitted AST
/// but cannot be executed during planning.
fn planner_tools(research: &Executor, interactive: bool) -> Vec<ToolDef> {
    let mut tools: Vec<ToolDef> = research
        .registry()
        .specs()
        .into_iter()
        .map(|s| ToolDef {
            name: s.name,
            description: s.description,
            input_schema: s.input_schema,
        })
        .collect();
    tools.push(ToolDef {
        name: "emit_plan".to_string(),
        description: "Emit the final Flux-Lang flow AST. Call when ready; pass the AST as `ast`."
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
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use flux_provider::ChunkStream;
    use flux_runtime::{DenyApprover, PermissionManager, ToolContext, ToolRegistry};
    use flux_system::{System, Workspace};

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

    fn research_executor() -> Executor {
        let dir = std::env::temp_dir().join(format!("flux-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let safe = full_registry().subset(Some(&[
            "read".to_string(),
            "grep".to_string(),
            "glob".to_string(),
        ]));
        let perms = PermissionManager::from_rules(
            &["read".to_string(), "grep".to_string(), "glob".to_string()],
            &[],
        );
        Executor::new(
            safe,
            perms,
            Arc::new(DenyApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
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
    fn is_safe_classifies_builtins() {
        let reg = full_registry();
        for (name, want) in [("read", true), ("grep", true), ("glob", true)] {
            assert_eq!(is_safe(&reg.get(name).unwrap().spec()), want, "{name}");
        }
        for name in ["write", "edit", "bash"] {
            assert!(
                !is_safe(&reg.get(name).unwrap().spec()),
                "{name} must not be safe"
            );
        }
    }

    #[test]
    fn planner_advertises_only_safe_tools_plus_synthetics() {
        let ex = research_executor();
        let mut names: Vec<String> = planner_tools(&ex, false)
            .into_iter()
            .map(|t| t.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["emit_plan", "glob", "grep", "read"]);
        assert!(planner_tools(&ex, true)
            .iter()
            .any(|t| t.name == "ask_user"));
    }

    #[tokio::test]
    async fn plan_researches_then_emits() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let research = research_executor();
        let p = mock(vec![
            tool_call("read", json!({"path": "Cargo.toml"})),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            &research,
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
    async fn plan_refuses_non_safe_tool_then_emits() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let research = research_executor();
        // `write` is not in the safe research registry → dispatch refuses it; planner still emits.
        let p = mock(vec![
            tool_call("write", json!({"path": "x", "content": "y"})),
            tool_call("emit_plan", serde_json::from_str(VALID_AST).unwrap()),
        ]);
        let out = plan(
            &p,
            "mock",
            "do it",
            &ops,
            &research,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert!(out.diagnostics.is_empty());
    }

    #[tokio::test]
    async fn plan_asks_the_user() {
        let reg = full_registry();
        let ops = OpRegistry::new(&reg);
        let research = research_executor();
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
            &research,
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
        let research = research_executor();
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
            &research,
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
        let research = research_executor();
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
            &research,
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
        let research = research_executor();
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
            &research,
            None,
            None,
            CompileOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.attempts, 2);
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
