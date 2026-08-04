---
id: L-139
title: "Structured task input and explicit context references"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-orchestrate, flux-runtime, flux-lsp]
note: "Pass typed values and provenance-bearing context to roles without flattening everything into an interpolated prompt"
---

# Structured task input and explicit context references

## Goal

Let task calls carry a structured input value and explicit context references in addition to a prose
task string. Preserve types, provenance, redaction, and deterministic context assembly through the
runtime boundary instead of forcing every value through prompt interpolation.

## Acceptance

- [ ] A design note fixes the task call shape, `task:` compatibility, context reference kinds,
      ordering, size/token limits, duplicate handling, missing resources, redaction, serialization,
      and provider adaptation.
- [ ] Structured values remain structured until the provider adapter; tests prove provider-specific
      rendering cannot change the Flux-visible input contract or reorder context silently.
- [ ] File/context references resolve through existing capabilities and workspace confinement, emit
      provenance in the trace, and cannot smuggle an unapproved read through prompt construction.
- [ ] Failing-first tests cover a nested input, multiple context kinds, missing/denied context,
      secret-bearing fields, size exhaustion, cancellation, and a provider with no native multipart
      or structured-input feature.
- [ ] Analyzer/type diagnostics, formatter round-trip, generated artifacts, syntax docs, LSP, and
      editor mirrors cover every accepted input/context form.
- [ ] A provider-neutral example asks a planner for a typed plan using a structured request and one
      policy file, with no vendor-specific message or schema syntax.

## Progress

- 2026-08-05: Proposed to make task data and context inspectable program structure rather than an
  opaque convention inside prompt strings.

## Notes

- Illustrative syntax:

  ```flux
  plan = task(
    role: "planner"
    input: {
      request
      constraints
      prior_decision
    }
    context: [file("POLICY.md"), value(evidence)]
  ) as ChangePlan
  ```

- `file(...)` in this example is a provenance-bearing context reference, not an ambient filesystem
  escape. Resolution must pass through the same guarded IO path as an explicit read.
