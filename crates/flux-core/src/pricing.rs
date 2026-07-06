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

/// Where a [`Money`] figure came from (C-34).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostSource {
    /// The provider itself reported this call's dollar cost (currently: OpenRouter's `cost` field
    /// on both wires) — strictly more truthful than the static table (routing/discount-aware).
    Reported,
    /// Computed from [`PricingTable`]'s static per-tier rates.
    Estimated,
}

/// A computed cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Money {
    /// Cost in US dollars.
    pub usd: f64,
    /// `true` when this spend bills against a flat-rate subscription (claude/codex) rather than
    /// metered API usage — the figure is the *equivalent* metered cost, not an incremental charge.
    pub subscription: bool,
    /// Whether `usd` came from the provider's own reported figure or from the static table (C-34).
    pub source: CostSource,
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
            | "aws"
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

/// Resolve flux's short model aliases to their canonical ids.
///
/// This is a **layer-forced mirror** of the canonical mapping in
/// `flux_providers::anthropic::resolve_model`: `flux-core` is L0 and cannot depend on L1
/// (`flux-providers`), so the cost model keeps its own copy to turn a user alias into the
/// canonical id it prices by. The provider crate remains the single source of truth for surfaces
/// that *can* reach it (CLI/SDK/server/TUI); keep these two tables in lock-step when an alias
/// changes. (The codex alias `gpt-5.5`-vs-legacy `*-codex` lives in `flux_providers::codex` and
/// is not mirrored here — pricing keys by the resolved canonical id, so it never sees the alias.)
fn resolve_alias(model: &str) -> &str {
    match model {
        "sonnet" => "claude-sonnet-4-6",
        "opus" => "claude-opus-4-8",
        "haiku" => "claude-haiku-4-5-20251001",
        other => other,
    }
}

/// Strip a Bedrock cross-region inference-profile routing prefix (`us.`/`eu.`/`apac.`/`global.`)
/// from an `anthropic.*` model id. The prefix picks the serving region, not the price — every
/// region bills the same rate — so pricing keys the region-less id.
fn strip_bedrock_region_prefix(model: &str) -> Option<&str> {
    ["us.", "eu.", "apac.", "global."]
        .iter()
        .find_map(|p| model.strip_prefix(p))
        .filter(|rest| rest.starts_with("anthropic."))
}

/// `true` when a model spec bills against a subscription (claude/codex), so any computed cost is the
/// *equivalent* metered figure rather than an incremental charge. Requires the `provider/` prefix —
/// a bare model id (e.g. `claude-opus-4-8`) is ambiguous between the metered `anthropic` provider and
/// the subscription `claude` provider, so it is reported as non-subscription.
pub fn is_subscription(spec: &str) -> bool {
    matches!(split_provider(spec).0, Some("claude") | Some("codex"))
}

/// The canonical attribution key for usage records: `provider/model`, with flux's short aliases
/// resolved and Bedrock's regional routing prefix stripped — stamped at **write** time on
/// `CallUsage`/`TurnStarted` events so `cost_summary`/`flux usage` never splits one backend's
/// spend across key variants (`gpt-5.5` vs `openai/gpt-5.5`, `us.anthropic.…` vs `anthropic.…`)
/// (C-15). A spec that already carries a known provider keeps it — unless a DIFFERENT known
/// provider is actually serving the call (an OpenRouter passthrough id like
/// `openrouter-anthropic` serving `anthropic/claude-sonnet-4.6`), in which case the serving
/// provider becomes the outer prefix and the embedded segment stays part of the model id (C-30).
/// A bare model id is prefixed with `provider` when that names a known provider (a `mock`/unknown
/// provider leaves the id bare, so hermetic tests and ad-hoc providers are untouched).
pub fn canonical_model_spec(provider: Option<&str>, model: &str) -> String {
    let (spec_provider, bare) = split_provider(model);
    let bare = resolve_alias(bare);
    let bare = strip_bedrock_region_prefix(bare).unwrap_or(bare);
    let passed = provider.filter(|p| known_provider(p));
    match (passed, spec_provider) {
        // The spec embeds a DIFFERENT known provider than the one actually serving the call: a
        // passthrough id (e.g. `openrouter-anthropic` serving `anthropic/claude-sonnet-4.6`).
        // The serving provider wins the outer prefix and the embedded segment stays part of the
        // model id — spend must land under the provider that bills for it (C-30). Historical
        // rows written under the dropped-outer form stay as separate rows; `merge_legacy_keys`
        // never guesses across providers.
        (Some(p), Some(sp)) if p != sp => format!("{p}/{sp}/{bare}"),
        (_, Some(sp)) => format!("{sp}/{bare}"),
        (Some(p), None) => format!("{p}/{bare}"),
        (None, None) => bare.to_string(),
    }
}

