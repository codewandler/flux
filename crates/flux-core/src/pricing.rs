//! Cross-provider pricing & cost model (pure, IO-free).
//!
//! This module turns a [`Usage`] record plus a model id into a [`Money`] cost. It carries a
//! built-in curated table of per-model, per-tier rates (input / output / cache-write / cache-read /
//! reasoning / audio-input / audio-output, each a price **per 1,000,000 tokens**), and computes
//!
//! ```text
//! cost = (input·r_in + output·r_out + cache_write·r_cw + cache_read·r_cr + reasoning·r_re
//!         + audio_input·r_ai + audio_output·r_ao) / 1e6
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
//! ## Audio tokens (C-38)
//! `audio_input_tokens`/`audio_output_tokens` are likewise **subsets** of `input_tokens`/
//! `output_tokens` (realtime voice-to-voice models report them as a split of the same totals, not
//! extra tokens) — so `audio_input`/`audio_output` are **surcharge** tiers over `input`/`output`,
//! exactly like `reasoning`. Every built-in row defaults them to `0.0` (audio bills as plain text);
//! the `gpt-realtime` family is the only row that sets them, since it is the only provider that
//! bills audio tokens apart from text.
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
    /// Audio-input tokens (C-38) — a **surcharge** over `input` per `audio_input_tokens` (already a
    /// subset of `input_tokens`, already billed once at `input`). `0.0` (default, and every
    /// pre-C-38 row) means audio bills as plain text. `#[serde(default)]` keeps existing serialized
    /// rates and `~/.flux/pricing.toml` overrides decoding without this field.
    #[serde(default)]
    pub audio_input: f64,
    /// Audio-output tokens (C-38) — a **surcharge** over `output` per `audio_output_tokens`. See
    /// `audio_input`.
    #[serde(default)]
    pub audio_output: f64,
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
    #[serde(default)]
    pub audio_input: Option<f64>,
    #[serde(default)]
    pub audio_output: Option<f64>,
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
        "sonnet" => "claude-sonnet-5",
        "opus" => "claude-opus-5",
        "haiku" => "claude-haiku-4-5",
        "fable" => "claude-fable-5",
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
    /// Verified against the vendors' public pricing pages on **2026-07-28**:
    /// - Anthropic: <https://platform.claude.com/docs/en/about-claude/pricing>
    /// - OpenAI: <https://developers.openai.com/api/docs/pricing> (base `gpt-5` is off the main
    ///   sheet but still served at its published rate, confirmed on the per-model page
    ///   <https://developers.openai.com/api/docs/models/gpt-5>)
    /// - AWS Bedrock: <https://aws.amazon.com/bedrock/pricing/> — Anthropic models bill at the
    ///   direct Anthropic list rates
    /// - OpenRouter: <https://openrouter.ai/api/v1/models> plus the public model pages for
    ///   `anthropic/claude-sonnet-4.6`, `deepseek/deepseek-v4-flash`,
    ///   `poolside/laguna-xs-2.1`, `qwen/qwen3.7-max`, `z-ai/glm-5.2`, and
    ///   `meta-llama/llama-3.3-70b-instruct`
    /// - models.dev: <https://models.dev/api.json> as a cross-check for provider-specific model
    ///   ids, context windows, and cache tiers where vendor pages expose only input/output prices
    ///
    /// `gpt-realtime`/`gpt-realtime-2` (C-38) verified separately against
    /// <https://developers.openai.com/api/docs/pricing> on **2026-07-06**.
    ///
    /// Cache tiers per vendor: Anthropic bills ephemeral (5-minute) cache writes at 1.25× input
    /// and cache reads at 0.1× input — both confirmed on the sheet; the 1-hour cache-write tier
    /// (2× input) is NOT modelled because flux never requests it. Most OpenAI rows have no
    /// cache-write premium and mirror `input`; GPT-5.6's Sol/Terra/Luna rows publish a 1.25×
    /// cache-write tier, while cached input remains 0.1× input.
    ///
    /// Audio tiers (`audio_input`/`audio_output`, C-38): a **surcharge** over the text rate, since
    /// `Usage::audio_input_tokens`/`audio_output_tokens` are subsets of `input_tokens`/
    /// `output_tokens` and already billed once at the text rate via those terms — see
    /// [`Usage`](crate::Usage) and [`PricingTable::cost`]'s doc comment.
    ///
    /// Known, deliberate approximations (the vendor sheet disagrees at the margin):
    /// - Bedrock regional/multi-region cross-region profiles (`us.`/`eu.`/`apac.`) carry a ~10%
    ///   premium over `global.` endpoints on 4.5+ models; this table prices every routing prefix
    ///   at the base (global) rate.
    /// - OpenAI's gpt-5.4/gpt-5.5/gpt-5.6 long-context premium (input beyond 272K tokens bills 2× input /
    ///   1.5× output) and priority service-tier premium are not modelled.
    /// - Rows with no current public sheet are marked **estimated** inline (`gpt-5-codex`,
    ///   delisted; `gpt-5.3-codex-spark`, documented by OpenAI as a Codex research-preview model);
    ///   the OpenRouter routed rows are listed prices as of the verification date but can float
    ///   across serving providers.
    pub fn builtin() -> Self {
        let mut rates = BTreeMap::new();
        let text = |input: f64, output: f64, cache_write: f64, cache_read: f64| Rates {
            input,
            output,
            cache_write,
            cache_read,
            reasoning: 0.0,
            audio_input: 0.0,
            audio_output: 0.0,
        };

        // --- Anthropic / Claude (input, output, cache_write, cache_read, reasoning) ---------------
        let fable = text(10.0, 50.0, 12.50, 1.00);
        rates.insert("claude-fable-5".to_string(), fable);
        let opus = text(5.0, 25.0, 6.25, 0.50);
        rates.insert("claude-opus-5".to_string(), opus);
        rates.insert("claude-opus-4-8".to_string(), opus);
        rates.insert("claude-opus-4-7".to_string(), opus);
        rates.insert("claude-opus-4-6".to_string(), opus);
        rates.insert("claude-opus-4-5".to_string(), opus);
        // Anthropic's introductory Sonnet 5 pricing runs through 2026-08-31. `PricingTable` is
        // deliberately static/IO-free, so this row should be revisited when that window closes.
        let sonnet5_intro = text(2.0, 10.0, 2.50, 0.20);
        rates.insert("claude-sonnet-5".to_string(), sonnet5_intro);
        let sonnet = text(3.0, 15.0, 3.75, 0.30);
        rates.insert("claude-sonnet-4-6".to_string(), sonnet);
        rates.insert("claude-sonnet-4-5-20250929".to_string(), sonnet);
        rates.insert("claude-sonnet-4-5".to_string(), sonnet);
        let haiku = text(1.0, 5.0, 1.25, 0.10);
        rates.insert("claude-haiku-4-5-20251001".to_string(), haiku);
        rates.insert("claude-haiku-4-5".to_string(), haiku);

        // --- AWS Bedrock (Anthropic models behind the SigV4 gate; rates match direct Anthropic) -----
        // Keyed by the **region-less** Bedrock model id: `resolve_model` emits cross-region
        // inference-profile ids (`us.`/`eu.`/`global.` + the id) whose price is the same in every
        // region, and `rates_for` strips that routing prefix before this lookup. Bedrock is
        // metered (pay-per-token via AWS), not a subscription.
        rates.insert("anthropic.claude-fable-5".to_string(), fable);
        rates.insert("anthropic.claude-sonnet-5".to_string(), sonnet5_intro);
        rates.insert("anthropic.claude-sonnet-4-6".to_string(), sonnet);
        rates.insert("anthropic.claude-sonnet-4-6-v1:0".to_string(), sonnet);
        rates.insert("anthropic.claude-sonnet-4-5-20250929".to_string(), sonnet);
        rates.insert(
            "anthropic.claude-sonnet-4-5-20250929-v1:0".to_string(),
            sonnet,
        );
        rates.insert("anthropic.claude-opus-5".to_string(), opus);
        rates.insert("anthropic.claude-opus-4-8".to_string(), opus);
        rates.insert("anthropic.claude-opus-4-7".to_string(), opus);
        rates.insert("anthropic.claude-opus-4-6".to_string(), opus);
        rates.insert("anthropic.claude-opus-4-6-v1".to_string(), opus);
        rates.insert("anthropic.claude-opus-4-5".to_string(), opus);
        rates.insert(
            "anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
            haiku,
        );

        // --- OpenAI / Codex (GPT-5 family; cache_write == input, no write premium) ----------------
        let gpt5 = text(1.25, 10.0, 1.25, 0.125);
        rates.insert("gpt-5".to_string(), gpt5);
        rates.insert("gpt-5.6".to_string(), text(5.0, 30.0, 6.25, 0.50));
        rates.insert("gpt-5.6-sol".to_string(), text(5.0, 30.0, 6.25, 0.50));
        rates.insert("gpt-5.6-terra".to_string(), text(2.5, 15.0, 3.125, 0.25));
        rates.insert("gpt-5.6-luna".to_string(), text(1.0, 6.0, 1.25, 0.10));
        rates.insert("gpt-5.5".to_string(), text(5.0, 30.0, 5.0, 0.50));
        rates.insert("gpt-5.4".to_string(), text(2.5, 15.0, 2.5, 0.25));
        rates.insert("gpt-5.4-mini".to_string(), text(0.75, 4.5, 0.75, 0.075));
        rates.insert("gpt-5.4-nano".to_string(), text(0.20, 1.25, 0.20, 0.020));
        let gpt53_codex = text(1.75, 14.0, 1.75, 0.175);
        rates.insert("gpt-5.3-codex".to_string(), gpt53_codex);
        // ESTIMATED: OpenAI documents Spark as a Codex research-preview model, while the public API
        // pricing sheet publishes only `gpt-5.3-codex`; models.dev currently tracks Spark at the
        // same token price. Keep historical Codex-session spend visible instead of `$?`.
        rates.insert("gpt-5.3-codex-spark".to_string(), gpt53_codex);
        // Legacy alias: the `codex` provider resolves `*-codex` → `gpt-5.5` before cost, but keep the
        // key so a raw `codex/gpt-5-codex` spec still prices (defence-in-depth, never the live path).
        // ESTIMATED: delisted from OpenAI's current sheet; kept at its last published list price
        // (which matched gpt-5), the rate historical events with the raw legacy id actually ran at.
        rates.insert("gpt-5-codex".to_string(), gpt5);

        // --- OpenAI Realtime (gpt-realtime family; voice-to-voice, C-38) --------------------------
        // Sheet as of 2026-07-06 (https://developers.openai.com/api/docs/pricing): text $4.00 in /
        // $0.40 cached / $24.00 out; audio $32.00 in / $0.40 cached / $64.00 out. `cache_write`
        // mirrors `input` (no OpenAI cache-write tier, same convention as gpt-5/gpt-5.5 above). The
        // `audio_input`/`audio_output` surcharges are the audio-over-text delta ($32-$4=$28 in,
        // $64-$24=$40 out) — the subset fields are already billed once at the text rate via the
        // parent `input`/`output` terms, so the surcharge tops that up to the audio rate rather than
        // billing audio twice. Cached audio folds into `cache_read` at the same $0.40 as cached
        // text — exact for this family (would diverge for a hypothetical tier with a different
        // cached-audio rate; documented approximation).
        let gpt_realtime = Rates {
            input: 4.0,
            output: 24.0,
            cache_write: 4.0,
            cache_read: 0.40,
            reasoning: 0.0,
            audio_input: 28.0,
            audio_output: 40.0,
        };
        rates.insert("gpt-realtime".to_string(), gpt_realtime);
        rates.insert("gpt-realtime-2".to_string(), gpt_realtime);

        // --- OpenRouter passthrough models (keyed by the OpenRouter model id, slash and all) -------
        rates.insert("anthropic/claude-sonnet-4.6".to_string(), sonnet);
        let deepseek_v4_flash = text(0.09, 0.18, 0.09, 0.018);
        rates.insert("deepseek/deepseek-v4-flash".to_string(), deepseek_v4_flash);
        rates.insert(
            "deepseek/deepseek-v4-flash:nitro".to_string(),
            deepseek_v4_flash,
        );
        rates.insert(
            "poolside/laguna-xs-2.1".to_string(),
            text(0.06, 0.12, 0.06, 0.03),
        );
        rates.insert(
            "qwen/qwen3.7-max".to_string(),
            text(1.25, 3.75, 1.5625, 0.25),
        );
        rates.insert(
            "qwen/qwen3.7-max-20260520".to_string(),
            text(1.25, 3.75, 1.5625, 0.25),
        );
        rates.insert("z-ai/glm-5.2".to_string(), text(0.42, 1.32, 0.42, 0.078));
        rates.insert(
            "z-ai/glm-5.2-20260616".to_string(),
            text(0.42, 1.32, 0.42, 0.078),
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
                audio_input: 0.0,
                audio_output: 0.0,
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
            + usage.reasoning_tokens as f64 * r.reasoning
            + usage.audio_input_tokens as f64 * r.audio_input
            + usage.audio_output_tokens as f64 * r.audio_output)
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
            audio_input: ov.audio_input.unwrap_or(base.audio_input),
            audio_output: ov.audio_output.unwrap_or(base.audio_output),
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
        // A bespoke table with a distinct, non-zero rate on every tier — including reasoning and
        // (C-38) the audio surcharges — so the per-tier multiplication is unambiguous.
        let mut table = PricingTable::default();
        table.set(
            "test-model",
            Rates {
                input: 2.0,
                output: 4.0,
                cache_write: 6.0,
                cache_read: 1.0,
                reasoning: 8.0,
                audio_input: 10.0,
                audio_output: 20.0,
            },
        );

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_creation_input_tokens: 200_000,
            cache_read_input_tokens: 2_000_000,
            reasoning_tokens: 100_000,
            audio_input_tokens: 50_000,
            audio_output_tokens: 25_000,
            ..Default::default()
        };
        // 1.0·2 + 0.5·4 + 0.2·6 + 2.0·1 + 0.1·8 + 0.05·10 + 0.025·20
        // = 2 + 2 + 1.2 + 2 + 0.8 + 0.5 + 0.5 = 9.0
        let money = table.cost(&usage, "test-model").unwrap();
        assert!((money.usd - 9.0).abs() < 1e-9, "got {}", money.usd);
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
        // sonnet → claude-sonnet-5 (intro rates): 2 + 10 = 12.
        let m = builtin.cost(&u, "claude/sonnet").unwrap();
        assert!((m.usd - 12.0).abs() < 1e-9, "got {}", m.usd);
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
            "anthropic/claude-sonnet-5"
        );
        assert_eq!(
            canonical_model_spec(Some("claude"), "fable"),
            "claude/claude-fable-5"
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
    /// Values verified against the vendor pricing pages on 2026-07-09 (source URLs in the
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
            assert_eq!(
                (r.audio_input, r.audio_output),
                (0.0, 0.0),
                "{model}: audio is a surcharge tier, 0.0 in every non-realtime built-in row"
            );
        };

        // Anthropic (5-minute ephemeral cache: write = 1.25x input, read = 0.1x input).
        pin("claude-fable-5", 10.0, 50.0, 12.50, 1.00);
        pin("claude-opus-5", 5.0, 25.0, 6.25, 0.50);
        pin("claude-opus-4-8", 5.0, 25.0, 6.25, 0.50);
        pin("claude-opus-4-7", 5.0, 25.0, 6.25, 0.50);
        pin("claude-opus-4-6", 5.0, 25.0, 6.25, 0.50);
        pin("claude-sonnet-5", 2.0, 10.0, 2.50, 0.20);
        pin("claude-sonnet-4-6", 3.0, 15.0, 3.75, 0.30);
        pin("claude-sonnet-4-5-20250929", 3.0, 15.0, 3.75, 0.30);
        pin("claude-haiku-4-5", 1.0, 5.0, 1.25, 0.10);

        // OpenAI (most rows: no cache-write tier, so cache_write == input; cached input = 0.1x input).
        // GPT-5.6 publishes a 1.25x cache-write tier.
        pin("gpt-5", 1.25, 10.0, 1.25, 0.125);
        pin("gpt-5.6", 5.0, 30.0, 6.25, 0.50);
        pin("gpt-5.6-sol", 5.0, 30.0, 6.25, 0.50);
        pin("gpt-5.6-terra", 2.5, 15.0, 3.125, 0.25);
        pin("gpt-5.6-luna", 1.0, 6.0, 1.25, 0.10);
        pin("gpt-5.5", 5.0, 30.0, 5.0, 0.50);
        pin("gpt-5.4", 2.5, 15.0, 2.5, 0.25);
        pin("gpt-5.4-mini", 0.75, 4.5, 0.75, 0.075);
        pin("gpt-5.3-codex", 1.75, 14.0, 1.75, 0.175);
        pin("codex/gpt-5.3-codex-spark", 1.75, 14.0, 1.75, 0.175);

        // AWS Bedrock bills Anthropic models at the direct Anthropic list rates (global endpoint).
        assert_eq!(
            t.rates_for("anthropic.claude-sonnet-4-6"),
            t.rates_for("claude-sonnet-4-6"),
            "Bedrock Sonnet must match the direct Anthropic rates"
        );
        assert_eq!(
            t.rates_for("anthropic.claude-fable-5"),
            t.rates_for("claude-fable-5"),
            "Bedrock Fable must match the direct Anthropic rates"
        );
        assert_eq!(
            t.rates_for("anthropic.claude-sonnet-5"),
            t.rates_for("claude-sonnet-5"),
            "Bedrock Sonnet 5 must match the direct Anthropic rates"
        );
        assert_eq!(
            t.rates_for("anthropic.claude-opus-5"),
            t.rates_for("claude-opus-5"),
            "Bedrock Opus 5 must match the direct Anthropic rates"
        );

        // OpenRouter passthrough + listed route prices as of 2026-07-09.
        pin("anthropic/claude-sonnet-4.6", 3.0, 15.0, 3.75, 0.30);
        pin(
            "openrouter/deepseek/deepseek-v4-flash:nitro",
            0.09,
            0.18,
            0.09,
            0.018,
        );
        pin("openrouter/poolside/laguna-xs-2.1", 0.06, 0.12, 0.06, 0.03);
        pin("openrouter/qwen/qwen3.7-max", 1.25, 3.75, 1.5625, 0.25);
        pin("openrouter/z-ai/glm-5.2", 0.42, 1.32, 0.42, 0.078);
        pin("meta-llama/llama-3.3-70b-instruct", 0.10, 0.32, 0.10, 0.10);
    }

    #[test]
    fn codex_prefixed_models_price_as_subscription_equivalents() {
        let t = PricingTable::builtin();
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };

        let sol = t.cost(&u, "codex/gpt-5.6-sol").unwrap();
        assert!(sol.subscription);
        assert!((sol.usd - 35.0).abs() < 1e-9);

        let luna = t.cost(&u, "codex/gpt-5.6-luna").unwrap();
        assert!(luna.subscription);
        assert!((luna.usd - 7.0).abs() < 1e-9);

        let spark = t.cost(&u, "codex/gpt-5.3-codex-spark").unwrap();
        assert!(spark.subscription);
        assert_eq!(spark.source, CostSource::Estimated);
        assert!((spark.usd - 15.75).abs() < 1e-9);

        let mini = t.cost(&u, "codex/gpt-5.4-mini").unwrap();
        assert!(mini.subscription);
        assert!((mini.usd - 5.25).abs() < 1e-9);
    }

    /// C-38: the `gpt-realtime` family's headline rates, INCLUDING the audio surcharges — pinned
    /// separately from [`builtin_pins_vendor_verified_headline_rates`] since it's the only built-in
    /// row where `audio_input`/`audio_output` are non-zero. Verified against
    /// <https://developers.openai.com/api/docs/pricing> on 2026-07-06.
    #[test]
    fn builtin_pins_gpt_realtime_audio_surcharges() {
        let t = PricingTable::builtin();
        for model in ["gpt-realtime", "gpt-realtime-2"] {
            let r = t
                .rates_for(model)
                .unwrap_or_else(|| panic!("{model} must be in the builtin table"));
            assert_eq!(
                (r.input, r.output, r.cache_write, r.cache_read, r.reasoning),
                (4.0, 24.0, 4.0, 0.40, 0.0),
                "{model}: text tiers"
            );
            assert_eq!(
                (r.audio_input, r.audio_output),
                (28.0, 40.0),
                "{model}: audio surcharge = audio rate ($32/$64) minus text rate ($4/$24)"
            );
        }
    }

    /// C-38: a realistic mixed text/audio/cached call on the built-in `gpt-realtime` row prices to a
    /// hand-computed dollar figure — proves the surcharge terms actually reach `cost()`'s dot
    /// product (not just that the rate row carries the right numbers).
    #[test]
    fn builtin_gpt_realtime_prices_mixed_audio_usage() {
        let t = PricingTable::builtin();
        let usage = Usage {
            input_tokens: 1_000_000, // fresh text input
            output_tokens: 500_000,  // total output (incl. the audio subset below)
            cache_read_input_tokens: 200_000,
            audio_input_tokens: 100_000, // subset of input_tokens
            audio_output_tokens: 50_000, // subset of output_tokens
            ..Default::default()
        };
        // input 1.0·4.0 + output 0.5·24.0 + cache_read 0.2·0.40
        // + audio_input 0.1·28.0 + audio_output 0.05·40.0
        // = 4.0 + 12.0 + 0.08 + 2.8 + 2.0 = 20.88
        let money = t
            .cost(&usage, "gpt-realtime")
            .expect("gpt-realtime must price");
        assert!((money.usd - 20.88).abs() < 1e-9, "got {}", money.usd);
        assert!(!money.subscription);
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
        // The codex provider resolves bare/legacy specs to `gpt-5.6-sol`; cost must resolve on that
        // canonical id, not just the legacy `gpt-5-codex`. Failing-first: before the GPT-5.6 update,
        // this path still used `gpt-5.5`, so new Codex spend could be misattributed.
        let codex_cost = table
            .cost(&usage, "codex/gpt-5.6-sol")
            .expect("codex/gpt-5.6-sol must price — the resolver emits this canonical id");
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

    /// C-38: the audio surcharge tiers fold through `apply_override`/`RateOverride` exactly like
    /// every other tier — partial (only the overridden field moves) and available on a model the
    /// built-in table already has a row for.
    #[test]
    fn apply_override_folds_audio_surcharges() {
        let mut table = PricingTable::builtin();
        let before = *table.rates_for("gpt-realtime").unwrap();

        table.apply_override(
            "gpt-realtime",
            &RateOverride {
                audio_input: Some(50.0),
                ..Default::default()
            },
        );
        let after = *table.rates_for("gpt-realtime").unwrap();
        assert_eq!(after.audio_input, 50.0);
        // Untouched tiers, including the other audio surcharge, keep their built-in values.
        assert_eq!(after.audio_output, before.audio_output);
        assert_eq!(after.input, before.input);
    }
}
