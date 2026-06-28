//! Anthropic Messages streaming wire types and their mapping to the normalized `flux_core` model.
//!
//! OpenRouter and ollama both proxy this exact SSE shape, so these types are shared across every
//! provider that speaks the Messages protocol.

use serde::Deserialize;

use flux_core::{StopReason, Usage};

/// A top-level server-sent event from the Anthropic Messages stream.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: MessageStartBody,
    },
    ContentBlockStart {
        index: usize,
        content_block: WireBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: WireDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: WireError,
    },
}

#[derive(Debug, Deserialize)]
pub struct MessageStartBody {
    pub model: String,
    #[serde(default)]
    pub usage: WireUsage,
}

/// The `content_block` field of a `content_block_start` event.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        // Anthropic sends an (empty) starting object here; the real input arrives via
        // input_json_delta events, so we accumulate those instead of reading this.
        #[serde(default)]
        #[allow(dead_code)]
        input: serde_json::Value,
    },
}

/// The `delta` field of a `content_block_delta` event.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // names mirror the Anthropic wire tags (text_delta, …)
pub enum WireDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Deserialize)]
pub struct MessageDeltaBody {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct WireUsage {
    #[serde(default, deserialize_with = "u64_or_zero")]
    pub input_tokens: u64,
    #[serde(default, deserialize_with = "u64_or_zero")]
    pub output_tokens: u64,
    #[serde(default, deserialize_with = "u64_or_zero")]
    pub cache_creation_input_tokens: u64,
    #[serde(default, deserialize_with = "u64_or_zero")]
    pub cache_read_input_tokens: u64,
}

/// Some Messages-compatible gateways (e.g. OpenRouter) send usage counters as an explicit `null`
/// instead of omitting them. `#[serde(default)]` only covers *absent* fields, so accept null too,
/// mapping it to 0. Harmless for Anthropic-direct, which always sends real numbers.
fn u64_or_zero<'de, D>(d: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(d)?.unwrap_or(0))
}

#[derive(Debug, Deserialize)]
pub struct WireError {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
}

impl From<WireUsage> for Usage {
    fn from(u: WireUsage) -> Self {
        Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

/// Map an Anthropic stop-reason string to the normalized enum.
pub fn map_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}
