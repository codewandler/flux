---
id: C-646
title: "Ops explorer: connector-ready catalog over the effective catalogue seam"
pillar: "Core"
status: ready
epic: ops-explorer
areas: [flux-tui, flux-cli]
design: docs/designs/operations-explorer-epic.md
note: "depends on C-643; re-scope against roadmap Milestone 1 (C-318/X-113) at pickup"
priority: 38
---

# Ops explorer: connector-ready catalog over the effective catalogue seam

## Goal

Connector-compiled operations appear in the explorer as first-class rows the moment an Exchange
binding provides them: the catalog consumes the connected-and-granted effective catalogue through
the declared seam (`platform` sourcing / vendor `reaches` on the operation contract), the source
facet distinguishes core / plugin / connector, and identity is disclosed (operation identity URLs)
— proving the DTO seam scales to the connector future without special-casing.

## Acceptance

- [ ] Connector/Exchange-provided operations render through the same `OpRow` path with a
      connector source facet and their platform/reach disclosures in the detail pane; a fixture
      source proves the rendering without a live Exchange.
- [ ] Rows disclose operation identity (the `$id` URL scheme used by the catalog exporter) where
      one exists; absence renders honestly rather than inventing one.
- [ ] Catalogue refresh between turns is respected: a changed effective catalogue is reflected on
      next entry into the explorer (no stale-forever cache), with a test over a source whose
      snapshot changes.
- [ ] Workspace gate green; WHATS-NEW.md updated.

## Progress

- 2026-08-06 filed.

## Notes

- Depends on C-643. **Re-scope at pickup** against the flux-roadmap Milestone 1 state — the
  effective-catalogue contract (flux C-318, exchange X-113) may land before this story is
  scheduled and changes what "consume the effective catalogue" concretely binds to.
- The connectors seam on the operation wire contract already exists (platform sourcing, vendor
  reach); this story must not invent a parallel vocabulary.
