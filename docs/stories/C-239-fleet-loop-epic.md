---
id: C-239
title: "The fleet runs the track / impl-coord loop — complete and usable (epic)"
pillar: Core
status: in-progress
epic: fleet-loop
design: docs/designs/fleet-loop.md
note: "EPIC — the model reasons, the host enforces: isolated story writers feed one ordered wave branch, one final gate and one publication decision"
---

# The fleet runs the track / impl-coord loop — complete and usable (epic)

## Goal
flux 0.36.0 ships a fleet coordinator that dispatches board items to remote agents over A2A. It does
not run the loop the `track` plugin runs: read a board, select a wave of independent items, give each
an isolated worker that implements and runs targeted checks and commits on a scratch branch, review
the diff *as evidence*, allow up to two rework rounds to the **same** worker, park after that,
integrate serially on one wave branch, run one full gate on the final combined tree, publish only on
green, then write the bookkeeping.

Make flux run that loop, with the contract in the place where it cannot be ignored: **the model
reasons, the host enforces.** A `WaveCoordinator` mechanically owns the irreversible, order-sensitive
actions; the model owns wave selection and diff review. The invariants — fenced ledger, one
writer/worktree per story, targeted checks before handoff, ordered integration, one full gate at the
wave boundary, never publish red, and park after two rounds — become host behaviour instead of prose,
so they hold even when the model is wrong or lazy.

## Acceptance
- [x] A design doc ([fleet-loop.md](../designs/fleet-loop.md)) covering the contract split, the
      verified assets, the six gaps, and the isolation/result-return correction that bounds scope.
- [x] The epic is broken into implementation stories on the board (F1…F10), ordered so the data path
      and the contract land before anything reasons over them.
- [ ] Headline proof: the offline end-to-end journey of F9 — a stub A2A worker, a `MemoryBoard`, two
      items, one integrating and one parking — passes with no network and no real model.
- [ ] These are true *mechanically*, each pinned by a test rather than asserted in prose:
      publishing without a successful wave gate is impossible · a red gate preserves the failed
      candidate and publishes nothing · one story has at most one writer/worktree · a third rework
      round parks instead of dispatching · a worker cannot write a fenced ledger path · a
      `Failed→Ready` retry leaves no stale `runner`/`task_id`.

## Progress
- 2026-07-30 — **design done**: [fleet-loop.md](../designs/fleet-loop.md), with the ten stories filed
  as F1…F10. The load-bearing decision is the contract split (host enforces / model reasons); the
  sharpest instance is `fleet.integrate`, which assembles an ordered wave but cannot publish it or
  write completion bookkeeping without one successful full gate on the combined tree.
- 2026-08-05 — **delivery contract corrected before F5 implementation**: one writer/worktree and
  targeted checks per story, one dependency-ordered integration branch, and one full gate at the
  final wave boundary. A red candidate is retained as evidence and never published.
- 2026-07-30 — **the scope boundary moved before any code landed.** A code-read found that
  per-worker filesystem isolation does not exist for remote workers *and is designed out*
  (`git_worktree_enter` is caller-local; `fleet-coordinator.md:303-311` declares the problem
  dissolved), and that a worker cannot return a branch or diff at all — `SpawnOutcome` has no
  artifact field and `flux-server` never populates `Task.artifacts`. **So the full
  code-implementation loop is a LOCAL-worker loop for now**; local children get real isolation via
  C-100. Remote code workers wait on `agent-fleet-runtime` (Docker isolation + artifact return).
  Two positive corrections from the same pass: A2A `contextId` continuity *is* implemented, so the
  rework path genuinely resumes the same worker; and `ProcessRuntime` is not an optimization but a
  **prerequisite for any wave larger than one**, because `FlowEngine`'s `turn_gate` means one worker
  serves one concurrent turn.
- 2026-07-30 — F1 (C-236) and F3 (C-238) in flight.

## Notes
- Sibling epics, and the division of labour between them: `fleet-coordinator` is what 0.36.0
  shipped (dispatch/status/cancel over A2A); `agent-fleet-runtime` (A-119…A-128) is the distributed
  half — it turns "the loop runs on one machine" into "the loop spans machines" and is explicitly
  later.
- F9 is **A-117**, which already exists under the `fleet-coordinator` epic and is `blocked`. It is
  unblocked by F1…F8, not by anything in its own epic.
- Deliberately out of scope: `DockerRuntime`/`KubernetesRuntime`/NDJSON-stdio/endpoint-broker
  discovery (A-123…A-126) · `JiraBoard`/`GitlabBoard` (A-115/A-118) · a flux-native *replacement*
  for the `track` plugin · remote code workers.
