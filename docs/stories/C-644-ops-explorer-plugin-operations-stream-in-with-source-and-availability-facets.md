---
id: C-644
title: "Ops explorer: plugin operations stream in, with source and availability facets"
pillar: "Core"
status: ready
epic: ops-explorer
areas: [flux-tui, flux-cli]
design: docs/designs/operations-explorer-epic.md
note: "depends on C-643 landing; do not dispatch in the same wave as C-643"
priority: 36
---

# Ops explorer: plugin operations stream in, with source and availability facets

## Goal

The explorer shows the whole installed surface, not just built-ins: plugin-projected operations
(manifest loading is subprocess-based, so potentially slow and partial) stream into the catalog
asynchronously after the instant built-in rows, each carrying a source badge; the filter grows
beyond category to risk, effects, and source facets, and rows disclose availability/placement
rather than pretending everything is callable.

## Acceptance

- [ ] The `Vec<OpRow>` hand-off from C-643 is generalized to a source trait (the
      `FleetBoardSource`-style injection seam in flux-tui) so plugin rows can arrive after
      startup; built-in rows still render instantly and the UI never blocks on a plugin
      subprocess. A slow or failing plugin degrades to a visible partial-catalog notice, never a
      hang or a crash (failing-first test with a stub source that delays and errors).
- [ ] Plugin ops appear with their projected names (`projected_name` semantics: qualification,
      `public_name` overrides) and a source facet distinguishing core / web / plugin(name); the
      detail pane shows plugin extras when present (secret purposes, redaction fields, platform
      sourcing/reach).
- [ ] Filters compose: category × risk × effects × source, all cycle-able from the keyboard and
      all visible in the header; empty-result states are explicit (test pinning a composed
      filter).
- [ ] Availability is honest: ops whose group/evidence gating or placement makes them currently
      unavailable are labelled so (via the executor's unavailability derivation), with a test
      pinning at least one unavailable representative.
- [ ] Workspace gate green; WHATS-NEW.md updated.

## Progress

- 2026-08-06 filed.

## Notes

- Depends on C-643 (module, DTO, entry point). Do not schedule into the same wave.
- Async snapshot precedent: the Board/Fleet overlay's spawned snapshot + UiEvent delivery.
