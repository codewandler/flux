---
id: C-133
title: Cache-efficiency telemetry + a repeatable A/B harness — measure before fixing
pillar: Core
status: done
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "`flux usage` is already CORRECT (per-call CallUsage, measured 32% over 813 calls) — the gap is the live path: TurnEnded.usage is the last round only, the model trace has no per-round cache split, and there is no repeatable scenario to A/B a fix against"
---

# Cache-efficiency telemetry + a repeatable A/B harness — measure before fixing

## Goal
Make prompt-cache efficiency observable and reproducible, so the rest of the epic can be validated
with numbers instead of impressions. Serves Core: no provider-cost change ships in flux without a
measurement that proves it.

## Acceptance
- [ ] Turn-level cache hit rate is reported as `Σ cache_read / Σ (input + cache_read + cache_creation)`
      over the turn's model calls, **not** the last round's ratio. Failing-first test in
      `crates/flux-core` proving a three-round turn whose rounds hit 90%/60%/20% reports the
      token-weighted turn figure, where today's path reports 20%.
- [ ] `Usage::accumulate` keeps its existing replace semantics for `input_tokens` /
      `cache_*_input_tokens` (the `ctx` figure and `flux-server`'s usage rows depend on it); the
      cumulative cache totals live in a separate accumulator/field. A test pins that `ctx` is
      unchanged by this story.
- [ ] `crates/flux-cli/src/rendering.rs::usage_annotation` renders the turn figure; its existing
      tests are updated and one new test covers the multi-round case.
- [ ] `FLUX_MODEL_TRACE=1` emits, per request: the realized `cache_control` breakpoint count and the
      byte offset/segment index of each, alongside the existing `system_bytes` / `system_segments` /
      `message_bytes` / `tools` fields (`crates/flux-provider/src/lib.rs:601`). On the response side
      the per-round `cache_read` / `cache_creation` / `input` split is emitted.
- [ ] A repeatable scenario under `bench/` runs a fixed multi-round turn against a
      caller-supplied `provider/model` and prints a per-round table (round, prompt tokens,
      cache_read, cache_creation, uncached input, hit %) plus the turn total. Deterministic in what
      it *sends* — same prompt, same tool set, same round count — so two runs are comparable.
- [ ] Harness baseline recorded in `docs/designs/llm-cache-review.md` alongside the whole-history
      baseline already captured there: the scripted turn run against `claude/*`,
      `anthropic/claude-sonnet-5`, and `codex/*`, so per-round shape (not just per-model totals) has
      a recorded starting point.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. `flux_core::CacheEfficiency` (read/write/fresh, `hit_rate`) added beside
  `Usage::accumulate` rather than changing it — occupancy and efficiency now have one accumulator
  each, and `ctx` is untouched.
- Model trace gained `cache_breakpoints` + `cache_breakpoints_in_messages` (the realized layout, not
  the intended one).
- Harness is `bench/cache-ab.sh`, built around the `FLUX_CACHE_TAIL=off` kill switch added in C-134.
  It alternates arm order, because the first naive A/B (47% → 97%) turned out to be a warm-cache
  artifact — reversing the order gave 79% on both arms. That trap is documented in the script.
- **Scope narrowed on evidence.** `flux usage` was already correct (per-call `CallUsage`), so this
  story became "make the LIVE path agree with it" rather than "build the metric". The whole-history
  baseline in the design doc came from running it as-is.

## Notes
- **`flux usage` is already correct — do not "fix" it.** It builds records from per-call `CallUsage`
  events (`crates/flux-cli/src/usage.rs:767-793`), so its `cache_read_share`
  (`usage.rs:1576,1622,2203`) is per-call-weighted, not last-round. The whole-history baseline in
  the design doc came from it. This story brings the *live* path up to the same standard; C-139 and
  C-140 are the display half of the same gap.
- The claude-vs-anthropic question this story must answer with the harness: both providers share
  `AnthropicProfile` and the codec verbatim (`crates/flux-providers/src/spec.rs:157`) and nothing
  disables caching for the OAuth transport. The whole-history data already argues against a large
  gap (claude/* 35% vs codex/* 29%), so the remaining question is per-round *shape* — does claude
  decay faster within a turn, and does a >5-minute gap cold-start it (C-135)?
- Do **not** change `Usage::accumulate`'s replace semantics. `crates/flux-core/src/stream.rs:70-93`
  documents why they exist; `sum_independent` next to it is the summing variant for independent
  calls and may be the right model to follow.
- Existing tests that will need updating: `flux-cli` `usage_annotation_shows_context_output_and_cache_hit_rate`
  (`crates/flux-cli/src/main.rs:2921`).
- Watch for overlap with C-139: both need the live path reading per-call usage. Whichever lands
  first should put the shared piece somewhere the other can reuse — likely a projection next to
  `cost_summary` in `flux-events` (`crates/flux-events/src/store/mod.rs:1270`).
