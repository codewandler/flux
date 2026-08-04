---
id: L-133
title: "Typed task result contracts at the Flux boundary"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-orchestrate, flux-lsp]
note: "A task returns a validated Flux value or a typed validation failure, independent of provider-native structured output"
---

# Typed task result contracts at the Flux boundary

## Goal

Allow a task call to declare the Flux type of its result. Provider adapters may optimize with native
structured-output support, but every provider must present the same language-level behavior: either
a value validated against the declared contract or a typed validation failure with useful paths and
evidence.

## Acceptance

- [ ] The design records the relationship between a Flux type, provider request hints, raw response,
      normalization, validation, trace events, redaction, and the value bound in the program.
- [ ] Failing-first tests use at least two provider capabilities (native structured output and plain
      text/JSON fallback) and prove identical observable Flux semantics for valid and invalid output.
- [ ] A malformed result produces structured validation data with field paths and expected/actual
      categories; it is not collapsed into a provider error string or mistaken for a valid negative
      result.
- [ ] Analyzer diagnostics reject unknown or unusable result types before execution; formatter,
      syntax docs, generated artifacts, LSP, and editor mirrors cover the accepted annotation form.
- [ ] Traces identify the requested Flux contract and validation outcome without persisting secrets
      or provider-specific schema internals beyond existing event policy.
- [ ] A provider-neutral example obtains a typed assessment and consumes its fields without a manual
      `parse` plus assertion chain.

## Progress

- 2026-08-05: Proposed as the language surface over the optional structured task-output runtime seam
  described by the shipped D-05 sub-agent hardening design.

## Notes

- Illustrative syntax:

  ```flux
  record Assessment {
    verdict: "accept" | "revise"
    reasons: List<String>
  }

  assessment = task(
    role: "assessor"
    input: { artifact, criteria }
  ) as Assessment
  ```

- The declared type is authoritative. Native provider schema support is an optimization, never a
  semantic fork and never required for portability.
- Coordinate with [sub-agent hardening](../designs/sub-agent-hardening.md); do not create a second
  task execution or response-normalization path.
