//! The resumable session handle.
//!
//! A [`Session`] is a cheap, cloneable handle to one conversation on a [`Client`](crate::Client)'s
//! engine: `{engine, session_id}` plus the client's operation guard. Sessions are how an embedder
//! resumes work across process restarts (pair with
//! [`Storage::dir`](crate::Storage::dir)) and where the streaming, flow-driven, and
//! observability front doors live.

use std::sync::Arc;

use flux_core::Message;
use flux_core::{PricingTable, Result, Usage};
use flux_events::{EfficiencySummary, EventContext, ModelCost, RunDiff, TurnSummary};
use flux_flow::ast::DraftAst;
use flux_flow::ast::RunEvent;
use flux_flow::engine::FlowEngine;
use flux_flow::fork::ForkPrefix;
use flux_flow::replay::ReplayReport;
use flux_flow::voice::{EngineVoiceHandler, VoiceSessionDriver, VoiceSink};
use flux_flow::AgentSink;
use flux_provider::{RealtimeConfig, RealtimeProvider};
use flux_runtime::ToolResult;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::events::{ChannelSink, TeeSink, TurnStream};
use crate::TurnOutput;

/// A handle to one conversation (an event-store session) on a client's engine.
///
/// Obtained from [`Client::create_session`](crate::Client::create_session),
/// [`Client::open_session`](crate::Client::open_session), or
/// [`Client::latest_session`](crate::Client::latest_session). Clones share the same session.
///
/// [`FlowEngine`] itself serializes its public turn entries, including calls made through the
/// advanced engine escape hatch. The SDK's broader operation guard also orders those turns against
/// replay, fork, and divergence operations that share the client's executor and stores.
#[derive(Clone)]
pub struct Session {
    pub(crate) engine: Arc<FlowEngine>,
    pub(crate) id: String,
    pub(crate) turn_guard: Arc<Mutex<()>>,
}

impl Session {
    /// The session id. Persist it (with [`Storage::dir`](crate::Storage::dir)) to resume this
    /// conversation in a later process via [`Client::open_session`](crate::Client::open_session).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Run one turn to completion, collecting the final text, the tools invoked, and usage.
    ///
    /// If the session is parked on a top-level `await` (a suspended flow), the input answers the
    /// `await` and the flow resumes — same engine behavior the CLI has.
    pub async fn send(&self, input: &str) -> Result<TurnOutput> {
        let _turn = self.turn_guard.lock().await;
        let mut sink = Collector::default();
        self.engine.run_turn(&self.id, input, &mut sink).await?;
        self.finalize(sink.0)
    }

    /// Drive an authored flow as this session's conversation (D-131). Runs the flow to its first
    /// top-level `await`, surfaces the flow's own **authored prompt** (its last emitted view) as the
    /// turn text, and returns with [`suspended`](TurnOutput::suspended) `= true`. Answer the prompt
    /// with [`send`](Self::send) to resume to the next `await`; a flow that completes without any
    /// `await` returns its result with `suspended = false`.
    ///
    /// Durable: with persistent [`Storage::dir`](crate::Storage::dir) the suspension survives a
    /// process restart — reopen the session with
    /// [`Client::open_session`](crate::Client::open_session) and `send` the answer. No adaptive
    /// model stage runs for the deterministic skeleton, but every op in the flow still dispatches through the one
    /// authorization → approval → guarded-IO envelope.
    pub async fn start_flow(&self, flow: &DraftAst) -> Result<TurnOutput> {
        let _turn = self.turn_guard.lock().await;
        let mut sink = Collector::default();
        self.engine
            .start_flow_turn(&self.id, flow, &mut sink)
            .await?;
        self.finalize(sink.0)
    }

    /// Whether this session is currently parked on a top-level `await` — i.e. a flow started with
    /// [`start_flow`](Self::start_flow) suspended and is waiting for the awaited input. Resume by
    /// sending it with [`send`](Self::send). A **non-consuming** check: it reports the state without
    /// clearing the suspension.
    pub fn suspended(&self) -> Result<bool> {
        self.engine.flow.has_suspension(&self.id)
    }

