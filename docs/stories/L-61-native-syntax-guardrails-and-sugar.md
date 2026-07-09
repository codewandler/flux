---
id: L-61
title: Native syntax — Confirm / Throttle / Debounce / Verify + Peek / Parse
pillar: Language
status: done
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "@json coverage 2/4 — GATED on isolation. Native text for the guard-rail nodes plus the peek/parse expression sugar (parse( needs a fmt(-style special-case)."
---

# Native syntax — Confirm / Throttle / Debounce / Verify + Peek / Parse

## Goal
Native text for the guard-rail `@json`-only nodes and the two expression-sugar nodes, per the
reviewed CST proposals, each across CST production + `format` arm + `cst_to_draft` lowering.

## Acceptance
- [ ] `confirm "message" [risk high]` + body; `throttle "name" <max> per <window_ms>` + body;
      `debounce "name" <wait_ms>` + body; `verify <cmd> contains <expect> [: "message"]`.
- [ ] `peek $name` (inline sugar); `parse(<value>, as: "f64")` — special-cased in the expr parser like
      `fmt(` (`parse.rs:1990`) so it does **not** lower to a `Call` to op `parse`.
- [ ] Failing-first per node: round-trips natively, text contains **no** `@json`; a
      `parse_does_not_collide_with_op_call` test pins the special-case.

## Progress
- Done 2026-07-09 (parse.rs + format.rs): `confirm "msg" [risk <level>]` + body,
  `throttle "name" <max> per <window>` + body, `debounce "name" <ms>` + body,
  `verify <cmd> contains <expect> [: "msg"]`, and the expression sugar `peek $x` +
  `parse(v, as: "T")` (special-cased in the atom parser like `fmt(`). Guards keep round-trip total:
  `confirm` renders `risk` only when it's a bare word; `verify` escapes to `@json` when an operand is
  a native-`Expr` or itself `@json`; `parse`/`peek` guard on `@json`/identifier. `guardrail_and_sugar_
  nodes_round_trip_natively` + the random property test pass; the `json_escape_line…` guard test moved
  to a permanently-`@json` newline name. Full suite green; clippy + fmt clean.

## Notes
- **Gated** on isolation. Depends on **L-59**. Confirm surfaces against
  [flux-lang-cst.md](../designs/flux-lang-cst.md).
