---
id: A-148
title: "First-class boards have explicit scope, profile, backend and planning documents (epic)"
pillar: Agent
status: in-progress
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-lang, flux-datasource, flux-capabilities, flux-sdk, flux-cli]
note: "Decision 0010 amends Decision 0006: common item core, closed general/planning/execution profiles, session/repository/workspace scopes, plus vision/roadmap/decision/design documents"
---

# First-class boards have explicit scope, profile, backend and planning documents

## Goal

Make a board a named Flux resource rather than a write-capable datasource hack or an assumed singleton.
Boards share identity, references, authority and a common item core while declaring the scope,
profile and backend needed for their actual purpose.

## Acceptance

- [ ] `BoardRegistry`, `BoardRef`, scope/profile/backend types and profile contract suites ship through
      A-134; multiple boards never rely on declaration order or exactly-one inference.
- [ ] Flux-Lang declares a board directly with scope/profile/backend and a bounded migration from
      `kind "board:*"` through L-130.
- [ ] General, planning and execution profiles expose their closed state machines and exact operation
      sets; the delivered eleven execution operations remain compatible.
- [ ] Planning boards own revisioned vision and roadmap singletons, stable decision and design
      collections, and story/epic links without placing documents into the work queue.
- [ ] Session state, Track repositories and federated workspaces pass their respective backend and
      scope contracts through C-548, C-549 and C-550.
- [ ] The SDK can register several named boards atomically with source-labelled collision errors and
      accurate `board:<binding>/item/<id>` subjects.
- [ ] `flux board` exposes the complete human and versioned agent surface, including a concise
      `flux board skill` guide whose examples are tested.
- [ ] The old single-lifecycle wording in the first-class-board design and public docs is absent.

## Progress

- 2026-08-05 — respecified from flux-roadmap Decision 0010. Decision 0006's separate board namespace
  and first-class declaration remain; its one-lifecycle/one-operation-set clause is superseded.

## Notes

- Canonical design: [native-board-fleet-cli.md](../designs/native-board-fleet-cli.md).
- Vendor tracker bindings remain Milestone 3+ and must implement a selected profile; they are not V1.
