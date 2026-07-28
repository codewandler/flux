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
    /// Audio-input tokens for this call (realtime voice-to-voice models, C-38) — a **subset of
    /// `input_tokens`** (the fresh, non-cached portion; cached audio folds into
    /// `cache_read_input_tokens` — see `flux_core::pricing`). Deliberately **not** added in
    /// [`Self::total`] / [`Self::context_tokens`], mirroring `reasoning_tokens`. `#[serde(default)]`
    /// keeps old event logs (written before this field existed) decodable.
    #[serde(default)]
    pub audio_input_tokens: u64,
    /// Audio-output tokens generated this call (C-38) — a **subset of `output_tokens`**. Mirrors
    /// `audio_input_tokens`.
    #[serde(default)]
    pub audio_output_tokens: u64,
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
        self.audio_output_tokens += call.audio_output_tokens;
        if call.context_tokens() > 0 {
            self.input_tokens = call.input_tokens;
            self.cache_read_input_tokens = call.cache_read_input_tokens;
            self.cache_creation_input_tokens = call.cache_creation_input_tokens;
            self.audio_input_tokens = call.audio_input_tokens;
        }
        // Reported cost is spend, not a snapshot — like output tokens, every call's slice adds to
        // the turn total. A call that doesn't report cost (`None`) leaves the accumulator
        // untouched, so a non-reporting follow-up can't erase cost already recorded.
        if let Some(c) = call.reported_cost_usd {
            *self.reported_cost_usd.get_or_insert(0.0) += c;
        }
    }

    /// Add an independent model call field-by-field into this total.
    ///
    /// Unlike [`Self::accumulate`], this sums the input/cache side as well as generated output:
    /// cognition operations, sub-agents, and separate journey calls each own a distinct prompt, so
    /// none is a re-sent snapshot that should replace an earlier call's context occupancy.
    pub fn sum_independent(&mut self, call: &Usage) {
        self.input_tokens += call.input_tokens;
        self.output_tokens += call.output_tokens;
        self.cache_creation_input_tokens += call.cache_creation_input_tokens;
        self.cache_read_input_tokens += call.cache_read_input_tokens;
        self.reasoning_tokens += call.reasoning_tokens;
        self.audio_input_tokens += call.audio_input_tokens;
        self.audio_output_tokens += call.audio_output_tokens;
        if let Some(cost) = call.reported_cost_usd {
            *self.reported_cost_usd.get_or_insert(0.0) += cost;
        }
    }
}

/// Cumulative prompt-cache accounting across the model calls of a turn (or a session).
///
/// [`Usage::accumulate`] deliberately **replaces** the input/cache side on every call, because the
/// figure it maintains is *context-window occupancy* — in an agent loop each round re-sends the
/// growing conversation, so the latest prompt size is the occupancy and summing would multiply-count
/// the re-sent prefix. That is the right answer for "how full is the window" and the wrong one for
/// "how much of what we sent was served from cache": under it, a twelve-round turn reports round
/// twelve only — the round with the longest message tail and therefore the *worst* ratio of the turn.
///
/// This is the other accumulator. It sums each call's prompt tiers, so [`Self::hit_rate`] is the
/// token-weighted share across every call, matching what `flux usage` computes offline from the
/// per-call `CallUsage` event log. The two coexist on purpose: `ctx` stays occupancy, cache stays
/// efficiency (C-133).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEfficiency {
    /// Prompt tokens served from cache, summed across calls.
    #[serde(default)]
    pub read: u64,
    /// Prompt tokens written into the cache, summed across calls.
    #[serde(default)]
    pub write: u64,
    /// Prompt tokens billed at full input rate — neither read from nor written to cache.
    #[serde(default)]
    pub fresh: u64,
}

impl CacheEfficiency {
    /// Fold one model call's prompt tiers in. Output/reasoning/audio are not prompt and are ignored.
    pub fn add(&mut self, call: &Usage) {
        self.read += call.cache_read_input_tokens;
        self.write += call.cache_creation_input_tokens;
        self.fresh += call.input_tokens;
    }

    /// Every prompt token accounted for, across all folded calls. Equals the sum of each call's
    /// [`Usage::context_tokens`].
    pub fn prompt_tokens(&self) -> u64 {
        self.read + self.write + self.fresh
    }

    /// The share of prompt tokens served from cache, in `0.0..=1.0`. Zero when nothing was folded.
    pub fn hit_rate(&self) -> f64 {
        match self.prompt_tokens() {
            0 => 0.0,
            total => self.read as f64 / total as f64,
        }
    }

