---
id: A-143
title: "Change a value and continue — recorded as an intervention, never silently applied"
pillar: Agent
status: backlog
design: docs/designs/interactive-debugger.md
epic: interactive-debugger
areas: [flux-tui, flux-runtime, flux-evidence]
note: "⚠ `flux fork --at N --inject <json>` (A-46, SHIPPED) already does stop/change/continue for RECORDED runs. The gap is live: a forked replay holds NOTHING (hermetic prefix, no side effects) while a live paused run holds real resources, so a changed value can contradict effects that already happened"
---

# Change it, and let the record say who did

## Goal

A value in a paused live run can be changed and the run continued — with the change recorded in the
evidence chain as a human intervention.

## What already exists

`flux fork <session> --at N --inject <json>` (A-46, shipped) replays a run hermetically to statement N,
substitutes a different value for its result, and runs the rest live through the real approval envelope.
`--edit` and `--replan` round it out; `flux replay` and `flux diff` inspect and compare. **Stop, change,
continue already exists** — for recorded runs, as a CLI.

⚠ **The live case is not the same feature.** A forked replay holds *nothing*: the prefix is hermetic, no
side effects, so injecting a value is free. A live paused run holds an open connection, a spawned
process, a half-written file — and a value changed underneath it can contradict effects that already
happened.

## Acceptance

- [ ] **Failing-first**: a test changing a value in a paused run, continuing, and asserting both the new
      value takes effect **and** the intervention appears in the evidence chain — failing at the merge
      base.
- [ ] ⚠ **The intervention is recorded.** A run whose evidence log does not show that a human altered
      its state has lost the property that makes flux's runs auditable — and that property is worth more
      than this feature. This is the invariant not to trade for ergonomics.
- [ ] ⚠ **The UI never implies a changed value un-did an effect.** Setting `$total = 5` after the invoice
      was sent for 500 does not un-send it. Say so where the change is made, not in a doc.
- [ ] ⚠ **Decide: fork or mutate in place.** `flux fork` already establishes that a divergence is a
      first-class replayable, diffable session rather than an edit of the original. Forking is far more
      honest here; mutating in place is cheaper and loses that. Decide explicitly and record why.
- [ ] The continued run's effects go through the approval envelope exactly as before. Nothing here
      weakens it.
- [ ] Full gate green.

## Notes

- Depends on [A-142](A-142-inspect-a-paused-run.md) and [A-140](A-140-pause-a-live-run.md).
- Read `flux fork`'s implementation first — its Mode A is this operation's offline twin, and its
  boundary handling (the Replay→Record scope swap) is the part most worth reusing.
- A DAP server is the obvious second surface once these semantics settle; do not shape them so as to
  preclude it.

## Progress

- Filed 2026-08-01 with the interactive-debugger epic.
