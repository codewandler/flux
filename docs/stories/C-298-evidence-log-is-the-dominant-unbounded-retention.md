---
id: C-298
title: "The evidence log is the largest unbounded structure in a long-lived runtime, and it has no trim API at all"
pillar: Core
status: in-progress
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

- [x] A failing-first demonstration of unbounded growth: a long-lived executor over N dispatches
      retains O(N) observations with no ceiling reachable by any caller. Measure it; do not assert it.
      → `an_unconfigured_evidence_log_retains_one_payload_per_dispatch`
      (`crates/flux-runtime/src/lib.rs`). Measured at the merge base: 32 dispatches → 96 observations
      / 21 680 payload bytes; 128 → 384 / 86 605. **3.99× for 4× the dispatches.** "No ceiling
      reachable by any caller" is proven structurally: the named test does not compile at the base,
      because neither `ResourceLimits` nor `EvidenceLog` had any such API (5 × `E0599`).
- [x] ⚠ **The obvious fix is forbidden by C-290's own acceptance, and this story must not quietly
      adopt it.** Dropping the oldest observations to fit a ceiling is a *silent truncation of an audit
      record* — the evidence log drives reactions, `metrics()`, and the audit trail. Whatever lands
      must either preserve the record elsewhere (spill to the event store) or summarise it in a way a
      reader can tell apart from the real thing. A `max_observations` knob that evicts is the wrong
      answer, and it is the answer someone will reach for.
      → **No observation is ever dropped.** The ceiling elides the oldest `data` *payloads* and
      replaces each with a self-describing marker (`evidence_payload_elided`, carrying
      `original_bytes` / `ceiling_bytes` / `knob`). Count, order, `kind` and `phase` are provably
      unchanged (`a_bound_ceiling_changes_no_index_no_count_no_kind_and_no_phase`). Both halves of the
      escape hatch hold: the marker is legible as a summary
      (`an_elided_payload_is_distinguishable_from_an_empty_one`), *and* C-14's per-turn flush has
      already spilled a completed turn's payloads verbatim to the event store.
- [x] State which consumers actually need the *whole* history versus a rolling window. Reactions and
      `metrics()` are the two named readers; if either only needs recent observations, that changes
      the shape from "bound it" to "separate the two".
      → Done — the table under **Consumer analysis** below. The story's hunch was right about
      reactions (rolling / per-observation) and **wrong about `metrics()`**, which is cumulative; and
      the analysis turned up a third, unnamed reader that is the real constraint: `flux-flow`'s
      durable flush addresses the log by *absolute index*.
- [x] Whatever ceiling exists is reachable from `ResourceLimits` (C-290 built it) and from
      `flux-config`'s `[limits]` table, so a host configures resource bounds in one place rather than
      two.
      → `ResourceLimits::with_max_evidence_payload_bytes` (`crates/flux-runtime/src/limits.rs`) and
      `[limits] max_evidence_payload_bytes` (`crates/flux-config/src/lib.rs`), routed through the
      existing `ResourceLimits::from_config`. Installed on the shared log by the one door a host
      already uses, `Executor::with_resource_limits`.
- [x] Exceeding it is observable and actionable, never silent — the same bar C-290 was held to.
      → Three ways: the in-band per-observation marker (which travels into the durable event-store
      mirror, so an *offline* auditor sees it too); the counters `EvidenceLog::elided_payloads` /
      `elided_payload_bytes` / `Executor::retained_evidence_payload_bytes`; and
      `EvidenceLog::compaction_notice` / `Executor::evidence_compaction_notice`, which names the knob
      and says where the full payloads still are — C-290's `ConcurrencyRefusal::message` bar.
- [ ] Full gate green.
      → Everything green **except** `flux-codegate`'s `plugin_builds_exclude_host_only_crates`, which
      needs `plugins/Cargo.lock` synced to `codewandler-flux-evidence 1.1.0`. That file is a
      coordinator-owned lockfile; see **Progress**.

## Notes

- ⚠ **`flux-evidence` is on the independently-versioned 1.x protocol line**, where SemVer is over the
  wire. Adding a trim/spill API is a public-surface change and obliges a version decision that
  `scripts/check-crate-versions.sh` **will** catch — unlike the workspace-versioned crates, this one
  is in its scope. Run it before pushing.
- Being outside C-290's `areas` is exactly why this was deferred rather than fudged, and that was the
  right call — it is a design question about audit integrity, not a knob.
- Related: [C-290](C-290-runtime-resource-limits.md) built `ResourceLimits` and bounded the op cache;
  its Progress records what it deliberately did not do.

## Consumer analysis

Acceptance item 3, grounded in the code rather than asserted. This is what decided the shape.

