//! The voice session driver — binds a realtime session to the runtime's `Executor`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use flux_provider::{RealtimeConnection, RealtimeEvent};
use flux_runtime::{Executor, ToolResult};

use super::sink::VoiceSink;

/// A flux-side handler that owns one voice **turn** — in production, a wrapper over
/// `FlowEngine::run_turn` (the same `session_id` across turns accumulates the conversation) so a
/// flux-lang flow decides each reply. The Phase-2 *engine-owned-turns* seam.
#[async_trait]
pub trait VoiceTurnHandler: Send + Sync {
    /// Handle one completed user turn (their transcript); return what the agent should say next.
    async fn turn(&self, user_text: &str) -> String;
}

/// Drives a realtime voice session: forwards audio/transcripts to a [`VoiceSink`] and routes the
/// model's tool calls through the runtime's `Executor` safety envelope — **off the audio loop**, so a
/// slow tool never stalls audio or barge-in. Tool calls run through `Executor::dispatch` exactly like
/// a text agent's: there is no bypass.
pub struct VoiceSessionDriver {
    executor: Arc<Executor>,
}

impl VoiceSessionDriver {
    /// Build a driver over a runtime executor.
    pub fn new(executor: Arc<Executor>) -> Self {
        Self { executor }
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
                        RealtimeEvent::ResponseDone => {
                            response_active = false;
                            response_done = true;
                            sink.response_done();
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
                        RealtimeEvent::InputTranscriptDone(t) => {
                            sink.input_transcript(&t);
                            // The flow advances one turn and decides the reply; speak it.
                            let reply = handler.turn(&t).await;
                            let _ = session.send_text(&reply).await;
                        }
                        RealtimeEvent::SpeechStarted => {
                            if response_active {
                                let _ = session.cancel_response().await;
                                response_active = false;
                            }
                            sink.barge_in();
                        }
                        RealtimeEvent::ResponseStarted => response_active = true,
                        RealtimeEvent::ResponseDone => {
                            response_active = false;
                            sink.response_done();
                        }
                        // In engine mode the flow owns tools; the model is STT/TTS only.
                        RealtimeEvent::SpeechStopped
                        | RealtimeEvent::SessionReady
                        | RealtimeEvent::ToolCall { .. } => {}
                        RealtimeEvent::Error { message, .. } => sink.error(&message),
                    },
                }
            }
        }

        session.close();
    }
}