    /// True when no call has been folded in yet — the empty state a surface renders as "no usage".
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens() == 0
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
///
/// Adjacently tagged (`{"t": "...", "c": ...}`) rather than externally tagged, so a unit-like
/// variant such as [`Chunk::Usage`] wrapping a single field still round-trips unambiguously and a
/// struct variant's fields sit under one `c` key instead of being spliced into the tag object. Added
/// (D-174) so a recorded model call's chunk stream can be persisted verbatim in a test-kit cassette
/// (`model.jsonl`) and replayed byte-for-byte — `serde_json::to_value`/`from_value` round-trips every
/// variant (see the `chunk_serde_round_trips_every_variant` test below).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "c", rename_all = "snake_case")]
pub enum Chunk {
    /// The turn has started; carries the resolved model id.
    MessageStart { model: String },
    /// An incremental piece of visible text.
    TextDelta(String),
    /// An incremental piece of extended-thinking text.
    ThinkingDelta(String),
    /// A fully assembled content block (emitted when the block completes).
    Block(ContentBlock),
    /// An incremental fragment of a `tool_use` block's `input` JSON, as it streams in — `name` is
    /// the tool being called (known from the block's start, before its arguments are complete)
    /// and `partial_json` is the next raw text fragment to append to that call's accumulating
    /// argument buffer. Purely additive/advisory (L-23): [`Chunk::Block`] remains the sole source
    /// of truth for the finished call, emitted once as usual when the block completes — a
    /// consumer that only cares about the final result can ignore every delta. Useful for tracing
    /// latency and showing progress while a tool with a large argument object is streaming. Not
    /// every codec emits this; its absence does not affect the completed tool call.
    ToolInputDelta { name: String, partial_json: String },
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

