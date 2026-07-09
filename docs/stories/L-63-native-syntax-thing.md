---
id: L-63
title: Native syntax — Thing (kind + selector grammar)
pillar: Language
status: backlog
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
- (not started — gated on isolation; depends on L-59)

## Notes
- **Gated** on isolation. Depends on **L-59**. Cleanly **deferrable** — the epic can ship native
  coverage for the other 15 without this. Distinct from the deferred *NL Thing resolution* (Name/Query
  selectors need a host `ThingResolver`); this is syntax only.
