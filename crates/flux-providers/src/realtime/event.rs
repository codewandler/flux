//! Server → client events from the OpenAI Realtime API.
//!
//! Ported from a downstream realtime client and trimmed to the events the session driver acts on; the
//! rest fold into [`ServerEvent::Other`] so unknown/added events never break parsing. These are the
//! provider-internal wire events; the driver sees the provider-agnostic `flux_provider::RealtimeEvent`
//! they map to.

use serde::Deserialize;
use serde_json::Value;

use flux_core::Usage;

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
    /// `response.done` — carries the response's token usage (C-38), when the wire reported one
    /// that parsed. `None` for a bare/usage-less `response.done` or a malformed `usage` object
    /// (never a parse error — A-33 discipline).
    ResponseDone(Option<Usage>),
    /// `error` — carries the raw error object for code/message extraction.
    Error(Value),
    /// Any other event type, ignored by the driver.
    Other,
}

impl ServerEvent {
    /// Parse a server event from a JSON text frame.
    ///
    /// `#[allow(clippy::disallowed_methods)]`: this crate's `clippy.toml` bans bare
    /// `serde_json::from_*` because an unhandled parse error can kill a whole chunk stream — but
    /// this one is a legitimate exception (documented there): the WS frame loop
    /// (`client.rs`'s `if let Ok(ev) = ServerEvent::parse(&text)`) already treats a malformed
    /// frame as skip-and-continue, never a fatal stream error. The `response.done` usage parse
    /// added below (C-38) is the same shape one level in: `.ok()` degrades a malformed/absent
    /// `usage` object to `None` rather than failing the whole event (A-33 discipline).
    #[allow(clippy::disallowed_methods)]
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
            "response.done" => {
                let usage = v
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .cloned()
                    .and_then(|u| serde_json::from_value::<WireUsage>(u).ok())
                    .map(normalize_usage);
                ServerEvent::ResponseDone(usage)
            }
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

// --- `response.done`'s `response.usage` wire shape (C-38) -----------------------------------------
//
// Every field `#[serde(default)]` so a missing/renamed key never fails the whole parse — a
// malformed or absent `usage` object just falls through to `.ok()` -> `None` in `ServerEvent::parse`
// above (A-33 discipline: tolerant parsing, never a stream error).

#[derive(Debug, Default, Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_token_details: WireInputDetails,
    #[serde(default)]
    output_token_details: WireOutputDetails,
}

#[derive(Debug, Default, Deserialize)]
struct WireInputDetails {
    /// Wire fidelity only — text tokens are already counted once via `input_tokens`, nothing
    /// separately keys off this split.
    #[serde(default)]
    #[allow(dead_code)]
    text_tokens: u64,
    #[serde(default)]
    audio_tokens: u64,
    /// Wire fidelity only — image input tokens bill at the text input rate (documented
    /// approximation in `flux_core::pricing`), so nothing keys off this split either.
    #[serde(default)]
    #[allow(dead_code)]
    image_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cached_tokens_details: WireCachedDetails,
}

#[derive(Debug, Default, Deserialize)]
struct WireCachedDetails {
    /// Wire fidelity only — see [`WireInputDetails::text_tokens`].
    #[serde(default)]
    #[allow(dead_code)]
    text_tokens: u64,
    #[serde(default)]
    audio_tokens: u64,
}

#[derive(Debug, Default, Deserialize)]
struct WireOutputDetails {
    /// Wire fidelity only — see [`WireInputDetails::text_tokens`].
    #[serde(default)]
    #[allow(dead_code)]
    text_tokens: u64,
    #[serde(default)]
    audio_tokens: u64,
}

/// Normalize the wire's whole-prompt-with-cached-subset shape into flux's fresh/cache-split
/// [`Usage`] — mirrors the HTTP codec's convention (`openai.rs`'s Responses-API usage mapping): the
/// wire reports the whole prompt with cached tokens as a SUBSET of it, but flux's `input_tokens` is
/// the fresh (non-cached) portion. Same normalization for the audio subset: the wire's `audio_tokens`
/// includes cached audio, so the fresh `audio_input_tokens` subtracts it out. Both subtractions
/// saturate — a malformed wire reporting cached > total must not underflow/panic.
fn normalize_usage(u: WireUsage) -> Usage {
    let cached = u.input_token_details.cached_tokens;
    let cached_audio = u.input_token_details.cached_tokens_details.audio_tokens;
    Usage {
        input_tokens: u.input_tokens.saturating_sub(cached),
        output_tokens: u.output_tokens,
        cache_read_input_tokens: cached,
        audio_input_tokens: u
            .input_token_details
            .audio_tokens
            .saturating_sub(cached_audio),
        audio_output_tokens: u.output_token_details.audio_tokens,
        ..Default::default()
    }
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

// Test-only fixture parsing (the `benign_cancel_race_detected` fixtures below) — same documented
// exemption pattern as the crate's other test modules (see `clippy.toml`).
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
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

    /// C-38: a full GA `response.done` with every split (fresh/cached text, fresh/cached audio,
    /// output text/audio) normalizes to the exact [`Usage`] the driver/pricing layer expects —
    /// fresh input/audio subtract their cached counts, `output_tokens` stays the wire total.
    #[test]
    fn response_done_parses_full_ga_usage_fixture() {
        let json = r#"{
            "type": "response.done",
            "response": {
                "id": "resp_1",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 200,
                    "input_token_details": {
                        "text_tokens": 700,
                        "audio_tokens": 300,
                        "cached_tokens": 400,
                        "cached_tokens_details": {
                            "text_tokens": 300,
                            "audio_tokens": 100
                        }
                    },
                    "output_token_details": {
                        "text_tokens": 150,
                        "audio_tokens": 50
                    }
                }
            }
        }"#;
        let usage = match ServerEvent::parse(json).unwrap() {
            ServerEvent::ResponseDone(usage) => usage.expect("a full usage object must parse"),
            other => panic!("expected ResponseDone, got {other:?}"),
        };
        assert_eq!(usage.input_tokens, 600, "1000 total - 400 cached");
        assert_eq!(usage.cache_read_input_tokens, 400);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(
            usage.audio_input_tokens, 200,
            "300 audio - 100 cached audio"
        );
        assert_eq!(usage.audio_output_tokens, 50);
    }

    /// C-38: `response.done` with no `usage` key at all (a bare completion, or a non-GA server)
    /// must parse to `ResponseDone(None)` — never a stream error (A-33 discipline).
    #[test]
    fn response_done_with_no_usage_parses_to_none() {
        let json = r#"{"type":"response.done","response":{"id":"resp_1"}}"#;
        match ServerEvent::parse(json).unwrap() {
            ServerEvent::ResponseDone(usage) => assert!(usage.is_none()),
            other => panic!("expected ResponseDone, got {other:?}"),
        }

        // Also with no `response` object at all.
        let bare = r#"{"type":"response.done"}"#;
        match ServerEvent::parse(bare).unwrap() {
            ServerEvent::ResponseDone(usage) => assert!(usage.is_none()),
            other => panic!("expected ResponseDone, got {other:?}"),
        }
    }

    /// C-38: a malformed `usage` (wrong shape entirely) must not fail the parse — it degrades to
    /// `ResponseDone(None)`, same as a missing one.
    #[test]
    fn response_done_with_garbage_usage_parses_to_none() {
        let json = r#"{"type":"response.done","response":{"usage":"not an object"}}"#;
        match ServerEvent::parse(json).unwrap() {
            ServerEvent::ResponseDone(usage) => assert!(usage.is_none()),
            other => panic!("expected ResponseDone, got {other:?}"),
        }
    }
}
