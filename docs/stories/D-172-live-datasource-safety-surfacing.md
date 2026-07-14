---
id: D-172
title: Wire live datasources through surfacing and typed authority
pillar: Agent
status: backlog
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 5; depends on D-171"
---

# Wire live datasources through surfacing and typed authority

## Goal

Make a registered live domain discoverable only when configured and ensure planning and dispatch
authorize its exact datasource plus backend resources through the mandatory envelope.

## Acceptance

- [ ] Registration returns one per-domain `ToolGroup` and ambient signal; the two operations stay
      hidden without the signal and surface with it or `FLUX_SURFACE_ALL`.
- [ ] Specs honestly declare read plus network/connection access, and `authority_requirements`
      returns exact `datasource.read` plus backend resource requirements from one invocation
      contract; plan preview and dispatch tests observe identical requirements.
- [ ] Stable permission subjects name `<domain>/<entity>` and never smuggle filter values, cursors,
      secrets, or handles into grants.
- [ ] The SDK conversational builder has one fallible convenience seam that installs tools, group,
      and ambient signal together and preserves duplicate-registration diagnostics.

## Progress

- Not started; blocked on D-171.
