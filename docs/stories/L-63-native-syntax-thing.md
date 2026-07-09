---
id: L-63
title: Native syntax — Thing (kind + selector grammar)
pillar: Language
status: done
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "@json coverage 4/4 — GATED on isolation. The heaviest/most structural node (kind-enum + selector); cleanly deferrable if scope needs trimming."
---

# Native syntax — Thing (kind + selector grammar)

## Goal
Give `Thing` (`ThingRef { kind, selector }`) native text — the last and heaviest `@json`-only node —
per the reviewed CST proposal, across CST production + `format` arm + `cst_to_draft` lowering.

## Acceptance
- [ ] `thing <kind> "<selector>"` for each self-identifying kind (e.g. `thing file "src/x.rs"`,
      `thing url "https://…"`, `thing id "PR-123"`); exact `ThingRef` kind-enum/selector variants
      confirmed against the type.
- [ ] CST production + native `format` arm + `cst_to_draft` lowering.
- [ ] Failing-first: a `Thing` round-trips natively (statement and inline positions), text contains
      **no** `@json`; the `@json` guard tests migrated off `Thing`.

## Progress
- Done 2026-07-09 (parse.rs + format.rs): native expression syntax `thing <kind> <selector> "<value>"`
  — kind ∈ {context/file/person/ticket/email/repo/dataset/calendar_event/url/secret} or
  `custom "<name>"`; selector ∈ {id/name/path/query/key}. Every `ThingRef` is spellable, so no guard
  needed. Native in expression position (op args, bind values — where references are used); a bare
  `thing` *statement* still uses `@json` (consistent with `fmt`/`jq` bare-statement rendering).
  `thing_references_round_trip_natively` + the random property test pass; full suite green; clippy +
  fmt clean. **Closes the @json coverage gap — all 16 formerly-@json-only node kinds now have native
  syntax.**

## Notes
- **Gated** on isolation. Depends on **L-59**. Cleanly **deferrable** — the epic can ship native
  coverage for the other 15 without this. Distinct from the deferred *NL Thing resolution* (Name/Query
  selectors need a host `ThingResolver`); this is syntax only.
