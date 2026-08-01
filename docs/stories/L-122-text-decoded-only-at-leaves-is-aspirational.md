---
id: L-122
title: "`cst_decode`'s \"token text is decoded only at leaves\" is a claim, not an enforced invariant — ~13 `semantic_line` sites still scan reconstructed text"
pillar: Language
status: ready
priority: 16
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "found by L-115's sweep. L-115 fixed `each`; the module header already claims the property for the whole module. The sharpest remaining case is `lower_scope`'s split_once('='), which is safe ONLY because a formatter guard three files away refuses to spell the shape that would break it — neither side says so"
---

# "Decoded only at leaves" is asserted in prose, not enforced

## Goal

Make `crates/flux-lang/src/cst_decode.rs`'s stated invariant true, or stop stating it.

[L-115](L-115-each-lowering-from-cst.md) fixed `each` headers, which were lowered by scanning
reconstructed header *text* — so a `->` inside a string literal was read as the collect arrow and
`each x in "a->b"` failed to parse. Its sweep found the same *shape* at roughly 13 other
`semantic_line` call sites. All are safe today. The module header, however, already claims the
property for the whole module, and a claim that holds by luck at 13 sites is the condition L-115
existed to remove.

⚠ **The sharpest case is a non-local coupling.** `lower_scope` uses `split_once('=')` on header
text. That is safe *only* because `crates/flux-lang/src/format.rs:1064` refuses to spell a bind-less
`scope <acquire>` — it enforces `bind.is_some() == acquire.is_some()` and otherwise falls back to
`@json`. So the lowerer's correctness depends on a formatter guard three files away, and **neither
side documents the dependency**. Relax that guard and `scope a("x=y")` splits inside the string
exactly the way `each` did.

A second instance of the same shape: `parse_optional_arrow` (used by `repeat`, `seq`, `pipe`,
`timeout`, `budget`, `fallback`, `loop`, `race`, `retry`, `once`, `with_tools`) `strip_prefix`es the
arrow off a text tail. Safe today because every one of those consumes its operand structurally
first — but it is the same helper shape that made `each` wrong, and it carries no doc comment saying
why it is safe.

## Acceptance

- [ ] **Failing-first**: a test that breaks `lower_scope` by exercising the shape the formatter guard
      currently prevents (constructing the AST directly, not via source), demonstrating the coupling
      is real rather than theoretical.
- [ ] The `lower_scope` ↔ `format.rs:1064` coupling is either **removed** (lower structurally, as
      L-115 did for `each`) or **documented on both sides**, each naming the other. A test should
      fail if the formatter guard is relaxed without the lowerer being fixed.
- [ ] `parse_optional_arrow` either reads structurally or carries a doc comment stating the precondition
      that makes it safe — that every caller consumes its operand structurally first.
- [ ] The module header's claim is made accurate: either the remaining `semantic_line` sites are
      converted, or the header says plainly which sites are structural and which are text-scanning
      and why that is currently safe. **No aspirational invariant left asserted as fact.**
- [ ] Ideally a check that fails when a *new* `semantic_line` call appears without justification —
      the same "scanner test verified to fire" shape C-391/C-392 used. Decide whether that is
      proportionate here and say so either way.
- [ ] Full gate green, including `bash scripts/build-portable-wasm.sh`.

## Notes

- L-115's precedent for the fix shape: read the whole construct's header structurally (item, keyword,
  arrow, modifier, target) rather than half of it. A half-structural header invites the next reader
  to scan the rest again.
- L-115's own risk note applies to any structural rewrite here: a token-level walk that stops at
  NEWLINE/COMMENT is blind to a token hidden inside a *nested* node, so a future grammar change that
  wraps a keyword in a node makes it silently invisible rather than an error. Pair any such walk with
  the grammar it reads.
- Related: [L-121](L-121-tree-walks-bounded-only-by-assumption.md) is the same theme on a different
  axis — a property that holds only because of an assumption about callers.

## Progress

- Filed 2026-08-01 from L-115's sweep, which deliberately fixed only `each` and reported the rest
  rather than widening its own scope.
