---
id: C-05
title: Cross-provider pricing & cost model
pillar: Core
status: backlog
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: per-model per-tier rates + `cost(&Usage, model)`; built-in table + `~/.flux/pricing.toml` override; normalize codecs' cache fields
---

# Cross-provider pricing & cost model

## Goal
Add the missing cost layer: per-model price rates and a `cost(&Usage, model)` function, plus normalize every
provider codec to populate cache/reasoning token fields so cost is comparable across all providers. This is
the foundation the reporting surface (C-06) consumes.

## Acceptance
- [x] **pricing table.** A built-in curated table keyed by model id carries per-tier rates (input / output /
      cache-write / cache-read / reasoning), overlaid by an optional user-editable `~/.flux/pricing.toml`
      (missing or partial file falls back to built-ins). Failing-first test `pricing_toml_overrides_builtin`
      (a fixture pricing.toml changes one model's input rate; others keep built-in rates).
- [x] **cost function.** `cost(&Usage, model) -> Money` applies each tier rate to the matching token count.
      Failing-first test `cost_applies_per_tier_rates` (a `Usage` with non-zero cache + reasoning tokens
      computes the expected total; unknown model → `None`/zero, not a panic).
- [x] **codec normalization.** The OpenAI Chat and Responses codecs populate the cache (and reasoning, where
      the wire provides it) token fields instead of leaving them 0. Failing-first tests
      `chat_usage_captures_cached_tokens` and (with C-03) `responses_usage_captures_cache_and_reasoning`
      so the three Messages providers and the two OpenAI paths all report cache tiers.
- [x] **`Usage` carries reasoning tokens** if not already representable (extend `flux_core::Usage` + its
      `accumulate` fold; serde-default so old event logs still decode). Test `usage_accumulate_folds_reasoning`.
- [x] **subscription labelling.** `cost()` marks claude/codex spend as *equivalent metered cost* (a flag/field),
      since it bills against a subscription, not the API. Test `subscription_cost_is_labelled`.
- [x] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- **Done (this pass).** Shipped the cost model + codec normalization.
  - **`Usage` + reasoning** (`crates/flux-core/src/stream.rs`): added `reasoning_tokens: u64`
    (`#[serde(default)]`); it is summed in `accumulate` like output but **excluded** from `total()` /
    `context_tokens()` (reasoning is a subset of output — avoids double-counting). Test
    `usage_accumulate_folds_reasoning`.
  - **Pure cost model** (`crates/flux-core/src/pricing.rs`, new module, L0/IO-free): `Rates` (per-1M-token
    tier prices), `RateOverride` (partial, for the toml overlay), `Money { usd, subscription }`,
    `PricingTable` (`builtin()` + `rates_for` + `cost` + `apply_override` + `set`), and a free
    `is_subscription(spec)`. `cost = Σ tokens_tier·rate_tier / 1e6`; unknown model → `None` (no panic).
    Lookup resolves flux aliases (sonnet/opus/haiku) and strips known `provider/` prefixes (exact match
    first so OpenRouter ids with slashes still resolve). Built-in seeds: Claude opus/sonnet/haiku
    (verified rates via the claude-api skill — $5/$25, $3/$15, $1/$5), GPT-5 / codex, two OpenRouter
    models (`// TODO verify rates` on the non-Claude entries). Tests `cost_applies_per_tier_rates`,
    `subscription_cost_is_labelled`, `apply_override_is_partial`.
  - **IO overlay** (`crates/flux-credentials/src/lib.rs`, L1): `load_pricing_table()` reads
    `~/.flux/pricing.toml` (`[models."<id>"]` partial-rate tables) and folds it onto the built-in table;
    missing/malformed file → built-ins. Mirrors the crate's existing `~/.flux` + `home()` conventions.
    Hermetic test `pricing_toml_overrides_builtin` (temp-dir fixture, no env mutation).
  - **Codec normalization** (`crates/flux-providers/src/openai.rs`): `map_chat_stream` now reads
    `prompt_tokens_details.cached_tokens` → `cache_read_input_tokens` (fresh `input_tokens` =
    `prompt_tokens - cached`); `map_responses_stream` reads `input_tokens_details.cached_tokens` →
    `cache_read_input_tokens` and `output_tokens_details.reasoning_tokens` → `reasoning_tokens`. Both
    leave `cache_creation` at 0 (OpenAI has no cache-write tier). Tests `chat_usage_captures_cached_tokens`,
    `responses_usage_captures_cache_and_reasoning`. The Messages path already populated all four cache
    fields; reasoning is mapped as 0 there (Anthropic folds thinking into output).
  - **Layering:** `cargo test -p flux-codegate` green — the math/table live in L0 `flux-core`, the IO
    overlay in L1 `flux-credentials`; no new cross-layer edge.
  - **Boundary:** only touched the usage *emission* inside the two OpenAI map fns; did not touch
    `import_codex`/JWT, `build_responses_body`, codex auth, `RefreshingToken`, or any aggregation surface.
  - Gate: `cargo build --workspace`, `cargo test -p flux-core -p flux-providers -p flux-credentials`,
    `clippy --workspace --all-targets -D warnings`, `fmt`, `cargo test -p flux-codegate` all green. (The
    pre-existing `flux-flow::skill_docs_in_sync` node-kind drift is unrelated to this story.)

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: a pricing/cost module (home it where it serves L6 reporting + L0 `Usage` without a layering
  violation — likely `flux-core` for the `Usage`-cost math + a small table, config overlay read at L6/runtime),
  `crates/flux-core/src/stream.rs` (`Usage`, `accumulate`), `crates/flux-providers/src/openai.rs` (Chat +
  Responses usage), `crates/flux-providers/src/messages/{mod,wire}.rs` (already cache-aware — keep parity).
- Reuse: `WireUsage` + `From<WireUsage> for Usage` (Messages path is the reference for cache fields);
  `~/.flux/` store conventions from `flux-credentials` for the pricing.toml location + 0600/atomic read.
- Run `cargo test -p flux-codegate` early — the cost module's home must not create a new cross-layer edge.
