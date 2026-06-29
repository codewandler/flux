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

use crate::AgentSink;
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

/// The per-turn state the reflexive ops need: which session to run in, the project/identity prompt to
/// seed the planner with, and the live sink. Swapped each turn via [`EngineLoopHost::set_turn`] so a
/// single host (installed once at engine construction) serves every turn — the session and sink vary,
/// the planner and executor do not.
struct TurnCtx {
    session_id: String,
    base_system: Option<String>,
    sink: Arc<Mutex<dyn AgentSink>>,
}

/// Retry-breaker thresholds. Without a guard the agent loop can replay a byte-identical failing (or
/// no-progress) `run_plan` round all the way to the iteration cap (~25), burning ~140 s for nothing.
/// We count consecutive identical `run_plan` transcripts and act: escalate the feedback, then stop.
const STALL_ESCALATE: u32 = 2; // 2nd identical round → inject a stronger "stop repeating" directive
const STALL_STOP: u32 = 4; // 4th identical round → end the turn honestly instead of looping

/// Per-turn loop-guard state for the retry-breaker. Reset each turn in [`EngineLoopHost::set_turn`].
#[derive(Default)]
struct LoopGuard {
    /// The previous `run_plan` transcript — an unchanged, non-empty one means the round made no progress.
    last_transcript: Option<String>,
    /// Consecutive count of that identical transcript.
    stall: u32,
    /// Armed once the stall reaches [`STALL_STOP`]: the honest message the next `plan` returns (as a
    /// `chat`) to terminate the loop via the flow's existing `case "chat"`.
    force_stop: Option<String>,
}

/// The engine-side reflexive host. Holds the stable machinery the two ops need that a `ToolContext`
/// does not carry — the planner (provider + model), the shared store, a `Weak` back to the executor it
/// re-enters — plus the per-turn [`TurnCtx`] (session + sink), updated each turn.
pub struct EngineLoopHost {
    /// Back-reference to the executor whose context holds THIS host. `run_plan` re-enters it so the inner
    /// run shares the SAME perms + approver + evidence + context — no envelope divergence. `Weak` breaks
    /// the executor ⇄ host cycle.
    executor: Weak<Executor>,
    /// The planner, interior-mutable so the REPL `/model` command can swap it on the shared host.
    provider: Mutex<Arc<dyn Provider>>,
    model: Mutex<String>,
    /// Shared with the outer flow run: inner values/symbols/trace land in the SAME session.
    store: Arc<FlowStore>,
    /// Active reentry depth, guarding against runaway `run_plan` recursion.
    depth: AtomicU32,
    opts: CompileOptions,
    /// Per-turn session + sink, set by [`set_turn`](Self::set_turn) before each run.
    turn: Mutex<TurnCtx>,
    /// Retry-breaker state, reset per turn — stops a stalled loop from replaying to the cap.
    guard: Mutex<LoopGuard>,
}

