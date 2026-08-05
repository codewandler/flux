---
id: C-518
title: "Usage Observatory — animated cross-harness, provider, and model replay in the TUI (epic)"
pillar: Core
status: backlog
epic: usage-observatory
note: "EPIC — turn the existing cross-harness usage import and per-call Flux events into a truthful 4h/1d/7d time-lapse: replay activity, compare harness/provider/model token and cost totals, and keep estimated or unattributed data visibly honest"
---

# Usage Observatory — animated cross-harness, provider, and model replay in the TUI (epic)

## Goal

Let an operator open a polished TUI observatory, choose a historical window such as 4 hours, 1 day,
7 days, or a custom range, and replay usage as a deterministic time-lapse across Flux, Codex, Claude
Code, and opencode. While the cursor advances, the same screen must make total and cumulative tokens,
calls, sessions, cache use, and cost comparable by harness, provider, model, and the
harness → provider → model hierarchy.

This is not a second accounting system. `flux usage` already discovers all four harnesses and builds
records with harness, session, model, timestamps, token tiers, cost, and explicit cost status
(`crates/flux-cli/src/usage.rs:141-176,221-280`). Flux-native history already has the stronger
per-call source: `EventKind::CallUsage { model, usage }`, with `TurnEnded.usage` retained as a
coarser compatibility total (`crates/flux-events/src/kind.rs:92-112`). The epic extracts one shared,
read-only usage timeline/projection from those proven inputs and gives it a replay clock and TUI;
it must not copy the private CLI folds into `flux-tui` or sum call and turn totals together.

## Experience

The primary wide layout is an observability cockpit, not an animated table:

```text
┌ usage observatory · 1d · 10× · 14:32 / 24:00 ── ▶ ──────────────┐
│ tokens 12.4M  │ cost $42.18  │ calls 5,299  │ sessions 318       │
├──────────────────────────── activity flow ───────────────────────┤
│ Flux ────────●──── OpenRouter ────────────────▶ openai/gpt-5.5   │
│ Codex ──────────●── OpenAI ───────────────────▶ gpt-5.5          │
│ Claude Code ─●──── Anthropic ─────────────────▶ claude-sonnet…   │
├──────────────────────────── timeline ─────────────────────────────┤
│ tokens ▁▂▆█▅▃▂▇██▆▃   cost ▁▁▂▄▃▂▁▅█▆▃▂   cursor 14:32          │
├──────────────────────┬────────────────────────────────────────────┤
│ group: harness       │ name          calls   tokens    cost  share│
│ metric: cost         │ Flux          2.1k     7.2M   $24.1    57%│
│ compare: previous 1d │ Codex         1.4k     3.8M   $11.6    27%│
└──────────────────────┴────────────────────────────────────────────┘
```

A pulse represents one usage-bearing call when the source has call-level time, or a labelled
coalesced group (`×N`) under load. Position communicates the harness → provider → model route; size
or density communicates token volume; color plus glyph/label communicates provider and cost status.
The animation is explanatory decoration over exact totals: pausing or disabling motion leaves every
comparison usable.

Playback controls cover play/pause, restart, forward/backward seek, 0.5× through 100×, and a `fit`
mode that compresses the selected range into a short replay. Analysis controls change window,
metric, grouping, sort, filter, and previous-period comparison. A focused bucket or pulse can be
inspected without exposing prompts or assistant text.

## Acceptance

- [ ] A shared, read-only timeline model feeds both the existing `flux usage` accounting semantics and
      the new TUI. It covers Flux, Codex, Claude Code, and opencode through the existing
      `HarnessKind` discovery contract (`crates/flux-capabilities/src/harness/mod.rs:75-129`) and does
      not introduce a second set of parsers or a dependency from `flux-tui` onto `flux-cli`.
- [ ] The normalization contract carries source harness, session, raw model, canonical model,
      provider (when proven), event/bucket time and time precision, all `Usage` tiers, calls, and cost
      provenance. Missing provider, timestamp, usage, or price remains explicit `unknown`/unpriced;
      provider is never guessed solely from a model-name prefix because a routed model and its maker
      are not necessarily the billing provider.
- [ ] Flux-native aggregation treats `CallUsage` as canonical per-call data and uses
      `TurnEnded.usage` only as the existing uncovered-turn legacy fallback. A fixture containing
      both proves that totals are not doubled, matching the selection rule in
      `crates/flux-events/src/projection.rs:553-590`.
- [ ] The TUI supports 4h, 1d, 7d, and custom windows; play/pause, restart, seek, speed selection, and
      fit-to-duration; and a deterministic virtual clock. Given the same fixture, range, and clock,
      replay frames and cumulative totals are stable.
- [ ] The activity-flow view, synchronized token/cost timeline, KPI totals, and comparison table all
      respond to one cursor and one filter set. Operators can group and drill through harness,
      provider, model, and harness → provider → model, then compare the selected range with the
      immediately preceding equal-length range.
- [ ] Exact metrics include calls, sessions, fresh input, output, cache write, cache read, reasoning,
      audio where present, and cost. Independent calls are summed field-by-field rather than through
      live-context `Usage::accumulate`, preserving the distinction already documented in
      `crates/flux-events/src/projection.rs:432-443`.
