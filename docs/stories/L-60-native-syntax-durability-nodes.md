---
id: L-60
title: Native syntax — Memo / Once / Checkpoint / Await (durability & idempotency)
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "@json coverage 1/4 — GATED on isolation. Native text for the single-header durability/idempotency nodes so they stop round-tripping via @json."
---

# Native syntax — Memo / Once / Checkpoint / Await (durability & idempotency)

## Goal
Give the four durability/idempotency `@json`-only nodes real native text (per the reviewed proposals
in the CST design doc), each implemented across CST production + `format` arm + `cst_to_draft`
lowering.

## Acceptance
- [ ] `memo $x[: T] = <expr>` (optional `@effect(tag)` line above, like `bind`).
- [ ] `once "label" [-> $bind]` + body; `checkpoint "label"` (top-level one-liner); `await [$b[: T] =]
      "source"`.
- [ ] Each: CST production + native `format` arm + `cst_to_draft` lowering.
- [ ] Failing-first per node: a constructed AST round-trips natively and the text contains **no**
      `@json`.
- [ ] `@json` guard tests migrated off `Once` (to a degenerate-shape example).

## Progress
- (not started — gated on isolation; depends on L-59)

## Notes
- **Gated** on isolation. Depends on **L-59**. Surfaces are proposals — confirm against
  [flux-lang-cst.md](../designs/flux-lang-cst.md) "Proposed surfaces" before implementing.
- `Memo` already has `@effect` plumbing (`parse.rs:2220`).
