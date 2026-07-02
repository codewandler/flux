---
id: L-25
title: Pre-authored flow-run resumable mode — reified halts for `flux flow run`
pillar: Language
status: backlog
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: extend the resumable entry point to the engine's non-loop flow path (authored .flux flows), and revisit whether the ledger subsumes checkpoint for that path too
---

# Pre-authored flow-run resumable mode

## Goal
Give authored flows (`flux flow run`, journeys) the same reified-halt + ledger + fast-forward
machinery the loop gets, so a failed long flow can be corrected and continued instead of re-run
from the top (today: checkpoint fast-forward only, defeated by any edit).

## Acceptance
- [ ] `flux flow run` (and the engine flow entry) can opt into resumable mode; a halted authored
      flow reports the structured halt; a corrected re-run fast-forwards the matching prefix.
- [ ] Checkpoint interplay decided and documented (ledger subsumption vs coexistence for authored
      flows); `once`/`saga` invariants re-verified on this path.
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic; post-MVP.)

## Notes
- Depends on L-22. Surface question (how a human corrects an authored flow mid-halt — editor?
  `flux flow resume`?) needs its own small design pass before implementation.
