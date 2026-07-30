---
id: L-93
title: Make Flux source compact without making it cryptic
pillar: Language
status: done
design: docs/designs/compact-readable-flux-syntax.md
note: Additive bare locals, named-input punning, multiline containers, readable calls, indexes, and durations
---

# Make Flux source compact without making it cryptic

## Goal

Reduce repeated symbol and input-map plumbing in authored Flux while preserving the JSON AST,
runtime semantics, old source compatibility, and exact formatter round-trips.

## Acceptance

- [x] Failing-first CST tests prove named call labels survive lowering instead of being discarded,
      multiline delimited expressions parse losslessly, and old/new spellings lower identically.
- [x] Bare locals are canonical; `$name` remains accepted and is emitted for reserved-name escapes.
- [x] Named call inputs and object fields support identifier punning without changing `Node::Call` or
      `Node::Obj`.
- [x] Canonical discarded-result calls use `op(...)`; legacy `do op ...` stays accepted.
- [x] Symbol-rooted bracket indexes and exact `ms`/`s`/`m` duration literals round-trip natively.
- [x] `format` and `format_compact` both satisfy `parse(format(ast)) == ast`.
- [x] Every checked-in `crates/flux-lang/**/*.flux` fixture is accepted; aspirational syntax is not
      disguised as an executable fixture.
- [x] Every root `examples/*.flux` program passes strict parsing, the LSP diagnostic path, and both
      canonical and compact format→parse projections for each embedded flow.
- [x] `.flux` file loading never content-sniffs a JSON `DraftAst`; JSON stays an explicit wire/API
      format and every checked-in `.flux` file is language source.
- [x] The crate tests, CLI-feature tests, clippy, fmt, and architecture gate are green.

## Progress

- 2026-07-30: Reviewed the implemented grammar, formatter, docs, and crate fixture corpus. Confirmed
  that `foo(a: 1)` currently drops `a`, multiline argument/object forms fail, and the flagship
  call-routing sketch is explicitly skipped by the corpus test.
- 2026-07-30: User selected additive compatibility with bare locals as the new canonical spelling.
- 2026-07-30: Landed the compact syntax, strict text-only `.flux` loading, converted all root
  examples, and added exact canonical/compact/CST/JSON-projection round trips. The representative
  old/new fixture shrank by 17.3%; the complete root example corpus shrank from 71,681 to 41,736
  bytes (41.8%).
- 2026-07-30: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and
  `cargo test -p flux-codegate` all passed.

## Notes

- No `DraftAst`, `Node`, JSON wire, analyzer, or runtime-semantics change is intended.
- File-scope type declarations and `Fmt`/`Lit` consolidation require separate breaking designs.