    /// Stamp the post-turn suspension state onto a collected [`TurnOutput`]. The suspension is a
    /// property of the session (persisted by the engine), not a sink event, so every turn door
    /// reads it back the same way — a flow-driven session reports whether it re-parked or completed.
    fn finalize(&self, mut out: TurnOutput) -> Result<TurnOutput> {
        out.suspended = self.engine.flow.has_suspension(&self.id)?;
        Ok(out)
    }

    /// Run one turn, streaming every event to your own [`AgentSink`] as it happens — text and
    /// thinking deltas, plan progress, tool calls **and tool results** — while still returning the
    /// collected [`TurnOutput`]. Cancel via the token: the engine drops the in-flight op and
    /// persists exactly one assistant message (the log stays a valid `user → assistant`
    /// alternation).
    pub async fn send_with(
        &self,
        input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<TurnOutput> {
        let _turn = self.turn_guard.lock().await;
        let mut tee = TeeSink {
            consumer: sink,
            collect: Collector::default(),
        };
        self.engine
            .run_turn_cancellable(&self.id, input, &mut tee, cancel)
            .await?;
        self.finalize(tee.collect.0)
    }

    /// Run one turn as a [`TurnStream`] — a stream of owned
    /// [`AgentEvent`](crate::events::AgentEvent)s plus `cancel()`/`finish()`. The turn runs on a
    /// spawned task (the client's turn guard still serializes it against other turns), so events
    /// arrive as they happen whether or not you are polling. Dropping the returned stream cancels
    /// the turn.
    ///
    /// # Panics
    /// Spawns the turn eagerly, so it must be called from within a Tokio runtime (like any
    /// `tokio::spawn`-based API); calling it outside one panics. Use [`send`](Self::send) or
    /// [`send_with`](Self::send_with) when you only have a future to `.await`.
    pub fn stream(&self, input: &str) -> TurnStream {
        let engine = self.engine.clone();
        let id = self.id.clone();
        let guard = self.turn_guard.clone();
        let input = input.to_string();
        let cancel = CancellationToken::new();
        let child = cancel.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let _turn = guard.lock().await;
            let mut sink = ChannelSink {
                tx,
                collect: Collector::default(),
            };
            engine
                .run_turn_cancellable(&id, &input, &mut sink, &child)
                .await?;
            let mut out = sink.collect.0;
            // Stamp the post-turn suspension state, exactly as `finalize` does for the awaited
            // doors — a streamed flow-driven turn reports whether it re-parked or completed.
            out.suspended = engine.flow.has_suspension(&id)?;
            Ok(out)
        });
        TurnStream {
            rx,
            handle: Some(handle),
            cancel,
        }
    }

    /// The conversation so far — the user/assistant messages projected from the event store.
    /// Survives restarts with persistent [`Storage`](crate::Storage).
    pub fn history(&self) -> Result<Vec<Message>> {
        self.engine.events.conversation(&self.id)
    }

    /// One [`TurnSummary`] per turn recorded on this session — model, timing, usage, and status.
    /// Projected from the event store (`begin_turn`/`end_turn`), so it reflects every turn ever run,
    /// across restarts with persistent [`Storage`](crate::Storage).
    pub fn turns(&self) -> Result<Vec<TurnSummary>> {
        self.engine.events.turns(&self.id)
    }

    /// The structured run trace — the ordered [`RunEvent`]s (plan steps, op successes/failures)
    /// recorded during this session's turns. The machine-readable companion to [`history`](Self::history).
    pub fn run_trace(&self) -> Result<Vec<RunEvent>> {
        self.engine.events.run_trace(&self.id)
    }

    /// Per-model cost for this session: one [`ModelCost`] row per model that billed, priced against
    /// `pricing`. Use [`PricingTable::builtin`](crate::PricingTable::builtin), or the `pricing`
    /// feature's [`load_pricing_table`](crate::pricing::load_pricing_table) to fold in a user's
    /// `~/.flux/pricing.toml`.
    pub fn cost(&self, pricing: &PricingTable) -> Result<Vec<ModelCost>> {
        self.engine.events.cost_summary(&self.id, pricing)
    }

    /// The session's efficiency roll-up (tokens, tool calls, repair rounds, …), or `None` if nothing
    /// has been recorded yet.
    pub fn efficiency(&self) -> Result<Option<EfficiencySummary>> {
        self.engine.events.efficiency(&self.id)
    }

    /// Run a **flow-driven** full-duplex voice session (D-132): an authored `flow` owns the whole
    /// call. The flow speaks first — it runs to its first top-level `await` and the realtime model
    /// speaks the authored prompt (STT/TTS only, no adaptive model stage) — each caller utterance resumes the
    /// suspension to the next prompt, and the flow completing hangs up (via
    /// [`VoiceSink::session_ended`]). This is the voice counterpart of
    /// [`start_flow`](Self::start_flow), and it rides the session's own persistent engine, so the
    /// suspension state is durable across a reconnect.
    ///
    /// Contrast with the **model-driven** [`FlowClient::run_voice_session`](crate::FlowClient::run_voice_session),
    /// where the model leads the conversation and calls tools; here the flow leads and the model is
    /// pure speech I/O. `provider` is any [`RealtimeProvider`]; `cancel` ends the call (e.g. a
    /// hangup).
    pub async fn run_voice_flow(
        &self,
        provider: &dyn RealtimeProvider,
        config: RealtimeConfig,
        flow: DraftAst,
        sink: &mut dyn VoiceSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let _operation = self.turn_guard.lock().await;
        // Flow-driven: the flow owns tools (the model is STT/TTS only), so no tool defs are declared.
        let conn = provider.connect(config).await?;
        let handler = EngineVoiceHandler::new(self.engine.clone(), self.id.clone(), flow);
        // The driver's own executor is unused in flow mode (the engine drives every turn through its
        // own envelope); reuse the engine's so no throwaway is built.
        VoiceSessionDriver::new(self.engine.executor.clone())
            .run_flow_turns(conn, sink, &handler, cancel)
            .await;
        Ok(())
    }

    /// Hermetically **replay** this recorded session (the time machine): re-run its plans with every
    /// leaf-op output served from the recorded cassette — **zero live dispatches**, side effects
    /// never re-fire, the model is never called — streaming the reconstructed turns to `sink`. `turn`
    /// replays a single 0-based turn; `None` replays the whole session. Returns a [`ReplayReport`]
    /// (the replayed plans, a divergence diagnostic, and cassette-cell accounting).
    ///
    /// Requires a cassette-recorded session — a run that dispatched ops with the cassette on (the
    /// default; disabled by `FLUX_CASSETTE=0`). A chat-only or pre-cassette session has no cells and
    /// errors honestly. Because replay serves every op from the cassette, it is safe to run against a
    /// client built with a deny-all approver and a never-called provider.
    pub async fn replay(
        &self,
        turn: Option<usize>,
        sink: &mut dyn AgentSink,
    ) -> Result<ReplayReport> {
        let _operation = self.turn_guard.lock().await;
        flux_flow::replay::replay_session(
            &self.engine.events,
            &self.engine.executor,
            &self.id,
            turn,
            sink,
        )
        .await
        .map_err(|e| flux_core::Error::Other(e.to_string()))
    }

    /// **Fork** this session at statement `at` of its recorded final plan (the time machine's
    /// counterfactual door): mint a fresh session correlated to this one, copy its conversation, and
    /// hermetically replay the prefix (statements `0..at`) into it — leaving **this** session
    /// untouched. Diverge the returned [`Fork`] with [`Fork::inject`]/[`Fork::edit`], then
    /// [`Fork::diff`] it against the original. Requires a cassette-recorded session (as
    /// [`replay`](Self::replay) does).
    pub async fn fork(&self, at: usize) -> Result<Fork> {
        let _turn = self.turn_guard.lock().await;
        let info = self.engine.events.info(&self.id)?;
        let fork_id = self.engine.events.create_session_with_context(
            &info.model,
            &EventContext {
                correlation_id: Some(self.id.clone()),
                agent_id: Some(format!("fork:{}@{}", self.id, at)),
                ..Default::default()
            },
        )?;
        // Copy the conversation so the fork's adaptive tail sees the same history.
        for m in self.engine.events.conversation(&self.id)? {
            self.engine.events.record_message(&fork_id, &m)?;
        }
        // Replay the prefix into the fork session. The replay is hermetic (cassette-served), so its
        // events don't need surfacing — discard them.
        let mut sink = DiscardSink;
        let prefix = flux_flow::fork::replay_prefix(
            &self.engine.events,
            &self.engine.flow,
            &self.engine.executor,
            &self.id,
            &fork_id,
            at,
            &mut sink,
        )
        .await
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
        Ok(Fork {
            engine: self.engine.clone(),
            id: fork_id,
            prefix,
            turn_guard: self.turn_guard.clone(),
        })
    }
}

