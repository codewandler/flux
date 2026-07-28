---
id: C-135
title: 1-hour TTL on the stable tools+system prefix — survive interactive pauses
pillar: Core
status: ready
priority: 5
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "no `ttl` field is set anywhere in flux-providers — every breakpoint rides the 5-minute default, so a human reading output for six minutes cold-starts the whole prefix on the next turn; likely the entire claude-vs-anthropic delta for interactive subscription use"
---

# 1-hour TTL on the stable tools+system prefix — survive interactive pauses

## Goal
Keep the stable prefix alive across the gaps that define interactive use, so the next turn reads the
catalog and persona from cache instead of rewriting them. Serves Core: subscription spend and
time-to-first-token on the surface people actually sit in front of.

## Acceptance
- [ ] The breakpoint covering the stable tools+system prefix carries
      `cache_control: {"type":"ephemeral","ttl":"1h"}`. Failing-first test on the built body in
      `crates/flux-providers/src/messages/mod.rs`.
- [ ] The rolling conversation tail breakpoint (C-134) stays on the 5-minute default — it is
      rewritten every round, so the 2× write premium would never pay back. Test asserts the two
      breakpoints carry different TTLs in the same body.
- [ ] TTL selection is a property of the segment layout, not hardcoded per provider: the
      `SystemSegment` layout (or the codec's view of it) decides, and `anthropic`, `claude`,
      `openrouter-anthropic`, and `bedrock` all inherit the same rule. Test covers at least
      `anthropic` and `claude`.
- [ ] `ollama-anthropic` is unaffected (`prompt_caching: false` — ollama ignores `cache_control`;
      see C-35). Test asserts no `ttl` leaks into that body.
- [ ] Live-validated with the C-133 harness: two turns separated by a >5-minute pause against
      `claude/*` show a cache read on the second turn's first round where the baseline shows a cache
      write. Before/after in the design doc.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- (not started)

## Notes
- Economics: 5m write is ~1.25× base input, 1h write is ~2×, reads are ~0.1×. 1h break-even is three
  requests — cleared inside a single multi-round turn, so this is close to free even when the pause
  never happens.
- This is the most likely *actual* claude-vs-anthropic differentiator. The two providers share
  `AnthropicProfile` and the codec exactly; what differs is that subscription claude is the
  interactive surface, where >5-minute human pauses are normal. C-133's baseline should confirm
  this before the story is judged.
- Watch for an interaction with C-134's union budget: TTL is per-breakpoint, so mixing TTLs does not
  change the count, but the tests for both stories touch the same builder — land C-134 first.
