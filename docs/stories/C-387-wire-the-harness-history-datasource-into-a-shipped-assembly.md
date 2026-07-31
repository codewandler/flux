---
id: C-387
title: Wire the harness-history datasource into a shipped assembly
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "C-214/C-215/C-216 built a redacted, permission-scoped, off-by-default transcript datasource — ingest_harness_history and datasource_tools_with_history are called ONLY from tests, and no config key exists anywhere to enable it"
---

# Wire the harness-history datasource into a shipped assembly

## Goal

Make the transcript reader the project already built reachable in the product, so a retrospective
has an authoritative source instead of model context.

## Acceptance

- [ ] A config key (e.g. `[datasource.harness] enable = ["flux"]`) threads from `flux-config` into
      `try_register_datasource_ops_with_history` on the CLI and app assembly paths, defaulting to
      disabled.
- [ ] Failing-first, through the real CLI assembly: with the key unset, `search`'s input schema has
      no `harness` property and no `datasource:harness.*` permission subjects; with it set to one
      harness, both appear and the enum lists only that harness.
- [ ] The containment properties C-215 established — redaction and escaping at ingest, per-harness
      permission subjects, off by construction — are re-asserted at the assembly boundary, not
      assumed from the library test.

## Progress

- 2026-08-01 — filed from validation of HAR-04. The library half is genuinely done; the product half
  does not exist.

## Notes

- Epic C-212 (`ready`) owns harness history; this is the wiring story it never got.