    #[test]
    fn usage_sum_independent_adds_every_counter_and_reported_cost() {
        let mut total = Usage {
            input_tokens: 10,
            output_tokens: 4,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 2,
            reasoning_tokens: 1,
            audio_input_tokens: 2,
            audio_output_tokens: 1,
            reported_cost_usd: Some(0.25),
        };
        total.sum_independent(&Usage {
            input_tokens: 20,
            output_tokens: 8,
            cache_creation_input_tokens: 6,
            cache_read_input_tokens: 4,
            reasoning_tokens: 2,
            audio_input_tokens: 4,
            audio_output_tokens: 2,
            reported_cost_usd: Some(0.5),
        });

        assert_eq!(total.input_tokens, 30);
        assert_eq!(total.output_tokens, 12);
        assert_eq!(total.cache_creation_input_tokens, 9);
        assert_eq!(total.cache_read_input_tokens, 6);
        assert_eq!(total.reasoning_tokens, 3);
        assert_eq!(total.audio_input_tokens, 6);
        assert_eq!(total.audio_output_tokens, 3);
        assert_eq!(total.reported_cost_usd, Some(0.75));
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

    /// C-38: `audio_output_tokens` sums across calls like `reasoning_tokens`; `audio_input_tokens`
    /// replaces alongside the other prompt-side counts (inside the same `context_tokens() > 0`
    /// gate), so a usage-less follow-up can't zero an already-recorded audio-input count. Both are
    /// subsets of `input_tokens`/`output_tokens`, so they never move `total()`/`context_tokens()`.
    #[test]
    fn usage_accumulate_folds_audio() {
        let mut acc = Usage::default();

        // First call: 100 input tokens (40 of them audio), 30 output tokens (12 audio).
        acc.accumulate(&Usage {
            input_tokens: 100,
            output_tokens: 30,
            audio_input_tokens: 40,
            audio_output_tokens: 12,
            ..Default::default()
        });
        // Second call re-sends a larger (still partly audio) prompt, generates more audio output.
        acc.accumulate(&Usage {
            input_tokens: 150,
            output_tokens: 20,
            audio_input_tokens: 60,
            audio_output_tokens: 8,
            ..Default::default()
        });

        // Output-side audio sums across calls.
        assert_eq!(acc.audio_output_tokens, 20);
        // Prompt-side audio is replaced by the latest call, like input_tokens itself.
        assert_eq!(acc.audio_input_tokens, 60);
        assert_eq!(acc.input_tokens, 150);

        // Audio is a subset of input/output, so it never perturbs total()/context_tokens().
        assert_eq!(acc.total(), 150 + 50);
        assert_eq!(acc.context_tokens(), 150);

        // A usage-less follow-up sums the output-side audio but must not zero the replaced
        // prompt-side audio count.
        acc.accumulate(&Usage {
            output_tokens: 5,
            audio_output_tokens: 3,
            ..Default::default()
        });
        assert_eq!(acc.audio_output_tokens, 23);
        assert_eq!(
            acc.audio_input_tokens, 60,
            "usage-less call must not zero it"
        );
    }

    /// D-174: every [`Chunk`] variant round-trips through `serde_json` byte-for-byte — the test
    /// kit's model cassette (`model.jsonl`) persists a recorded call's raw chunk stream and must
    /// replay it exactly, so a variant this misses would silently corrupt a fixture.
    #[test]
    fn chunk_serde_round_trips_every_variant() {
        let samples = vec![
            Chunk::MessageStart {
                model: "claude-sonnet".into(),
            },
            Chunk::TextDelta("hello".into()),
            Chunk::ThinkingDelta("pondering".into()),
            Chunk::Block(ContentBlock::Text {
                text: "hello".into(),
            }),
            Chunk::Block(ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "read".into(),
                input: serde_json::json!({"path": "a.txt"}),
            }),
            Chunk::ToolInputDelta {
                name: "read".into(),
                partial_json: "{\"path\":".into(),
            },
            Chunk::Usage(Usage {
                input_tokens: 12,
                output_tokens: 3,
                ..Default::default()
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
            Chunk::Done { stop_reason: None },
            Chunk::StreamDiagnostic {
                dropped_frames: 2,
                detail: "unparseable SSE frame".into(),
            },
        ];
        for chunk in samples {
            let json = serde_json::to_string(&chunk).expect("chunk serializes");
            let back: Chunk = serde_json::from_str(&json).expect("chunk deserializes");
            assert_eq!(chunk, back, "round-trip mismatch via {json}");
        }
    }

    /// C-133: the two accumulators answer different questions and must not be conflated.
    /// `Usage::accumulate` reports context-window occupancy (last round wins); `CacheEfficiency`
    /// reports the token-weighted cache share across every call. A three-round turn whose rounds hit
    /// 90% / 60% / 20% must report the weighted turn figure, not round three's 20%.
    #[test]
    fn cache_efficiency_is_token_weighted_across_calls_not_the_last_round() {
        let round = |read: u64, fresh: u64| Usage {
            input_tokens: fresh,
            output_tokens: 10,
            cache_read_input_tokens: read,
            ..Default::default()
        };
        // 90k/100k, 60k/100k, 20k/100k prompts.
        let calls = [
            round(90_000, 10_000),
            round(60_000, 40_000),
            round(20_000, 80_000),
        ];

        let mut occupancy = Usage::default();
        let mut efficiency = CacheEfficiency::default();
        for call in &calls {
            occupancy.accumulate(call);
            efficiency.add(call);
        }

        // Occupancy keeps its replace semantics: the last round's prompt IS the window occupancy.
        assert_eq!(occupancy.context_tokens(), 100_000);
        assert_eq!(occupancy.cache_read_input_tokens, 20_000);
        // …and reading a hit rate off it would report the turn's worst round.
        let last_round_ratio =
            occupancy.cache_read_input_tokens as f64 / occupancy.context_tokens() as f64;
        assert!((last_round_ratio - 0.20).abs() < 1e-9);

        // Efficiency sums all three: (90k + 60k + 20k) / 300k.
        assert_eq!(efficiency.prompt_tokens(), 300_000);
        assert_eq!(efficiency.read, 170_000);
        assert_eq!(efficiency.fresh, 130_000);
        assert!((efficiency.hit_rate() - 170_000.0 / 300_000.0).abs() < 1e-9);
        // The turn figure is materially better than the last round's — that gap is the bug C-133 closes.
        assert!(efficiency.hit_rate() > last_round_ratio + 0.3);
    }

    /// Output, reasoning, and audio are not prompt tokens; folding a call must not count them.
    #[test]
    fn cache_efficiency_counts_only_the_prompt_tiers() {
        let mut eff = CacheEfficiency::default();
        eff.add(&Usage {
            input_tokens: 100,
            output_tokens: 5_000,
            cache_creation_input_tokens: 200,
            cache_read_input_tokens: 700,
            reasoning_tokens: 4_000,
            audio_input_tokens: 50,
            audio_output_tokens: 60,
            reported_cost_usd: Some(1.0),
        });
        assert_eq!(eff.read, 700);
        assert_eq!(eff.write, 200);
        assert_eq!(eff.fresh, 100);
        // The three tiers reconstruct exactly one call's context_tokens().
        assert_eq!(eff.prompt_tokens(), 1_000);
    }

    #[test]
    fn cache_efficiency_empty_state_is_zero_not_a_division_by_zero() {
        let eff = CacheEfficiency::default();
        assert!(eff.is_empty());
        assert_eq!(eff.prompt_tokens(), 0);
        assert_eq!(eff.hit_rate(), 0.0);
    }
}
