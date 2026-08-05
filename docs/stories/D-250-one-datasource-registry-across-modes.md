---
id: D-250
title: "One datasource registry — enumerate every declared datasource across indexed and live modes"
pillar: Agent
status: backlog
design: docs/designs/datasource-discoverability.md
areas: [flux-capabilities, flux-datasource]
note: "Decision 0006 rule 2: the two contracts stay two clean traits; what merges is identity — `sources` reports both modes, so the agent can answer \"what do I know?\" completely"
---

# One datasource registry — enumerate every declared datasource across indexed and live modes

## Goal

Flux maintains one registry of declared datasources so a program or agent can enumerate every
datasource across both access modes. Today identity is split: `sources` (D-114) enumerates the
indexed backend only, while live domains are visible only as `<domain>.list`/`<domain>.get` pairs in
the catalog — there is no single answer to "which datasources are declared here?".

## Acceptance

- [ ] A registry seam holds every declared datasource with its name and access mode (*indexed* |
      *live*), populated by indexed registration, `try_register_live_datasource`, and the SDK seams
      — registration remains an owner decision; the registry adds no runtime registration op.
- [ ] `sources` reports both modes: indexed sources keep their entity/record-count rows; live
      domains appear with their declared entities and mode instead of a record count. Failing-first
      test: a host with one indexed source and one live domain sees both in one `sources` result;
      today it sees only the indexed source.
- [ ] Harness history remains a selector feature of the index, not a third mode, and the registry
      does not invent one.
- [ ] The two traits (`DatasourceBackend`, `LiveDatasource`) are unchanged — this story merges
      identity, not contracts.
- [ ] `website/docs/agent/datasources.md` documents the unified enumeration where it documents
      `sources` today.
- [ ] Standard gate green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-04 by C-514 from Decision 0006 rule 2. This resumes the registry direction of
  [datasource-discoverability.md](../designs/datasource-discoverability.md) (see its 2026-08-04
  disposition note) with the scope widened from "indexed sources" to "declared datasources across
  both modes".
- Once D-252 lands (Exchange-bound and catalogue kinds), those declarations enumerate through the
  same registry — the registry is the seam that makes `sources` complete, whatever declares the
  datasource.
