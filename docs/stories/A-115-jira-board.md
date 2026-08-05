---
id: A-115
title: JiraBoard through Exchange-governed operations, with a configurable status↔state mapping
pillar: Agent
status: backlog
epic: first-class-board
design: docs/designs/first-class-board.md
areas: [flux-capabilities]
note: "re-pointed by Decision 0006: the vendor mapping is a connector board member and every write an admitted Exchange operation — the plugin path this was written over is deleted by Milestone 5; the status↔State mapping stays config, not code"
---

# JiraBoard through Exchange-governed operations, with a configurable status↔state mapping

## Goal

Make Jira the system of record for a fleet without inventing a second integration — through the
Decision 0006 declared-surface pattern, not the plugin path this story was originally written over
(Milestone 5 deletes it). The flux-connectors Jira connector declares a **board member**: the
status↔`State` mapping and a per-verb binding of each board operation onto its own declared
operations. Exchange binds that member per tenant to a connection label, and every board write
executes as an admitted, granted operation. Flux keeps the fixed 11-op `WorkBoard` surface and its
closed state machine identical over that backend, so every durable fact round-trips to Jira and
there is no second source of truth to reconcile.

## Acceptance

- [ ] `JiraBoard` implements `WorkBoard` over Exchange-governed operations and **passes the shared
      contract suite from A-113 unmodified**, against a recorded/stubbed Exchange — offline, no
      credentials in CI.
- [ ] The Jira-status ↔ `State` mapping is **configuration on the declared board member**, not
      hardcoded: `State::InProgress` maps to whatever the project's workflow calls it.
- [ ] Failing-first test: a configured mapping that cannot satisfy the state machine (an unreachable
      `State`, or two states mapped to one status) is **rejected at bind/registration**, not at
      first write — matching the "validated once at registration" convention and 0006's
      "declared surfaces are enforced" rule.
- [ ] Every mutation is an admitted operation under existing grant metadata — no new authorization
      machinery, no request Flux constructs on its own. Board subjects report in the `board:`
      namespace (D-251).
- [ ] `fleet.dispatch`'s `task_id` / `runner` write-back (A-116) lands somewhere durable on the Jira
      issue and survives a coordinator restart.

## Progress

- (not started)

## Notes

- 2026-08-04 (C-514): re-pointed from `plugins/jira` to Exchange-governed operations per Decision
  0006's board generalization ("Boards are their own first-class surface"). The original acceptance
  named plugin ops (`jira.issue.transition.run`, …) and a `LiveAccess::Network` declaration on the
  backend; both belonged to the plugin path Milestone 5 removes. Egress and credentials are now
  Exchange's: the backend declares no vendor network access of its own.
- Gated on the connector board-member vocabulary and Exchange tenant Board bindings — designed with
  Milestone 3, per the epic ([first-class-board.md](../designs/first-class-board.md) §vendor
  backends). Depends on A-113 (shipped). Original design context:
  [fleet-coordinator.md §3](../designs/fleet-coordinator.md).
- No consumer-specific workflow names in the repo: the mapping ships with a documented example, not
  a company's actual statuses.
