---
id: C-462
title: "The compaction threshold is a flat character count, blind to the model's context window"
pillar: Core
status: done
priority: 6
areas: [flux-agent, flux-flow]
note: "decision: keep 48,000 chars as an intentional fixed history budget; whole-request headroom is not derivable from a nominal model window, and existing overrides remain the honest tuning seam"
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

- [x] A decision, with reasoning recorded: scale the threshold to the model's context window, or keep a
      flat budget and say why.
- [ ] If scaled: a failing-first test pinning that two models with different context windows get
      different effective thresholds, and that an explicit `FLUX_COMPACT_CHARS` / per-agent override
      still wins over the derived value.
- [x] If kept flat: the reasoning lives at `DEFAULT_COMPACT_THRESHOLD_CHARS`, next to the C-443
      justification already there.
- [x] ⚠ Whatever is decided, the `0`-disables contract and the existing override precedence
      (per-agent > env > default) are unchanged.

## Decision

Keep `DEFAULT_COMPACT_THRESHOLD_CHARS` fixed at 48,000 characters. It is an absolute budget for the
one request component that grows with a session — retained conversation history — rather than an
estimate of total model headroom. Items that are roughly fixed within a session (harness and stage
instructions, project context, skills, and tool schemas) can already consume very different amounts
of the same nominal window. Scaling only history from a model's advertised maximum would therefore
produce a precise-looking threshold without knowing the request's remaining capacity.

Flux also has no context-window metadata in its provider or runtime contracts. A model-name table
would be stale for evolving aliases and undefined for unknown, local, routed, or custom models. On a
small model, stale high metadata would weaken the safety guard; on a large one, a large fraction would
increase repeated prompt cost and delay a deliberately lossy summary. A real headroom-aware feature
would need to size the complete provider-framed request against authoritative per-request capacity.
That is a different design, not a constant derived from a model id.

The existing override is the honest seam for deployments that do know their model and workload:
per-agent `compact_threshold_chars` > `FLUX_COMPACT_CHARS` > the fixed default, with `0` continuing to
disable compaction. The full decision is recorded in
[fixed compaction history budget](../designs/fixed-compaction-history-budget.md).

## Progress

- Filed 2026-08-02, spun out of C-443 at the coordinator's request.
- 2026-08-03: confirmed no provider/runtime contract carries model context-window metadata; exact
  token-counter seams were separately retired by C-469.
- 2026-08-03: read-only evidence sweep of 112,724 events / 1,474 streams found 5,095 `call_usage`
  rows across 816 streams. Complete prompt usage was 14,353 tokens at p50, 96,663 at p95, and
  672,150 at the maximum — evidence that whole-request size varies independently of retained message
  history. The same store had 1,133 message-bearing streams, only seven with 20+ messages, and
  multi-turn histories averaged 5,474 serialized characters, so it still does not justify retuning
  the 48,000 value for heavy interactive use.
- 2026-08-03: a failing-first website contract rejected the former “open question” wording. Code,
  design, and user docs now record the fixed-history-budget decision; behavior, `0`, and precedence
  are unchanged.

## Notes

- **Evidence boundary:** C-443's store is 85% two-message one-shot sessions with a mean conversation
  9% of the threshold, and the expanded sweep still has only seven 20+ message sessions. It supports
  keeping a safe default; it does not claim 48,000 is optimal for every heavy interactive workload.
- **Metadata answer:** Flux does not know a model's context window. A derived threshold would need a
  per-model table or provider-reported value, and inventing one badly is worse than the flat budget.
- ✅ [C-469](C-469-tokencounter-has-no-production-implementor.md) resolved the adjacent token-count
  ambiguity: Flux deliberately uses deterministic estimates for Flux-Lang context slicing and has no
  provider-owned exact-count capability. C-462 therefore keeps the trigger in its native character
  units; do not recreate an exact-token extension point merely to convert those units.
- ✅ [C-466](C-466-compact-threshold-default-drifts.md) removed the CLI's numeric and prose copies;
  the intentional default now has one code owner and cannot drift across surfaces.
- Precedence to preserve: per-agent `settings.compact_threshold_chars` > `FLUX_COMPACT_CHARS` >
  default (`crates/flux-app/src/app.rs`, `crates/flux-cli/src/execution.rs`).
- Related: [C-441](C-441-context-management-doc.md) documents the threshold; if this story changes it,
  that page changes with it.
- ⚠ Renumbered C-444 → C-462 at integration: C-444 was already taken by the SDK secure-defaults story, filed in a parallel epic while this agent was running.
