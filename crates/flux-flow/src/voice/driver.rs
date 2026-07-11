//! The voice session driver — binds a realtime session to the runtime's `Executor`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use flux_core::Usage;
use flux_events::{EventKind, EventStore, NewEvent};
use flux_provider::{RealtimeConnection, RealtimeEvent};
use flux_runtime::{Executor, ToolResult};

use super::sink::VoiceSink;
use crate::ast::DraftAst;
use crate::engine::FlowEngine;
use crate::AgentSink;

/// Optional `CallUsage` persistence for a [`VoiceSessionDriver`] (C-38) — the voice analogue of the
/// engine emitting one `EventKind::CallUsage` per provider call (see `flux-flow`'s engine). Configure
/// via [`VoiceSessionDriver::with_usage_recording`]; every response that reports usage appends one
/// un-turn-scoped row (voice sessions have no turns), so `cost_summary` prices a voice session
/// exactly like a text one.
pub struct UsageRecording {
    /// The event log to append `CallUsage` rows to.
    pub events: Arc<EventStore>,
    /// The stream (session id) rows are appended under.
    pub session_id: String,
    /// The canonical attribution key (C-15) stamped on every row. Callers should build this with
    /// `flux_core::canonical_model_spec(Some("openai"), model)` — `"openai"` names the realtime
    /// family's provider, matching the C-15 convention every other call-site uses.
    pub model_spec: String,
}

/// What a [`VoiceTurnHandler`] turn resolves to: keep the call open, or end it. A flow-driven
/// session (D-132) maps a re-suspended flow to [`Continue`](VoiceReply::Continue) (speak the next
/// authored prompt, await the caller) and a completed flow to [`Complete`](VoiceReply::Complete)
/// (speak the final line, then hang up).
pub enum VoiceReply {
    /// Speak this, then await the caller's next turn.
    Continue(String),
    /// Speak this final line, then end the session (the flow completed).
    Complete(String),
}

/// A flux-side handler that owns the voice conversation — in production [`EngineVoiceHandler`], a
/// wrapper over `FlowEngine` so an authored flux-lang flow drives each turn (D-132). The realtime
/// model is the acoustic front-end (STT in, TTS out); the flow owns the logic and speaks first.
#[async_trait]
pub trait VoiceTurnHandler: Send + Sync {
    /// The opening line spoken **before** any caller input — a flow-driven session runs its flow to
    /// the first `await` and speaks that authored prompt. Default: nothing (the caller speaks first,
    /// the pre-D-132 reactive behavior).
    async fn start(&self) -> Option<VoiceReply> {
        None
    }
    /// Handle one completed user turn (their transcript); return what the agent should say next and
    /// whether the session continues.
    async fn turn(&self, user_text: &str) -> VoiceReply;
}

/// Drives a realtime voice session: forwards audio/transcripts to a [`VoiceSink`] and routes the
/// model's tool calls through the runtime's `Executor` safety envelope — **off the audio loop**, so a
/// slow tool never stalls audio or barge-in. Tool calls run through `Executor::dispatch` exactly like
/// a text agent's: there is no bypass.
pub struct VoiceSessionDriver {
    executor: Arc<Executor>,
    usage_recording: Option<UsageRecording>,
}

impl VoiceSessionDriver {
    /// Build a driver over a runtime executor.
    pub fn new(executor: Arc<Executor>) -> Self {
        Self {
            executor,
            usage_recording: None,
        }
    }

    /// Enable `CallUsage` persistence for this driver's sessions (C-38) — see [`UsageRecording`].
    pub fn with_usage_recording(mut self, rec: UsageRecording) -> Self {
        self.usage_recording = Some(rec);
        self
    }

    /// Persist one response's usage as an un-turn-scoped `CallUsage` row, when
    /// [`Self::with_usage_recording`] configured it. Mirrors the engine's own zero-usage skip (no
    /// placeholder rows for a token-less response, `usage.total() == 0`) and is non-fatal — a DB
    /// hiccup must never break the audio loop.
    fn record_usage(&self, usage: Option<&Usage>) {
        let (Some(rec), Some(usage)) = (&self.usage_recording, usage) else {
            return;
        };
        if usage.total() == 0 {
            return;
        }
        let _ = rec.events.append(
            &rec.session_id,
            NewEvent::new(EventKind::CallUsage {
                model: rec.model_spec.clone(),
                usage: usage.clone(),
            }),
        );
    }

