//! Cross-provider pricing & cost model (pure, IO-free).
//!
//! This module turns a [`Usage`] record plus a model id into a [`Money`] cost. It carries a
//! built-in curated table of per-model, per-tier rates (input / output / cache-write / cache-read /
//! reasoning, each a price **per 1,000,000 tokens**), and computes
//!
//! ```text
//! cost = (input·r_in + output·r_out + cache_write·r_cw + cache_read·r_cr + reasoning·r_re) / 1e6
//! ```
//!
//! It is deliberately pure: there is no IO here. The optional user override file
//! (`~/.flux/pricing.toml`) is read in a higher, IO-permitted layer (`flux-credentials`), which
//! parses partial overrides into [`RateOverride`]s and folds them onto [`PricingTable::builtin`] via
//! [`PricingTable::apply_override`].
//!
//! ## Reasoning tokens
//! `reasoning_tokens` are a **subset of `output_tokens`** (the provider already counts them as
//! output). To avoid double-billing, every built-in rate sets the `reasoning` tier to `0.0`: ordinary
//! output already covers reasoning at the output rate. The reasoning tier exists as a **surcharge**
//! knob so a user (or a future provider) that prices reasoning apart from ordinary output can set a
//! non-zero rate via `pricing.toml`.
//!
//! ## Subscription providers
//! `claude` (Claude Max / Claude-Code OAuth) and `codex` (ChatGPT/Codex OAuth) bill against a flat
//! subscription, not metered API usage. When the model spec carries a `claude/` or `codex/` provider
//! prefix, the returned [`Money`] is flagged [`Money::subscription`] = `true`: the dollar figure is
//! the *equivalent* metered cost, clearly labelled, not an actual incremental charge.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stream::Usage;

/// Per-tier price, in **US dollars per 1,000,000 tokens**.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rates {
    /// Fresh (uncached) input tokens.
    pub input: f64,
    /// Generated output tokens (includes reasoning at this rate unless `reasoning` overrides it).
    pub output: f64,
    /// Cache-creation ("cache write") input tokens.
    pub cache_write: f64,
    /// Cache-read input tokens.
    pub cache_read: f64,
    /// Reasoning tokens — a **surcharge** over `output`. Default `0.0` because reasoning is a subset
    /// of output and already billed at the output rate; set it non-zero only to price reasoning apart.
    pub reasoning: f64,
}

/// A computed cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Money {
    /// Cost in US dollars.
    pub usd: f64,
    /// `true` when this spend bills against a flat-rate subscription (claude/codex) rather than
    /// metered API usage — the figure is the *equivalent* metered cost, not an incremental charge.
    pub subscription: bool,
}

/// A partial override for one model's [`Rates`]: any field left `None` keeps the built-in value.
/// This is what `~/.flux/pricing.toml` deserializes into (per model) before being folded onto the
/// built-in table; see [`PricingTable::apply_override`].
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RateOverride {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub reasoning: Option<f64>,
}

/// A price book: model id → per-tier [`Rates`]. Build the curated defaults with
/// [`PricingTable::builtin`], then optionally fold user overrides on top with
/// [`PricingTable::apply_override`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PricingTable {
    rates: BTreeMap<String, Rates>,
}

/// Provider prefixes flux understands (mirrors `KNOWN_PROVIDERS` in the CLI). Used to recognise and
/// strip a leading `provider/` from a model spec without mistaking an OpenRouter model id (which
/// itself contains slashes, e.g. `anthropic/claude-sonnet-4.6`) for a prefix.
fn known_provider(p: &str) -> bool {
    matches!(
        p,
        "anthropic"
            | "claude"
            | "openai"
            | "codex"
            | "openrouter"
            | "openrouter-anthropic"
            | "ollama"
            | "ollama-anthropic"
    )
}

/// Split a `provider/model` spec into `(Some(provider), model)` when the leading segment is a known
/// provider; otherwise `(None, spec)`.
fn split_provider(spec: &str) -> (Option<&str>, &str) {
    match spec.split_once('/') {
        Some((p, rest)) if known_provider(p) => (Some(p), rest),
        _ => (None, spec),
    }
}

/// Resolve flux's short model aliases to their canonical ids (mirrors `resolve_anthropic_alias`).
fn resolve_alias(model: &str) -> &str {
    match model {
        "sonnet" => "claude-sonnet-4-6",
        "opus" => "claude-opus-4-8",
        "haiku" => "claude-haiku-4-5-20251001",
        other => other,
    }
}

