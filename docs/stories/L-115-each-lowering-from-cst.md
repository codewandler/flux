---
id: L-115
title: "Lower `each` headers from CST structure, not reconstructed text"
pillar: Language
status: in-progress
priority: 4
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "Review F2, HIGH — `each x in \"a->b\"` is a parse error and format emits un-reparseable output; header.split_once(\"->\") sees string content"
---

# Lower `each` headers from CST structure, not reconstructed text

## Goal

`each` lowering rebuilds its header as text and splits on the first `"->"` substring
(`cst_decode.rs:393-407`), so any string containing `->` in the source position breaks parsing —
`each part in split(text, "->")` is rejected (reproduced at 0.45.0) — and `format` emits
un-reparseable output for `Each { source: Lit("a->b") }`, violating round-trip totality. Lower
from the CST's token structure instead; the tree already knows whether a top-level `ARROW` exists.

## Acceptance

- [x] Failing-first: `each x in "a->b"` and `each part in split(text, "->")` parse; `-> collect`
      and `-> flat collect` forms still lower correctly, including with `->` inside the source
      expression's strings.
- [x] Round-trip: `parse(format(ast)) == ast` for `Each` sources containing `->`; the property
      pools gain `->`-bearing strings (`tests/roundtrip_property.rs` STRINGS/OPS pools) so the
      class stays dead — and a sweep for other `semantic_line`-based lowerings with the same
      pattern (grep `split_once`/`contains` over reconstructed text in cst_decode.rs) either
      clears them or files follow-ups.
- [x] The misleading `each collect` diagnostics disappear for these inputs.

## Progress

- `lower_each` reads its header from the statement's **direct** token children
  (`cst_decode.rs`'s new `direct_header_tokens`), with child nodes opaque: the `ARROW` the parser
  emitted is the only arrow it can see, and a `->` inside the source expression's strings is now
  structurally invisible. `semantic_line`/`split_once("->")` are gone from that function, along
  with the `has_arrow && flat` bookkeeping they forced.
- `flat` is read as an `IDENT` token equal to `flat`; because `flat` is a reserved word, a collect
  target *named* `flat` can only be spelled `$flat` (a `VAR` token), so the two stay distinguishable
  without any text scan. Both spellings are pinned.
- The **sweep is clear** — `each` was the only offender. Every other `split_once`/`contains` over
  reconstructed text in `cst_decode.rs` splits on the *first* token of a header or on a delimiter
  that provably precedes any string operand: `lower_ctx_append` (`+=`, rhs is symbols only),
  `lower_scope` / `lower_await` (`=`, and the formatter refuses the bind-less `scope` shape so the
  binding always precedes the first `=`), `lower_bind` (`=` searched only inside a `:`-type, which
  has no strings), `parse_params` (`:` over header types), `parse_decl_header` (`->` return type,
  no strings in a flow header). The two other headers that *do* put a string or JSON operand before
  an arrow — `once "…" -> $b` and `with_tools […] -> $b` — already consume the operand with
  `parse_string_prefix`/`parse_setting_prefix` and then `strip_prefix("->")` the tail, so they were
  never scanning. Verified by parsing `once "a->b" -> r`, `with_tools ["a->b"] -> x`,
  `scope h = lock("a=b")`, `await r = "a=b"` and `x: String = "a=b"` at the merge base: all `Ok`.
  No follow-ups owed.
- Corpus: all 21 `.flux` modules in the tree (including `crates/flux-flow/assets/agent-loop.flux`
  and `examples/`) parse; the fenced `each` headers under `docs/`, `website/docs/`,
  `crates/flux-lang/docs/syntax.md` and `crates/flux-markdown/tests/corpus/` are all plain
  `$var`/ident sources with `-> c` / `-> flat c`, which the new test table pins. The change only
  *widens* what is accepted, so nothing in the corpus could regress.

## Notes

- Suggested fix: match on the header's token-level `ARROW` outside string tokens (the CST is
  lossless; `cst_decode.rs`'s own module header disclaims text reconstruction — make it true).
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F2.
