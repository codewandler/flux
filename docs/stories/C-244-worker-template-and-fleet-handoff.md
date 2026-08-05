---
id: C-244
title: "A local sub-agent returns one typed, host-verified fleet handoff"
pillar: Core
status: ready
priority: 46
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-flow, flux-runtime, flux-orchestrate]
note: "V1 handoff is local and Flux-native: exact commit/write set/test argv/before/after; self-report never substitutes for host evidence"
---

# A local sub-agent returns one typed, host-verified fleet handoff

## Goal

Give a native local story worker a structured result channel sufficient for review and integration,
with the host independently verifying every claimed artifact.

## Acceptance

- [ ] `FleetHandoff` carries BoardRef, worker/session/worktree, branch, commit, normalized write set,
      test argv, failing-before evidence, passing-after evidence and summary; it has a published
      output schema and no prose re-parsing path.
- [ ] Failing-first test proves a local worker can return the handoff through `SpawnOutcome` and the
      coordinator records its evidence. A missing/malformed field records refusal without partial
      board evidence.
- [ ] The host runs the typed test argv before implementation and requires failure, then reruns it at
      the returned commit and requires success. A worker's contradictory claim is rejected.
- [ ] Documentation-only work declares a validation argv and an explicit no-failing-test reason;
      arbitrary behavioral work cannot use that path.
- [ ] Observed diff paths are normalized and compared with the approved write set and ledger fences.
      Expansion parks or serializes according to the coordinator decision; it is never silently
      accepted.
- [ ] The handoff names an exact commit in the shared repository; a branch name alone is invalid.
      Remote artifact transport and foreign process workers remain out of scope.
- [ ] Cancellation and crash preserve the worktree/commit/evidence for resume. Targeted runtime and
      orchestrate tests pass; A-117's wave owns the full gate.

## Notes

- Depends on delivered C-240/C-243 and on C-547 for machine schemas.
