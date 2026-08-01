---
id: L-103
title: "`fluxlang fmt` — the canonical formatter as a CLI, comment-preserving"
pillar: Language
status: in-progress
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

- [x] `fluxlang fmt` canonicalizes every legacy spelling the parser accepts (sigiled locals,
      braced single-object calls, space-keyword headers, `do` calls, bare-ms numbers, body-line
      `until`, legacy `await … when`) — failing-first test drives a mixed-dialect fixture and
      asserts the canonical output plus AST equality with the input.
- [x] Comments and blank-line structure survive formatting (the CST-based `format_source`
      seam, not the AST-only `format`) — test with interleaved comments at every block level.
- [x] `fmt` is idempotent: `fmt(fmt(x)) == fmt(x)` over the shipped examples corpus.
- [x] `--check` mode: exit 0 on canonical input, non-zero with a diff summary otherwise.
- [x] The new subcommand is covered under `--features cli` and the feature-gate ledger
      (`scripts/check-feature-gated-tests.sh`) still passes.

## Progress

- **Done.** `fluxlang fmt [FILE…]` rewrites in place, stdin→stdout with no file, `--check` reports a
  per-side diff summary and exits non-zero. Canonicalization lives in a new
  `flux_lang::canonicalize` module; `crates/flux-lang/src/bin/fluxlang.rs` is the plumbing.
- **The story's suggested implementation did not hold, and this is the main thing to know.**
  `format_cst::format_source` is a *layout* formatter — it recomputes indentation and spacing and is
  otherwise faithful to the author's tokens. It returns `None` for every legacy spelling in the
  epic's table (verified against all seven). So `fmt` is not argument plumbing over it: the
  canonicalization pass had to be written. It is expressed as **byte-range splices over the lossless
  CST**, with `format_cst` run afterwards as the layout pass, so comments and declaration order are
  never rebuilt — they are simply not edited.
- **`format_source`/`format_module` are deliberately unchanged**, so the LSP's "format document" and
  `website_contract.rs`'s fixed-point assertions keep their current meaning. Wiring the LSP to the
  canonicalizing entry point is a separate decision (P5 territory).
- **Two places where the sigil is canonical, not legacy** — both found by running the pass over the
  real corpus, both now pinned by tests:
  - inside a native formula (`BIN_EXPR`/`UNARY_EXPR`), `b = a * 2` is a *parse error* ("expected a
    native expression") and `format` itself emits `b = $a * 2`;
  - on two or more positional arguments, `concat(a, b)` is the **named-input pun** surface (it means
    `a: a, b: b`, and `concat(a, a)` is a duplicate-named-argument error), which is why
    `format.rs`'s `fmt_call_args` keeps `$` there too.
  The reserved-name decision uses `ast::is_bare_symbol_name`, the parser/formatter SSOT, rather than
  a second hand-written keyword list.
- **L-112's question is answered one way by construction:** the pass rewrites spelling only, so
  `grep(pattern: …, glob: …)` keeps the **author's** order. The AST-level `format` still alphabetizes
  (its args live in a `BTreeMap`). L-112 still owns making the two agree.
- **Run over the real corpus, not a fixture** (`tests/canonical_corpus.rs`, outside the `cli` feature
  so `cargo test --workspace` reaches it): all **21** `.flux` files in the tree and **279** fenced
  `flux` blocks in `docs/`, `website/`, `crates/*/docs/`. Each asserts same-module, same-comment-
  multiset, and idempotence. `fluxlang fmt --check` over the 21 files reports 20 non-canonical and 0
  rejected — that is L-104's mechanical migration, ready to run.
- Nothing here touches `cst_decode`'s `semantic_line` sites, so **L-122's scope is not absorbed**:
  the pass reads CST node kinds and direct-child tokens, never reconstructed text.

## Notes

- Suggested implementation: wrap `format_cst::format_source` (already proof-carrying — it refuses
  to return output that doesn't reparse/lower identically, `format_cst.rs:43-63`); the subcommand
  is mostly argument plumbing in `crates/flux-lang/src/bin/fluxlang.rs`.
- The formatter currently alphabetizes named inputs (sorted map) — L-112 decides whether to
  preserve author order or bless alphabetical; `fmt` inherits that decision.
