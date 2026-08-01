---
id: A-141
title: "What pause means for an effect already in flight — and saying it honestly"
pillar: Agent
status: backlog
design: docs/designs/run-control.md
epic: run-control
areas: [flux-tui, flux-runtime, docs]
note: "the story that makes pause trustworthy or not. A pause pressed while an HTTP request is in flight, a subprocess is running or a model is streaming cannot un-send any of it. ⚠ A pause that reports stopped while effects continue is WORSE than no pause — it invites the operator to relax at the wrong moment"
---

# A pause that does not pause is worse than none

## Goal

Define, implement and state exactly what pausing does to work already underway — so the indicator never
means something the runtime is not doing.

## Acceptance

- [ ] **Failing-first**: a test pausing while an operation is in flight and asserting the UI reports
      what is still finishing rather than claiming a full stop — failing at the merge base.
- [ ] Each in-flight class is decided and documented: an HTTP request, a spawned process, a streaming
      model call, a sub-agent turn. ⚠ The honest answer is likely *"pause takes effect at the next
      boundary, and here is what is still finishing"* — say that plainly rather than implying a stop
      that did not happen.
- [ ] The UI distinguishes **pausing** from **paused**. Conflating them is the whole defect.
- [ ] ⚠ An effect that completes during a pause is recorded normally in the evidence chain. A pause must
      not create a gap in the audit trail — that would trade a real guarantee for a UI affordance.
- [ ] The documentation states plainly that pause is **not** a way to stop a dangerous operation, and
      points at the approval envelope, which runs before the effect.
- [ ] Full gate green.

## Notes

- Depends on [A-140](A-140-pause-a-live-run.md).
- ⚠ This repo's recurring defect class is a guard or comment that agrees with its own assumption. A
  paused indicator that asserts a stop the runtime does not implement is exactly that, on the surface
  an operator trusts most.
- SIGSTOP is not the answer — it freezes the UI too, ages out network peers, and holds every resource.
  Considered and rejected in the design.

## Progress

- Filed 2026-08-01 with the run-control epic.
