---
id: C-35
title: Prompt caching on the openrouter-anthropic wire — stop paying the gather round uncached
pillar: Core
status: backlog
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
- [ ] The reason `prompt_caching: false` was chosen for openrouter is dug out (git blame / provider
      docs) and recorded here; if it was a real constraint (e.g. non-Anthropic serving providers
      rejecting `cache_control` despite `require_parameters`), the flip is scoped to models/routes
      where caching is honored rather than blanket.
- [ ] `cache_control` breakpoints ride the `openrouter-anthropic` codec (failing-first test on the
      built body, mirroring `anthropic.rs`'s `cache_control` test).
- [ ] A live probe (1–2 cheap calls, user-approved spend) shows `cache_read_input_tokens > 0` on a
      second call sharing the prefix, surfaced by `flux usage` cache-read%.
- [ ] C-30 pricing still prices cached reads correctly for openrouter passthrough keys (no `$?
      (unpriced)` regressions); gate green.

## Progress
- (not started)

## Notes
- Born from I-03's honest caveat: `cache-read 0%` on both legs of the cutover measurement — the
  +35% spend delta overstates the multi-pass cost on cache-serving wires.
- Quirk seam: `MessagesQuirks.prompt_caching` (`crates/flux-providers/src/messages/quirks.rs`);
  profiles: anthropic/bedrock `true`, openrouter/ollama `false`.
- OpenRouter attaches provider routing via `extra_body.provider.require_parameters = true` already;
  cache_control interplay with non-Anthropic providers of the same model is exactly the thing to
  verify before flipping the default.
