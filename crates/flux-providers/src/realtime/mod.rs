//! OpenAI Realtime — the concrete full-duplex (voice-to-voice) provider.
//!
//! Implements [`flux_provider::RealtimeProvider`] over a WebSocket (ported from a downstream
//! `realtime` crate). The half-duplex HTTP family in this crate (`messages`/`openai`/…) is unrelated;
//! this is the WebSocket sibling, behind the `realtime` Cargo feature so the default build stays lean.
//!
//! `openai_realtime(api_key)` (or `openai_realtime_from_env`) yields a provider whose `connect` opens a
//! session; the model's tool calls surface as [`RealtimeEvent::ToolCall`] for a higher layer to route
//! through the safety envelope.

mod client;
mod config;
mod event;

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use tokio::sync::mpsc;

use flux_core::{AudioEncoding, AudioFormat, Error, Result};
use flux_provider::realtime::{
    RealtimeConfig, RealtimeConnection, RealtimeEvent, RealtimeEventStream, RealtimeProvider,
    RealtimeSession, TurnDetection as SeamTurnDetection,
};
use flux_provider::TokenSource;

use client::{connect_ws, RealtimeHandle};
use config::{
    default_model, ClientConfig, SessionConfig, Tool, TurnDetection as WireTurnDetection,
};
use event::{is_benign_cancel_race, ServerEvent};

/// How the provider authenticates — an API key or a refreshing OAuth token source. Mirrors the
/// HTTP `OpenAiCred` pattern; `Credential::apply` is reqwest-bound and useless against a tungstenite
/// handshake, so the bearer header is set on the WS request directly (see `client`).
enum Secret {
    ApiKey(String),
    OAuth(Arc<dyn TokenSource>),
}

/// The OpenAI Realtime provider.
pub struct OpenAiRealtime {
    secret: Secret,
}

/// `openai-realtime` provider via API key.
pub fn openai_realtime(api_key: impl Into<String>) -> OpenAiRealtime {
    OpenAiRealtime {
        secret: Secret::ApiKey(api_key.into()),
    }
}

