---
id: C-298
title: "The evidence log is the largest unbounded structure in a long-lived runtime, and it has no trim API at all"
pillar: Core
status: ready
priority: 5
areas: [flux-evidence, flux-runtime]
note: "named by C-290's review as the uncomfortable half: C-290 bounded the op cache honestly, but the DOMINANT leak was outside its areas and is now the only one left unaddressed"
---

# The evidence log is the largest unbounded retention in a long-lived runtime

## Goal

C-290 let a host bound what a runtime *uses* rather than only what it *spends*, and narrowed the
memory half honestly: it bounded the executor's op cache in bytes (previously capped at 512 *entries*
and unbounded in bytes) and declined two other structures with reasons. Its review agreed the
narrowing was honest — and then said the uncomfortable part plainly:

> the evidence log is the **largest** unbounded structure of the three and it is the one that got
> deferred … it leaves the dominant leak untouched with no follow-up story in the tree.

This is that story. `flux_evidence::EvidenceLog` is a bare `Vec<Observation>`
(`crates/flux-evidence/src/lib.rs:115-134`) with **no `clear`, no `retain`, no trim API of any kind**,
never reset per turn. Every long-lived `Executor` grows it for the process lifetime — one observation
per dispatch, plus several per gated call.

## Acceptance

- [ ] A failing-first demonstration of unbounded growth: a long-lived executor over N dispatches
      retains O(N) observations with no ceiling reachable by any caller. Measure it; do not assert it.
- [ ] ⚠ **The obvious fix is forbidden by C-290's own acceptance, and this story must not quietly
      adopt it.** Dropping the oldest observations to fit a ceiling is a *silent truncation of an audit
      record* — the evidence log drives reactions, `metrics()`, and the audit trail. Whatever lands
      must either preserve the record elsewhere (spill to the event store) or summarise it in a way a
      reader can tell apart from the real thing. A `max_observations` knob that evicts is the wrong
      answer, and it is the answer someone will reach for.
- [ ] State which consumers actually need the *whole* history versus a rolling window. Reactions and
      `metrics()` are the two named readers; if either only needs recent observations, that changes
      the shape from "bound it" to "separate the two".
- [ ] Whatever ceiling exists is reachable from `ResourceLimits` (C-290 built it) and from
      `flux-config`'s `[limits]` table, so a host configures resource bounds in one place rather than
      two.
- [ ] Exceeding it is observable and actionable, never silent — the same bar C-290 was held to.
- [ ] Full gate green.

## Notes

- ⚠ **`flux-evidence` is on the independently-versioned 1.x protocol line**, where SemVer is over the
  wire. Adding a trim/spill API is a public-surface change and obliges a version decision that
  `scripts/check-crate-versions.sh` **will** catch — unlike the workspace-versioned crates, this one
  is in its scope. Run it before pushing.
- Being outside C-290's `areas` is exactly why this was deferred rather than fudged, and that was the
  right call — it is a design question about audit integrity, not a knob.
- Related: [C-290](C-290-runtime-resource-limits.md) built `ResourceLimits` and bounded the op cache;
  its Progress records what it deliberately did not do.
