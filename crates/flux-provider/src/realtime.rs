//! The full-duplex, session-oriented model seam — sibling of [`Provider`](crate::Provider).
//!
//! [`Provider`](crate::Provider) is **half-duplex**: build one [`Request`](crate::Request), consume
//! one [`ChunkStream`](crate::ChunkStream), done. A voice-to-voice (speech-to-speech) model is
//! **full-duplex**: open a long-lived session, then *concurrently* push input audio and pull output
//! audio / transcripts / tool-calls until hangup, with the **model** driving acoustic turn-taking
//! (server-VAD), barge-in, and response cancellation. That doesn't fit `stream(Request) ->
//! ChunkStream`, so it gets its own trait that coexists with `Provider`.
//!
//! Tool calls the model emits arrive as plain data ([`RealtimeEvent::ToolCall`]); a higher layer (the
//! voice session driver) routes them through the runtime's `Executor` safety envelope. This keeps the
//! seam free of any runtime (L2) type.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use flux_core::{AudioFormat, Result};

use crate::ToolDef;

/// A full-duplex, session-oriented model. The sibling of [`Provider`](crate::Provider).
#[async_trait]
pub trait RealtimeProvider: Send + Sync {
    /// A short provider name (e.g. `"openai-realtime"`).
    fn name(&self) -> &str;
    /// Open a session — returns the control handle plus the server-event stream.
    async fn connect(&self, config: RealtimeConfig) -> Result<RealtimeConnection>;
}

/// A boxed, sendable stream of [`RealtimeEvent`]s from the model.
pub type RealtimeEventStream = Pin<Box<dyn Stream<Item = Result<RealtimeEvent>> + Send>>;

/// An open realtime session: the input/control half plus the output event stream.
pub struct RealtimeConnection {
    /// Send audio / text / control; held behind `Arc` so it clones into off-loop tasks.
    pub session: Arc<dyn RealtimeSession>,
    /// Server events (audio, transcripts, tool calls, …).
    pub events: RealtimeEventStream,
}

/// The input/control half of a realtime session.
#[async_trait]
pub trait RealtimeSession: Send + Sync {
    /// Append a chunk of input audio, in the session's [`RealtimeConfig::input_format`].
    async fn send_audio(&self, frame: &[u8]) -> Result<()>;
    /// Commit the input buffer (a no-op under server-VAD).
    async fn commit_audio(&self) -> Result<()>;
    /// Inject a user text turn and request a response (DTMF-as-text, tests, flow-driven turns).
    async fn send_text(&self, text: &str) -> Result<()>;
    /// Ask the model to produce a response.
    async fn create_response(&self) -> Result<()>;
    /// Cancel the in-progress response (barge-in). **Idempotent**: cancelling with no active
    /// response is `Ok` (the implementation swallows the provider's benign no-active-response race).
    async fn cancel_response(&self) -> Result<()>;
    /// Return a tool call's output to the model, correlated by `call_id`. The caller issues
    /// [`create_response`](Self::create_response) once per turn afterwards (so several tool outputs in
    /// one turn don't each fire a conflicting response).
    async fn send_tool_result(&self, call_id: &str, output: &str) -> Result<()>;
    /// Close the session.
    fn close(&self);
}

/// A server → client event from a realtime model.
///
/// Carries **decoded bytes** and **plain strings** only — never a runtime type — so this L1 enum
/// stays layer-clean (base64 and wire shapes are the implementation's concern).
#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// The session is configured and ready.
    SessionReady,
    /// A chunk of output audio, decoded to raw bytes in the session's `output_format`.
    AudioDelta(Vec<u8>),
    /// A delta of the model's spoken-output transcript.
    OutputTranscriptDelta(String),
    /// A delta of the caller's input transcription (when the model transcribes input audio).
    InputTranscriptDelta(String),
    /// The caller's input transcription for a turn is complete — carries the full transcript.
    /// A flow-driven (engine-owned-turns) driver consumes this as one user turn.
    InputTranscriptDone(String),
    /// A text-output delta.
    TextDelta(String),
    /// The caller began speaking — a barge-in signal.
    SpeechStarted,
    /// The caller stopped speaking.
    SpeechStopped,
    /// The model began a response.
    ResponseStarted,
    /// The model wants to call a tool. Route through the safety envelope, then
    /// [`RealtimeSession::send_tool_result`].
    ToolCall {
        /// Correlation id for the result.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded arguments object.
        arguments: String,
    },
    /// The model finished a response.
    ResponseDone,
    /// A provider error.
    Error {
        /// Provider error code, if any.
        code: Option<String>,
        /// Human-readable message.
        message: String,
    },
}

/// Configuration for opening a realtime session.
#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    /// Model id (e.g. `"gpt-realtime"`).
    pub model: String,
    /// System instructions / persona.
    pub system: Option<String>,
    /// Tools the model may call — built once from the live tool registry by the driver.
    pub tools: Vec<ToolDef>,
    /// Voice name (e.g. `"alloy"`).
    pub voice: Option<String>,
    /// Format of audio sent to the model.
    pub input_format: AudioFormat,
    /// Format of audio received from the model.
    pub output_format: AudioFormat,
    /// Turn-detection mode.
    pub turn_detection: TurnDetection,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Provider-specific extras.
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl RealtimeConfig {
    /// A voice-agent config: model-native PCM16 in/out, server-VAD, the given instructions.
    pub fn voice_agent(model: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: Some(instructions.into()),
            tools: Vec::new(),
            voice: None,
            input_format: AudioFormat::OPENAI_PCM16,
            output_format: AudioFormat::OPENAI_PCM16,
            turn_detection: TurnDetection::ServerVad {
                threshold: None,
                prefix_padding_ms: None,
                silence_duration_ms: None,
            },
            temperature: None,
            metadata: serde_json::Map::new(),
        }
    }

    /// Replace the tool set (the driver fills this from the registry).
    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }
}

/// How turn boundaries are detected.
#[derive(Debug, Clone)]
pub enum TurnDetection {
    /// Server-side voice-activity detection (the model decides turn boundaries).
    ServerVad {
        /// Activation threshold (0..1); `None` lets the provider choose.
        threshold: Option<f32>,
        /// Audio to include before detected speech, in ms.
        prefix_padding_ms: Option<u32>,
        /// Silence before a turn is considered ended, in ms.
        silence_duration_ms: Option<u32>,
    },
    /// Semantic (model-judged) turn detection.
    SemanticVad {
        /// Provider-specific eagerness hint.
        eagerness: Option<String>,
    },
    /// No detection — the client delimits turns via [`RealtimeSession::commit_audio`] +
    /// [`RealtimeSession::create_response`].
    None,
}
