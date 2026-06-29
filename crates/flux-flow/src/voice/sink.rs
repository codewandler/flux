//! The output half of a voice session.

use flux_runtime::ToolResult;
use serde_json::Value;

/// A consumer of voice-session output — a full-duplex cousin of [`crate::AgentSink`]. A telephony /
/// WebRTC channel implements this (or a thin adapter to its own event type) and pushes caller audio
/// back through the session's [`flux_provider::RealtimeSession`] handle.
pub trait VoiceSink: Send {
    /// A chunk of output audio, in the session's model-native output format.
    fn audio(&mut self, _frame: &[u8]) {}
    /// A delta of the model's spoken-output transcript (or text output).
    fn output_transcript(&mut self, _text: &str) {}
    /// A delta/segment of the caller's input transcript.
    fn input_transcript(&mut self, _text: &str) {}
    /// The caller barged in — flush any queued playout.
    fn barge_in(&mut self) {}
    /// The model is calling a tool (before the envelope runs it).
    fn tool_call(&mut self, _name: &str, _input: &Value) {}
    /// A tool returned (after the envelope).
    fn tool_result(&mut self, _name: &str, _result: &ToolResult) {}
    /// The model finished a response.
    fn response_done(&mut self) {}
    /// A provider/session error.
    fn error(&mut self, _message: &str) {}
}