/// The `(provider?, canonical model id)` pair behind [`canonical_model_spec`] — the read-side
/// merge key `cost_summary` uses to fold legacy key variants written before write-time stamping.
pub fn canonical_model_parts(spec: &str) -> (Option<&str>, &str) {
    let (provider, bare) = split_provider(spec);
    let bare = resolve_alias(bare);
    let bare = strip_bedrock_region_prefix(bare).unwrap_or(bare);
    (provider, bare)
}

/// `true` when a model spec names a metered **cloud** provider — i.e. a pricing-table miss there
/// hides real spend and should surface as the `$?` (unpriced) marker rather than staying silent.
/// Local `ollama*` and unrecognized/mock providers return `false` (nothing is billed there, so
/// silence on a table miss is correct). Shared by every cost-display surface (`flux-cli`'s turn
/// suffix, the TUI's cumulative header) so "which specs get the `$?` marker" has one definition —
/// see `unpriced_marker_applies` in `flux-cli` (thin delegate) and `record_usage` in `flux-tui`.
pub fn is_metered_cloud_spec(spec: &str) -> bool {
    match canonical_model_parts(spec).0 {
        Some(p) => !p.starts_with("ollama"),
        None => false,
    }
}

/// Resolve a sub-agent role's `model:` frontmatter override against the **parent's** provider
/// (A-41). Sub-agents always run on the parent's provider — there is no per-sub-agent provider
/// factory — but a role's `model:` value speaks the same provider-prefixed spec form `-m` accepts
/// (e.g. `openrouter/deepseek/deepseek-v4-flash`), so a naive verbatim pass-through reaches the
/// wire and 400s mid-turn the moment a user writes that natural form in role frontmatter. This
/// reuses [`split_provider`]/[`known_provider`] (the same prefix-matching [`canonical_model_spec`]
/// uses) rather than a second ad-hoc parser, so it never naively splits on the first `/` — an
/// OpenRouter model id legitimately contains one (`vendor/model`).
///
/// - A **bare** model id, or one prefixed by any segment that isn't a recognised provider name, is
///   not a prefix at all — it's just part of the model id — and passes through **unchanged**.
/// - A model id prefixed by exactly the **parent's own** provider name (string-exact — `openrouter`
///   and `openrouter-anthropic` are distinct providers, never treated as a prefix of one another)
///   has that prefix **stripped**, leaving the provider-local slug the wire expects.
/// - A model id prefixed by any **other** known provider name is rejected: sub-agents cannot
///   target a different provider than their parent, so this fails fast with a diagnostic naming
///   both providers instead of surfacing as a raw wire error mid-turn.
pub fn resolve_role_model(parent_provider: &str, role_model: &str) -> crate::Result<String> {
    if let Some(bare) = role_model.strip_prefix(&format!("{parent_provider}/")) {
        return Ok(bare.to_string());
    }
    if let (Some(other), _) = split_provider(role_model) {
        if other != parent_provider {
            return Err(crate::Error::Config(format!(
                "role model '{role_model}' targets provider '{other}', but sub-agents always run \
                 on the parent's provider ('{parent_provider}'); drop the '{other}/' prefix, or \
                 omit `model:` to inherit '{parent_provider}'"
            )));
        }
    }
    Ok(role_model.to_string())
}

