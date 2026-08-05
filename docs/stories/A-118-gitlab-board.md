---
id: A-118
title: GitlabBoard — a second Exchange-governed tracker proves the WorkBoard port generalizes
pillar: Agent
status: backlog
epic: first-class-board
design: docs/designs/first-class-board.md
areas: [flux-capabilities]
note: "re-pointed by Decision 0006 away from plugins/gitlab (Milestone 5 deletes the plugin path); deferrable — its value is the proof that WorkBoard is not 'Jira with a trait on top'"
---

# GitlabBoard — a second Exchange-governed tracker proves the WorkBoard port generalizes

## Goal

Implement `WorkBoard` over a GitLab connector board member bound through Exchange, so the port is
demonstrated against two independent real trackers rather than one. A port validated by a single
backend plus an in-memory double is a Jira shape with a trait on top; a second tracker is what makes
"the state source is abstract" a checked claim instead of an intention. Like A-115 this rides the
Decision 0006 declared-surface pattern — connector-declared mapping, Exchange tenant binding, every
write an admitted operation — not the `plugins/gitlab` path this story was originally written over.

## Acceptance

- [ ] `GitlabBoard` implements `WorkBoard` over Exchange-governed operations and **passes the shared
      contract suite from A-113 unmodified**, offline against a recorded/stubbed Exchange.
- [ ] The label/state mapping is configuration on the declared board member, on the same footing as
      `JiraBoard`'s status mapping, validated at bind/registration.
- [ ] Failing-first test: any place the contract suite had to be relaxed or special-cased to
      accommodate GitLab is instead fixed in the **port** — the suite stays backend-agnostic.
- [ ] Every mutation is an admitted operation under existing grant metadata; the backend declares no
      vendor network access of its own — egress and credentials are Exchange's.

## Progress

- (not started)

## Notes

- 2026-08-04 (C-514): re-pointed from `plugins/gitlab` to Exchange-governed operations per Decision
  0006, and re-homed under the first-class-board epic. The original `access()`/`LiveAccess::Network`
  acceptance belonged to the plugin path Milestone 5 removes.
- Gated on the connector board-member vocabulary and Exchange tenant Board bindings (Milestone 3+),
  and sequenced after A-115. Original design context:
  [fleet-coordinator.md §3](../designs/fleet-coordinator.md); epic design:
  [first-class-board.md](../designs/first-class-board.md).
- Deferrable: the epic can ship its Flux-side split (L-130, A-134) without this. Keep it filed so
  the generalization claim is not quietly dropped.
