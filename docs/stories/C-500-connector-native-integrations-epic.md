---
id: C-500
title: "Connector-native integrations replace the official plugin fleet (epic)"
pillar: Core
status: in-progress
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "EPIC — Flux embeds one Exchange client while all official integrations execute in Exchange; the Flux release ends with no plugin artifacts or fallback"
---

# Connector-native integrations replace the official plugin fleet

## Goal

Make connectors the single official integration model: Flux embeds one native Exchange client and
projects the operations Exchange grants to it, while every official external integration executes in
Exchange and the Flux release ultimately carries no plugin artifact or fallback path.

## Acceptance

- [ ] The vision, ecosystem description, roadmap, public docs and plugin-pack documentation agree
      that every official external integration executes through Exchange and that Flux has no local
      connector/plugin fallback (C-501).
- [x] The proposed local official-connector runtime host is explicitly superseded and must not be
      implemented (C-502, closed by C-508).
- [ ] Flux's core binary authenticates as a Service Account, consumes the effective catalogue, and
      invokes the existing one-shot HTTP path without receiving a tenant credential (C-503).
- [ ] A reusable cutover suite proves each legacy plugin's observable contract through Exchange
      before that adapter is removed (C-504).
- [ ] All eighteen current integration crates are retired only after their connector migration waves
      complete, then all plugin support and distribution infrastructure is removed from the Flux
      release pipeline (C-505, C-506).
- [ ] Flux remains complete without Exchange for its language, agent loop and core tools; official
      external integrations are unavailable when Exchange is unavailable.

## Progress

- 2026-08-03: Filed after auditing the active tree and linked worktrees. The connector WebSocket
  program and remote approval exist in pending Flux worktrees and are dependencies, not duplicate
  backlog; C-493/C-494 are reserved by the pending 0.52 maintenance worktree.
- 2026-08-03: C-508 adopted flux-roadmap Decision 0001 and removed the contradicted local execution
  placement before any C-502…C-506 implementation began.

## Notes

- Connector counterpart: flux-connectors C-495. Exchange counterpart: X-111.
- C-503 consumes C-318 and Exchange's effective catalogue/one-shot invoke contract. General
  subscriptions, streams, cancellation and leases belong to the later lifecycle milestone.
