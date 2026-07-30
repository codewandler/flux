---
id: L-94
title: "Flux notation workbench — one AST, several readable projections (epic)"
pillar: Language
status: in-progress
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang, flux-cli]
note: "EPIC — Railflux output first; canonical named-option headers plus Glyph, Tape, S-Flux, and a deliberately deferred Railflux reader"
---

# Flux notation workbench — one AST, several readable projections (epic)

## Goal

Give the single Flux `DraftAst` several purpose-built projections without creating alternate
runtimes: finish the normal human syntax, ship Railflux as a terminal-first ASCII dataflow view,
and retain Glyph, Tape, and S-Flux as independently testable compact notations.

## Acceptance

- [ ] L-95 ships deterministic, total 7-bit Railflux output before any Railflux parser is built.
- [ ] L-96 makes call-like named options canonical on suitable control headers while old source
      remains accepted.
- [ ] L-97, L-98, and L-99 each prove their notation against the same triage fixture and the
      `DraftAst` round-trip contract, including a raw-node escape.
- [ ] L-100 remains dependent on the stabilized L-95 grammar and rejects ambiguous diagrams.
- [ ] `.flux` remains canonical Flux source; every alternate input surface requires an explicit
      notation selection and no runtime or safety-envelope behavior changes.

## Progress

- 2026-07-30: Epic opened from the notation brainstorm. User selected Railflux output as the first
  and most important deliverable; parsing it is intentionally later.

## Notes

- L-93 is the completed foundation: bare locals, direct calls, named inputs, punning, indexes, and
  durations are already canonical. L-96 is a focused completion, not another syntax rewrite.
- Delivery order and representative forms are fixed in the design.
