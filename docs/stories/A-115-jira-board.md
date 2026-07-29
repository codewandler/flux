---
id: A-115
title: JiraBoard over the existing jira plugin, with a configurable status↔state mapping
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [plugins, flux-capabilities]
note: "the status↔State mapping is config, not code — Jira workflows differ per project, and a hardcoded transition name makes the backend work at exactly one company"
---

# JiraBoard over the existing jira plugin, with a configurable status↔state mapping

## Goal
Make Jira the system of record for a fleet without inventing a second integration:
`plugins/jira` already ships issue CRUD, transitions, comments and search (`jira.issue.create`,
`jira.issue.transition.run`, `jira.issue.comment.add`, `jira.issue.search`, `jira.issue.edit`) plus
`jira.issues` / `jira.users` datasources. `JiraBoard` maps `WorkBoard` onto those, so every durable
fact round-trips to Jira and there is no second source of truth to reconcile.

## Acceptance
- [ ] `JiraBoard` implements `WorkBoard` and **passes the shared contract suite from A-113
      unmodified**, against a recorded/stubbed Jira — offline, no credentials in CI.
- [ ] The Jira-status ↔ `State` mapping is **configuration**, declared on the datasource decl, not
      hardcoded: `State::InProgress` maps to whatever the project's workflow calls it.
- [ ] Failing-first test: a configured mapping that cannot satisfy the state machine (an unreachable
      `State`, or two states mapped to one status) is **rejected at registration**, not at first
      write — matching the "validated once at registration" convention.
- [ ] `access()` declares the concrete `LiveAccess::Network { subject }` for the Jira origin, so the
      backend's egress is policy-visible like any other live datasource.
- [ ] `fleet.dispatch`'s `task_id` / `runner` write-back (A-116) lands somewhere durable on the Jira
      issue and survives a coordinator restart.

## Progress
- (not started)

## Notes
- Design: [fleet-coordinator.md §3](../designs/fleet-coordinator.md).
- Depends on A-113. Independent of A-114 and A-116 — can run in parallel with both.
- No consumer-specific workflow names in the repo: the mapping ships with a documented example, not
  a company's actual statuses.
