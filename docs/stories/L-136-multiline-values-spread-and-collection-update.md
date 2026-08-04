---
id: L-136
title: "Readable multiline values, spread, and collection updates"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-lsp]
note: "One canonical way to build and evolve large records/lists; coordinate with L-110 so braces remain one value model"
---

# Readable multiline values, spread, and collection updates

## Goal

Make large structured values readable and composable without temporary JSON strings or repeated
whole-object reconstruction. Add one indentation-friendly multiline form, record/list spread, and
clear append/update operations on local values, coordinated with L-110's unified value-template
model.

## Acceptance

- [ ] A design note fixes separators/newline rules, trailing delimiters, shorthand fields, spread
      precedence, duplicate-key diagnostics, mutation versus rebinding semantics, and ordering.
- [ ] L-110 is either complete or this story records a compatible shared lowering plan; no syntax
      reintroduces an author-visible `lit` versus template split.
- [ ] Failing-first parser/runtime tests cover nested multiline values, empty values, record and list
      spread, conflicting fields, collection append/update, invalid operand types, and evaluation
      order.
- [ ] Formatter idempotence and parse/format/parse tests pin comments, indentation, shorthand, spread,
      and stable member order; generated artifacts and every editor grammar mirror are updated.
- [ ] AST/event compatibility is explicit, and value construction remains pure—no spread or update
      can trigger hidden reads or operations.
- [ ] A provider-neutral example combines findings from two generic checks and appends a cycle to a
      history without serializing through JSON.

## Progress

- 2026-08-05: Proposed as the data-construction counterpart to L-135; explicitly sequenced with
  L-110 rather than introducing another object/list syntax family.

## Notes

- Illustrative syntax:

  ```flux
  cycle = {
    ...base
    state: "ready"
    findings: [...lint.findings, ...tests.findings]
  }

  history += cycle
  ```

- The design must decide whether `+=` mutates a local collection or is canonical sugar for rebinding.
  Trace and analyzer behavior should not depend on an implementation accident.