/// `openai-realtime` provider from `OPENAI_KEY` (then `OPENAI_API_KEY`) — the single key read that
/// replaces scattered downstream key reads.
pub fn openai_realtime_from_env() -> Result<OpenAiRealtime> {
    let key = std::env::var("OPENAI_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map_err(|_| Error::Auth("OPENAI_KEY / OPENAI_API_KEY not set".into()))?;
    if key.trim().is_empty() {
        return Err(Error::Auth("OPENAI_KEY is empty".into()));
    }
    Ok(openai_realtime(key))
}

/// `openai-realtime` provider backed by a refreshing OAuth [`TokenSource`].
pub fn openai_realtime_oauth(token: Arc<dyn TokenSource>) -> OpenAiRealtime {
    OpenAiRealtime {
        secret: Secret::OAuth(token),
    }
}

#[async_trait]
impl RealtimeProvider for OpenAiRealtime {
    fn name(&self) -> &str {
        "openai-realtime"
    }

    async fn connect(&self, config: RealtimeConfig) -> Result<RealtimeConnection> {
        let api_key = match &self.secret {
            Secret::ApiKey(k) => k.clone(),
            Secret::OAuth(ts) => ts.access_token().await?,
        };
        let model = if config.model.is_empty() {
            default_model()
        } else {
            config.model.clone()
        };
        let cc = ClientConfig {
            api_key,
            model,
            session: Some(to_session_config(&config)),
        };
        let (handle, rx) = connect_ws(cc).await?;
        Ok(RealtimeConnection {
            session: Arc::new(OpenAiSession { handle }),
            events: map_events(rx),
        })
    }
}

/// The control half — wraps the lifted [`RealtimeHandle`].
struct OpenAiSession {
    handle: RealtimeHandle,
}

#[async_trait]
impl RealtimeSession for OpenAiSession {
    async fn send_audio(&self, frame: &[u8]) -> Result<()> {
        self.handle.append_audio(frame).await
    }
    async fn commit_audio(&self) -> Result<()> {
        self.handle.commit_audio().await
    }
    async fn send_text(&self, text: &str) -> Result<()> {
        self.handle.send_user_text(text).await
    }
    async fn create_response(&self) -> Result<()> {
        self.handle.create_response().await
    }
    async fn cancel_response(&self) -> Result<()> {
        self.handle.cancel_response().await
    }
    async fn send_tool_result(&self, call_id: &str, output: &str) -> Result<()> {
        self.handle.send_function_output(call_id, output).await
    }
    fn close(&self) {
        self.handle.close();
    }
}

/// Map flux's provider-agnostic [`RealtimeConfig`] onto the OpenAI wire [`SessionConfig`].
fn to_session_config(cfg: &RealtimeConfig) -> SessionConfig {
    let turn_detection = match &cfg.turn_detection {
        SeamTurnDetection::ServerVad {
            threshold,
            prefix_padding_ms,
            silence_duration_ms,
        } => Some(WireTurnDetection {
            kind: "server_vad".into(),
            threshold: threshold.map(f64::from),
            prefix_padding_ms: *prefix_padding_ms,
            silence_duration_ms: *silence_duration_ms,
            eagerness: None,
        }),
        SeamTurnDetection::SemanticVad { eagerness } => Some(WireTurnDetection {
            kind: "semantic_vad".into(),
            threshold: None,
            prefix_padding_ms: None,
            silence_duration_ms: None,
            eagerness: eagerness.clone(),
        }),
        SeamTurnDetection::None => None,
    };
    let tools = if cfg.tools.is_empty() {
        None
    } else {
        Some(
            cfg.tools
                .iter()
                .map(|t| {
                    Tool::function(
                        t.name.clone(),
                        t.description.clone(),
                        t.input_schema.clone(),
                    )
                })
                .collect(),
        )
    };
    SessionConfig {
        modalities: Some(vec!["audio".into(), "text".into()]),
        instructions: cfg.system.clone(),
        voice: cfg.voice.clone(),
        input_audio_format: Some(encoding_str(cfg.input_format)),
        output_audio_format: Some(encoding_str(cfg.output_format)),
        turn_detection,
        tools,
        temperature: cfg.temperature.map(f64::from),
    }
}

/// The OpenAI audio-format string for an [`AudioFormat`].
fn encoding_str(fmt: AudioFormat) -> String {
    match fmt.encoding {
        AudioEncoding::Pcm16 => "pcm16",
        AudioEncoding::G711Ulaw => "g711_ulaw",
        AudioEncoding::G711Alaw => "g711_alaw",
        AudioEncoding::Opus => "opus",
    }
    .to_string()
}

/// Wrap the event receiver in a stream, mapping wire [`ServerEvent`]s onto provider-agnostic
/// [`RealtimeEvent`]s, decoding audio and swallowing the benign barge-in cancel race.
fn map_events(mut rx: mpsc::Receiver<ServerEvent>) -> RealtimeEventStream {
    Box::pin(async_stream::stream! {
        while let Some(ev) = rx.recv().await {
            if let Some(mapped) = map_event(ev) {
                yield Ok::<RealtimeEvent, Error>(mapped);
            }
        }
    })
}

fn map_event(ev: ServerEvent) -> Option<RealtimeEvent> {
    Some(match ev {
        ServerEvent::SessionCreated => RealtimeEvent::SessionReady,
        ServerEvent::AudioDelta(b64) => {
            return STANDARD.decode(&b64).ok().map(RealtimeEvent::AudioDelta)
        }
        ServerEvent::AudioTranscriptDelta(s) => RealtimeEvent::OutputTranscriptDelta(s),
        ServerEvent::InputTranscriptDelta(s) => RealtimeEvent::InputTranscriptDelta(s),
        ServerEvent::InputTranscriptDone(s) => RealtimeEvent::InputTranscriptDone(s),
        ServerEvent::TextDelta(s) => RealtimeEvent::TextDelta(s),
        ServerEvent::FunctionCall {
            call_id,
            name,
            arguments,
        } => RealtimeEvent::ToolCall {
            call_id,
            name,
            arguments,
        },
        ServerEvent::SpeechStarted => RealtimeEvent::SpeechStarted,
        ServerEvent::SpeechStopped => RealtimeEvent::SpeechStopped,
        ServerEvent::ResponseCreated => RealtimeEvent::ResponseStarted,
        ServerEvent::ResponseDone => RealtimeEvent::ResponseDone,
        ServerEvent::Error(v) => {
            if is_benign_cancel_race(&v) {
                return None;
            }
            let err = v.get("error");
            let code = err
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
                .map(String::from);
            let message = err
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("realtime error")
                .to_string();
            RealtimeEvent::Error { code, message }
        }
        // session.updated and any other event carry nothing the driver acts on.
        ServerEvent::SessionUpdated | ServerEvent::Other => return None,
    })
}
