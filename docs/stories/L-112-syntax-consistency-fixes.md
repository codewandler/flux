---
id: L-112
title: "Syntax consistency batch — verify's message, durations everywhere, named-input order"
pillar: Language
status: backlog
priority: 42
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang]
note: "P10 — verify grows assert's comma-message; bare-ms numbers deprecated in time positions; decide author-order vs alphabetical named inputs"
---

# Syntax consistency batch — verify's message, durations everywhere, named-input order

## Goal

Three small inconsistencies with outsized teaching cost: `verify … contains "x": "message"` uses a
colon-suffix message while its sibling `assert` uses a comma; time positions accept bare
milliseconds (`timeout 30000`) alongside durations (`30s`, `500ms`); and the formatter silently
alphabetizes named inputs because call args live in a sorted map — `grep(pattern:…, glob:…)`
round-trips as `grep(glob:…, pattern:…)` (reproduced at 0.45.0).

## Acceptance

- [ ] `verify <call> contains <expect>, "message"` parses and is what `format` emits; the colon
      form joins the L-106 deprecation table.
- [ ] Bare-number time operands join the deprecation table; `format` already emits durations —
      add a test pinning that every canonical time spelling carries a unit.
- [ ] Named-input ordering is *decided*: either `Call` inputs become an order-preserving map (wire
      compat test for JSON round-trip) or alphabetical is blessed in the spec and `fluxlang fmt`
      applies it — the current silent reorder disappears either way, with a test.

## Progress
-

## Notes

- Ordering decision leans author-order: diffs stay minimal and `fmt` stops rewriting lines the
  author didn't touch; the cost is a serde structure change on a wire type — check
  protocol-line implications before choosing.
