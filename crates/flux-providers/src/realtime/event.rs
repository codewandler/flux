//! Server → client events from the OpenAI Realtime API.
//!
//! Ported from a downstream realtime client and trimmed to the events the session driver acts on; the
//! rest fold into [`ServerEvent::Other`] so unknown/added events never break parsing. These are the
//! provider-internal wire events; the driver sees the provider-agnostic `flux_provider::RealtimeEvent`
//! they map to.

use serde_json::Value;

/// A parsed server event.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// `session.created`.
    SessionCreated,
    /// `session.updated`.
    SessionUpdated,
    /// `response.output_audio.delta` — base64 PCM16 audio chunk from the model.
    AudioDelta(String),
    /// `response.output_audio_transcript.delta` — transcript of the model's speech.
    AudioTranscriptDelta(String),
    /// `conversation.item.input_audio_transcription.delta` — transcript of the caller's speech.
    InputTranscriptDelta(String),
    /// `conversation.item.input_audio_transcription.completed` — full caller transcript for a turn.
    InputTranscriptDone(String),
    /// `response.output_text.delta` — text output delta.
    TextDelta(String),
    /// A completed tool call (off `response.output_item.done`).
    FunctionCall {
        /// Correlation id for the call's output.
        call_id: String,
        /// Function name.
        name: String,
        /// JSON-encoded arguments string.
        arguments: String,
    },
    /// `input_audio_buffer.speech_started` — the user began speaking (barge-in signal).
    SpeechStarted,
    /// `input_audio_buffer.speech_stopped`.
    SpeechStopped,
    /// `response.created`.
    ResponseCreated,
    /// `response.done`.
    ResponseDone,
    /// `error` — carries the raw error object for code/message extraction.
    Error(Value),
    /// Any other event type, ignored by the driver.
    Other,
}

impl ServerEvent {
    /// Parse a server event from a JSON text frame.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let v: Value = serde_json::from_str(text)?;
        let t = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let ev = match t.as_str() {
            // Accept both the beta (`response.audio.*`) and GA (`response.output_audio.*`) names.
            "response.audio.delta" | "response.output_audio.delta" => {
                ServerEvent::AudioDelta(str_field(&v, "delta"))
            }
            "response.audio_transcript.delta" | "response.output_audio_transcript.delta" => {
                ServerEvent::AudioTranscriptDelta(str_field(&v, "delta"))
            }
            "conversation.item.input_audio_transcription.delta" => {
                ServerEvent::InputTranscriptDelta(str_field(&v, "delta"))
            }
            "conversation.item.input_audio_transcription.completed" => {
                ServerEvent::InputTranscriptDone(str_field(&v, "transcript"))
            }
            "response.text.delta" | "response.output_text.delta" => {
                ServerEvent::TextDelta(str_field(&v, "delta"))
            }
            // The function name lives on the completed `function_call` ITEM — NOT on
            // `response.function_call_arguments.done` (which carries only call_id + arguments).
            "response.output_item.done" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    ServerEvent::FunctionCall {
                        call_id: str_field(&item, "call_id"),
                        name: str_field(&item, "name"),
                        arguments: str_field(&item, "arguments"),
                    }
                } else {
                    ServerEvent::Other
                }
            }
            "input_audio_buffer.speech_started" => ServerEvent::SpeechStarted,
            "input_audio_buffer.speech_stopped" => ServerEvent::SpeechStopped,
            "session.created" => ServerEvent::SessionCreated,
            "session.updated" => ServerEvent::SessionUpdated,
            "response.created" => ServerEvent::ResponseCreated,
            "response.done" => ServerEvent::ResponseDone,
            "error" => ServerEvent::Error(v),
            _ => ServerEvent::Other,
        };
        Ok(ev)
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether an `error` event is the benign barge-in cancel race. A `response.cancel` can lose the race
/// with the model finishing its turn, and GA server-VAD already auto-interrupts on detected speech —
/// in both cases GA replies `response_cancel_not_active`. The response is stopped either way, so this
/// is the expected outcome of a barge-in cancel, not a failure.
pub fn is_benign_cancel_race(event: &Value) -> bool {
    event
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        == Some("response_cancel_not_active")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    #[test]
    fn parses_audio_delta() {
        let pcm = [1u8, 2, 3, 4];
        let b64 = STANDARD.encode(pcm);
        let json = format!(r#"{{"type":"response.audio.delta","delta":"{b64}"}}"#);
        match ServerEvent::parse(&json).unwrap() {
            ServerEvent::AudioDelta(d) => assert_eq!(STANDARD.decode(d).unwrap(), pcm),
            other => panic!("expected AudioDelta, got {other:?}"),
        }
    }

    #[test]
    fn ga_output_audio_delta_aliases_audio_delta() {
        let json = r#"{"type":"response.output_audio.delta","delta":"AAAA"}"#;
        assert!(matches!(
            ServerEvent::parse(json).unwrap(),
            ServerEvent::AudioDelta(_)
        ));
    }

    #[test]
    fn parses_function_call() {
        let json = r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":"kb_search","arguments":"{\"q\":\"hi\"}"}}"#;
        match ServerEvent::parse(json).unwrap() {
            ServerEvent::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name, "kb_search");
                assert_eq!(arguments, r#"{"q":"hi"}"#);
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn parses_input_transcription() {
        let ev = ServerEvent::parse(
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"book a table"}"#,
        )
        .unwrap();
        match ev {
            ServerEvent::InputTranscriptDone(t) => assert_eq!(t, "book a table"),
            other => panic!("expected InputTranscriptDone, got {other:?}"),
        }
    }

    #[test]
    fn speech_started_is_barge_in() {
        let ev = ServerEvent::parse(r#"{"type":"input_audio_buffer.speech_started"}"#).unwrap();
        assert!(matches!(ev, ServerEvent::SpeechStarted));
    }

    #[test]
    fn benign_cancel_race_detected() {
        let v: Value = serde_json::from_str(
            r#"{"type":"error","error":{"code":"response_cancel_not_active"}}"#,
        )
        .unwrap();
        assert!(is_benign_cancel_race(&v));
        let other: Value =
            serde_json::from_str(r#"{"type":"error","error":{"code":"invalid_request"}}"#).unwrap();
        assert!(!is_benign_cancel_race(&other));
    }

    #[test]
    fn unknown_event_folds_into_other() {
        let ev = ServerEvent::parse(r#"{"type":"rate_limits.updated","rate_limits":[]}"#).unwrap();
        assert!(matches!(ev, ServerEvent::Other));
    }
}
