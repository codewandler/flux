//! Owned streaming events — the async-iterator shape of a turn.
//!
//! [`AgentEvent`] mirrors the [`AgentSink`](crate::AgentSink) trait **1:1** (every sink method has
//! a variant, nothing is dropped); [`TurnStream`] is a live turn as a stream of those events plus
//! `cancel()` and `finish()`. Use [`Session::stream`](crate::Session::stream) when you want to
//! consume events with a loop instead of implementing a sink; use
//! [`Session::send_with`](crate::Session::send_with) when you already have an `AgentSink`.

use std::pin::Pin;
use std::task::{Context, Poll};

use flux_core::{Result, Usage};
use flux_flow::AgentSink;
use flux_runtime::ToolResult;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::session::Collector;
use crate::TurnOutput;

/// One streaming event from a live turn. Mirrors [`AgentSink`](crate::AgentSink) 1:1 — each
/// variant corresponds to the sink method of the same name.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    /// `AgentSink::text_delta` — a chunk of the assistant's streaming answer.
    TextDelta(String),
    /// `AgentSink::thinking_delta` — a chunk of the model's thinking stream, when the provider
    /// surfaces one.
    ThinkingDelta(String),
    /// `AgentSink::planning` — the planner started (`true`) / finished (`false`) composing a plan.
    Planning(bool),
    /// `AgentSink::plan_delta` — a progressive plan-skeleton headline while the plan streams.
    PlanDelta(String),
    /// `AgentSink::tool_call` — an op is about to dispatch with this input.
    ToolCall {
        /// The op name.
        name: String,
        /// The op's input value.
        input: serde_json::Value,
    },
    /// `AgentSink::tool_result` — an op finished; the full two-face result (canonical `content` +
    /// optional model-facing `view`).
    ToolResult {
        /// The op name.
        name: String,
        /// The op's result.
        result: ToolResult,
    },
    /// `AgentSink::observation` — an audit observation made during dispatch.
    Observation(flux_evidence::Observation),
    /// `AgentSink::turn_end` — the turn completed; usage if the provider reported it.
    TurnEnd {
        /// Token usage for the turn.
        usage: Option<Usage>,
    },
}

/// A live turn: a stream of [`AgentEvent`]s plus control. Produced by
/// [`Session::stream`](crate::Session::stream); the turn runs on a spawned task, so events arrive
/// as they happen whether or not you are polling.
///
/// Also implements [`futures::Stream`], so `while let Some(ev) = stream.next().await` and
/// combinator pipelines both work.
pub struct TurnStream {
    pub(crate) rx: mpsc::UnboundedReceiver<AgentEvent>,
    pub(crate) handle: tokio::task::JoinHandle<Result<TurnOutput>>,
    pub(crate) cancel: CancellationToken,
}

impl TurnStream {
    /// Abort the turn. The engine's cancellable path drops the in-flight op and persists exactly
    /// one assistant message, so the session log stays a valid `user → assistant` alternation.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The next event, or `None` once the turn is over and the stream is drained.
    pub async fn next(&mut self) -> Option<AgentEvent> {
        self.rx.recv().await
    }

    /// Wait for the turn to complete and return the collected [`TurnOutput`] (the same output a
    /// plain [`Session::send`](crate::Session::send) would have returned). Undelivered events are
    /// dropped — read them via [`next`](Self::next) first if you want them.
    pub async fn finish(self) -> Result<TurnOutput> {
        match self.handle.await {
            Ok(out) => out,
            Err(e) => Err(flux_core::Error::Other(format!("turn task failed: {e}"))),
        }
    }
}

impl futures::Stream for TurnStream {
    type Item = AgentEvent;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// An [`AgentSink`] that forwards owned [`AgentEvent`]s over a channel while also collecting the
/// turn into a [`TurnOutput`]. Send failures are ignored: a dropped receiver means the consumer
/// stopped listening, not that the turn should fail.
pub(crate) struct ChannelSink {
    pub(crate) tx: mpsc::UnboundedSender<AgentEvent>,
    pub(crate) collect: Collector,
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, text: &str) {
        self.collect.text_delta(text);
        let _ = self.tx.send(AgentEvent::TextDelta(text.to_string()));
    }
    fn thinking_delta(&mut self, text: &str) {
        self.collect.thinking_delta(text);
        let _ = self.tx.send(AgentEvent::ThinkingDelta(text.to_string()));
    }
    fn planning(&mut self, active: bool) {
        self.collect.planning(active);
        let _ = self.tx.send(AgentEvent::Planning(active));
    }
    fn plan_delta(&mut self, headline: &str) {
        self.collect.plan_delta(headline);
        let _ = self.tx.send(AgentEvent::PlanDelta(headline.to_string()));
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.collect.tool_call(name, input);
        let _ = self.tx.send(AgentEvent::ToolCall {
            name: name.to_string(),
            input: input.clone(),
        });
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.collect.tool_result(name, result);
        let _ = self.tx.send(AgentEvent::ToolResult {
            name: name.to_string(),
            result: result.clone(),
        });
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.collect.observation(o);
        let _ = self.tx.send(AgentEvent::Observation(o.clone()));
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.collect.turn_end(usage.clone());
        let _ = self.tx.send(AgentEvent::TurnEnd { usage });
    }
}

/// An [`AgentSink`] tee: forwards every event to a consumer sink while also collecting the turn
/// into a [`TurnOutput`] — how [`Session::send_with`](crate::Session::send_with) returns an output
/// without stealing the consumer's stream.
pub(crate) struct TeeSink<'a> {
    pub(crate) consumer: &'a mut dyn AgentSink,
    pub(crate) collect: Collector,
}

impl AgentSink for TeeSink<'_> {
    fn text_delta(&mut self, text: &str) {
        self.collect.text_delta(text);
        self.consumer.text_delta(text);
    }
    fn thinking_delta(&mut self, text: &str) {
        self.collect.thinking_delta(text);
        self.consumer.thinking_delta(text);
    }
    fn planning(&mut self, active: bool) {
        self.collect.planning(active);
        self.consumer.planning(active);
    }
    fn plan_delta(&mut self, headline: &str) {
        self.collect.plan_delta(headline);
        self.consumer.plan_delta(headline);
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.collect.tool_call(name, input);
        self.consumer.tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.collect.tool_result(name, result);
        self.consumer.tool_result(name, result);
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.collect.observation(o);
        self.consumer.observation(o);
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.collect.turn_end(usage.clone());
        self.consumer.turn_end(usage);
    }
}
