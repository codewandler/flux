---
id: C-172
title: Give the A/B harness a working OpenRouter arm, then measure the unification
pillar: Core
status: done
epic: messages-provider-unification
design: docs/designs/messages-provider-unification.md
note: "cache-ab.sh:57's `openrouter*` glob matches the plain chat path but selects FLUX_CACHE_TAIL — a switch that does nothing on that wire — so both arms run byte-identical bodies while the harness reports 'no difference'. The guard added for unprefixed specs (cache-ab.sh:58-62) exists for exactly this failure and doesn't catch it."
---

# Give the A/B harness a working OpenRouter arm, then measure the unification

## Goal
Make `bench/cache-ab.sh` able to actually A/B the OpenRouter path, then use it to record the
before/after this epic claims. Serves Core: the epic's headline number is unmeasurable until the
harness picks a kill switch that does something on the wire under test.

## Acceptance
- [ ] Failing-first: a test or dry-run assertion that `-m openrouter/anthropic/claude-opus-4.6`
      selects a kill switch that changes the request body. Today it selects `FLUX_CACHE_TAIL` and both
      arms are identical.
- [ ] The `case` at `cache-ab.sh:55-63` is split **more-specific-first** — `openrouter-anthropic/*`
      and `ollama-anthropic/*` before the bare `openrouter*`/`ollama*` globs, which currently swallow
      them. Note this ordering hazard in a comment; it is the same shape of bug as the unprefixed-spec
      one already documented there.
- [ ] After C-169 lands, `openrouter/anthropic/*` rides the Messages wire and so takes
      `FLUX_CACHE_TAIL` correctly — verify that rather than assuming, since the glob now means
      something different than it did when written.
- [ ] Measured before/after recorded in the design doc: hit rate and cost for
      `openrouter/anthropic/*`, against the 0% / $24.86-per-32-days baseline.
- [ ] **Methodology, non-negotiable:** alternate arm order across runs and confirm step counts match
      before believing a pair. The cache is content-addressed and org-scoped, so arm 2 reads arm 1's
      writes — a naive same-prompt A/B is worthless and misled the previous epic twice (a claude pair
      "showed" 47%→97% and inverted to 79%/79% on reversal).
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. The `openrouter*` glob's lie is fixed at the root: C-169 put every OpenRouter
  model on the Messages wire, so `FLUX_CACHE_TAIL` now actually changes the body it selects for.
  Retired `openrouter-anthropic` from the comment, split `ollama-anthropic/*` ahead of the bare
  `ollama*` glob, and documented the ordering hazard for whoever adds the next variant.
- Measurement recorded in the design doc's "Validation — live, 2026-07-28" section: the cold/warm
  pair (0% → 62%, $0.0473 → $0.0194), the 1h TTL tier probe, and the non-Anthropic vendor check.
- Methodology note: the epic's before/after is deliberately **not** an env-flag A/B. The control is
  the recorded 32-day history — 91 calls with *zero* cache writes — because the old path could not
  write a breakpoint at all, so no flag on the new build reproduces it. The warm-cache trap the
  previous epic hit does not apply to a comparison whose control has no writes to read.

## Notes
- The harness's `arm()` function (`cache-ab.sh:65-74`) is generic over the switch name, so only the
  selection `case` needs to change.
- Cost is directly comparable on this provider, unlike the codex arms — OpenRouter is metered and
  reports per-call cost, so the dollar delta is real rather than an estimate.
