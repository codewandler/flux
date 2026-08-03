---
id: C-500
title: "Connector-native integrations replace the official plugin fleet (epic)"
pillar: Core
status: in-progress
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "EPIC — Flux keeps guarded runtime kinds and local-first execution, while all 18 vendor-specific plugin adapters move to flux-connectors and become remotely usable through Exchange"
---

# Connector-native integrations replace the official plugin fleet

## Goal

Make connectors the single official integration model: Flux locally executes connector bundles
through generic guarded runtimes, can call the same bundles through Exchange, and ultimately carries
no vendor-specific integration crate under `plugins/`.

## Acceptance

- [ ] The vision, ecosystem description, roadmap, public docs and plugin-pack documentation agree
      that plugins are a connector runtime/compatibility format rather than the permanent home of
      Docker, Kubernetes, SQL, observability or vendor adapters (C-501).
- [ ] Flux can execute every connector runtime locally without a vendor switch or a second path
      around authorization → approval → guarded IO (C-502).
- [ ] Flux can authenticate as a Service Account and consume Exchange `invoke`, `subscribe`, streamed
      output and lease lifecycle without receiving a tenant credential (C-503).
- [ ] One conformance suite proves connector behavior across local and hosted placements before a
      native adapter is removed (C-504).
- [ ] All eighteen current integration crates are retired only after their connector migration waves
      complete, and the support/distribution crates receive an explicit disposition (C-505, C-506).
- [ ] `flux must never require flux-exchange`: the local connector path remains complete after the
      last official native adapter is gone.

## Progress

- 2026-08-03: Filed after auditing the active tree and linked worktrees. The connector WebSocket
  program and remote approval exist in pending Flux worktrees and are dependencies, not duplicate
  backlog; C-493/C-494 are reserved by the pending 0.52 maintenance worktree.

## Notes

- Connector counterpart: flux-connectors C-495. Exchange counterpart: X-111.
- Reuses C-394/C-397/C-399/C-435, D-215/D-220 and the pending generated-channel program C-481…C-488.
