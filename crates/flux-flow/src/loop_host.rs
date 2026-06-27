//! `EngineLoopHost` — the engine-side implementation of the reflexive [`LoopHost`] capability that lets
//! the agent loop be written *in flux-lang*. It turns two hidden Rust control-flow moves into two
//! ordinary, audited ops:
//!
//! - **`plan(feedback)`** re-enters the planner ([`compile_turn`]) — the model produces a graph, exactly
//!   as it does for a normal turn. The model stays the planner, never the runtime.
//! - **`run_plan(plan)`** re-enters the interpreter ([`execute_flow`]) over the **same** [`Executor`] and
//!   the **same** session/store, so every inner op still traverses authorization → approval → guarded IO,
//!   and inner symbols/trace land in the one session the next `plan` will see.
//!
//! The no-bypass envelope therefore holds **recursively**: a plan that runs a plan that runs a plan… is
//! the same safety surface at every level, hard-capped by [`MAX_REENTRY_DEPTH`] (the shipped `flux-app`
//! `SpawnOp` depth-guard pattern).
//!
//! **Construction is cyclic.** The host needs the executor it is installed on (to re-enter it), but that
//! executor's [`ToolContext`] holds the host — a cycle. [`install`](EngineLoopHost::install) breaks it
//! with [`Arc::new_cyclic`] + a `Weak<Executor>`, mirroring `flux-app`'s `Engine`.
//!
//! **The sink is shared, not buffered.** `execute_flow` takes a `&mut dyn AgentSink`, but an inner run
//! happens *inside* an outer op — the outer sink is already borrowed. [`SharedSink`] is a proxy over an
//! `Arc<Mutex<dyn AgentSink>>` that locks per write (never across an `.await`), so the outer turn and
//! every inner `run_plan` stream into the **same** surface live, with sub-steps interleaved.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::AgentSink;
use flux_core::{Error, Message, Result};
use flux_provider::Provider;
use flux_runtime::{Executor, LoopHost, ToolResult};

use crate::ast::DraftAst;
use crate::compile::{compile_turn, CompileOptions, TurnOutput};
use crate::registry::OpRegistry;
use crate::runtime::execute_flow;
use crate::state::FlowStore;

/// Hard cap on reflexive reentry (flow → `run_plan` → flow → …). Mirrors `flux-app`'s `MAX_SPAWN_DEPTH`:
/// a plan that recursively runs plans is stopped here rather than blowing the stack.
const MAX_REENTRY_DEPTH: u32 = 16;

/// The engine-side reflexive host. Holds everything the two ops need that a `ToolContext` does not carry:
/// the planner (provider + model), the live sink, the shared session/store, and a `Weak` back to the
/// executor it re-enters. Installed onto that executor's context per model-in-the-loop run.
pub struct EngineLoopHost {
    /// Back-reference to the executor whose context holds THIS host. `run_plan` re-enters it so the inner
    /// run shares the SAME perms + approver + evidence + context — no envelope divergence. `Weak` breaks
    /// the executor ⇄ host cycle.
    executor: Weak<Executor>,
    provider: Arc<dyn Provider>,
    model: String,
    /// The agent identity / project context prepended to the planner prompt (as in `engine.rs`).
    base_system: Option<String>,
    /// Shared with the outer flow run: inner values/symbols/trace land in the SAME session.
    store: Arc<FlowStore>,
    session_id: String,
    /// The live sink shared by the outer turn and every inner run (sub-steps stream live, not buffered).
    sink: Arc<Mutex<dyn AgentSink>>,
    /// Active reentry depth, guarding against runaway `run_plan` recursion.
    depth: AtomicU32,
    opts: CompileOptions,
}

impl EngineLoopHost {
    /// Wire the reflexive capability onto `executor` and return it as a shared `Arc<Executor>` to drive
    /// the outer flow with. `store`/`session_id` are shared with the outer run; `sink` is the shared live
    /// surface (build the outer flow's sink with [`SharedSink::new`] over the **same** handle).
    ///
    /// Construction is cyclic: the host re-enters this very executor, so it can only be wired in *after*
    /// the executor exists — [`Arc::new_cyclic`] hands us the `Weak<Executor>` to close the loop.
    #[allow(clippy::too_many_arguments)]
    pub fn install(
        mut executor: Executor,
        provider: Arc<dyn Provider>,
        model: String,
        base_system: Option<String>,
        store: Arc<FlowStore>,
        session_id: String,
        sink: Arc<Mutex<dyn AgentSink>>,
        opts: CompileOptions,
    ) -> Arc<Executor> {
        Arc::new_cyclic(|weak: &Weak<Executor>| {
            let host = Arc::new(EngineLoopHost {
                executor: weak.clone(),
                provider,
                model,
                base_system,
                store,
                session_id,
                sink,
                depth: AtomicU32::new(0),
                opts,
            });
            executor.set_loop_host(host);
            executor
        })
    }

