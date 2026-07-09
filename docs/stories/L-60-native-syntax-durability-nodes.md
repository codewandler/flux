---
id: L-60
title: Native syntax — Memo / Once / Checkpoint / Await (durability & idempotency)
pillar: Language
status: done
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
- Done 2026-07-09: native syntax landed in the semantic parser (`parse.rs` + `format.rs`) — the
  proven path where the AST/analyzer/round-trip harness already live (the CST powers the LSP).
  `memo $x[: T] = expr` (with `@effect` via `set_effect`), `once "label" [-> $bind]` + body,
  `checkpoint "label"`, `await [$b[: T] =] "source"`. Guard: an `await` `as_type` without a binding
  has no native spelling → stays `@json`. `durability_nodes_round_trip_natively` + the random
  round-trip **property test** pass; the `json_fallback` guard test migrated to dotted (permanently
  `@json`) names. Full flux-lang suite green; clippy + fmt clean.

## Notes
- **Gated** on isolation. Depends on **L-59**. Surfaces are proposals — confirm against
  [flux-lang-cst.md](../designs/flux-lang-cst.md) "Proposed surfaces" before implementing.
- `Memo` already has `@effect` plumbing (`parse.rs:2220`).