/// A counterfactual branch of a [`Session`], created by [`Session::fork`]. The recorded prefix has
/// been replayed hermetically into a fresh session (the original is untouched); diverge it with
/// [`inject`](Self::inject) or [`edit`](Self::edit), then compare with [`diff`](Self::diff). The
/// fork is itself a real, recorded session — reach it as a [`Session`] via [`session`](Self::session)
/// (for `history`/`turns`/`replay`/further forks).
pub struct Fork {
    engine: Arc<FlowEngine>,
    id: String,
    prefix: ForkPrefix,
    turn_guard: Arc<Mutex<()>>,
}

impl Fork {
    /// The fork session's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The fork as a [`Session`] handle — read its `history`/`turns`, `replay` it, or fork it again.
    pub fn session(&self) -> Session {
        Session {
            engine: self.engine.clone(),
            id: self.id.clone(),
            turn_guard: self.turn_guard.clone(),
        }
    }

    /// Diverge by **injecting** a different value at the fork point's bound statement (skipping the
    /// op that produced it), then run the plan's tail through the real envelope, streaming to `sink`.
    /// Errors if the fork point isn't a `bind` statement, or the diverged tail halts.
    pub async fn inject(&self, value: &serde_json::Value, sink: &mut dyn AgentSink) -> Result<()> {
        let _turn = self.turn_guard.lock().await;
        let out = flux_flow::fork::diverge_inject(
            &self.engine.flow,
            &self.engine.executor,
            &self.id,
            &self.prefix,
            value,
            sink,
        )
        .await
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
        if let Some(halt) = out.failure {
            return Err(flux_core::Error::Other(format!(
                "fork tail halted: {}",
                halt.message
            )));
        }
        Ok(())
    }