impl EngineLoopHost {
    /// Wire the reflexive capability onto `executor`, returning the shared `Arc<Executor>` to drive flows
    /// with **and** the host handle (to [`set_turn`](Self::set_turn) before each run). `store` is shared
    /// with the runs; the initial `session_id`/`sink`/`base_system` seed the first turn.
    ///
    /// Construction is cyclic: the host re-enters this very executor, so it can only be wired in *after*
    /// the executor exists — [`Arc::new_cyclic`] hands us the `Weak<Executor>` to close the loop. The
    /// host is captured out of the constructor through a slot so the caller gets both halves.
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
    ) -> (Arc<Executor>, Arc<EngineLoopHost>) {
        let slot: Arc<Mutex<Option<Arc<EngineLoopHost>>>> = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        let executor = Arc::new_cyclic(move |weak: &Weak<Executor>| {
            let host = Arc::new(EngineLoopHost {
                executor: weak.clone(),
                provider: Mutex::new(provider),
                model: Mutex::new(model),
                store,
                depth: AtomicU32::new(0),
                opts,
                turn: Mutex::new(TurnCtx {
                    session_id,
                    base_system,
                    sink,
                }),
                guard: Mutex::new(LoopGuard::default()),
            });
            *slot2.lock().unwrap() = Some(host.clone());
            executor.set_loop_host(host);
            executor
        });
        let host = slot.lock().unwrap().take().expect("host captured");
        (executor, host)
    }

    /// Point the host at a new turn: the session to run in, the planner's base system prompt (project
    /// context + matched skills), and the live sink. Called by the engine bootstrap before each turn.
    pub fn set_turn(
        &self,
        session_id: String,
        base_system: Option<String>,
        sink: Arc<Mutex<dyn AgentSink>>,
    ) {
        *self.turn.lock().unwrap() = TurnCtx {
            session_id,
            base_system,
            sink,
        };
        // Fresh turn → reset the retry-breaker so a prior turn's stall never bleeds in.
        *self.guard.lock().unwrap() = LoopGuard::default();
    }

    /// Swap the planner (provider + model) — the REPL `/model` command, applied to the shared host so
    /// subsequent `plan` calls use the new model.
    pub fn set_model(&self, provider: Arc<dyn Provider>, model: String) {
        *self.provider.lock().unwrap() = provider;
        *self.model.lock().unwrap() = model;
    }

    fn executor(&self) -> Result<Arc<Executor>> {
        self.executor
            .upgrade()
            .ok_or_else(|| Error::Other("loop host: the executor is no longer alive".into()))
    }

    /// Retry-breaker: fold one `run_plan` transcript into the loop-guard. An unchanged (non-empty)
    /// transcript means the round made no progress; on repeats we escalate the fed-back directive,
    /// and once the stall persists we arm a hard stop (the next `plan` ends the turn). Returns the
    /// transcript to feed back — augmented with an explicit directive when the loop is repeating.
    fn guard_transcript(&self, transcript: String) -> String {
        let mut g = self.guard.lock().unwrap();
        let stalled = !transcript.trim().is_empty()
            && g.last_transcript.as_deref() == Some(transcript.as_str());
        if stalled {
            g.stall += 1;
        } else {
            // New (or empty) transcript = progress: reset the counter and remember this round.
            g.stall = 1;
            g.last_transcript = Some(transcript.clone());
        }
        let stall = g.stall;
        if stall >= STALL_STOP {
            g.force_stop = Some(format!(
                "Stopping: the last step made no progress — it ran {stall}× in a row with an \
                 identical result, so I could not complete this turn."
            ));
            return format!(
                "[loop-guard] STOP — this exact step has repeated {stall}× with no change; the turn \
                 will now end.\n{transcript}"
            );
        }
        if stall >= STALL_ESCALATE {
            return format!(
                "[loop-guard] You have produced this EXACT same step and result {stall}× in a row — \
                 it is NOT making progress. Do not repeat it: change the call (e.g. supply the \
                 missing argument) or take a different approach; if you cannot, answer in prose.\n\
                 {transcript}"
            );
        }
        transcript
    }
}