/// `true` when a model spec bills against a subscription (claude/codex), so any computed cost is the
/// *equivalent* metered figure rather than an incremental charge. Requires the `provider/` prefix —
/// a bare model id (e.g. `claude-opus-4-8`) is ambiguous between the metered `anthropic` provider and
/// the subscription `claude` provider, so it is reported as non-subscription.
pub fn is_subscription(spec: &str) -> bool {
    matches!(split_provider(spec).0, Some("claude") | Some("codex"))
}

impl PricingTable {
    /// The built-in curated rate table. Prices are USD per 1M tokens.
    ///
    /// `// TODO verify rates` — these are plausible public list prices captured for the mechanism's
    /// sake; confirm against each vendor's current pricing page before relying on the exact figures.
    /// Cache-write ≈ 1.25× input and cache-read ≈ 0.1× input follow the standard Anthropic ephemeral
    /// (5-minute) cache economics; OpenAI has no cache-write tier (cached input is just discounted),
    /// so `cache_write` mirrors `input` and `cache_read` ≈ 0.1× input there.
    pub fn builtin() -> Self {
        let mut rates = BTreeMap::new();

        // --- Anthropic / Claude (input, output, cache_write, cache_read, reasoning) ---------------
        let opus = Rates {
            input: 5.0,
            output: 25.0,
            cache_write: 6.25,
            cache_read: 0.50,
            reasoning: 0.0,
        };
        rates.insert("claude-opus-4-8".to_string(), opus);
        rates.insert("claude-opus-4-7".to_string(), opus);
        rates.insert(
            "claude-sonnet-4-6".to_string(),
            Rates {
                input: 3.0,
                output: 15.0,
                cache_write: 3.75,
                cache_read: 0.30,
                reasoning: 0.0,
            },
        );
        let haiku = Rates {
            input: 1.0,
            output: 5.0,
            cache_write: 1.25,
            cache_read: 0.10,
            reasoning: 0.0,
        };
        rates.insert("claude-haiku-4-5-20251001".to_string(), haiku);
        rates.insert("claude-haiku-4-5".to_string(), haiku);

        // --- OpenAI / Codex (GPT-5 family; cache_write == input, no write premium) ----------------
        let gpt5 = Rates {
            input: 1.25,
            output: 10.0,
            cache_write: 1.25,
            cache_read: 0.125,
            reasoning: 0.0,
        };
        rates.insert("gpt-5".to_string(), gpt5);
        rates.insert("gpt-5-codex".to_string(), gpt5);

        // --- OpenRouter passthrough models (keyed by the OpenRouter model id, slash and all) -------
        rates.insert(
            "anthropic/claude-sonnet-4.6".to_string(),
            Rates {
                input: 3.0,
                output: 15.0,
                cache_write: 3.75,
                cache_read: 0.30,
                reasoning: 0.0,
            },
        );
        rates.insert(
            "meta-llama/llama-3.3-70b-instruct".to_string(),
            Rates {
                input: 0.12,
                output: 0.30,
                cache_write: 0.12,
                cache_read: 0.12,
                reasoning: 0.0,
            },
        );

        PricingTable { rates }
    }

    /// Look up the rates for a model spec. Tries, in order: an exact match on the full spec (so an
    /// OpenRouter id like `anthropic/claude-sonnet-4.6` matches before its `anthropic/` prefix is
    /// stripped), then the provider-stripped model id, then the alias-resolved id.
    pub fn rates_for(&self, spec: &str) -> Option<&Rates> {
        if let Some(r) = self.rates.get(spec) {
            return Some(r);
        }
        let (_, model) = split_provider(spec);
        if let Some(r) = self.rates.get(model) {
            return Some(r);
        }
        let alias = resolve_alias(model);
        if alias != model {
            if let Some(r) = self.rates.get(alias) {
                return Some(r);
            }
        }
        None
    }

    /// Compute the cost of a usage record under a model spec. Returns `None` for an unknown model
    /// (never panics). The [`Money::subscription`] flag is set per [`is_subscription`].
    pub fn cost(&self, usage: &Usage, model: &str) -> Option<Money> {
        let r = self.rates_for(model)?;
        let usd = (usage.input_tokens as f64 * r.input
            + usage.output_tokens as f64 * r.output
            + usage.cache_creation_input_tokens as f64 * r.cache_write
            + usage.cache_read_input_tokens as f64 * r.cache_read
            + usage.reasoning_tokens as f64 * r.reasoning)
            / 1_000_000.0;
        Some(Money {
            usd,
            subscription: is_subscription(model),
        })
    }

