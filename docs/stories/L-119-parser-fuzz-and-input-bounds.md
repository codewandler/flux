---
id: L-119
title: "Raw-text fuzzing for the parser front-end, and input-size bounds"
pillar: Language
status: ready
priority: 18
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "Review F12 — every property test generates ASTs; the tolerant-recovery text paths (the LSP's value proposition) see only hand-written cases; no size bound before as-u32 casts"
---

# Raw-text fuzzing for the parser front-end, and input-size bounds

## Goal

All existing property tests generate *ASTs* and round-trip them; no test feeds the lexer/CST
random or mutated raw text, so the tolerant-recovery paths are exercised only by hand-picked
cases, and there is no input-size bound before offsets are cast `as u32`
(`lexer.rs:313-315`). Add a raw-text robustness lane: for arbitrary input bytes, tolerant parse
never panics/aborts, losslessness holds (reconstructed text == input), and strict parse either
succeeds or returns a located error.

## Acceptance

- [ ] A seeded raw-text generator/mutator (same no-new-dev-deps philosophy as
      `roundtrip_property.rs`'s xorshift — or a `cargo-fuzz` target if a real fuzz lane is
      preferred; decide in the story) drives ≥10k inputs per run through `parse_cst` +
      `lower`: mutated corpus files, random indentation, stray sigils, unpaired `"""`, mixed
      CRLF, unicode, and F1's nested-statement shapes.
- [ ] Invariants asserted per input: no panic, CST losslessness, error ranges within input bounds.
- [ ] Input larger than a documented bound (e.g. 16 MiB) is refused with a clear error before
      lexing — failing-first test; the bound is far above any legitimate `.flux` file and far
      below the `u32` offset domain.
- [ ] The lane runs in CI (the adversarial-assurance workflow is the natural home; note Miri
      there covers only `lexer::tests` — widening Miri is optional scope, record the decision).

## Progress
-

## Notes

- This is the assurance floor under L-114/L-115: those fix known crashes; this makes the *class*
  expensive to reintroduce.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F12 (grep evidence: no
  fuzz/ dir, no cargo-fuzz/proptest/arbitrary in either workspace).
