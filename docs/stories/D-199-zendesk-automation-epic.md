---
id: D-199
title: "Zendesk automation — deterministic support workflows with bounded AI (epic)"
pillar: Agent
status: in-progress
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [flux-cli, flux-lang, plugins, docs]
note: "EPIC — L-92 (--entry) and A-136 (reference flow) shipped; the plugin is withdrawn, so D-200/D-201/D-202 await the flux-connectors interop"
---

# Zendesk automation — deterministic support workflows with bounded AI (epic)

## Goal

Ship the first complete reference for deterministic third-party automation in Flux-Lang: configure
one Zendesk API token, select a named workflow from one `.flux` file, run typed Zendesk calls through
the plugin safety envelope, and optionally use a model for bounded analysis without granting it write
control.

## Acceptance

- [ ] L-92, D-200, D-201, A-136, and D-202 are done with their named failing-first tests.
- [ ] A local install can run `setup`, `triage`, `brief`, and `eod` using the documented command
      forms; model failure preserves useful deterministic output.
- [ ] The reference workflow contains no Zendesk write operation, while the plugin's separately
      callable writes are typed, accurately gated, and concurrency-safe.
- [ ] Both root and nested plugin workspace gates are green; unavailable live credentials are
      reported as a skipped smoke leg rather than simulated success.

## Progress

- 2026-07-30 — epic and implementation stories filed; design locked in
  [zendesk-automation.md](../designs/zendesk-automation.md).
- 2026-07-30 — L-92, D-200, D-201, and A-136 are implemented and done. D-202's documentation,
  catalogs, smoke, and release note are done; the epic stays in progress until unrelated concurrent
  root-gate failures clear and a separate signed plugin-pack release is cut.
- 2026-07-30 — D-202 is closed: the concurrent remediation work landed and both workspace gates are
  green on the integrated tree. Two acceptance bullets remain, and neither is source work: the
  documented `setup`/`triage`/`brief`/`eod` run needs live Zendesk credentials (the smoke leg skips
  honestly without them), and the signed plugin-pack release carrying `flux-plugin-zendesk` is cut
  separately from the core release. The epic stays open on those two.
