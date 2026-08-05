---
id: A-140
title: "Pause a live run from the TUI, and continue it"
pillar: Agent
status: ready
priority: 7
design: docs/designs/run-control.md
epic: run-control
areas: [flux-tui, flux-runtime]
note: "⚠ flux already suspends three ways — `await`/journeys, ParkedAsk, and approval — but ALL are cooperative and declared in advance. This is operator-initiated: stop now, wherever you are. Approval is the mechanism to extend: it already proves the runtime can hold a turn mid-flight without losing state"
---

# Stop, because I said so

## Goal

A hotkey suspends a running agent loop at the next safe boundary and resumes it, with no lost state.

## What already exists, and why it is not this

flux suspends in three places today, and every one is **declared in advance**:

- **`await` / durable journeys** — the program says where it suspends.
- **`ParkedAsk`** (`crates/flux-app/src/park.rs`) — a journey asks a human and parks.
- **Approval** — every effect stops at the envelope for a human decision.

This story adds an **operator-initiated** halt at a point nobody declared. ⚠ Treating that as the same
problem is how it ships broken. But approval is the right mechanism to extend: it already proves the
runtime can hold a turn mid-flight while a human decides, without losing state or leaking resources.

## Acceptance

- [ ] **Failing-first**: a test pausing a running loop and asserting it stops advancing and then
      resumes with its state intact — failing at the merge base.
- [ ] A hotkey pauses and resumes. Paused state is unmistakable in the UI.
- [ ] ⚠ **"Safe boundary" is defined and documented**, and [A-141](A-141-what-pause-means-for-an-effect-in-flight.md)
      owns what happens to an effect already in flight. Do not ship a pause whose meaning is undefined
      at the moment it matters most.
- [ ] ⚠ **Pause is never presented as a safety control.** The approval envelope stops a dangerous
      operation *before* it happens; pause is an operator convenience. Documenting them as
      interchangeable would weaken a real guarantee with a soft one.
- [ ] No backpressure on a run that is not paused. A pause feature that costs throughput when unused is
      a regression.
- [ ] Works under `--yes`, where there are no approval points at all — that is precisely the run you
      most want to halt. ⚠ If the first increment only pauses at approval points, it must say so rather
      than appearing to work and then not stopping an autonomous run.
- [ ] Full gate green.

## Notes

- Pairs with [A-137](A-137-the-step-thread.md): the natural place to show paused state is the step
  thread, and the natural place to press the key is while watching it.
- Foundation for [interactive-debugger](../designs/interactive-debugger.md); build no inspection or
  mutation here.
- ⚠ Open, and it matters: do timeouts keep running while paused? A paused run whose provider connection
  ages out was not really paused.
- ⚠ Open: per-run or global when sub-agents are in flight. A parent that pauses while its children
  continue is worse than no pause.
- C-570 is deliberately different: the worker reaches a loop-declared safe checkpoint, reports and
  yields cooperatively. This story remains operator-initiated pause at a boundary nobody declared.

## Progress

- Filed 2026-08-01 with the run-control epic.
