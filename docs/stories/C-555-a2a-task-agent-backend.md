---
id: C-555
title: "An admitted A2A agent executes through the generic task-agent backend"
pillar: Core
status: backlog
epic: remote-fleet-membership
design: docs/designs/remote-fleet-membership.md
areas: [flux-orchestrate, flux-a2a, flux-runtime]
note: "follow-up remote execution — membership is C-554; exact artifacts still cross host verification"
---

# An admitted A2A agent executes through the generic task-agent backend

## Goal

Bind an admitted remote A2A worker to the generic lifecycle without weakening local fleet handoff,
rework or integration invariants.

## Acceptance

- [ ] C-552 start/steer/status/cancel/resume/result maps to authenticated A2A tasks with durable ids
      and bounded acknowledgement waits.
- [ ] Every call requires a current C-554 admission/lease and concrete BoardRef; endpoint possession
      alone grants nothing.
- [ ] Remote artifact/commit receipts include verifiable provenance and are materialized into an
      isolated local verification boundary before handoff acceptance.
- [ ] Disconnect, duplicate terminal result, late cancellation and lease expiry are deterministic and
      preserve inspectable evidence.
- [ ] Loopback and adversarial remote fixtures pass without network-dependent public services.
- [ ] Remote workers cannot apply, push, release, deploy or mutate coordinator goals/roadmap.

## Notes

- Depends on C-552 and C-554.