    fn executor(&self) -> Result<Arc<Executor>> {
        self.executor
            .upgrade()
            .ok_or_else(|| Error::Other("loop host: the executor is no longer alive".into()))
    }
}

#[async_trait]
impl LoopHost for EngineLoopHost {
    /// Re-enter the planner. `input.feedback` (the working conversation seed) becomes the planner's
    /// message; the returned `Plan` is `{kind: "plan", ast, complete}` for an emitted graph or
    /// `{kind: "chat", text}` for a prose answer. (P4 will assemble the working conversation from the
    /// persisted log; for now the feedback string is the seed.)
    async fn plan(&self, input: Value) -> Result<Value> {
        let feedback = input
            .get("feedback")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let conversation = vec![Message::user_text(feedback)];

        let executor = self.executor()?;
        let ops = OpRegistry::new(executor.registry());
        let out = compile_turn(
            &*self.provider,
            &self.model,
            &conversation,
            self.base_system.as_deref(),
            &ops,
            None,
            None,
            None,
            self.opts.clone(),
        )
        .await?;

        let plan = match out {
            TurnOutput::Plan(c) => serde_json::json!({
                "kind": "plan",
                "ast": serde_json::to_value(&c.ast).unwrap_or(Value::Null),
                "complete": c.complete.is_some(),
            }),
            TurnOutput::Chat(text) => serde_json::json!({ "kind": "chat", "text": text }),
        };
        Ok(plan)
    }

    /// Re-enter the interpreter to run the plan's `ast` in the current session, streaming live. A `chat`
    /// plan has nothing to run, so its text is surfaced as the result. Bounded by [`MAX_REENTRY_DEPTH`].
    async fn run_plan(&self, plan: Value) -> Result<Value> {
        // Depth guard: increment, ensure we decrement on every exit, then check.
        let prev = self.depth.fetch_add(1, Ordering::SeqCst);
        let _guard = DepthGuard(&self.depth);
        if prev >= MAX_REENTRY_DEPTH {
            return Err(Error::Other(format!(
                "run_plan: reflexive reentry exceeded max depth {MAX_REENTRY_DEPTH}"
            )));
        }

        if plan.get("kind").and_then(|v| v.as_str()) == Some("chat") {
            let text = plan
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(serde_json::json!({ "transcript": "", "result": text, "steps": 0 }));
        }

        let ast_val = plan
            .get("ast")
            .cloned()
            .ok_or_else(|| Error::Other("run_plan: the plan has no `ast` to run".into()))?;
        let ast: DraftAst = serde_json::from_value(ast_val)
            .map_err(|e| Error::Other(format!("run_plan: invalid plan ast: {e}")))?;

        let executor = self.executor()?;
        // A fresh proxy over the shared sink: the inner run streams live, interleaved under the outer op.
        let mut sink = SharedSink(self.sink.clone());
        let outcome = execute_flow(
            self.store.as_ref(),
            executor.as_ref(),
            &self.session_id,
            &ast,
            &mut sink,
        )
        .await
        .map_err(|e| Error::Other(e.to_string()))?;

        let mut out = serde_json::json!({
            "transcript": outcome.transcript,
            "result": outcome.result,
            "steps": outcome.steps,
        });
        if let Some(susp) = &outcome.suspension {
            out["suspension"] = serde_json::json!({ "source": susp.source, "node": susp.node.0 });
        }
        Ok(out)
    }
}

/// Decrements the active reentry depth when a `run_plan` unwinds (success, error, or early return).
struct DepthGuard<'a>(&'a AtomicU32);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A live [`AgentSink`] proxy over a shared handle. Every method locks the shared sink for the duration
/// of that **one** call only — never across an `.await` — so the outer turn and an inner `run_plan` can
/// both stream into the same surface without buffering and without a second `&mut` borrow. Construct one
/// per run (cheap: an `Arc` clone) and pass `&mut` of it to `execute_flow`.
pub struct SharedSink(Arc<Mutex<dyn AgentSink>>);

impl SharedSink {
    /// Wrap a shared sink handle. The outer flow and every inner run get their own `SharedSink` over the
    /// same `Arc<Mutex<…>>`, so all streamed output lands on one surface.
    pub fn new(sink: Arc<Mutex<dyn AgentSink>>) -> Self {
        Self(sink)
    }
}

