---
id: A-84
title: Validate the adaptive family ceiling on resume
pillar: Agent
status: done
design: docs/designs/resumed-adaptive-family-ceiling.md
note: "A serialized state created before A-83 can already contain five small families and bypass the signal-time guard when exploration resumes."
---

# Validate the adaptive family ceiling on resume

## Goal

Make the four-family adaptive catalog limit a state-entry invariant, including deserialized and
resumed state created before the cumulative signal guard existed.

## Acceptance

- [x] Failing-first: `resumed_adaptive_state_rejects_fifth_family_before_catalog_expansion`
      deserializes a state with five distinct one-operation families and proves the current resume
      path accepts it before the fix.
- [x] Every adaptive catalog expansion validates the deduplicated family set before operation
      schemas are selected or used.
- [x] Resumed states with at most four distinct live families retain their existing behavior,
      including duplicate serialized names that do not widen the active set.
- [x] A-83's signal-time cumulative guard and semantic-expansion regressions remain green.
- [x] Focused `flux-flow` tests, formatting, clippy, and the diff whitespace check pass.

## Progress

- 2026-07-14 — release audit found that A-83 guarded only new signal mutation: a durable state
  already carrying five families still reached `selected_specs_for_state` and expanded when its
  independent operation/schema budgets were small enough.
- 2026-07-14 — the failing-first regression returned five selected operation specs. The shared
  expansion boundary now rejects five distinct families first; the resume, signal-expansion, and
  focused crate regressions pass.

## Notes

- Published `v0.23.0` predates this release-blocking correction, so the immutable fix ships in
  `v0.23.1`.
