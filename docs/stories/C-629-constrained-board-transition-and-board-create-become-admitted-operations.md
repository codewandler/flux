---
id: C-629
title: "Constrained board.transition and board.create become admitted operations"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "decision 0015 (roadmap) runs the planner host-side until these exist; direction-constrained transition (backlog->ready) plus create, both committing their writes"
---

# Constrained board.transition and board.create become admitted operations

## Goal

Decision 0015 (roadmap repository) grants a planner promotion and creation authority but runs it
host-side, because no admitted operation exists for either. To move the planner inside Fleet — or
give any coordinator a bounded planning surface — `board.transition` and `board.create` must
become admitted operations whose constraints are enforced by the host, not by prompt text.

## Acceptance

- [ ] A `board.transition` operation exists whose grant can be constrained to a direction (backlog->ready) and refuses any other transition at the host layer.
- [ ] A `board.create` operation exists; both commit their writes (C-625) so a loop's planning output is never invisible.
- [ ] The coordinator/planner ceiling can name them like any other operation and admission validates the constraint syntax.
- [ ] Granting the unconstrained verbs remains impossible from a loop ceiling (widening stays a human act).
