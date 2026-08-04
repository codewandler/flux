---
id: L-140
title: "First-class Option and Result values for recoverable control flow"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-runtime, flux-lsp]
note: "Unify lenient access, optional reads, validation failures, and settled branches under typed exhaustive control flow"
---

# First-class Option and Result values for recoverable control flow

## Goal

Give absence and recoverable failure explicit types and exhaustive control flow. Unify lenient
access, optional reads, validation outcomes, and settled parallel branches so programs no longer use
sentinel strings, null ambiguity, or fatal assertions for expected alternatives.

## Acceptance

- [ ] A design note inventories existing `?`, null, optional lookup, parse/read failure, retry,
      validation, and branch-result behavior, then fixes one coherent `Option<T>` / `Result<T, E>`
      model and migration path.
- [ ] The exact constructor, propagation, defaulting, access, and exhaustive-match spellings are
      decided with L-111 so Flux gains one canonical conditional/pattern vocabulary.
- [ ] Failing-first analyzer/runtime tests cover `some`/`none`, `ok`/`err`, nested types, propagation,
      defaulting, exhaustive and unreachable arms, discarded errors, serialization, and source paths.
- [ ] Effect failure becomes a `Result` only at an explicit recovery boundary; fatal invariants,
      cancellation, denied approval, and process-level safety failures cannot be accidentally swallowed.
- [ ] L-134 validation recovery and L-137 settled fan-out use the same result representation rather
      than bespoke status records.
- [ ] Formatter round-trip, generated artifacts, syntax docs, LSP exhaustiveness diagnostics, and
      every editor mirror cover the accepted forms.
- [ ] A provider-neutral example handles an optional configuration read and a recoverable typed
      parse without sentinel values.

## Progress

- 2026-08-05: Proposed as the common data model for expected absence/failure across the epic.

## Notes

- Illustrative syntax:

  ```flux
  config = read?("config.json")
  match config
    some text
      parsed = parse?(text) as Config
      match parsed
        ok value
          use(value)
        err problem
          report(problem)
    none
      use(default_config)
  ```

- The story must settle whether `?` constructs an option/result, propagates one, or remains lenient
  access syntax. One token must not carry several context-dependent meanings without analyzer clarity.
