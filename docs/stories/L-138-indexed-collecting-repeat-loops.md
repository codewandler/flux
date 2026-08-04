---
id: L-138
title: "Indexed, collecting repeat loops"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-runtime, flux-lsp]
note: "Expose iteration and prior output, collect yielded values, and reuse L-116's bounded repeat semantics"
---

# Indexed, collecting repeat loops

## Goal

Express bounded refinement and polling cycles without manually incrementing counters or rebuilding
history lists. Extend `repeat` with an index binding, explicit yielded value, optional previous value,
and collected result while retaining L-116's budget, cancellation, and trace discipline.

## Acceptance

- [ ] L-116's repeat/loop budget decision is complete or this story explicitly adopts its resulting
      semantics; no second loop engine or incompatible budget model is introduced.
- [ ] A design note fixes zero/one-based indexing, the type and first-iteration value of `previous`,
      `yield` requirements, result collection, `until` timing, early return, nested loops, and traces.
- [ ] Failing-first tests cover zero iterations, one and maximum iterations, early satisfaction,
      exhausted limit, missing yield, previous-value access, nested scope, cancellation, and budget
      exhaustion.
- [ ] The analyzer derives a stable collection type and rejects references to loop-only bindings
      outside their scope; an empty history has an unambiguous type.
- [ ] Formatter round-trip, generated AST artifacts, syntax docs, LSP, and editor mirrors cover the
      accepted index/yield/collection syntax.
- [ ] A provider-neutral example records the history of up to three attempts to improve a generic
      candidate and exposes the last successful cycle safely.

## Progress

- 2026-08-05: Proposed as concise surface syntax over the hardened bounded-repeat semantics, not as
  a relaxation of loop limits.

## Notes

- Illustrative syntax:

  ```flux
  repeat 3 as iteration, until: cycle.ready -> history
    candidate = improve(candidate, previous?.findings ?? [])
    cycle = check(candidate)
    yield { iteration, candidate, findings: cycle.findings, ready: cycle.ready }
  ```

- Decide whether `previous` is a keyword, an explicit header binding, or ordinary access on the
  collected sequence. Prefer the smallest form that remains obvious on the first iteration.
