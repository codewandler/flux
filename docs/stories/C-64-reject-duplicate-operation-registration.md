---
id: C-64
title: Reject duplicate operation registration
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: ToolRegistry and PluginBuilder silently replace handlers while catalogs may retain conflicting specs
---

# Reject duplicate operation registration

## Goal

Make operation identity unique and auditable so a plugin/custom pack cannot silently replace a
built-in tool or pair one manifest specification with another handler.

## Acceptance

- [x] `ToolRegistry` registration returns a path-aware/source-aware duplicate error; identical and
      differing duplicate specifications are both rejected.
- [x] Intentional replacement, if required by a real caller, uses a separately named explicit API
      whose call sites document why replacement is safe.
- [x] `PluginBuilder` rejects duplicate operation names before serving: its manifest and handler map
      cannot disagree through last-wins insertion.
- [x] Failing-first tests cover built-in versus custom/plugin collision, two installed plugins
      projecting the same public name, duplicate manifest specs, and duplicate handlers with
      different risk/effect metadata.
- [x] Every registry-assembly call site handles registration failure without silently dropping an
      operation; catalogs and group membership remain deterministic.
- [x] SDK/API compatibility and error propagation are documented, with a migration note if the
      registration signature changes publicly.

## Progress

- 2026-07-14 — Made tool/plugin registration source-aware, fallible, atomic, and duplicate-rejecting,
  with explicit `replace_from` for reviewed replacement. Runtime, plugin, SDK, catalog, and
  first-party plugin collision suites cover identical and conflicting names.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- C-68 should build its typed plugin registration API on this fail-closed identity seam.
