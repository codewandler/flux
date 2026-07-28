---
id: A-91
title: "Transactional turns — a compensating undo for the world, not just the session (epic)"
pillar: Agent
status: backlog
epic: transactional-turns
design:
note: "EPIC — every mutating op declares its compensator; the runtime synthesizes a reverse ActionBatch so `flux undo --turn N` rolls back real effects"
---

# Transactional turns — a compensating undo for the world, not just the session (epic)

## Goal
The Time Machine (A-45..A-47) and the Lab replay/fork the session; nothing undoes effects in the
world. Since every effect is a frozen `ActionBatch` of literal calls, the runtime could require
each mutating op to declare its compensator (write → restore prior bytes, `git push` → the exact
revert) and synthesize a reverse-batch at approval time. `flux undo --turn 14` becomes one command,
and "no compensator declared" becomes a policy-visible risk signal. No harness on the market has
turn-level rollback of real effects.

## Acceptance
- [ ] A design doc (`docs/designs/transactional-turns.md`) covering: the compensator contract on
      mutating ops, reverse-batch synthesis at approval time, ordering/partial-failure semantics,
      and the "no compensator declared" risk signal surfaced to policy/approval.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: `flux undo --turn <n>` rolls back a turn's real filesystem effects via the
      synthesized reverse batch, executed through the same approval + guarded-IO envelope.

## Progress
- (not started — epic filed from the 2026-07-28 out-of-the-box ideas session)

## Notes
- Builds on the frozen `ActionBatch` invariant and the C-43 cassette capture (prior op outputs are
  already recorded — the raw material for restore-style compensators).
- The most defensible headline of the six: "the only agent with undo for the real world."
- Code-reading re-assessment: complementary to (not replaced by) forward atomicity — a batch that
  fails at op 3 of 5 still leaves half-applied state today (`StagingDisposition` only routes
  gather-vs-capture; no snapshot/rollback machinery exists in `crates/`). The worktree epic
  (C-97..C-100) plus `flux-system`'s linked-worktree writable roots are plausible substrate for
  staging a batch and making "atomic-or-rolled-back" an envelope guarantee, with compensators
  covering the effects a shadow worktree can't (network, external services).
