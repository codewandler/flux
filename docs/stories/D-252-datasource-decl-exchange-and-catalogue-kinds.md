---
id: D-252
title: "DatasourceDecl kind extension — a tenant Exchange Datasource binding and the compiled-in connectors catalogue"
pillar: Agent
status: backlog
areas: [flux-lang, flux-cli, flux-capabilities]
note: "Decision 0006 rule 3: Flux-Lang declares every datasource — the `datasource` decl keeps its indexed program-local kinds and gains two new ones; unknown kinds stay hard errors, registration stays an owner decision"
---

# DatasourceDecl kind extension — Exchange-bound and connectors-catalogue kinds

## Goal

The `datasource` declaration keeps its indexed program-local kinds (`markdown`, `openapi`) and gains
two: a kind naming a **tenant Exchange Datasource binding** by connection label, and a kind binding
the **compiled-in connectors catalogue**. With this, Flux-Lang declares every datasource — the
Decision 0006 rule that makes the registry (D-250) complete and keeps registration an owner
decision, never a runtime model decision.

## Acceptance

- [ ] The `datasource` declaration accepts a kind that names a tenant Exchange Datasource binding by
      connection label; at startup it binds through the embedded Exchange client's existing live
      registration seam. Exchange unavailable means that datasource is unavailable — no local vendor
      adapter, no local index fallback (0006 rule 8). Program-local indexed kinds are unaffected.
- [ ] The declaration accepts a kind binding the compiled-in connectors catalogue, and that binding
      is **indexed** mode (0006 rule 9) — the catalogue is a local compiled dataset, and a live
      binding would cost the search surface.
- [ ] Unknown kinds remain hard startup errors naming the kinds that exist — the misspelled-kind
      rule already documented for `markdown`/`openapi` extends to the new kinds. Failing-first test
      per new kind.
- [ ] Registration remains an owner decision: no runtime op registers a datasource; the SDK seam
      remains the embedder path.
- [ ] Exchange-bound datasources are validated once at registration against the effective
      catalogue's datasources section (0006 rule 12: declared surfaces are enforced, not
      decorative).
- [ ] The exact kind spellings are decided in this story against the Flux-Lang naming conventions
      and recorded here before implementation.
- [ ] `website/docs/agent/datasources.md` and `website/docs/agent/programs.md` document the new
      kinds when they ship.
- [ ] Standard gate green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-04 by C-514 from Decision 0006 rules 3, 8 and 9.
- Sequencing: the Exchange-bound kind consumes the effective-catalogue datasources section
  (flux-exchange X-113 growth) and the connector datasource surface (flux-connectors, Milestone 2
  runtime-declaration work) — this story does not precede them. The catalogue kind depends on the
  connectors catalogue datasource stories (`connectors/C-137`…`C-140`, amended per 0006).
- The board is not a datasource kind: `kind "board:*"` retires under L-130 in the
  first-class-board epic, not here.