| Consumer | What it reads | Needs |
|---|---|---|
| `flux-flow::flush_observations` — the C-14 durable event-store spill (`engine.rs:1342`) | `all()[watermark..]`, where the watermark is a plain `AtomicUsize` | **Absolute index stability.** Compacting the front makes `start.min(all.len())` clamp, silently stopping the audit spill and then re-flushing the wrong entries. This reader was *not* named in the story and is the hardest constraint. |
| `flux-tools::MetricsOp` — `metrics()` (`evidence.rs:184`) | `by_kind("tool_call"/"tool_error"/"turn.iteration").count()` | **Cumulative counts.** Not a rolling window, contrary to the story's guess. A model branches on these to decide whether to retry or stop; a count that shrinks is a corrupted progress signal. `flux-tools` is fenced by A-103, so it could not have been adapted here anyway. |
| `flux-flow::evidence_kind_count` (`engine.rs:1373`) | the same counts, snapshotted per turn and diffed | **Monotonic counts.** Feeds the `turn.iteration` baseline next to max-iterations termination — a declared safety-invariant area. A count that can shrink underflows the diff. |
| `flux-cognition::recorded_usage` (`lib.rs:57`) + `consult` | the **payloads** of every `cognition.usage` / `consult.usage` observation, summed | Whole-history *payloads*. The one reader payload elision can degrade — see Deviations/risk note below. |
| Reactions — `DestructiveEscalation` (`lib.rs:114`) | one observation's `kind`, at dispatch | Never the history. Elision cannot change its verdict. |
| Reactions — `GroupSurfacer` / `resolve_active_groups` (`lib.rs:~470`) | the **current turn's** signals, in a throwaway `EvidenceLog` | Rolling window only — already documented in-crate as "evaluated against current signals (not the append-only historical log)". |
| `evidence` op, `/evidence` render | recent history, for a model or a human | Rolling window. |

**What this rules out.** Four of seven readers depend on the log's *shape* — its length, its indices,
its per-kind counts — rather than on its payloads. So "bound it by dropping entries" is not merely
distasteful (C-290's audit-integrity argument); it is a correctness break in three separate places,
one of them a fenced crate and one of them adjacent to turn termination. The story's framing —
"separate the two" — is right, but the split is not history-vs-window. It is **shape vs payload**:
the shape is what every structural reader needs and is cheap; the payload is what is
arbitrary-sized, unbounded, and needed by almost nobody after the fact.

## Progress

Landed on `impl/C-298`. `EvidenceLog` gained an opt-in retained-*payload* ceiling
(`set_max_payload_bytes`, default off per C-290's rule) that elides oldest-payload-first behind a
self-describing marker and never drops an observation. Reachable from
`ResourceLimits::with_max_evidence_payload_bytes` and `[limits] max_evidence_payload_bytes`, installed
on the shared log by `Executor::with_resource_limits`.

**What is deliberately NOT delivered: an entry-count ceiling.** The consumer table above is why — an
entry ceiling means dropping entries, and three readers forbid that. A long-lived runtime therefore
still retains a fixed-size header (`kind` + `phase` + marker) per observation, so entry count remains
O(N); what it no longer retains is unbounded *payload*, which was the dominant and the only unbounded
term. Delivering an entry ceiling honestly needs (a) the C-14 watermark re-expressed in absolute
sequence coordinates that survive compaction — a `flux-flow` change — and (b) cumulative per-kind
counters that `metrics()` reads instead of `by_kind().count()` — a `flux-tools` change, fenced by
A-103. That is a follow-up story, not a knob.

**One remaining gate item, and it needs the coordinator.** `flux-evidence` is on the independently
versioned 1.x protocol line, so changing its source obliges a version bump — proven both ways:
without the bump `scripts/check-crate-versions.sh` fails
(`codewandler-flux-evidence changed since v0.40.0 but is still 1.0.0`); with it,
`plugins/Cargo.lock` goes stale and `flux-codegate`'s `plugin_builds_exclude_host_only_crates` fails
on `cannot update the lock file … because --locked was passed`. Both lockfiles are coordinator-owned,
so this branch bumps the manifest to **1.1.0** and leaves them untouched. To finish:
`cargo metadata --manifest-path plugins/Cargo.toml >/dev/null` and commit `plugins/Cargo.lock` plus
the root `Cargo.lock` one-line version move.

**A note on whether 1.1.0 is the right call.** `crates/flux-evidence/Cargo.toml` says the protocol
version "moves only when the wire vocabulary changes", and C-298 does **not** change it —
`the_ceiling_is_not_part_of_the_serialized_record` proves the serialized shape is still exactly
`{"observations": …}`, and the ceiling is host-installed runtime state marked `#[serde(skip)]`. The
bump is forced by `check-crate-versions.sh` being *content*-based, not wire-based. So this is
additive-MINOR on a wire-compatible change: a plugin built against 1.0 keeps resolving, and **no
plugin pack release is owed**. If the coordinator would rather the protocol line not move for a
wire-compatible edit, that is a change to the script's rule, not to this diff.
