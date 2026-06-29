---
id: C-05
title: Cross-provider pricing & cost model
pillar: Core
status: backlog
priority:
design: docs/designs/subscription-providers-and-cost.md
theme: subscription-providers-cost
---

# Cross-provider pricing & cost model

## Goal
Add the missing cost layer: per-model price rates and a `cost(&Usage, model)` function, plus normalize every
provider codec to populate cache/reasoning token fields so cost is comparable across all providers. This is
the foundation the reporting surface (C-06) consumes.

## Acceptance
- [ ] **pricing table.** A built-in curated table keyed by model id carries per-tier rates (input / output /
      cache-write / cache-read / reasoning), overlaid by an optional user-editable `~/.flux/pricing.toml`
      (missing or partial file falls back to built-ins). Failing-first test `pricing_toml_overrides_builtin`
      (a fixture pricing.toml changes one model's input rate; others keep built-in rates).
- [ ] **cost function.** `cost(&Usage, model) -> Money` applies each tier rate to the matching token count.
      Failing-first test `cost_applies_per_tier_rates` (a `Usage` with non-zero cache + reasoning tokens
      computes the expected total; unknown model → `None`/zero, not a panic).
- [ ] **codec normalization.** The OpenAI Chat and Responses codecs populate the cache (and reasoning, where
      the wire provides it) token fields instead of leaving them 0. Failing-first tests
      `chat_usage_captures_cached_tokens` and (with C-03) `responses_usage_captures_cache_and_reasoning`
      so the three Messages providers and the two OpenAI paths all report cache tiers.
- [ ] **`Usage` carries reasoning tokens** if not already representable (extend `flux_core::Usage` + its
      `accumulate` fold; serde-default so old event logs still decode). Test `usage_accumulate_folds_reasoning`.
- [ ] **subscription labelling.** `cost()` marks claude/codex spend as *equivalent metered cost* (a flag/field),
      since it bills against a subscription, not the API. Test `subscription_cost_is_labelled`.
- [ ] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- (not started)

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: a pricing/cost module (home it where it serves L6 reporting + L0 `Usage` without a layering
  violation — likely `flux-core` for the `Usage`-cost math + a small table, config overlay read at L6/runtime),
  `crates/flux-core/src/stream.rs` (`Usage`, `accumulate`), `crates/flux-providers/src/openai.rs` (Chat +
  Responses usage), `crates/flux-providers/src/messages/{mod,wire}.rs` (already cache-aware — keep parity).
- Reuse: `WireUsage` + `From<WireUsage> for Usage` (Messages path is the reference for cache fields);
  `~/.flux/` store conventions from `flux-credentials` for the pricing.toml location + 0600/atomic read.
- Run `cargo test -p flux-codegate` early — the cost module's home must not create a new cross-layer edge.