impl PricingTable {
    /// The built-in curated rate table. Prices are USD per 1M tokens.
    ///
    /// Verified against the vendors' public pricing pages on **2026-07-02**:
    /// - Anthropic: <https://platform.claude.com/docs/en/about-claude/pricing>
    /// - OpenAI: <https://developers.openai.com/api/docs/pricing> (base `gpt-5` is off the main
    ///   sheet but still served at its published rate, confirmed on the per-model page
    ///   <https://developers.openai.com/api/docs/models/gpt-5>)
    /// - AWS Bedrock: <https://aws.amazon.com/bedrock/pricing/> — Anthropic models bill at the
    ///   direct Anthropic list rates
    /// - OpenRouter: <https://openrouter.ai/anthropic/claude-sonnet-4.6> and
    ///   <https://openrouter.ai/meta-llama/llama-3.3-70b-instruct>
    ///
    /// Cache tiers per vendor: Anthropic bills ephemeral (5-minute) cache writes at 1.25× input
    /// and cache reads at 0.1× input — both confirmed on the sheet; the 1-hour cache-write tier
    /// (2× input) is NOT modelled because flux never requests it. OpenAI has no cache-write tier —
    /// cached input is simply discounted (0.1× input on the current sheet) — so `cache_write`
    /// mirrors `input` there.
    ///
    /// Known, deliberate approximations (the vendor sheet disagrees at the margin):
    /// - Bedrock regional/multi-region cross-region profiles (`us.`/`eu.`/`apac.`) carry a ~10%
    ///   premium over `global.` endpoints on 4.5+ models; this table prices every routing prefix
    ///   at the base (global) rate.
    /// - OpenAI's gpt-5.5 long-context premium (input beyond 272K tokens bills 2× input /
    ///   1.5× output) is not modelled.
    /// - Rows with no current public sheet are marked **estimated** inline (`gpt-5-codex`,
    ///   delisted); the OpenRouter llama row is the listed price as of the verification date but
    ///   floats across serving providers.
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

        // --- AWS Bedrock (Anthropic models behind the SigV4 gate; rates match direct Anthropic) -----
        // Keyed by the **region-less** Bedrock model id: `resolve_model` emits cross-region
        // inference-profile ids (`us.`/`eu.`/`global.` + the id) whose price is the same in every
        // region, and `rates_for` strips that routing prefix before this lookup. Bedrock is
        // metered (pay-per-token via AWS), not a subscription.
        rates.insert(
            "anthropic.claude-sonnet-4-6".to_string(),
            Rates {
                input: 3.0,
                output: 15.0,
                cache_write: 3.75,
                cache_read: 0.30,
                reasoning: 0.0,
            },
        );
        rates.insert("anthropic.claude-opus-4-6-v1".to_string(), opus);
        rates.insert(
            "anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
            haiku,
        );

