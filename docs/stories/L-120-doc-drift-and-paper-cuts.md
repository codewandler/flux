---
id: L-120
title: "flux-lang drift & paper-cut batch from the 2026-08-01 review"
pillar: Language
status: ready
priority: 22
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "Review F9–F13 LOW/INFO — replace_ident byte/char mix, `_`-separator diagnostic, reference.md expr whitelist, ~HOME on the pure path, ledger count, Prism split_once"
---

# flux-lang drift & paper-cut batch from the 2026-08-01 review

## Goal

Clear the review's LOW/INFO findings in one sweep, each with its own small test where behavior
changes.

## Acceptance

- [ ] **F9** `replace_ident` (`format.rs:407-417`) stops slicing a `Vec<char>` by a byte length —
      a non-ASCII expr var name from a wire AST formats to reparseable text; property pool gains a
      non-ASCII formula variable.
- [ ] **F10** `1_000` in literal position either parses as a number or errors with a source-line
      diagnostic — the serde-internal "line 1 column 2" message (`cst_decode.rs:2348-2350`)
      disappears; lexer (`lexer.rs:137-146`), durations, and literals agree on one separator rule.
- [ ] **F11** `docs/reference.md:1080-1087`'s hand-written expr whitelist matches
      `expr.rs:696-799` (add `sum/any/all/has/join/split/first/last`); consider generating the
      list from the evaluator's function table so it cannot drift again.
- [ ] **F13a** `interpolate_str`'s `~` → `$HOME` expansion (`runtime.rs:3782-3791`) moves off the
      pure path (host-provided setting via the trait seam, or removed) — same flow + same store
      yields the same values on different hosts; resume-ledger determinism test.
- [ ] **F13b** `scripts/check-feature-gated-tests.sh:53`'s "8 tests" prose matches reality (11),
      or drops the count.
- [ ] **F13c** The Prism keyword extraction (`named_option_headers.rs:225-234`) anchors to the
      flux language definition instead of the first `keyword:` occurrence in the file.

## Progress
-

## Notes

- syntax.md-side doc fixes (`"form"`, `?`, `do`, the `$` contradiction) are L-105's, not here —
  don't double-own them.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F9–F13.