    /// Run the session until the event stream ends or `cancel` fires.
    pub async fn run(
        &self,
        conn: RealtimeConnection,
        sink: &mut dyn VoiceSink,
        cancel: &CancellationToken,
    ) {
        // In-flight tool dispatches observe a hangup via the shared per-turn cancel token.
        self.executor.context().set_cancel(cancel.clone());

        let RealtimeConnection {
            session,
            mut events,
        } = conn;
        // Off-loop tool tasks report their results back here so `sink` stays single-threaded.
        let (done_tx, mut done_rx) = mpsc::channel::<(String, ToolResult)>(32);

        // Continuation is debounced: one `create_response` per response, after *all* of its tool
        // calls resolve — never one-per-call (which would fire conflicting responses).
        let mut response_active = false;
        let mut response_done = false;
        let mut response_had_tools = false;
        let mut tools_in_flight: u32 = 0;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                Some((name, result)) = done_rx.recv() => {
                    sink.tool_result(&name, &result);
                    tools_in_flight = tools_in_flight.saturating_sub(1);
                }
                ev = events.next() => match ev {
                    None => break,
                    Some(Err(e)) => sink.error(&e.to_string()),
                    Some(Ok(ev)) => match ev {
                        RealtimeEvent::SessionReady => {}
                        RealtimeEvent::AudioDelta(frame) => sink.audio(&frame),
                        RealtimeEvent::OutputTranscriptDelta(t) | RealtimeEvent::TextDelta(t) => {
                            sink.output_transcript(&t);
                        }
                        RealtimeEvent::InputTranscriptDelta(t)
                        | RealtimeEvent::InputTranscriptDone(t) => sink.input_transcript(&t),
                        RealtimeEvent::SpeechStarted => {
                            // Barge-in: cancel only an active response (the impl swallows the benign
                            // no-active-response race), then flush the caller side.
                            if response_active {
                                let _ = session.cancel_response().await;
                                response_active = false;
                            }
                            // A barge-in is a new user turn — disarm any pending forced continuation so
                            // we don't make the model speak a tool-driven reply over the user. The
                            // in-flight tool result still flows back to the model for history.
                            response_done = false;
                            response_had_tools = false;
                            sink.barge_in();
                        }
                        RealtimeEvent::SpeechStopped => {}
                        RealtimeEvent::ResponseStarted => {
                            response_active = true;
                            response_done = false;
                            response_had_tools = false;
                        }
                        RealtimeEvent::ResponseDone { usage } => {
                            response_active = false;
                            response_done = true;
                            self.record_usage(usage.as_ref());
                            sink.response_done(usage.as_ref());
                        }
                        RealtimeEvent::ToolCall { call_id, name, arguments } => {
                            // Default malformed/empty args to an empty object (tools expect an object).
                            let params: Value = serde_json::from_str(&arguments)
                                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
                            sink.tool_call(&name, &params);
                            response_had_tools = true;
                            tools_in_flight += 1;
                            let exec = self.executor.clone();
                            let session = session.clone();
                            let done_tx = done_tx.clone();
                            tokio::spawn(async move {
                                let result = exec.dispatch(&name, params).await; // full envelope
                                let _ = session.send_tool_result(&call_id, result.view()).await;
                                let _ = done_tx.send((name, result)).await;
                            });
                        }
                        RealtimeEvent::Error { message, .. } => sink.error(&message),
                    },
                }
            }

            // Once the model has finished a response that made tool calls and every result is back,
            // ask it to continue — exactly once.
            if response_done && response_had_tools && tools_in_flight == 0 {
                let _ = session.create_response().await;
                response_done = false;
                response_had_tools = false;
            }
        }

        session.close();
    }

    /// **Phase-2 spike (experimental).** Run an *engine-owned-turns* session: a flux-side `handler`
    /// (in production, a `FlowEngine::run_turn` over a flux-lang flow) owns the conversation across
    /// turns. Each completed user turn (`InputTranscriptDone`) is handed to [`VoiceTurnHandler::turn`],
    /// whose reply is spoken via [`RealtimeSession::send_text`]; the realtime model is the acoustic
    /// front-end (transcribe in, speak out) while the flow drives the logic.
    ///
    /// This reuses per-turn handling (the D-09 "agent-turn-per-event" shape applied to voice), **not**
    /// cross-turn `await` — a single suspendable flow owning the whole call stays future work. The
    /// handler runs inline here for simplicity; a production driver would spawn it off the audio loop
    /// like a tool call. The executor this driver holds is unused in this mode (the flow owns tools).
    ///
    /// Configure [`RealtimeConfig::turn_detection`](flux_provider::RealtimeConfig) so the model does
    /// **not** auto-respond (`TurnDetection::None`, or server-VAD with response creation off) —
    /// otherwise the model's own auto-reply races the flow's `send_text` reply and both speak.
    pub async fn run_flow_turns(
        &self,
        conn: RealtimeConnection,
        sink: &mut dyn VoiceSink,
        handler: &dyn VoiceTurnHandler,
        cancel: &CancellationToken,
    ) {
        let RealtimeConnection {
            session,
            mut events,
        } = conn;
        let mut response_active = false;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                ev = events.next() => match ev {
                    None => break,
                    Some(Err(e)) => sink.error(&e.to_string()),
                    Some(Ok(ev)) => match ev {
                        RealtimeEvent::AudioDelta(frame) => sink.audio(&frame),
                        RealtimeEvent::OutputTranscriptDelta(t) | RealtimeEvent::TextDelta(t) => {
                            sink.output_transcript(&t);
                        }
                        RealtimeEvent::InputTranscriptDelta(t) => sink.input_transcript(&t),
                        // The flow speaks first (D-132): run it to its first `await` and speak the
                        // authored prompt before any caller utterance. A no-op for a reactive
                        // (caller-speaks-first) handler, whose `start` defaults to `None`.
                        RealtimeEvent::SessionReady => {
                            if let Some(reply) = handler.start().await {
                                if speak_reply(session.as_ref(), sink, reply).await {
                                    break;
                                }
                            }
                        }
                        RealtimeEvent::InputTranscriptDone(t) => {
                            sink.input_transcript(&t);
                            // The flow advances one turn (resumes its suspension) and decides the
                            // reply; speak it, and end the session if the flow completed.
                            let reply = handler.turn(&t).await;
                            if speak_reply(session.as_ref(), sink, reply).await {
                                break;
                            }
                        }
                        RealtimeEvent::SpeechStarted => {
                            if response_active {
                                let _ = session.cancel_response().await;
                                response_active = false;
                            }
                            sink.barge_in();
                        }
                        RealtimeEvent::ResponseStarted => response_active = true,
                        RealtimeEvent::ResponseDone { usage } => {
                            response_active = false;
                            self.record_usage(usage.as_ref());
                            sink.response_done(usage.as_ref());
                        }
                        // In engine mode the flow owns tools; the model is STT/TTS only.
                        RealtimeEvent::SpeechStopped
                        | RealtimeEvent::ToolCall { .. } => {}
                        RealtimeEvent::Error { message, .. } => sink.error(&message),
                    },
                }
            }
        }

        session.close();
    }
}

