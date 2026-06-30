//! Configuration for an OpenAI Realtime session (provider-internal).
//!
//! Ported from a downstream realtime client. These are the OpenAI-specific wire types; the
//! provider maps flux's `RealtimeConfig` onto them and renders the GA `session` shape.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `session.update` configuration. Unset fields are omitted so the API keeps its defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Output modalities, e.g. `["audio", "text"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,
    /// System instructions / persona.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Voice name (e.g. `"alloy"`, `"verse"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Input audio format: `"pcm16"`, `"g711_ulaw"`, or `"g711_alaw"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_audio_format: Option<String>,
    /// Output audio format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_audio_format: Option<String>,
    /// Server-side turn detection (VAD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_detection: Option<TurnDetection>,
    /// Tools exposed to the model (function-calling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

impl SessionConfig {
    /// Render this config as a **GA** Realtime `session` object — the nested shape the GA
    /// `/v1/realtime` API expects. GA moved the audio formats, voice, and turn detection under
    /// `audio.input` / `audio.output`, renamed `modalities` → `output_modalities`, and tags the
    /// session `type: "realtime"`. (The beta flat shape is rejected with `beta_api_shape_disabled`.)
    pub fn to_ga_session(&self) -> Value {
        let mut session = serde_json::Map::new();
        session.insert("type".into(), Value::String("realtime".into()));

        if let Some(instructions) = &self.instructions {
            session.insert("instructions".into(), Value::String(instructions.clone()));
        }
        if let Some(modalities) = &self.modalities {
            // GA carries a single output modality; audio output already includes its transcript.
            let out = if modalities.iter().any(|m| m == "audio") {
                "audio"
            } else {
                "text"
            };
            session.insert("output_modalities".into(), json!([out]));
        }

        let mut input = serde_json::Map::new();
        if let Some(fmt) = &self.input_audio_format {
            input.insert("format".into(), audio_format(fmt));
        }
        if let Some(td) = &self.turn_detection {
            input.insert(
                "turn_detection".into(),
                serde_json::to_value(td).unwrap_or(Value::Null),
            );
        }
        let mut output = serde_json::Map::new();
        if let Some(fmt) = &self.output_audio_format {
            output.insert("format".into(), audio_format(fmt));
        }
        if let Some(voice) = &self.voice {
            output.insert("voice".into(), Value::String(voice.clone()));
        }
        let mut audio = serde_json::Map::new();
        if !input.is_empty() {
            audio.insert("input".into(), Value::Object(input));
        }
        if !output.is_empty() {
            audio.insert("output".into(), Value::Object(output));
        }
        if !audio.is_empty() {
            session.insert("audio".into(), Value::Object(audio));
        }

        if let Some(tools) = &self.tools {
            session.insert(
                "tools".into(),
                serde_json::to_value(tools).unwrap_or(Value::Null),
            );
        }
        if let Some(temperature) = &self.temperature {
            session.insert("temperature".into(), json!(temperature));
        }
        Value::Object(session)
    }
}

/// The GA typed audio-format object for a format string. PCM is tagged 24 kHz (the model side runs at
/// 24 kHz; callers resample to/from it). G.711 telephony formats carry no rate.
fn audio_format(fmt: &str) -> Value {
    match fmt {
        "pcm16" => json!({ "type": "audio/pcm", "rate": 24_000 }),
        "g711_ulaw" => json!({ "type": "audio/pcmu" }),
        "g711_alaw" => json!({ "type": "audio/pcma" }),
        other => json!({ "type": other }),
    }
}

/// Server-side voice-activity turn detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnDetection {
    /// Detection type, e.g. `"server_vad"` / `"semantic_vad"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Activation threshold (0..1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Audio to include before detected speech, in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<u32>,
    /// Silence before a turn is considered ended, in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<u32>,
    /// Eagerness hint for `semantic_vad` (e.g. `"low"`/`"medium"`/`"high"`/`"auto"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eagerness: Option<String>,
}

/// A function tool exposed to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Tool type — `"function"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Function name.
    pub name: String,
    /// Function description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON-Schema parameters.
    pub parameters: Value,
}

impl Tool {
    /// Build a function tool.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            name: name.into(),
            description: Some(description.into()),
            parameters,
        }
    }
}

/// How to connect: credentials, model, and the initial session config.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// OpenAI API key.
    pub api_key: String,
    /// Realtime model id.
    pub model: String,
    /// Session config sent as `session.update` on connect.
    pub session: Option<SessionConfig>,
}

/// The default realtime model id (`OPENAI_REALTIME_MODEL` or `gpt-realtime`).
pub fn default_model() -> String {
    std::env::var("OPENAI_REALTIME_MODEL").unwrap_or_else(|_| "gpt-realtime".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_ga_session_shape() {
        let cfg = SessionConfig {
            modalities: Some(vec!["audio".into(), "text".into()]),
            instructions: Some("be brief".into()),
            voice: Some("alloy".into()),
            input_audio_format: Some("pcm16".into()),
            output_audio_format: Some("pcm16".into()),
            turn_detection: Some(TurnDetection {
                kind: "server_vad".into(),
                threshold: None,
                prefix_padding_ms: None,
                silence_duration_ms: None,
                eagerness: None,
            }),
            ..Default::default()
        };
        let v = cfg.to_ga_session();
        assert_eq!(v["type"], "realtime");
        assert_eq!(v["instructions"], "be brief");
        assert_eq!(v["output_modalities"], json!(["audio"]));
        assert_eq!(v["audio"]["input"]["format"]["type"], "audio/pcm");
        assert_eq!(v["audio"]["input"]["format"]["rate"], 24_000);
        assert_eq!(v["audio"]["input"]["turn_detection"]["type"], "server_vad");
        assert_eq!(v["audio"]["output"]["voice"], "alloy");
        // None of the rejected beta-shaped flat fields leak through.
        assert!(v.get("modalities").is_none());
        assert!(v.get("input_audio_format").is_none());
        assert!(v.get("voice").is_none());
    }

    #[test]
    fn g711_format_carries_no_rate() {
        let v = audio_format("g711_ulaw");
        assert_eq!(v["type"], "audio/pcmu");
        assert!(v.get("rate").is_none());
    }

    #[test]
    fn function_tool_shape() {
        let t = Tool::function("kb_search", "search the KB", json!({"type":"object"}));
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["type"], "function");
        assert_eq!(v["name"], "kb_search");
        assert_eq!(v["parameters"]["type"], "object");
    }
}