    /// Diverge by **editing** the plan (an alternate `DraftAst` for the tail from the fork point),
    /// then run it through the real envelope, streaming to `sink`. Errors if the diverged tail halts.
    pub async fn edit(&self, edited: &DraftAst, sink: &mut dyn AgentSink) -> Result<()> {
        let _turn = self.turn_guard.lock().await;
        let out = flux_flow::fork::diverge_edit(
            &self.engine.flow,
            &self.engine.executor,
            &self.id,
            &self.prefix,
            edited,
            sink,
        )
        .await
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
        if let Some(halt) = out.failure {
            return Err(flux_core::Error::Other(format!(
                "fork tail halted: {}",
                halt.message
            )));
        }
        Ok(())
    }

    /// The aligned per-statement divergence between `original` and this fork
    /// ([`flux_events::run_diff`] over the two run traces): which statements ran the same, which
    /// plan changed, and which hit a different recorded world.
    pub fn diff(&self, original: &Session) -> Result<RunDiff> {
        let a = original.engine.events.run_trace(&original.id)?;
        let b = self.engine.events.run_trace(&self.id)?;
        Ok(flux_events::run_diff(&a, &b))
    }
}

/// A throwaway [`AgentSink`] that discards every event — used where a driver requires a sink but the
/// caller doesn't consume it (the hermetic prefix replay inside [`Session::fork`]).
struct DiscardSink;
impl AgentSink for DiscardSink {}

/// Collects a turn's stream into a [`TurnOutput`].
#[derive(Default)]
pub(crate) struct Collector(pub(crate) TurnOutput);

impl AgentSink for Collector {
    fn text_delta(&mut self, t: &str) {
        self.0.text.push_str(t);
    }
    fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
        self.0.tool_calls.push(name.to_string());
    }
    fn tool_result(&mut self, _name: &str, _result: &ToolResult) {}
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.0.usage = usage;
    }
}
