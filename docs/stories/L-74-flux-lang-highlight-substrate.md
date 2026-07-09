---
id: L-74
title: flux_lang::highlight — CST-classified highlight spans (L0 substrate)
pillar: Language
status: done
epic: flux-render
design: docs/designs/flux-render.md
note: "the shared highlight substrate: parse_cst walk classifies every token into HighlightClass spans by kind + parent-node kind; total (highlights invalid source); also the substrate flux-lsp L-69 semantic tokens adapts later"
---

# flux_lang::highlight — CST-classified highlight spans (L0 substrate)

## Goal
A pure L0 module `flux_lang::highlight` — `highlight(src: &str) -> Vec<(rowan::TextRange, HighlightClass)>`
— that classifies every token of a `.flux` source by walking the lossless CST (`parse_cst`), using
token kind **and parent node kind** (the leading `IDENT` of a `WHEN_STMT`/`FLOW_DECL`/… is a
`Keyword`; `$x` is a `Var`; `@effect` an `Annotation`). More accurate than keyword-list string
matching (the lexer deliberately does not classify keywords, `syntax.rs:39`), and **total**: invalid
or incomplete source still yields spans. This is the shared substrate for the `flow_render` SVG
source view (L-76) and, later, flux-lsp semantic tokens ([[L-69]] becomes a thin LSP adapter over it).

## Acceptance
- [x] NEW `crates/flux-lang/src/highlight.rs` (+ `pub mod highlight;` in `lib.rs`) with
  `pub enum HighlightClass { Keyword, Op, Var, Annotation, String, Number, Comment, Punct, Type, Error }`
  and `pub fn highlight(src: &str) -> Vec<(rowan::TextRange, HighlightClass)>`. No new deps.
- [x] Failing-first unit tests in the module: on a small snippet, the `flow`/`when`/`do` leading
  idents classify `Keyword`, an op name classifies `Op`, `$x` → `Var`, `@effect` → `Annotation`,
  string/number/comment tokens classify as themselves, a `thing`-selector type name → `Type`.
- [x] Total on malformed source: a snippet that produces `ERROR` nodes still returns spans and never
  panics (test asserts non-empty spans + no panic on a torn/incomplete flow).
- [x] A `"""triple"""` multi-line string yields a span whose `TextRange` covers all its lines
  (line-splitting is the consumer's job, per the design's multi-line-token note).
- [x] Gate green: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Progress
- 2026-07-09 — DONE. `flux_lang::highlight` implemented as a pure walk over `parse_cst`'s CST:
  kind-based classes (STRING/NUMBER/VAR/ANNOTATION/COMMENT/ERROR/punct) apply unconditionally;
  `IDENT` classifies by parent-node context — leading ident of ~45 stmt/clause/arm/header kinds →
  `Keyword`, `NAME` by *its* parent (type_ref positions → `Type`, obj/named-arg labels → `Punct`,
  else `Op`), `true`/`false`/`null` → `Number` (the `lit` role), param names + `FIELD_EXPR` path
  segments → `Var`, `@effect` tag → `Annotation`, `thing` second ident → `Type`, small parent-scoped
  interior-keyword sets (`in`/`flat`, `for`/`every`, `contains`, …). 7 unit tests incl. a byte-level
  totality helper (every non-whitespace byte covered by exactly one ordered span), mutation-checked
  (sabotaging the keyword rule fails 5/7). Whole-workspace gate + codegate green. Not added to the
  flux-flow facade re-export (matches the pattern: CST modules `lexer`/`parser`/`syntax` aren't
  either; L-76 consumes `flux_lang::highlight` directly).

## Notes
- Design: [flux-render.md](../designs/flux-render.md) § "1. `flux_lang::highlight`".
- CST entry points: `flux_lang::parser::parse_cst(src) -> Parse`, `Parse::syntax()`
  (`crates/flux-lang/src/parser.rs:40,34`); `SyntaxKind`/`SyntaxToken` in `crates/flux-lang/src/syntax.rs`.
- ⚠ L-59 (CST lowering) is in flight in the same files' neighborhood — coordinate before touching
  `parser.rs`/`syntax.rs`; this story should only *read* the CST, not change it.
- Unblocks [[L-69]] (semantic tokens) and is consumed by [[L-76]] (`flow_render` source view).
