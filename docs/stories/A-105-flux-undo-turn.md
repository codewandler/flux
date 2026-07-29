---
id: A-105
title: "flux undo --turn <n> — reverse-batch reconstruction, LIFO execution, itemized report"
pillar: Agent
status: backlog
epic: transactional-turns
design: docs/designs/transactional-turns.md
note: "the epic's headline verb; undo is NOT privileged — it runs through the ordinary approval + guarded envelope, so the undo itself records compensations and is undoable"
---

# flux undo --turn <n> — reverse-batch reconstruction, LIFO execution, itemized report

## Goal
The epic's headline: one command that rolls back a turn's real effects, reconstructed from the
stored reverse actions (A-104) and executed through the same envelope as any other work. Sits beside
the Time Machine verbs (`replay`, `fork`, `diff`, `export`) but is the first one that *writes*.

## Acceptance
- [ ] `flux undo --turn <n>` loads the turn's `Compensated` events via a kind-filtered read and
      builds an `ActionBatch` from their `reverse` actions in **reverse execution order** (LIFO).
- [ ] **Failing-first headline test**: a turn that wrote a file is undone and the file's prior bytes
      are restored. Impossible today — there is no verb and no stored reverse.
- [ ] LIFO correctness: a turn writing the same path twice, then undone, yields the **pre-turn**
      bytes (not the intermediate) — its own test.
- [ ] The undo batch executes through the ordinary approval + guarded-IO envelope, **not** a
      privileged path — pinned by a test asserting the approval gate and guarded `System` are both
      hit. Consequence: the undo records its own compensations and is itself undoable; assert that
      round-trip.
- [ ] Sequential execution stops at the **first** failed compensator, reporting the exact boundary
      ("actions 8..5 reversed; 4 failed: <error>; 3..1 not attempted") and leaving the remainder
      unattempted — no interleaved half-state, no auto-rollback-of-a-rollback.
- [ ] Re-running `flux undo` after fixing the cause works, because it re-reads the same stored
      reverse actions — its own test.
- [ ] Itemized report names every action that was **not** reversed with its `why` (a turn that sent
      mail and wrote a file reports the file restored and the mail not reversed, by name).
- [ ] A turn recorded before this epic reports as un-undoable, honestly — no crash, no silent
      success.
- [ ] Run addressing matches the existing verbs (`s_42`, `last`).

## Progress
- Not started.

## Notes
- Design: [transactional-turns.md](../designs/transactional-turns.md).
- Blocked by A-104.
- Deliberately non-goals for this story: no `--all`, no auto-undo, no undo of a turn other than by
  explicit id. Widening the blast radius of a write verb is its own decision.
