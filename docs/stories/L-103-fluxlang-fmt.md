---
id: L-103
title: "`fluxlang fmt` — the canonical formatter as a CLI, comment-preserving"
pillar: Language
status: ready
priority: 10
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang, flux-cli]
note: "P1 — parse any accepted dialect, rewrite canonical, keep comments; --check mode for CI; everything else in the epic depends on it"
---

# `fluxlang fmt` — the canonical formatter as a CLI, comment-preserving

## Goal

Give `.flux` files a mechanical migration path to the canonical dialect: `fluxlang fmt [FILE...]`
rewrites in place (stdin→stdout when no file), `--check` exits non-zero on non-canonical input.
Today the canonical formatter exists only as a library (`format`/`format_source`); the CLI has no
such subcommand (verified against `fluxlang --help` at 0.45.0).

## Acceptance

- [ ] `fluxlang fmt` canonicalizes every legacy spelling the parser accepts (sigiled locals,
      braced single-object calls, space-keyword headers, `do` calls, bare-ms numbers, body-line
      `until`, legacy `await … when`) — failing-first test drives a mixed-dialect fixture and
      asserts the canonical output plus AST equality with the input.
- [ ] Comments and blank-line structure survive formatting (the CST-based `format_source`
      seam, not the AST-only `format`) — test with interleaved comments at every block level.
- [ ] `fmt` is idempotent: `fmt(fmt(x)) == fmt(x)` over the shipped examples corpus.
- [ ] `--check` mode: exit 0 on canonical input, non-zero with a diff summary otherwise.
- [ ] The new subcommand is covered under `--features cli` and the feature-gate ledger
      (`scripts/check-feature-gated-tests.sh`) still passes.

## Progress
-

## Notes

- Suggested implementation: wrap `format_cst::format_source` (already proof-carrying — it refuses
  to return output that doesn't reparse/lower identically, `format_cst.rs:43-63`); the subcommand
  is mostly argument plumbing in `crates/flux-lang/src/bin/fluxlang.rs`.
- The formatter currently alphabetizes named inputs (sorted map) — L-112 decides whether to
  preserve author order or bless alphabetical; `fmt` inherits that decision.
