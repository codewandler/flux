---
id: L-132
title: "Structural records, enums, and bounded refinements"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-lsp]
note: "Name recurring protocol shapes locally: records, optional fields, lists, finite values, and deterministic refinements"
---

# Structural records, enums, and bounded refinements

## Goal

Let a Flux program name and compose the data contracts it already exchanges. Cover the common
protocol shapes—records, optional fields, lists, finite string values, nesting, and simple
deterministic constraints—without requiring a provider schema language or arbitrary executable code
inside a type.

## Acceptance

- [ ] A design note fixes the declaration syntax, type identity rules, generic/type-parameter scope,
      optional-field semantics, refinement vocabulary, diagnostics, AST/wire representation, and
      compatibility behavior before implementation.
- [ ] Failing-first parser/analyzer tests cover duplicate declarations, unknown types, missing and
      excess fields, nested paths, enum mismatch, optional fields, list members, and failed
      refinements with source-located diagnostics.
- [ ] Runtime validation returns structured path/expected/actual diagnostics; it never depends on a
      provider's native schema implementation.
- [ ] Parse/format/parse equivalence, generated AST artifacts, syntax docs, and all maintained editor
      grammar mirrors cover the accepted declaration and annotation forms.
- [ ] A provider-neutral example validates a generic quality check such as the one below, including
      at least one nested collection and one failed refinement test.
- [ ] The implementation cannot invoke operations, read secrets, or bypass the ordinary effect and
      approval envelope from a refinement.

## Progress

- 2026-08-05: Proposed as the type vocabulary on which L-133 and the rest of the epic can depend.

## Notes

- Illustrative syntax; the design decision may choose literal unions instead of a separate `enum`
  declaration and may defer generics if they distort inference:

  ```flux
  enum State = "pending" | "ready" | "failed"

  record Message {
    code: String
    text: String
  }

  record Check {
    state: State
    score: Number where 0 <= self <= 1
    messages: List<Message>
    owner?: String
  }
  ```

- Start with refinements whose validation is total, deterministic, bounded, and representable in
  the analyzer. Regex or arbitrary predicates need separate justification.