        // --- OpenAI / Codex (GPT-5 family; cache_write == input, no write premium) ----------------
        let gpt5 = Rates {
            input: 1.25,
            output: 10.0,
            cache_write: 1.25,
            cache_read: 0.125,
            reasoning: 0.0,
        };
        rates.insert("gpt-5".to_string(), gpt5);
        rates.insert(
            "gpt-5.5".to_string(),
            Rates {
                input: 5.0,
                output: 30.0,
                cache_write: 5.0,
                cache_read: 0.50,
                reasoning: 0.0,
            },
        );
        // Legacy alias: the `codex` provider resolves `*-codex` → `gpt-5.5` before cost, but keep the
        // key so a raw `codex/gpt-5-codex` spec still prices (defence-in-depth, never the live path).
        // ESTIMATED: delisted from OpenAI's current sheet; kept at its last published list price
        // (which matched gpt-5), the rate historical events with the raw legacy id actually ran at.
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
        // ESTIMATED: multi-provider routed — the OpenRouter listed price as of 2026-07-02; the
        // effective rate floats with the serving provider. No caching tiers published (cache
        // tiers mirror input so cached tokens never under-bill).
        rates.insert(
            "meta-llama/llama-3.3-70b-instruct".to_string(),
            Rates {
                input: 0.10,
                output: 0.32,
                cache_write: 0.10,
                cache_read: 0.10,
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
        // Bedrock cross-region inference profiles (`us.`/`eu.`/`apac.`/`global.`) share one price;
        // the table keys the region-less id, so strip the routing prefix.
        if let Some(rest) = strip_bedrock_region_prefix(model) {
            if let Some(r) = self.rates.get(rest) {
                return Some(r);
            }
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
    /// with no reported cost either (never panics). The [`Money::subscription`] flag is set per
    /// [`is_subscription`].
    ///
    /// **C-34: reported cost short-circuits the table.** When `usage.reported_cost_usd` is `Some`
    /// (currently only OpenRouter, on both wires), that figure is authoritative — it is the
    /// provider's own charge, routing/discount-aware in a way the static table can never be — and is
    /// returned directly as [`CostSource::Reported`], **without** consulting the table at all. This
    /// is what prices models the table has no row for (killing the `$? (unpriced)` marker) and lets
    /// a reported `0.0` (a `:free` model) correctly return `Some(0.0)` rather than `None`. Only when
    /// no cost was reported does this fall through to the table math, returning
    /// [`CostSource::Estimated`].
    pub fn cost(&self, usage: &Usage, model: &str) -> Option<Money> {
        if let Some(usd) = usage.reported_cost_usd {
            return Some(Money {
                usd,
                subscription: is_subscription(model),
                source: CostSource::Reported,
            });
        }
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
            source: CostSource::Estimated,
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
            ..Default::default()
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

    /// C-15: the write-time attribution key — bare ids get the provider prefix, existing specs
    /// are preserved, aliases resolve, Bedrock regional prefixes strip, unknown providers stay bare.
    #[test]
    fn canonical_model_spec_prefixes_bare_ids_and_preserves_specs() {
        assert_eq!(
            canonical_model_spec(Some("openai"), "gpt-5.5"),
            "openai/gpt-5.5"
        );
        // A spec embedding a DIFFERENT known provider is a passthrough id: the serving
        // (passed) provider wins the outer prefix and the embedded segment stays part of the
        // model id — spend must land under the provider that actually bills for it.
        assert_eq!(
            canonical_model_spec(Some("anthropic"), "openai/gpt-5.5"),
            "anthropic/openai/gpt-5.5"
        );
        // Same provider passed and embedded: no double prefix.
        assert_eq!(
            canonical_model_spec(Some("openai"), "openai/gpt-5.5"),
            "openai/gpt-5.5"
        );
        // Alias resolution + prefixing.
        assert_eq!(
            canonical_model_spec(Some("anthropic"), "sonnet"),
            "anthropic/claude-sonnet-4-6"
        );
        // Bedrock regional routing prefix strips to the region-less id.
        assert_eq!(
            canonical_model_spec(Some("aws"), "us.anthropic.claude-sonnet-4-6"),
            "aws/anthropic.claude-sonnet-4-6"
        );
        // An unknown provider (mock/ad-hoc) leaves the id bare — hermetic tests untouched.
        assert_eq!(canonical_model_spec(Some("mock"), "mock"), "mock");
        assert_eq!(canonical_model_spec(None, "gpt-5.5"), "gpt-5.5");
    }

    /// C-30: an OpenRouter passthrough id must keep the SERVING provider as the outer prefix —
    /// `canonical_model_spec` used to drop `openrouter-anthropic` because the spec's own first
    /// segment (`anthropic`) is also a known provider, silently mislabeling OpenRouter spend as
    /// Anthropic in every stored usage key.
    #[test]
    fn canonical_model_spec_keeps_outer_openrouter_provider() {
        assert_eq!(
            canonical_model_spec(Some("openrouter-anthropic"), "anthropic/claude-sonnet-4.6"),
            "openrouter-anthropic/anthropic/claude-sonnet-4.6"
        );
        assert_eq!(
            canonical_model_spec(Some("openrouter"), "openai/gpt-4o"),
            "openrouter/openai/gpt-4o"
        );
    }

    /// A-41: a role `model:` prefixed by the parent's OWN provider is accepted — the prefix is
    /// stripped so the provider-local slug reaches the wire, not the full spec verbatim (the live
    /// failure: `openrouter/deepseek/deepseek-v4-flash` under a parent on `openrouter` 400ed).
    #[test]
    fn resolve_role_model_strips_matching_parent_provider_prefix() {
        assert_eq!(
            resolve_role_model("openrouter", "openrouter/deepseek/deepseek-v4-flash").unwrap(),
            "deepseek/deepseek-v4-flash"
        );
    }

    /// A-41: a role `model:` naming a DIFFERENT known provider than the parent fails fast with a
    /// diagnostic naming both providers, instead of reaching the wire as a raw spec and 400ing
    /// mid-turn.
    #[test]
    fn resolve_role_model_rejects_a_different_known_provider() {
        let err = resolve_role_model("openrouter", "anthropic/claude-sonnet-4-6").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("openrouter"),
            "names the parent provider: {msg}"
        );
        assert!(
            msg.contains("anthropic"),
            "names the requested provider: {msg}"
        );
    }

    /// A-41: `openrouter` and `openrouter-anthropic` are distinct providers — exact-string prefix
    /// matching only, never a substring/prefix-of-prefix match either direction.
    #[test]
    fn resolve_role_model_distinguishes_openrouter_variants() {
        // Parent is plain `openrouter`; role names the passthrough variant `openrouter-anthropic` —
        // a DIFFERENT provider, must reject even though it shares a textual prefix.
        assert!(resolve_role_model(
            "openrouter",
            "openrouter-anthropic/anthropic/claude-sonnet-4.6"
        )
        .is_err());
        // Parent is `openrouter-anthropic`; role names plain `openrouter` — also different, must
        // reject.
        assert!(resolve_role_model("openrouter-anthropic", "openrouter/openai/gpt-4o").is_err());
        // Parent is `openrouter-anthropic`; role matches it exactly — strips.
        assert_eq!(
            resolve_role_model(
                "openrouter-anthropic",
                "openrouter-anthropic/anthropic/claude-sonnet-4.6"
            )
            .unwrap(),
            "anthropic/claude-sonnet-4.6"
        );
    }

    /// A-41: bare provider-local slugs (the current working form) and ids with an unrecognised
    /// leading segment (not a known provider name, so not a prefix at all) pass through unchanged.
    #[test]
    fn resolve_role_model_passes_bare_and_unknown_prefixed_ids_unchanged() {
        assert_eq!(resolve_role_model("openrouter", "haiku").unwrap(), "haiku");
        assert_eq!(
            resolve_role_model("openrouter", "deepseek/deepseek-v4-flash").unwrap(),
            "deepseek/deepseek-v4-flash"
        );
    }

    /// C-30: the full passthrough spec (as now stamped at write time) still prices via
    /// `rates_for`'s provider-prefix strip, and reads as metered (not subscription).
    #[test]
    fn rates_for_resolves_full_openrouter_spec() {
        let builtin = PricingTable::builtin();
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        let full = builtin
            .cost(&u, "openrouter-anthropic/anthropic/claude-sonnet-4.6")
            .expect("the full passthrough spec must price");
        let bare = builtin.cost(&u, "anthropic/claude-sonnet-4.6").unwrap();
        assert!(
            (full.usd - bare.usd).abs() < 1e-9,
            "same row must price both forms"
        );
        assert!(
            !full.subscription,
            "openrouter passthrough is metered, not subscription"
        );
    }

    /// C-20: pin the headline rows to the vendor-verified rates so accidental edits are caught.
    /// Values verified against the vendor pricing pages on 2026-07-02 (source URLs in the
    /// [`PricingTable::builtin`] doc comment). Failing-first: `gpt-5.5` shipped at gpt-5's launch
    /// rates (1.25 / 10.0 / cached 0.125) — OpenAI's current sheet prices it at 5.0 / 30.0 with
    /// 0.50 cached input — and the OpenRouter llama row shipped as 0.12 / 0.30 vs the listed
    /// 0.10 / 0.32; this test failed on both until the table was corrected.
    #[test]
    fn builtin_pins_vendor_verified_headline_rates() {
        let t = PricingTable::builtin();
        let pin = |model: &str, input: f64, output: f64, cache_write: f64, cache_read: f64| {
            let r = t
                .rates_for(model)
                .unwrap_or_else(|| panic!("{model} must be in the builtin table"));
            assert_eq!(
                (r.input, r.output, r.cache_write, r.cache_read),
                (input, output, cache_write, cache_read),
                "{model}: (input, output, cache_write, cache_read)"
            );
            assert_eq!(
                r.reasoning, 0.0,
                "{model}: reasoning is a surcharge tier, 0.0 in every built-in row"
            );
        };

        // Anthropic (5-minute ephemeral cache: write = 1.25x input, read = 0.1x input).
        pin("claude-opus-4-8", 5.0, 25.0, 6.25, 0.50);
        pin("claude-opus-4-7", 5.0, 25.0, 6.25, 0.50);
        pin("claude-sonnet-4-6", 3.0, 15.0, 3.75, 0.30);
        pin("claude-haiku-4-5", 1.0, 5.0, 1.25, 0.10);

        // OpenAI (no cache-write tier: cache_write == input; cached input = 0.1x input).
        pin("gpt-5", 1.25, 10.0, 1.25, 0.125);
        pin("gpt-5.5", 5.0, 30.0, 5.0, 0.50);

        // AWS Bedrock bills Anthropic models at the direct Anthropic list rates (global endpoint).
        assert_eq!(
            t.rates_for("anthropic.claude-sonnet-4-6"),
            t.rates_for("claude-sonnet-4-6"),
            "Bedrock Sonnet must match the direct Anthropic rates"
        );

        // OpenRouter passthrough + the listed llama price as of 2026-07-02 (floats across routes).
        pin("anthropic/claude-sonnet-4.6", 3.0, 15.0, 3.75, 0.30);
        pin("meta-llama/llama-3.3-70b-instruct", 0.10, 0.32, 0.10, 0.10);
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
        // The codex provider resolves to `gpt-5.5` (C-03); cost must resolve on that canonical id,
        // not just the legacy `gpt-5-codex`. Failing-first: before `gpt-5.5` was added to the table
        // this returned `None` (codex spend unpriced) — the C-03 model-resolution fix had made the
        // resolver emit an id the pricing table didn't carry.
        let codex_cost = table
            .cost(&usage, "codex/gpt-5.5")
            .expect("codex/gpt-5.5 must price — the resolver emits this canonical id");
        assert!(codex_cost.subscription);
        assert!(codex_cost.usd > 0.0);

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

        // AWS Bedrock (metered via AWS, not a sub): the canonical spec `aws/<bedrock-id>` must
        // price against the Bedrock model-id entries. Failing-first: before the `anthropic.*`
        // entries were added this returned `None` (Bedrock spend unpriced).
        let aws_cost = table
            .cost(&usage, "aws/us.anthropic.claude-sonnet-4-6")
            .expect("aws/us.anthropic.claude-sonnet-4-6 must price — Bedrock Anthropic rates");
        assert!(
            !aws_cost.subscription,
            "Bedrock is metered, not a subscription"
        );
        assert!(aws_cost.usd > 0.0);
        // Every cross-region routing prefix prices identically — the region picks the serving
        // stack, not the rate. Failing-first: with rates keyed `us.anthropic.*` an eu-region run
        // (resolve_model emits `eu.anthropic.*`) showed no cost suffix.
        for spec in [
            "aws/eu.anthropic.claude-sonnet-4-6",
            "aws/global.anthropic.claude-haiku-4-5-20251001-v1:0",
            "aws/eu.anthropic.claude-opus-4-6-v1",
        ] {
            let c = table
                .cost(&usage, spec)
                .unwrap_or_else(|| panic!("{spec} must price via region-prefix stripping"));
            assert!(c.usd > 0.0 && !c.subscription, "{spec}");
        }

        // The free function agrees.
        assert!(is_subscription("claude/sonnet"));
        assert!(is_subscription("codex/gpt-5.5"));
        assert!(!is_subscription("anthropic/claude-opus-4-8"));
        assert!(!is_subscription("claude-opus-4-8"));
    }

    /// C-34: `PricingTable::cost` prefers a call's provider-reported cost over the static table —
    /// the single choke point that kills `$? (unpriced)` for OpenRouter models. An untabled model
    /// prices when the caller reported cost (previously `None`, forever). A TABLED model's reported
    /// figure still wins over the table math (routing/discounts the static table can't know). A
    /// tabled model with NO reported figure prices exactly as before (table math, unaffected). A
    /// reported `0.0` (a `:free` OpenRouter model) must still be `Some` — `0.0` is a real answer,
    /// not "unknown" — so it never shows the `$?` marker.
    #[test]
    fn reported_cost_beats_table_and_prices_untabled_models() {
        let table = PricingTable::builtin();

        // An untabled model: no reported cost → None (today's behavior, the `$?` marker case).
        let plain = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        };
        assert!(
            table
                .cost(&plain, "openrouter/some/untabled-model")
                .is_none(),
            "untabled + unreported must stay None"
        );

        // An untabled model WITH reported cost → Some(Reported), the exact reported figure —
        // this is the whole point of the story.
        let reported = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            reported_cost_usd: Some(0.001234),
            ..Default::default()
        };
        let money = table
            .cost(&reported, "openrouter/some/untabled-model")
            .expect("reported cost must price an untabled model");
        assert_eq!(money.usd, 0.001234);
        assert_eq!(money.source, CostSource::Reported);

        // A TABLED model (claude-sonnet-4-6) with a reported figure: the reported figure wins over
        // the table math, even though the table could price it too.
        let tabled_reported = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            reported_cost_usd: Some(9.99),
            ..Default::default()
        };
        let money = table
            .cost(&tabled_reported, "claude-sonnet-4-6")
            .expect("tabled model still prices");
        assert_eq!(
            money.usd, 9.99,
            "reported cost must short-circuit the table, not just supplement it"
        );
        assert_eq!(money.source, CostSource::Reported);

        // The SAME tabled model with NO reported figure: exact table math, unchanged — table
        // pricing must not regress for the vast majority of (non-OpenRouter) calls.
        let tabled_plain = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let money = table.cost(&tabled_plain, "claude-sonnet-4-6").unwrap();
        assert!((money.usd - 18.0).abs() < 1e-9, "got {}", money.usd);
        assert_eq!(money.source, CostSource::Estimated);

        // A reported 0.0 (a `:free` OpenRouter model) must still be Some — 0.0 is a real answer,
        // not "unknown" — so the caller never renders the `$?` marker for it.
        let free = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            reported_cost_usd: Some(0.0),
            ..Default::default()
        };
        let money = table
            .cost(&free, "openrouter/some/free-model")
            .expect("reported 0.0 must still be Some, not None");
        assert_eq!(money.usd, 0.0);
        assert_eq!(money.source, CostSource::Reported);
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