- [ ] Cost is calculated per call before aggregation, preserving the current reported-cost precedence
      and mixed reported/estimated behavior (`crates/flux-events/src/projection.rs:446-537`). Every
      dollar view shows reported, table-estimated/subscription-equivalent, and unpriced coverage;
      unknown cost is never displayed as `$0`. Historical table estimates state which pricing basis
      was used rather than implying provider-reported historical billing.
- [ ] Dense intervals coalesce route-identical calls into bounded `×N` pulses without changing exact
      totals. A seven-day stress fixture keeps input responsive and memory/visible-pulse counts
      bounded; rendering never iterates the full history on every frame.
- [ ] The observatory has deliberate wide, medium, and compact layouts; uses the existing TUI theme
      rather than hardcoded colors; remains understandable in monochrome; and offers reduced-motion
      or no-animation operation. Snapshot/state tests cover at least wide, narrow, empty, unknown
      provider, partially priced, burst-heavy, pause, seek-backward, and range-change states.
- [ ] The shipped entry point is discoverable from the TUI and help. It preserves C-140's useful live
      per-session `/usage` behavior while making historical replay a clear mode or view rather than
      silently replacing current-turn semantics.
- [ ] Replay and inspection require only usage metadata. No prompt, assistant answer, tool argument,
      or transcript body is loaded merely to render the observatory.
- [ ] Each implementation child named below is filed before implementation with a failing-first test,
      and the epic closes only when all are done or explicitly retired with a reason. The standard
      workspace build/test/clippy/fmt and `flux-codegate` gate is green at close.

## Child stories

1. [**C-519 — Extract one shared cross-harness usage timeline.**](C-519-shared-cross-harness-usage-timeline.md)
   Move reusable extraction and accounting inputs out of CLI-only private structs while preserving
   `flux usage` output and discovery/read-only limits.
2. [**C-520 — Project truthful usage attribution and cost.**](C-520-truthful-usage-attribution-and-cost.md)
   Normalize harness/provider/raw+canonical model, timestamp precision, per-tier usage, and
   reported/estimated/unpriced cost; prove legacy Flux logs do not double-count.
3. [**C-521 — Build adaptive usage buckets and comparisons.**](C-521-adaptive-usage-buckets-and-comparisons.md)
   Build plot-width-aware timeline buckets, cumulative totals, filters, hierarchy grouping, and
   equal-previous-period deltas with stable zero-baseline behavior.
4. [**C-522 — Ship the static Usage Observatory TUI.**](C-522-static-usage-observatory-tui.md)
   Ship responsive KPI, timeline, flow-route, comparison, filter, and inspector views before motion
   so the analysis remains useful with animation disabled.
5. [**C-523 — Add deterministic usage replay and bounded animation.**](C-523-deterministic-usage-replay-and-animation.md)
   Add the virtual clock, playback controls, interpolated pulses, burst coalescing, checkpoints for
   backward seeks, and bounded rendering work.
6. [**C-524 — Close Usage Observatory accessibility, performance, and entry points.**](C-524-usage-observatory-closure.md)
   Preserve the live `/usage` view, wire help and navigation, add monochrome/reduced-motion coverage,
   and prove seven-day responsiveness.

These are sequencing boundaries. The first three establish truthful data semantics; the visual
stories consume them rather than re-deriving accounting inside widgets.

## Progress

- 2026-08-04: Filed C-519 through C-524 from the accepted six sequencing boundaries. Each child now
  names its failing-first proof, dependencies, and non-overlapping ownership; the first three establish
  truthful shared data semantics before the static and animated TUI layers consume them.
- Epic filed from the operator request after reconciling it with the repository rather than the
  earlier workspace-level sketch. The key correction is that Flux already has a substantial
  cross-harness accounting surface: `UsageRecord` includes harness and cost provenance, records are
  time-filtered before totals, calls are counted from filtered usage records, and output is already
  combined by model (`crates/flux-cli/src/usage.rs:1331-1467,1497-1503`). The new work is a reusable
  timeline plus TUI replay, not a new SQLite analytics product.

## Notes

- **Existing surfaces to preserve:** C-05 pricing, C-06 usage/cost attribution and reporting, C-34
  provider-reported cost precedence, C-38 realtime/audio usage, and C-140's live in-TUI usage overlay.
- **The provider gap is real.** The current shared event variant stores `model` and `usage`, while the
  CLI's cross-harness record stores `harness`, `session`, and `model`; neither shape has an independent
  proven provider field (`crates/flux-events/src/kind.rs:103-112`,
  `crates/flux-cli/src/usage.rs:167-176`). The first child must define evidence-based attribution and
  preserve `unknown`, not parse a polished but false provider lane.
- **Reuse the honest cost fold.** The existing projection prices each call before summing so reported
  costs do not eclipse table estimates in a mixed row, and returns `None` when nothing can be priced
  (`crates/flux-events/src/projection.rs:446-537`). The observatory should expose that provenance,
  not flatten it.
- **No transcript dependency.** This feature visualizes usage streams, not conversation content. That
  keeps it separate from C-212's secret-bearing cross-harness history datasource and its ingestion
  safety envelope.
- **Non-goals for this epic:** cloud sync, organization billing, invoice reconciliation, prompt or
  answer replay, arbitrary external telemetry backends, and editing pricing from inside the TUI.
