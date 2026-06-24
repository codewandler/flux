//! The normalized streaming protocol emitted by every provider.
//!
//! A provider turns its native stream (SSE, NDJSON, …) into a `Stream<Item = Result<Chunk>>`.
//! Incremental deltas (`TextDelta`, `ThinkingDelta`) are emitted as they arrive for live
//! rendering; a fully assembled `Block` is emitted when a content block completes, so consumers
//! can reconstruct the final assistant `Message` without re-parsing deltas.

use serde::{Deserialize, Serialize};

use crate::content::ContentBlock;

/// Token accounting for a single provider turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl Usage {
    /// Total billable tokens across input, output, and cache.
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    #[serde(other)]
    Unknown,
}

/// One unit of a streamed provider response.
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    /// The turn has started; carries the resolved model id.
    MessageStart { model: String },
    /// An incremental piece of visible text.
    TextDelta(String),
    /// An incremental piece of extended-thinking text.
    ThinkingDelta(String),
    /// A fully assembled content block (emitted when the block completes).
    Block(ContentBlock),
    /// Updated token usage (may be emitted more than once per turn).
    Usage(Usage),
    /// The turn is complete.
    Done { stop_reason: Option<StopReason> },
}