/// Speak a [`VoiceReply`] over the realtime session. Returns `true` when the session should end (the
/// flow completed): the final line is spoken, the consumer is signalled via
/// [`VoiceSink::session_ended`] (its hangup/handoff hook), and the driver loop breaks into
/// `session.close()`.
async fn speak_reply(
    session: &dyn flux_provider::RealtimeSession,
    sink: &mut dyn VoiceSink,
    reply: VoiceReply,
) -> bool {
    match reply {
        VoiceReply::Continue(text) => {
            let _ = session.send_text(&text).await;
            false
        }
        VoiceReply::Complete(text) => {
            if !text.trim().is_empty() {
                let _ = session.send_text(&text).await;
            }
            sink.session_ended(&text);
            true
        }
    }
}

/// A [`VoiceTurnHandler`] backed by a [`FlowEngine`] running an authored flow — the production bridge
/// for a flow-driven voice session (D-132). `start` runs the flow to its first `await` and speaks the
/// authored prompt; each caller turn resumes the suspension (the engine's suspension-first routing);
/// the flow completing ends the call. Model cognition runs only where the flow calls it (an
/// `ai_segment`), through the engine's shared safety envelope — the driver's own executor is unused.
pub struct EngineVoiceHandler {
    engine: Arc<FlowEngine>,
    session_id: String,
    flow: DraftAst,
}

impl EngineVoiceHandler {
    /// Bridge a `FlowEngine` + an authored flow into the voice driver's [`VoiceTurnHandler`] seam.
    pub fn new(engine: Arc<FlowEngine>, session_id: impl Into<String>, flow: DraftAst) -> Self {
        Self {
            engine,
            session_id: session_id.into(),
            flow,
        }
    }

    /// Classify the just-run turn from the store: a re-suspended flow keeps the call open (speak the
    /// next authored prompt), a completed flow ends it (speak the final line, then hang up).
    fn classify(&self, spoken: String) -> VoiceReply {
        let spoken = spoken.trim().to_string();
        match self.engine.flow.has_suspension(&self.session_id) {
            Ok(true) => VoiceReply::Continue(spoken),
            _ => VoiceReply::Complete(spoken),
        }
    }
}

#[async_trait]
impl VoiceTurnHandler for EngineVoiceHandler {
    async fn start(&self) -> Option<VoiceReply> {
        let mut cap = PromptCapture::default();
        let _ = self
            .engine
            .start_flow_turn(&self.session_id, &self.flow, &mut cap)
            .await;
        Some(self.classify(cap.text))
    }

    async fn turn(&self, user_text: &str) -> VoiceReply {
        let mut cap = PromptCapture::default();
        let _ = self
            .engine
            .run_turn(&self.session_id, user_text, &mut cap)
            .await;
        self.classify(cap.text)
    }
}

/// A tiny [`AgentSink`] that captures a turn's spoken text — the flow's authored prompt, surfaced by
/// the engine via `text_delta` (D-131) — so an [`EngineVoiceHandler`] can hand it to the realtime
/// channel. Everything else (tool activity, observations) is ignored: voice speaks only the prompt.
#[derive(Default)]
struct PromptCapture {
    text: String,
}

impl AgentSink for PromptCapture {
    fn text_delta(&mut self, text: &str) {
        self.text.push_str(text);
    }
}
