---
id: L-115
title: "Lower `each` headers from CST structure, not reconstructed text"
pillar: Language
status: ready
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

- [ ] Failing-first: `each x in "a->b"` and `each part in split(text, "->")` parse; `-> collect`
      and `-> flat collect` forms still lower correctly, including with `->` inside the source
      expression's strings.
- [ ] Round-trip: `parse(format(ast)) == ast` for `Each` sources containing `->`; the property
      pools gain `->`-bearing strings (`tests/roundtrip_property.rs` STRINGS/OPS pools) so the
      class stays dead — and a sweep for other `semantic_line`-based lowerings with the same
      pattern (grep `split_once`/`contains` over reconstructed text in cst_decode.rs) either
      clears them or files follow-ups.
- [ ] The misleading `each collect` diagnostics disappear for these inputs.

## Progress
-

## Notes

- Suggested fix: match on the header's token-level `ARROW` outside string tokens (the CST is
  lossless; `cst_decode.rs`'s own module header disclaims text reconstruction — make it true).
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F2.
