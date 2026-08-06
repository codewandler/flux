---
id: C-568
title: "Every agent starts under an explicit loop harness (epic)"
pillar: Core
status: in-progress
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-agent, flux-flow, flux-runtime, flux-orchestrate, flux-cli]
note: "bind behavior at agent start; configured Fleet loops add review, typed progress/yield and hierarchical budgets"
---

# Every agent starts under an explicit loop harness

## Goal

Make the behavior around every agent an admitted, versioned runtime contract rather than an ambient
default or backend accident. General agents retain the resolved adaptive default; Fleet writers,
reviewers, research tasks and future backends receive purpose-specific loops, reporting and budgets.

## Acceptance

- [ ] Every agent creation path resolves and snapshots the explicit loop binding contracted by
      [C-569](C-569-resolve-loop-binding-at-every-agent-start.md); no running agent carries “use
      whatever default this constructor happens to choose.”
- [ ] Fleet's five-writer, three-repository proof completes implementation handoffs under explicit
      operator-authored bindings from C-569; no dedicated Fleet workhorse story is required.
- [ ] A worker can durably report meaningful progress and cooperatively yield at a safe checkpoint,
      and its parent/main can acknowledge and resume it without parsing prose
      ([C-570](C-570-agent-progress-and-cooperative-yield.md)).
- [ ] Fresh review and same-session repair execute through explicit reviewer/repair loop bindings
      while host evidence and the two-rework ceiling remain authoritative
      ([C-572](C-572-fleet-review-and-rework-loops.md)).
- [ ] Every frozen implementation candidate starts tool-free review and structured reflection
      concurrently; final handoff requires both receipts and C-587 collects reflection findings in a
      central bounded improvement projection.
- [ ] Fleet, wave, task, agent and loop scopes reserve and settle one narrow-only budget envelope;
      time, calls, tokens and host-counted resources are bounded before monetary enforcement plugs
      into the same ledger ([C-542](C-542-granular-time-token-budgets-visible-in-tui.md),
      [C-571](C-571-hierarchical-fleet-budget-ledger.md), [C-130](C-130-monetary-budgets-epic.md)).
- [ ] C-552/C-553 backends discover and refuse unsupported loop/report/yield/budget behavior rather
      than silently invoking a foreign harness default.
- [ ] C-573 uses freshness-labelled live metrics to tune only pre-authorized model, effort,
      concurrency and reservation choices at safe boundaries, with hysteresis and durable reasons.
- [ ] C-583 exposes live Fleet capacity through one typed operation/CLI contract, safely drains on
      scale-down and keeps one Fleet worker equal to one admitted agent rather than hiding nested
      worker pools inside story writers.
- [ ] Public architecture, loop, Fleet and SDK documentation teach the separation between loop
      behavior, execution backend, Fleet membership, Board backend and UI projection.

## Progress

- 2026-08-05 — filed from the native Fleet dogfood: five fresh, capability-bounded workers all
  exhausted the general adaptive explorer before producing a commit. Existing loop, activity,
  review, pause and budget contracts were inventoried in the linked design before the story split.
  The metrics-driven controller was then added as C-573 from operator feedback; C-583 records the
  manual operation/CLI capacity actuator that controller will later use. C-587 makes independent
  review plus harness reflection mandatory before handoff so live Fleet work feeds self-improvement.

## Notes

- C-567 is postponed optional policy/convenience work. Operators can run their own authored
  sub-agent loops through C-569 without waiting for it.
- Decision 0010's one-writer, fresh-review, host-verification and publication fences remain intact.
