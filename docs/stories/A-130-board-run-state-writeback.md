---
id: A-130
title: Board write-back of runner and task_id — make "the board is the run registry" true
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-capabilities, flux-orchestrate]
note: "filed from A-116's implementor report — design §5 says the board IS the run registry, but no op can write the two fields that make it one"
---

# Board write-back of runner and task_id — make "the board is the run registry" true

## Goal
[fleet-coordinator.md §5](../designs/fleet-coordinator.md) claims run state needs no second store
because `fleet.dispatch` writes the worker's `task_id` and `runner` address back onto the board
`Item` — "the board is the run registry", which is what makes crash recovery "restart, sweep,
re-derive".

As both implementors reported, that write path does not exist: A-113 lands `WorkBoard` with
`Item.runner` / `Item.task_id` as *fields* but no op that sets them, and `ItemDraft` does not carry
them. Until this lands, the design's crash-recovery story is a claim, not a property.

## Acceptance
- [ ] A board operation that records a dispatch — either a seventh op or an extension of `claim` to
      carry `runner` + `task_id` atomically with the claim. **Decide it in this story and say why**;
      atomicity with `claim` is the argument for the extension, and a distinct op is the argument for
      keeping `claim`'s contract narrow.
- [ ] Failing-first test: after `fleet.dispatch`, a fresh reader of the board can recover the
      dispatch — worker address and task id — with no in-memory state whatsoever.
- [ ] Failing-first test: crash recovery end-to-end — a new process over the same board re-derives
      every in-flight item and its worker, and the sweep resumes. This is A-117's headline claim, so
      the test belongs here or is shared with it.
- [ ] Concrete `permission_subjects` on whatever op results, consistent with A-113's `<domain>/item/<id>`.
- [ ] The design doc's §5 is updated to describe the op that actually exists.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from A-116's handoff, corroborated by A-113's. Both implementors independently
  reported the same gap from opposite sides, which is the strongest signal the design had a hole.
- Depends on A-113. Blocks A-117's crash-recovery Acceptance and A-128's monitor journey.
