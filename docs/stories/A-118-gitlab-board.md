---
id: A-118
title: GitlabBoard — a second real tracker proves the WorkBoard port generalizes
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [plugins, flux-capabilities]
note: "deferrable past the epic's first release — its value is the proof that WorkBoard is not 'Jira with a trait on top'"
---

# GitlabBoard — a second real tracker proves the WorkBoard port generalizes

## Goal
Implement `WorkBoard` over the existing `plugins/gitlab`, so the port is demonstrated against two
independent real trackers rather than one. A port validated by a single backend plus an in-memory
double is a Jira shape with a trait on top; a second tracker is what makes "the state source is
abstract" a checked claim instead of an intention.

## Acceptance
- [ ] `GitlabBoard` implements `WorkBoard` and **passes the shared contract suite from A-113
      unmodified**, offline.
- [ ] The label/state mapping is configuration on the same footing as `JiraBoard`'s status mapping,
      validated at registration.
- [ ] Failing-first test: any place the contract suite had to be relaxed or special-cased to
      accommodate GitLab is instead fixed in the **port** — the suite stays backend-agnostic.
- [ ] `access()` declares the concrete GitLab origin as its `LiveAccess::Network` subject.

## Progress
- (not started)

## Notes
- Design: [fleet-coordinator.md §3](../designs/fleet-coordinator.md).
- Deferrable: the epic can ship its headline proof (A-117) without this. File it now so the
  generalization claim is not quietly dropped.
