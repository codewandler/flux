//! The resumable session handle.
//!
//! A [`Session`] is a cheap, cloneable handle to one conversation on a [`Client`](crate::Client)'s
//! engine: `{engine, session_id}` plus the client's turn guard. Sessions are how an embedder
//! resumes work across process restarts (pair with
//! [`Storage::dir`](crate::Storage::dir)) and where the streaming, flow-driven, and
//! observability front doors live.

use std::sync::Arc;

use flux_core::Message;
use flux_core::{Result, Usage};
use flux_flow::ast::DraftAst;
use flux_flow::engine::FlowEngine;
use flux_flow::AgentSink;
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
/// One engine runs one turn at a time (the planner loop is stateful per turn), so concurrent
/// `send`s — on one session or across sessions of the same client — serialize on the client's
/// internal turn guard rather than interleaving.
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
    /// [`Client::open_session`](crate::Client::open_session) and `send` the answer. No planner runs
    /// for the deterministic skeleton, but every op in the flow still dispatches through the one
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
}

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