impl AgentSink for SharedSink {
    fn text_delta(&mut self, t: &str) {
        self.0.lock().unwrap().text_delta(t);
    }
    fn thinking_delta(&mut self, t: &str) {
        self.0.lock().unwrap().thinking_delta(t);
    }
    fn planning(&mut self, active: bool) {
        self.0.lock().unwrap().planning(active);
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        self.0.lock().unwrap().tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.0.lock().unwrap().tool_result(name, result);
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.0.lock().unwrap().observation(o);
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        self.0.lock().unwrap().turn_end(usage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use serde_json::json;

    use flux_core::{Chunk, ContentBlock, StopReason};
    use flux_provider::{ChunkStream, Request};
    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    /// A provider that replays canned chunk sequences, one per `stream()` call (mirrors the engine tests).
    struct MockProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
    }

    #[async_trait]
    impl Provider for MockProvider {
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

    /// Echo the `text` param back as content.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "echo",
                "echo text",
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            )
        }
        async fn execute(&self, _c: &ToolContext, params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A sink that records (into shared handles) the op names dispatched and text streamed — so the test
    /// can inspect what reached the shared surface after the sink is moved behind the `Arc<Mutex<…>>`.
    #[derive(Clone, Default)]
    struct Recorder {
        tools: Arc<Mutex<Vec<String>>>,
        text: Arc<Mutex<String>>,
    }
    struct RecSink(Recorder);
    impl AgentSink for RecSink {
        fn text_delta(&mut self, t: &str) {
            self.0.text.lock().unwrap().push_str(t);
        }
        fn tool_call(&mut self, name: &str, _input: &Value) {
            self.0.tools.lock().unwrap().push(name.to_string());
        }
    }

    /// One model turn that emits a plan via the `emit_plan` tool call carrying `ast`.
    fn emit_plan(ast: Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "p1".into(),
                name: "emit_plan".into(),
                input: json!({ "ast": ast }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    /// The keystone: `plan` re-enters the planner (the mock emits a one-node echo plan), `run_plan`
    /// re-enters the interpreter over the SAME executor + session, the inner `echo` streams live into the
    /// SAME shared sink as the outer turn, and its result flows back out as the flow's return.
    #[tokio::test]
    async fn plan_then_run_plan_reenters_and_streams_live() {
        // A shared live sink, recording into handles the test keeps.
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));

        // Executor over echo + the reflexive ops, all pre-allowed; allow-all approver.
        let dir = std::env::temp_dir().join(format!("flux-loop-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into(), "plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );

        // The planner will emit a one-node plan: $g = echo("hi").
        let echo_ast = json!({
            "body": [{
                "kind": "bind", "name": "g",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(echo_ast)])),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());

        // Wire the reflexive capability onto the executor (shares the same envelope + session + sink).
        let executor = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store.clone(),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        // The outer flow IS the loop, in flux-lang: $p = plan(...); $out = run_plan($p); return $out.
        let outer_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "p",
                  "value": { "kind": "call", "op": "plan",
                             "args": [{ "kind": "lit", "value": "echo hi for me" }] } },
                { "kind": "bind", "name": "out",
                  "value": { "kind": "call", "op": "run_plan",
                             "args": [{ "kind": "var", "name": "p" }] } },
                { "kind": "return", "value": { "kind": "var", "name": "out" } }
            ]
        }))
        .unwrap();

        let mut outer = SharedSink::new(shared.clone());
        let outcome = execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &outer_ast,
            &mut outer,
        )
        .await
        .unwrap();

        // Every level streamed into the ONE shared sink: the outer ops AND the inner echo.
        let tools = rec.tools.lock().unwrap().clone();
        assert!(
            tools.contains(&"plan".to_string()),
            "plan dispatched: {tools:?}"
        );
        assert!(
            tools.contains(&"run_plan".to_string()),
            "run_plan dispatched: {tools:?}"
        );
        assert!(
            tools.contains(&"echo".to_string()),
            "the INNER echo streamed live into the shared sink: {tools:?}"
        );
        // The interleaving is real: echo (inner) comes after run_plan (outer) opened.
        let run_at = tools.iter().position(|t| t == "run_plan").unwrap();
        let echo_at = tools.iter().position(|t| t == "echo").unwrap();
        assert!(echo_at > run_at, "echo runs inside run_plan: {tools:?}");

        // The inner plan's result flowed back out: the flow returns the run_plan Outcome carrying "hi".
        assert!(
            outcome.result.contains("hi"),
            "outer return carries the inner Outcome: {}",
            outcome.result
        );

        // The plan was actually executed in the shared session: $g bound to the echo output.
        let g = store
            .resolve("sess", &crate::ast::SymbolName("g".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(g, Some(crate::ast::Value::String("hi".into())));
    }
}
