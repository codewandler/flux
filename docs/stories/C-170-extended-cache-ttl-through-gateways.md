---
id: C-170
title: Verify and enable the one-hour cache TTL through the gateways
pillar: Core
status: done
epic: messages-provider-unification
design: docs/designs/messages-provider-unification.md
note: "`OpenRouterProfile.extended_cache_ttl` is false, commented 'Unverified through the OpenRouter proxy' (openrouter.rs:53-55) — but OpenRouter documents a one-hour `ttl` and the four-breakpoint limit, the same contract AnthropicProfile uses. Verify live, then enable, so the stable prefix survives interactive pauses on every transport, not just Anthropic-direct."
---

# Verify and enable the one-hour cache TTL through the gateways

## Goal
Give gateway-served Anthropic models the same 1h stable-prefix TTL that Anthropic-direct gets, so a
model caches maximally regardless of how it is reached. Serves Core: C-135 established the 1h prefix
as the fix for interactive pauses defeating the 5-minute default; today only one transport benefits.

## Acceptance
- [ ] Live verification first, implementation second: a long-system-prompt request through OpenRouter
      carrying `cache_control: {"type":"ephemeral","ttl":"1h"}` either succeeds or 4xxes. Record which,
      with the response body, in the design doc.
- [ ] If accepted, `OpenRouterProfile.extended_cache_ttl` becomes `true` for anthropic-served slugs
      only (it already gates `prompt_caching` on `model.starts_with("anthropic/")`), and the stale
      "Unverified" comment is replaced with the verification date.
- [ ] Same question answered for `BedrockProfile` (`bedrock.rs:72`) — verify, then enable or record
      why not.
- [ ] If a wire rejects it, the quirk stays off and the rejection is documented. Do not enable on
      inference: this is the exact failure the `4a76315` review pass caught, where the 1h TTL rode the
      `prompt_caching` quirk and so reached Bedrock and OpenRouter unverified, risking a 4xx on every
      long-system-prompt request.
- [ ] `cache_layout_contract` (`messages/mod.rs:1140`) grows a gateway row so the realized breakpoint
      layout is pinned per profile rather than only for Anthropic-direct — including that the tail
      breakpoint stays on the 5m default while the stable prefix takes 1h.
- [ ] `only_the_anthropic_direct_profile_asks_for_the_extended_ttl` (`messages/mod.rs:1096`) is
      renamed/retargeted to match whatever the verified truth turns out to be.
- [ ] Cost accounting stays honest: Anthropic bills a 1h write at 2x base input vs 1.25x for 5m, and
      `Rates::cache_write_1h` already carries that surcharge — confirm it applies on the gateway rows
      too, or the win will be over-reported exactly as it was in the last epic.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. Verified BEFORE enabling, as the acceptance demanded: a direct probe of
  OpenRouter's `/api/v1/messages` with `ttl: "1h"` returned 200 and landed the write in
  `cache_creation.ephemeral_1h_input_tokens: 7725`, where the plain ephemeral form landed 7725 in
  the 5m tier for the same prompt. Held even with OpenRouter routing to Amazon Bedrock upstream.
- `OpenRouterProfile.extended_cache_ttl` is now `anthropic_served` (same gate as `prompt_caching`),
  and the stale "Unverified through the OpenRouter proxy" comment is replaced with the evidence.
- `cache_layout_contract` grew an `openrouter` row driven by the real profile, so the realized
  layout is pinned per profile rather than only for Anthropic-direct.
  `only_the_anthropic_direct_profile_asks_for_the_extended_ttl` renamed to
  `the_extended_ttl_is_asked_for_only_where_it_is_verified` and retargeted.
- Cost honesty: the 1h surcharge is driven by the reported per-TTL split rather than by provider, so
  it reaches gateway rows automatically — pinned by `the_one_hour_write_surcharge_reaches_gateway_rows`
  and the stale pricing comment corrected.
- **Bedrock-direct deliberately NOT enabled** — no credentialed probe was run, and enabling on
  inference from the OpenRouter result is exactly the failure `4a76315` caught. Left as follow-up.

## Notes
- OpenRouter's documented contract: explicit per-block `cache_control` breakpoints, limit of four,
  `ttl: "1h"` supported, minimum cacheable prefix 1024–4096 tokens depending on model generation.
- The minimum-prefix gate is an open question inherited from the last epic: `segmented_system_field`
  has no size gate at all (`CACHE_MIN_CHARS` guards only the unsegmented path), so a breakpoint below
  the model's minimum silently does not cache. Worth measuring here rather than assuming.
