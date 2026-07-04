//! The normalized streaming protocol emitted by every provider.
//!
//! A provider turns its native stream (SSE, NDJSON, …) into a `Stream<Item = Result<Chunk>>`.
//! Incremental deltas (`TextDelta`, `ThinkingDelta`) are emitted as they arrive for live
//! rendering; a fully assembled `Block` is emitted when a content block completes, so consumers
//! can reconstruct the final assistant `Message` without re-parsing deltas.

use serde::{Deserialize, Serialize};

use crate::content::ContentBlock;

/// Token accounting for a single provider turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Reasoning / "thinking" tokens generated this call. These are a **subset of
    /// `output_tokens`** (the provider already counts them there), so they are deliberately **not**
    /// added in [`Self::total`] or [`Self::context_tokens`] — doing so would double-count. They are
    /// tracked separately only so cost models can apply a distinct reasoning rate where a provider
    /// prices reasoning apart from ordinary output. `#[serde(default)]` keeps old event logs
    /// (written before this field existed) decodable.
    #[serde(default)]
    pub reasoning_tokens: u64,
    /// The provider's own dollar figure for this call (C-34), when it reports one (OpenRouter's
    /// `cost` field on both wires; `None` for providers that don't report — e.g. direct
    /// Anthropic/OpenAI/Bedrock). When present, [`PricingTable::cost`](crate::PricingTable::cost)
    /// prefers it over the static table — the provider's own number is strictly more truthful
    /// (routing/discount-aware) and needs zero table maintenance. `#[serde(default,
    /// skip_serializing_if = "Option::is_none")]` keeps old event-log rows (written before this
    /// field existed) decodable AND keeps the wire byte-identical for every non-reporting provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cost_usd: Option<f64>,
}

impl Usage {
    /// Total billable tokens across input, output, and cache. `reasoning_tokens` is **not** added —
    /// it is a subset of `output_tokens`, which is already counted.
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    /// The prompt size of a single call — the context-window occupancy (fresh input + both cache
    /// tiers). Distinct from [`Self::total`], which also counts generated output. `reasoning_tokens`
    /// is output, not prompt, so it does not appear here.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    /// Fold one model call's usage into a turn-level accumulator. Output tokens are **summed** (each
    /// call generates new tokens), and `reasoning_tokens` is summed alongside them (it is part of the
    /// generated output). The input/cache counts are **replaced** by this call's — in the agent loop
    /// every successive call re-sends the growing conversation, so the latest prompt size *is* the
    /// context-window occupancy; summing the input side would multiply-count the re-sent (and largely
    /// cache-read) prefix. The replace is skipped for a call that reported no prompt at all, so a
    /// usage-less follow-up can't zero an already-recorded context figure.
    pub fn accumulate(&mut self, call: &Usage) {
        self.output_tokens += call.output_tokens;
        self.reasoning_tokens += call.reasoning_tokens;
        if call.context_tokens() > 0 {
            self.input_tokens = call.input_tokens;
            self.cache_read_input_tokens = call.cache_read_input_tokens;
            self.cache_creation_input_tokens = call.cache_creation_input_tokens;
        }
        // Reported cost is spend, not a snapshot — like output tokens, every call's slice adds to
        // the turn total. A call that doesn't report cost (`None`) leaves the accumulator
        // untouched, so a non-reporting follow-up can't erase cost already recorded.
        if let Some(c) = call.reported_cost_usd {
            *self.reported_cost_usd.get_or_insert(0.0) += c;
        }
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
    /// Emitted when a codec tolerated and skipped unparseable provider bytes mid-stream.
    /// Not a failure — informational, for planner-side accounting and diagnostics.
    StreamDiagnostic { dropped_frames: u32, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_accumulate_folds_reasoning() {
        let mut acc = Usage::default();

        // First call: 100 input + 40 cache-read prompt, 30 output of which 12 are reasoning.
        acc.accumulate(&Usage {
            input_tokens: 100,
            output_tokens: 30,
            cache_read_input_tokens: 40,
            reasoning_tokens: 12,
            ..Default::default()
        });
        // Second call: re-sends a larger prompt, generates 20 more output (8 reasoning).
        acc.accumulate(&Usage {
            input_tokens: 150,
            output_tokens: 20,
            cache_read_input_tokens: 60,
            reasoning_tokens: 8,
            ..Default::default()
        });

        // Output and reasoning are summed across calls.
        assert_eq!(acc.output_tokens, 50);
        assert_eq!(acc.reasoning_tokens, 20);
        // Prompt-side counts are replaced by the latest call (context-window occupancy).
        assert_eq!(acc.input_tokens, 150);
        assert_eq!(acc.cache_read_input_tokens, 60);

        // Reasoning is a subset of output, so it is excluded from total() / context_tokens().
        // total = input(150) + output(50) + cache_creation(0) + cache_read(60); reasoning excluded.
        assert_eq!(acc.total(), 150 + 50 + 60);
        assert_eq!(acc.context_tokens(), 150 + 60);

        // A usage-less follow-up still folds output+reasoning but doesn't zero the prompt counts.
        acc.accumulate(&Usage {
            output_tokens: 5,
            reasoning_tokens: 3,
            ..Default::default()
        });
        assert_eq!(acc.output_tokens, 55);
        assert_eq!(acc.reasoning_tokens, 23);
        assert_eq!(acc.input_tokens, 150);
    }

    /// C-34: `reported_cost_usd` sums across calls like `output_tokens`/`reasoning_tokens` — a
    /// turn's total spend is the sum of what every call in it actually cost, not a replace. A
    /// `None` call (a provider that doesn't report cost, or a usage-less follow-up) must not erase
    /// cost already accumulated from an earlier reporting call.
    #[test]
    fn usage_accumulate_sums_reported_cost() {
        let mut acc = Usage::default();
        assert_eq!(acc.reported_cost_usd, None, "nothing accumulated yet");

        // First call reports cost.
        acc.accumulate(&Usage {
            output_tokens: 10,
            reported_cost_usd: Some(0.002),
            ..Default::default()
        });
        assert_eq!(acc.reported_cost_usd, Some(0.002));

        // A second reporting call SUMS, not replaces.
        acc.accumulate(&Usage {
            output_tokens: 5,
            reported_cost_usd: Some(0.0005),
            ..Default::default()
        });
        assert!(
            (acc.reported_cost_usd.unwrap() - 0.0025).abs() < 1e-12,
            "got {:?}",
            acc.reported_cost_usd
        );

        // A follow-up call that reports NO cost (e.g. a usage-less/non-reporting call) must not
        // erase the cost already recorded.
        acc.accumulate(&Usage {
            output_tokens: 1,
            ..Default::default()
        });
        assert!(
            (acc.reported_cost_usd.unwrap() - 0.0025).abs() < 1e-12,
            "a None call must not erase prior recorded cost: {:?}",
            acc.reported_cost_usd
        );

        // A fresh accumulator that never sees a reporting call stays None throughout (a
        // non-reporting provider's turn must not spuriously show Some(0.0)).
        let mut never_reported = Usage::default();
        never_reported.accumulate(&Usage {
            output_tokens: 20,
            ..Default::default()
        });
        assert_eq!(never_reported.reported_cost_usd, None);
    }
}