    /// Fold a partial override onto this table for `model`. The base is this table's current exact
    /// entry for `model` (or [`Rates::default`] if absent); each `Some` field of `ov` replaces the
    /// corresponding tier, each `None` keeps the base value. The key is stored exactly as given
    /// (override files use canonical model ids).
    pub fn apply_override(&mut self, model: &str, ov: &RateOverride) {
        let base = self.rates.get(model).copied().unwrap_or_default();
        let merged = Rates {
            input: ov.input.unwrap_or(base.input),
            output: ov.output.unwrap_or(base.output),
            cache_write: ov.cache_write.unwrap_or(base.cache_write),
            cache_read: ov.cache_read.unwrap_or(base.cache_read),
            reasoning: ov.reasoning.unwrap_or(base.reasoning),
        };
        self.rates.insert(model.to_string(), merged);
    }

    /// Insert or replace a model's full rates outright.
    pub fn set(&mut self, model: impl Into<String>, rates: Rates) {
        self.rates.insert(model.into(), rates);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_applies_per_tier_rates() {
        // A bespoke table with a distinct, non-zero rate on every tier — including reasoning — so the
        // per-tier multiplication is unambiguous.
        let mut table = PricingTable::default();
        table.set(
            "test-model",
            Rates {
                input: 2.0,
                output: 4.0,
                cache_write: 6.0,
                cache_read: 1.0,
                reasoning: 8.0,
            },
        );

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_creation_input_tokens: 200_000,
            cache_read_input_tokens: 2_000_000,
            reasoning_tokens: 100_000,
        };
        // 1.0·2 + 0.5·4 + 0.2·6 + 2.0·1 + 0.1·8 = 2 + 2 + 1.2 + 2 + 0.8 = 8.0
        let money = table.cost(&usage, "test-model").unwrap();
        assert!((money.usd - 8.0).abs() < 1e-9, "got {}", money.usd);
        assert!(!money.subscription);

        // Unknown model → None, no panic.
        assert!(table.cost(&usage, "no-such-model").is_none());

        // The built-in table resolves flux aliases and provider prefixes.
        let builtin = PricingTable::builtin();
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        // sonnet → claude-sonnet-4-6: 3 + 15 = 18.
        let m = builtin.cost(&u, "claude/sonnet").unwrap();
        assert!((m.usd - 18.0).abs() < 1e-9, "got {}", m.usd);
        // `anthropic/claude-sonnet-4-6` resolves to the same rates via prefix-strip.
        let m2 = builtin.cost(&u, "anthropic/claude-sonnet-4-6").unwrap();
        assert!((m2.usd - 18.0).abs() < 1e-9, "got {}", m2.usd);
    }

    #[test]
    fn subscription_cost_is_labelled() {
        let table = PricingTable::builtin();
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 1_000,
            ..Default::default()
        };

        // claude/codex providers → labelled as subscription (equivalent metered cost).
        assert!(table.cost(&usage, "claude/opus").unwrap().subscription);
        assert!(
            table
                .cost(&usage, "codex/gpt-5-codex")
                .unwrap()
                .subscription
        );

        // Metered API providers → not subscription.
        assert!(
            !table
                .cost(&usage, "anthropic/claude-opus-4-8")
                .unwrap()
                .subscription
        );
        assert!(!table.cost(&usage, "openai/gpt-5").unwrap().subscription);
        // A bare model id (no provider prefix) is reported as non-subscription.
        assert!(!table.cost(&usage, "claude-opus-4-8").unwrap().subscription);

        // The free function agrees.
        assert!(is_subscription("claude/sonnet"));
        assert!(is_subscription("codex/gpt-5-codex"));
        assert!(!is_subscription("anthropic/claude-opus-4-8"));
        assert!(!is_subscription("claude-opus-4-8"));
    }

    #[test]
    fn apply_override_is_partial() {
        let mut table = PricingTable::builtin();
        let before = *table.rates_for("claude-opus-4-8").unwrap();

        // Override only the input rate.
        table.apply_override(
            "claude-opus-4-8",
            &RateOverride {
                input: Some(99.0),
                ..Default::default()
            },
        );
        let after = *table.rates_for("claude-opus-4-8").unwrap();
        assert_eq!(after.input, 99.0);
        // Other tiers are untouched.
        assert_eq!(after.output, before.output);
        assert_eq!(after.cache_read, before.cache_read);
    }
}
