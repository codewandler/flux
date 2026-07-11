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
use flux_flow::engine::FlowEngine;
use flux_flow::AgentSink;
use flux_runtime::ToolResult;
use tokio::sync::Mutex;

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
        Ok(sink.0)
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
