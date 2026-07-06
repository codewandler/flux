---
id: C-35
title: Prompt caching on the openrouter-anthropic wire — stop paying the gather round uncached
pillar: Core
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "I-03 measured it: gather-shaped turns double uncached-in (20k→41k, corpus spend +35%) because openrouter.rs pins prompt_caching: false; anthropic/bedrock profiles cache the same prefix"
---

# Prompt caching on the openrouter-anthropic wire — stop paying the gather round uncached

## Goal
The multi-pass loop's gather rounds re-send the prompt prefix; on cache-serving wires that re-read
is ~90% discounted, but `OpenRouterProfile::quirks_for` pins `prompt_caching: false`
(`crates/flux-providers/src/openrouter.rs`), so every gather round bills fully uncached. I-03
measured the cost on the 5-prompt TTFF corpus: grep-count uncached-in 20.1k → 41.1k, corpus spend
+35% (design doc "I-03 measurement results"). OpenRouter passes Anthropic `cache_control` through
to Anthropic-served models; enable it (guarded by whatever made it false — find out why before
flipping) so the phased loop's economics match its UX win.

## Acceptance
- [x] The reason `prompt_caching: false` was chosen for openrouter is dug out (git blame / provider
      docs) and recorded here; if it was a real constraint (e.g. non-Anthropic serving providers
      rejecting `cache_control` despite `require_parameters`), the flip is scoped to models/routes
      where caching is honored rather than blanket.
- [x] `cache_control` breakpoints ride the `openrouter-anthropic` codec (failing-first test on the
      built body, mirroring `anthropic.rs`'s `cache_control` test).
- [x] A live probe (1–2 cheap calls, user-approved spend) shows `cache_read_input_tokens > 0` on a
      second call sharing the prefix, surfaced by `flux usage` cache-read%.
- [x] C-30 pricing still prices cached reads correctly for openrouter passthrough keys (no `$?
      (unpriced)` regressions); gate green.

## Progress
- 2026-07-06 acceptance-1 research (pre-implementation): the `false` came in with A-03's
  cache-stable layout (`d9e7519`) as a *deliberate conservative default*, documented in
  `OpenRouterProfile`'s own doc comment: "Conservative across the gateway's many non-Claude
  models: prompt caching and the Anthropic `output_config.effort` are off (not all upstreams
  accept them) … Per-model refinements (e.g. caching on for Claude slugs) belong in `quirks_for`,
  which currently ignores the model." So it WAS a real constraint (non-Anthropic upstreams of the
  same gateway may reject/ignore `cache_control`), and the planned fix is the pre-designed seam:
  key `quirks_for` on the model and enable `prompt_caching` for `anthropic/…` slugs only —
  scoped, not blanket. `ollama-anthropic` stays `false` (ollama ignores `cache_control`).
- 2026-07-06 implemented (`crates/flux-providers/src/openrouter.rs`): `OpenRouterProfile::quirks_for`
  now keys on the model — `prompt_caching: model.starts_with("anthropic/")` (vendor-prefix match,
  not substring; a slug that merely mentions "claude" stays conservative). Doc comment rewritten to
  record the C-35 rationale. Failing-first tests (both confirmed FAILED pre-fix, green post-fix):
  `openrouter::tests::profile_enables_prompt_caching_for_anthropic_slugs_only` (+ negative cases
  `z-ai/glm-4.6`, `someone/claude-clone`) and
  `openrouter::tests::codec_body_carries_cache_control_for_anthropic_slugs` (mirrors anthropic.rs's
  codec test: `body["system"][0]["cache_control"]["type"] == "ephemeral"` + `require_parameters`
  still rides). Existing conservative tests untouched and green.
- 2026-07-06 live probe (2 calls, fresh HOME, `-m openrouter-anthropic/anthropic/claude-sonnet-4.6`):
  call 1 `cache write 27.2k · $0.1033`; call 2 `cache 27.2k (99% hit) · $0.0093` — **11× cheaper**.
  `flux usage` reports `cache-read 99%` on the second turn and prices it (no `$? (unpriced)`
  marker) → the C-30 pricing acceptance holds. Package gate green: `cargo test -p flux-providers`
  (102 passed), clippy `-D warnings` clean, fmt clean.

## Notes
- Born from I-03's honest caveat: `cache-read 0%` on both legs of the cutover measurement — the
  +35% spend delta overstates the multi-pass cost on cache-serving wires.
- Quirk seam: `MessagesQuirks.prompt_caching` (`crates/flux-providers/src/messages/quirks.rs`);
  profiles: anthropic/bedrock `true`, openrouter/ollama `false`.
- OpenRouter attaches provider routing via `extra_body.provider.require_parameters = true` already;
  cache_control interplay with non-Anthropic providers of the same model is exactly the thing to
  verify before flipping the default.
