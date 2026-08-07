---
id: C-687
title: "A supervisor authorization model for approvals over the network"
pillar: "Core"
status: backlog
epic: remote-agents
areas: [flux-server]
design: docs/designs/operating-a-deployed-host.md
note: "named as required by the remote-approval work and by the fleet-across-machines design; never filed until now — principal auth is refused for approvals precisely because this model does not exist"
---

# A supervisor authorization model for approvals over the network

## Goal

`--remote-approval` parks every effect that needs a human and serves it at `GET /approvals`, but it
supports only the shared operator token or an open loopback: principal authentication is *refused*
for approvals, deliberately, because answering an approval is a different authority from calling an
agent and no model says who may do it. That refusal is the right default and it is also the ceiling
on every multi-operator topology — a deployed agent that several engineers share, the fleet
coordinator whose decisions and applies need a human word, a hosted single-org Exchange where
members are authenticated but not all of them are supervisors. The fleet-across-machines design
names this as the blocker for multi-operator supervision and nobody had filed it. Define the
model: who may answer an approval, how that authority is declared and audited, and how it composes
with the deployment-declared operator authority Decision 0019 already established.

## Acceptance

- [ ] A supervisor authority is declarable per deployment, distinct from both "can reach the agent"
      and "is an operator" — deny-by-default, granted explicitly, and never inferred from
      authentication alone.
- [ ] An approval answer records the supervising principal in the durable audit record: who
      approved, which exact effect, when; an unanswered effect still denies on timeout.
- [ ] Principal authentication becomes usable for approvals under a declared supervisor grant,
      and the shared-token mode remains available and documented as the single-operator case.
- [ ] Two supervisors cannot double-answer one parked effect (the first answer settles it), and a
      supervisor's authority is revocable without restarting the agent.
- [ ] The model is stated where the deployment profiles can encode it (C-685) and where the fleet
      can consume it for coordinator decisions.
