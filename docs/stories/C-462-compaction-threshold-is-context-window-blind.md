---
id: C-462
title: "The compaction threshold is a flat character count, blind to the model's context window"
pillar: Core
status: ready
priority: 6
areas: [flux-agent, flux-flow]
note: "spun out of C-443: 48,000 chars ≈ 12k tokens is applied uniformly to a 200k and a 1M-token model alike. C-443 deliberately did not act on it — changing the default needs its own evidence"
---

# One threshold for every context window

## Goal

Decide whether the compaction trigger should scale with the model's context window instead of being a
flat character count, and either re-scale it or record why a flat budget is right.

## The finding

[C-443](C-443-zero-compacted-rows.md) established that compaction works, records durably, and is
essentially never reached: 1 of 1,126 sessions in a 112,114-event store ever crossed the threshold.
While establishing that, it surfaced a separate question it deliberately declined to answer.

`DEFAULT_COMPACT_THRESHOLD_CHARS = 48_000` (`crates/flux-agent/src/lib.rs`) is a **flat character
count**, applied identically regardless of which model the session is running. 48,000 characters is
roughly 12k tokens. That is:

- ~6% of a 200k-token context window
- ~1.2% of a 1M-token window
- but a substantial fraction of a small local model's window

So the same trigger that is conservative-to-the-point-of-irrelevant on a frontier model could be
roughly right on a small one. The threshold's *stated purpose* is to stop a long-lived agent growing
unbounded "until the provider's context window errors" (A-22) — a purpose defined in terms of a window
whose size the threshold does not consult.

Compaction is not free: it spends a provider call and permanently replaces detail with a summary. On a
1M-token model, firing at 1.2% of the window pays that cost for no memory pressure at all.

## Acceptance

- [ ] A decision, with reasoning recorded: scale the threshold to the model's context window, or keep a
      flat budget and say why.
- [ ] If scaled: a failing-first test pinning that two models with different context windows get
      different effective thresholds, and that an explicit `FLUX_COMPACT_CHARS` / per-agent override
      still wins over the derived value.
- [ ] If kept flat: the reasoning lives at `DEFAULT_COMPACT_THRESHOLD_CHARS`, next to the C-443
      justification already there.
- [ ] ⚠ Whatever is decided, the `0`-disables contract and the existing override precedence
      (per-agent > env > default) are unchanged.

## Progress

- Filed 2026-08-02, spun out of C-443 at the coordinator's request. Not yet investigated.

## Notes

- ⚠ **This needs evidence C-443 did not have.** C-443's store is 85% two-message one-shot sessions with
  a mean conversation 9% of the threshold, so it cannot say what a heavy interactive session does. A
  re-scaling decision made against that store would be guessing. What would settle it: a store from
  sustained interactive use, or the token accounting already recorded in `call_usage` events.
- Does flux know a model's context window at all? If not, that is the first sub-question — a derived
  threshold needs a per-model window table or a provider-reported value, and inventing one badly is
  worse than the flat budget.
- ⚠ **Blocked in practice on [C-469](C-469-tokencounter-has-no-production-implementor.md).** The
  adjacent question now has an answer: flux does not know a request's *token count* either.
  `TokenCounter` (`crates/flux-provider/src/lib.rs:210`) has no production implementor, and the one
  call site passes `None`, so every budget in the tree runs on a 4-chars-per-token estimate. A scaling
  decision made on top of an estimate of an estimate is not evidence. Do C-469 first, or scale on
  characters deliberately and say so.
- ⚠ Before re-scaling, note that the number lives in **three** places on the CLI path —
  [C-466](C-466-compact-threshold-default-drifts.md) consolidates them. Changing the constant today does
  not change the CLI's default.
- Precedence to preserve: per-agent `settings.compact_threshold_chars` > `FLUX_COMPACT_CHARS` >
  default (`crates/flux-app/src/app.rs`, `crates/flux-cli/src/execution.rs`).
- Related: [C-441](C-441-context-management-doc.md) documents the threshold; if this story changes it,
  that page changes with it.
- ⚠ Renumbered C-444 → C-462 at integration: C-444 was already taken by the SDK secure-defaults story, filed in a parallel epic while this agent was running.
