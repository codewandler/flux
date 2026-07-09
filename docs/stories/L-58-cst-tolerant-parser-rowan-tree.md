---
id: L-58
title: Tolerant event parser + rowan green tree with ERROR-node recovery
pillar: Language
status: done
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "CST foundation — GATED on isolation. The error-recovery core: a hand-written event parser that always completes a tree, wrapping unexpected input in ERROR nodes and resyncing at NEWLINE/DEDENT."
---

# Tolerant event parser + rowan green tree with ERROR-node recovery

## Goal
Build the tolerant parser stage: a hand-written recursive descent over the L-57 token stream that
emits start/token/finish/error events, builds a **rowan** green tree, and **always completes a tree**
— wrapping unexpected input in `ERROR` nodes and resyncing at the next `NEWLINE`/`DEDENT` so a live,
incomplete buffer still yields a full CST plus a diagnostic list.

## Acceptance
- [x] New `crates/flux-lang/src/parser.rs` (named `parser` not `cst`) — a hand-written tolerant
      recursive descent building a **rowan** `GreenNode` directly (equivalent to the event model) +
      `parse_cst(src) -> Parse { green, errors }`. `SyntaxNode`/`SyntaxToken` typed aliases in
      `syntax.rs`; per-kind typed accessor wrappers land with the lowering in L-59.
- [x] Every node/token carries a `TextRange` (structural — rowan tree).
- [x] `recovers_and_reports_multiple_errors`: a buffer with independent broken lines produces one
      complete, lossless tree, reports the errors, and later good statements still parse (a
      `BIND_STMT` survives after two bad lines).
- [x] `tree_is_lossless` + `clean_flow_has_no_errors`: the tree round-trips byte-for-byte; a valid
      flow parses with zero errors. Resync is at `NEWLINE`/`DEDENT` (block boundaries).

## Progress
- Done 2026-07-09: `parser.rs` — cursor over the L-57 token stream (trivia fed to the tree,
  INDENT/DEDENT/NEWLINE drive structure), rowan `GreenNodeBuilder`, checkpoint-based node wrapping,
  precedence-climbing expressions, and productions for the full flow grammar incl. the 16 new
  keywords (memo/once/checkpoint/await/confirm/throttle/debounce/verify/peek/parse/try/race/scope/
  saga/pipe/thing) so their syntax parses now; native **lowering** of the new nodes is L-60..L-63.
  Module-level non-flow decls captured as opaque `DECL` nodes (scope note in the design). 3 parser
  tests + 285 lib tests green; clippy + fmt clean. **Grammar fidelity is proven at L-59** (re-point
  `parse` → the existing 282-test oracle + round-trip invariant). **Uncommitted.**

## Notes
- Depends on **L-57**. Named `parser.rs`; hand-written recursive descent rather than a separate
  event stream (same result — a rowan tree — with less indirection).
- Reproduces the current native grammar; the 16 `@json`-only nodes parse but lower via `@json` until
  L-60–L-63 add their native lowering. Behaviour-preserving lowering is L-59.