#[async_trait]
impl LoopHost for EngineLoopHost {
    /// Re-enter the planner over the **working conversation**: the persisted session log (the real
    /// `user → assistant` history, including the user's request) plus the loop-carried `$feedback` —
    /// last iteration's results — appended as an ephemeral turn. The feedback accretes across `repeat`
    /// iterations via last-writer-wins symbol rebinding (no new machinery), so each `plan` sees what the
    /// prior `run_plan` produced without any of it being persisted. Returns a `Plan`:
    /// `{kind: "plan", ast, complete}` for an emitted graph or `{kind: "chat", text}` for a prose answer.
    async fn plan(&self, input: Value) -> Result<Value> {
        // Retry-breaker hard stop: a prior `run_plan` flagged a stalled loop. End the turn honestly
        // as a prose answer (the flow's `case "chat"` terminates the loop) instead of re-planning.
        let force_stop = self.guard.lock().unwrap().force_stop.take();
        if let Some(msg) = force_stop {
            return Ok(serde_json::json!({ "kind": "chat", "text": msg }));
        }
        let feedback = input
            .get("feedback")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Snapshot the per-turn session + base system (drop the lock before the model call).
        let (session_id, base_system) = {
            let t = self.turn.lock().unwrap();
            (t.session_id.clone(), t.base_system.clone())
        };
        // Working conversation = persisted history + the loop-carried feedback (ephemeral). This is
        // engine.rs's `working` vector, relocated into the op so the loop itself stays in flux-lang.
        let mut conversation = self.store.conversation(&session_id).unwrap_or_default();
        if !feedback.trim().is_empty() {
            conversation.push(Message::user_text(feedback));
        }
        // Degenerate case (no history, no feedback): still hand the planner a turn to act on.
        if conversation.is_empty() {
            conversation.push(Message::user_text(""));
        }

        let executor = self.executor()?;
        let ops = OpRegistry::new(executor.registry());
        let provider = self.provider.lock().unwrap().clone();
        let model = self.model.lock().unwrap().clone();
        let out = compile_turn(
            &*provider,
            &model,
            &conversation,
            base_system.as_deref(),
            &ops,
            None,
            None,
            None,
            self.opts.clone(),
        )
        .await
        // Surface a provider failure (credit/auth/rate-limit/transport) as a readable sentence — the
        // error flows out through the op envelope and becomes the turn's answer, never raw JSON.
        .map_err(|e| Error::Other(crate::engine::planner_error(&e)))?;

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

        let (session_id, sink) = {
            let t = self.turn.lock().unwrap();
            (t.session_id.clone(), t.sink.clone())
        };
        let executor = self.executor()?;
        // A fresh proxy over the shared sink: the inner run streams live, interleaved under the outer op.
        let mut sink = SharedSink(sink);

        // Surface the compiled plan BEFORE executing it — auditable, and so the user sees what is about
        // to run before any per-op approval prompt (the `flow.plan` observation the surfaces render as
        // the plan tree). The inner ops then stream + gate live underneath.
        let risk = crate::runtime::plan_risk(&ast, executor.registry());
        sink.observation(&flux_evidence::Observation::new(
            "flow.plan",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "plan": crate::render::render_pretty(&ast),
                "plan_ast": serde_json::to_value(&ast).unwrap_or(Value::Null),
                "risk": risk.summary(),
                "ops": risk.ops.len(),
            }),
        ));

        // Plan-level approval: the user approves the plan as a whole here — its tree + aggregate risk
        // were just surfaced. A read-only plan needs no approval; a mutating one prompts ONCE, and then
        // every inner op runs without a per-op prompt (the scope guard, held across `execute_flow`,
        // tells `dispatch` to skip the gate). `always` trusts all plans for the session. On rejection we
        // feed a stop-signal back so the model ends the turn in prose rather than re-planning forever.
        let _scope = if risk.mutating {
            // Let the surface drain + render the plan tree (the observation above) BEFORE we print the
            // approval prompt and — on a TTY — switch to raw mode for the keypress. A CLI renders the
            // observation asynchronously (drained off an mpsc as the loop yields); without this yield the
            // prompt can print ahead of the tree, and the tree can render under raw mode (no `\n`→`\r\n`),
            // staircasing it. Yielding here parks us so the drain runs in cooked mode first.
            tokio::task::yield_now().await;
            match executor.approve_plan(&risk.summary(), risk.ops.len()).await {
                Some(scope) => Some(scope),
                None => {
                    return Ok(serde_json::json!({
                        "transcript": "[plan rejected by user] The user declined to run this plan. \
                                       Do not propose another — acknowledge briefly and stop.",
                        "result": "",
                        "steps": 0,
                    }));
                }
            }
        } else {
            None
        };

        let outcome = match execute_flow(
            self.store.as_ref(),
            executor.as_ref(),
            &session_id,
            &ast,
            &mut sink,
        )
        .await
        {
            Ok(o) => o,
            // A plan step failed (a bad edit, an unsupported `jq`, a denied op…). Feed the error back as
            // the outcome transcript so the loop re-plans (model-in-the-loop self-correction), exactly as
            // the old Rust loop did — rather than letting it abort the whole turn.
            Err(e) => {
                let transcript = self.guard_transcript(format!(
                    "[plan error] {e}\nAdjust and emit another plan, or answer in prose."
                ));
                return Ok(serde_json::json!({
                    "transcript": transcript,
                    "result": "",
                    "steps": 0,
                }));
            }
        };

        // A suspension means the turn is awaiting input — real progress, not a stall — so skip the
        // retry-breaker and feed back the raw transcript.
        let transcript = if outcome.suspension.is_some() {
            outcome.transcript.clone()
        } else {
            self.guard_transcript(outcome.transcript.clone())
        };
        let mut out = serde_json::json!({
            "transcript": transcript,
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

/// One captured [`AgentSink`] call, forwarded over a channel. The agent-loop bootstrap drives the loop
/// flow through an owned, shareable [`ChannelSink`] (so the `'static` loop host can hold it for reentrant
/// `run_plan`), and drains these events onto its **borrowed** `&mut dyn AgentSink` *concurrently* with the
/// run — live streaming, no buffer-then-replay, and `run_turn`'s signature is untouched (the mpsc proxy
/// the design called for).
pub enum SinkEvent {
    Text(String),
    Thinking(String),
    Planning(bool),
    ToolCall(String, Value),
    ToolResult(String, ToolResult),
    Observation(flux_evidence::Observation),
    TurnEnd(Option<flux_core::Usage>),
}

impl SinkEvent {
    /// Replay this captured call onto a real sink (called by the bootstrap's drain loop).
    pub fn apply(self, sink: &mut dyn AgentSink) {
        match self {
            SinkEvent::Text(t) => sink.text_delta(&t),
            SinkEvent::Thinking(t) => sink.thinking_delta(&t),
            SinkEvent::Planning(a) => sink.planning(a),
            SinkEvent::ToolCall(n, i) => sink.tool_call(&n, &i),
            SinkEvent::ToolResult(n, r) => sink.tool_result(&n, &r),
            SinkEvent::Observation(o) => sink.observation(&o),
            SinkEvent::TurnEnd(u) => sink.turn_end(u),
        }
    }
}

/// An [`AgentSink`] that forwards every call as a [`SinkEvent`] over an unbounded channel. Owned and
/// cheap to share (just the sender behind the `Arc<Mutex<…>>`); the bootstrap drains the receiver onto
/// the real sink as events arrive.
pub struct ChannelSink(tokio::sync::mpsc::UnboundedSender<SinkEvent>);

impl ChannelSink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<SinkEvent>) -> Self {
        Self(tx)
    }
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, t: &str) {
        let _ = self.0.send(SinkEvent::Text(t.to_string()));
    }
    fn thinking_delta(&mut self, t: &str) {
        let _ = self.0.send(SinkEvent::Thinking(t.to_string()));
    }
    fn planning(&mut self, active: bool) {
        let _ = self.0.send(SinkEvent::Planning(active));
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        let _ = self
            .0
            .send(SinkEvent::ToolCall(name.to_string(), input.clone()));
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let _ = self
            .0
            .send(SinkEvent::ToolResult(name.to_string(), result.clone()));
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        let _ = self.0.send(SinkEvent::Observation(o.clone()));
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        let _ = self.0.send(SinkEvent::TurnEnd(usage));
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

    /// Build a minimal host (no canned plans needed) for exercising the retry-breaker directly.
    fn guard_test_host() -> Arc<EngineLoopHost> {
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(Recorder::default())));
        let dir = std::env::temp_dir().join(format!("flux-loop-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store,
            "sess".into(),
            shared,
            CompileOptions::default(),
        );
        host
    }

    /// Fix 2: an identical `run_plan` transcript repeating means the loop is not progressing. The
    /// retry-breaker stays quiet on the first occurrence, escalates the fed-back directive on repeats,
    /// and arms a hard stop once the stall persists — so a stuck loop ends in a few rounds, not 25.
    #[tokio::test]
    async fn retry_breaker_escalates_then_arms_a_hard_stop() {
        let host = guard_test_host();
        let stuck = "[plan error] python_run: provide either `script` or `module`".to_string();
        // Round 1 (stall=1): first sighting — not flagged.
        assert!(!host.guard_transcript(stuck.clone()).contains("loop-guard"));
        // Round 2 (stall=2 = STALL_ESCALATE): escalate, no stop yet.
        let r2 = host.guard_transcript(stuck.clone());
        assert!(r2.contains("[loop-guard]") && r2.contains("NOT making progress"));
        assert!(host.guard.lock().unwrap().force_stop.is_none());
        // Round 3 (stall=3): still escalating.
        assert!(host
            .guard_transcript(stuck.clone())
            .contains("[loop-guard]"));
        assert!(host.guard.lock().unwrap().force_stop.is_none());
        // Round 4 (stall=4 = STALL_STOP): hard stop armed — the next `plan` will end the turn.
        assert!(host.guard_transcript(stuck.clone()).contains("STOP"));
        assert!(host.guard.lock().unwrap().force_stop.is_some());
        // A changed transcript = progress: the counter resets and nothing is flagged.
        assert!(!host
            .guard_transcript("a different result".into())
            .contains("loop-guard"));
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
        let (executor, _host) = EngineLoopHost::install(
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

        // One audit trail across the reentry: the INNER run's `tool_call` markers land in the SAME
        // shared evidence log as the outer ops — because `run_plan` re-enters the same executor/context.
        let calls: Vec<String> = executor
            .evidence()
            .by_kind("tool_call")
            .filter_map(|o| {
                o.data
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .map(String::from)
            })
            .collect();
        assert!(
            calls.contains(&"echo".to_string()) && calls.contains(&"run_plan".to_string()),
            "inner + outer tool calls share one evidence log: {calls:?}"
        );
    }

    /// A provider that records the conversation each `stream()` call saw, then replays a canned response.
    struct RecordingProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        fn name(&self) -> &str {
            "rec"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let convo = req
                .messages
                .iter()
                .map(|m| m.text().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            self.seen.lock().unwrap().push(convo);
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// P4: the loop-carried working conversation. A 2-round `repeat` rebinds `$feedback` to the last
    /// `run_plan` Outcome each iteration (last-writer-wins symbol rebinding, no new machinery); round 2's
    /// `plan` therefore sees round 1's transcript in the conversation handed to the planner.
    #[tokio::test]
    async fn loop_carried_feedback_lets_round_two_see_round_one() {
        let rec_sink = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec_sink.clone())));

        let dir = std::env::temp_dir().join(format!("flux-loop-feedback-{}", std::process::id()));
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

        // Round 1 emits a plan echoing a unique marker; round 2 echoes another.
        let r1 = json!({ "body": [{ "kind": "call", "op": "echo",
            "args": [{ "kind": "lit", "value": "ROUND1-MARKER" }] }] });
        let r2 = json!({ "body": [{ "kind": "call", "op": "echo",
            "args": [{ "kind": "lit", "value": "ROUND2-DONE" }] }] });
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(r1), emit_plan(r2)])),
            seen: Mutex::new(Vec::new()),
        });

        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        // $feedback = ""; repeat 2 { $p = plan($feedback); $r = run_plan($p); $feedback = fmt("{r}") }
        // (`bind` only accepts call/expr/fmt/jq/parse/thing, so the loop-carry copies $r via `fmt`.)
        let loop_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "feedback", "value": { "kind": "fmt", "template": "" } },
                { "kind": "repeat", "max": 2, "body": [
                    { "kind": "bind", "name": "p", "value": { "kind": "call", "op": "plan",
                        "args": [{ "kind": "var", "name": "feedback" }] } },
                    { "kind": "bind", "name": "r", "value": { "kind": "call", "op": "run_plan",
                        "args": [{ "kind": "var", "name": "p" }] } },
                    { "kind": "bind", "name": "feedback", "value": { "kind": "fmt", "template": "{r}" } }
                ]}
            ]
        }))
        .unwrap();

        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        let seen = provider.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "the planner was consulted twice");
        assert!(
            !seen[0].contains("ROUND1-MARKER"),
            "round 1's planner saw an empty working conversation: {:?}",
            seen[0]
        );
        assert!(
            seen[1].contains("ROUND1-MARKER"),
            "round 2's planner saw round 1's transcript via loop-carried $feedback: {:?}",
            seen[1]
        );
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mutating echo: declares a `FilesystemWrite` intent so `plan_risk` marks the plan mutating
    /// (and therefore approval-worthy). Not pre-allowed, so without plan-level approval it would prompt.
    struct WriteEchoTool;
    #[async_trait]
    impl Tool for WriteEchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "writeecho",
                "echo (mutating)",
                json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            )
        }
        fn intents(&self, _p: &Value) -> flux_spec::IntentSet {
            use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
            let mut s = flux_spec::IntentSet::new();
            s.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: "out.txt".into(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
            s
        }
        async fn execute(&self, _c: &ToolContext, p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                p.get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// Counts plan-level vs per-op approval calls. Per-op `request` returns `Deny`, so any op that
    /// wrongly prompts fails — a clean run proves the per-op gate was skipped.
    struct CountingApprover {
        plan_calls: Arc<AtomicUsize>,
        op_calls: Arc<AtomicUsize>,
        allow: bool,
    }
    #[async_trait]
    impl flux_runtime::Approver for CountingApprover {
        async fn request(
            &self,
            _t: &str,
            _s: &[String],
            _i: &flux_spec::IntentSet,
        ) -> flux_runtime::ApprovalChoice {
            self.op_calls.fetch_add(1, Ordering::SeqCst);
            flux_runtime::ApprovalChoice::Deny
        }
        async fn request_plan(&self, _summary: &str, _ops: usize) -> flux_runtime::ApprovalChoice {
            self.plan_calls.fetch_add(1, Ordering::SeqCst);
            if self.allow {
                flux_runtime::ApprovalChoice::Allow
            } else {
                flux_runtime::ApprovalChoice::Deny
            }
        }
    }

    /// Returns the executor `Arc` alongside the host — the host holds only a `Weak<Executor>`, so the
    /// caller must keep this alive for the duration of the run (the real engine does via `FlowEngine`).
    fn setup_host(
        reg: ToolRegistry,
        perms: PermissionManager,
        approver: Arc<dyn flux_runtime::Approver>,
        rec: Recorder,
        tag: &str,
    ) -> (Arc<Executor>, Arc<EngineLoopHost>) {
        let dir =
            std::env::temp_dir().join(format!("flux-planapproval-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(reg, perms, approver, ToolContext::new(system));
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec)));
        EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store,
            "sess".into(),
            shared,
            CompileOptions::default(),
        )
    }

    fn mutating_plan() -> serde_json::Value {
        json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"writeecho","args":[{"kind":"lit","value":"hi"}]}
        ]}})
    }

    /// A mutating plan is approved ONCE at the plan level; its ops then run with no per-op prompt.
    #[tokio::test]
    async fn run_plan_approves_a_mutating_plan_once_then_runs_all_ops() {
        let rec = Recorder::default();
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let op_calls = Arc::new(AtomicUsize::new(0));
        let approver = Arc::new(CountingApprover {
            plan_calls: plan_calls.clone(),
            op_calls: op_calls.clone(),
            allow: true,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteEchoTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::new(),
            approver,
            rec.clone(),
            "approve-once",
        );

        host.run_plan(mutating_plan()).await.unwrap();

        assert_eq!(
            plan_calls.load(Ordering::SeqCst),
            1,
            "asked once, at the plan level"
        );
        assert_eq!(
            op_calls.load(Ordering::SeqCst),
            0,
            "no per-op prompt inside the approved plan"
        );
        assert!(
            rec.tools.lock().unwrap().contains(&"writeecho".to_string()),
            "the op ran"
        );
    }

    /// Rejecting the plan runs nothing and feeds a stop-signal back.
    #[tokio::test]
    async fn run_plan_rejection_runs_nothing() {
        let rec = Recorder::default();
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let op_calls = Arc::new(AtomicUsize::new(0));
        let approver = Arc::new(CountingApprover {
            plan_calls: plan_calls.clone(),
            op_calls: op_calls.clone(),
            allow: false,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteEchoTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::new(),
            approver,
            rec.clone(),
            "reject",
        );

        let out = host.run_plan(mutating_plan()).await.unwrap();

        assert_eq!(plan_calls.load(Ordering::SeqCst), 1);
        assert!(
            out["transcript"]
                .as_str()
                .unwrap_or("")
                .contains("rejected"),
            "rejection is fed back: {out}"
        );
        assert!(
            !rec.tools.lock().unwrap().contains(&"writeecho".to_string()),
            "a rejected plan runs no ops"
        );
    }

    /// A read-only plan needs no approval at all — neither a plan prompt nor a per-op prompt.
    #[tokio::test]
    async fn run_plan_skips_approval_for_a_read_only_plan() {
        let rec = Recorder::default();
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let op_calls = Arc::new(AtomicUsize::new(0));
        let approver = Arc::new(CountingApprover {
            plan_calls: plan_calls.clone(),
            op_calls: op_calls.clone(),
            allow: true,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        // Reads are pre-allowed (as in the real CLI), so a pure-read plan never gates.
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
        let (_ex, host) = setup_host(reg, perms, approver, rec.clone(), "readonly");

        host.run_plan(json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}
        ]}}))
        .await
        .unwrap();

        assert_eq!(
            plan_calls.load(Ordering::SeqCst),
            0,
            "no plan prompt for a read-only plan"
        );
        assert_eq!(
            op_calls.load(Ordering::SeqCst),
            0,
            "no per-op prompt either"
        );
        assert!(
            rec.tools.lock().unwrap().contains(&"echo".to_string()),
            "the read ran"
        );
    }
}
