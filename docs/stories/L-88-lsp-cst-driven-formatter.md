---
id: L-88
title: CST-driven formatter — canonical spacing with comments, and modules that format at all
pillar: Language
status: backlog
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: format_document returns None for every multi-declaration module (main.rs:93-96, pinned by the test at :1591) and downgrades a commented flow to an indentation-only reindent (main.rs:97-102) — so the canonical formatter runs only on comment-free single flows
---

# CST-driven formatter — canonical spacing with comments, and modules that format at all

## Goal

`:format` produces canonical Flux-Lang for the files people actually write — commented flows and
multi-declaration modules — instead of silently doing nothing or only fixing indentation.

## Why (evidence)

- `format_document` (`crates/flux-lsp/src/main.rs:103-131`) has three outcomes: a module returns
  `None` (documented at `main.rs:93-96` — `Program` groups declarations by kind and cannot reproduce
  source order, so formatting could reorder the author's file; pinned by
  `formatting_is_deliberately_disabled_for_modules`, `main.rs:1591`); a flow *with* comments takes
  the `reindent` path (`main.rs:1330`), which canonicalizes indentation only and leaves interior
  spacing verbatim; only a comment-free flow reaches `flux_lang::format::format`.
- The code names the residual itself: "Full canonical spacing *with* comments is the documented
  remaining work" (`main.rs:102`).
- `document_formatting_provider` is advertised (`main.rs:199`) with no range or on-type variant, so
  formatting a selection is unavailable.

## Acceptance

- [ ] Formatting is driven from the CST rather than from a reparsed `DraftAst`, so comments and
      declaration order are structural facts rather than things to work around.
- [ ] A flow carrying comments formats to full canonical spacing with every comment preserved,
      attached to the construct it was written against (leading vs trailing on the same line).
- [ ] A multi-declaration module formats and preserves source declaration order; the
      `formatting_is_deliberately_disabled_for_modules` test is replaced by one asserting order
      preservation.
- [ ] The reparse-equivalence safety net stays: the formatted buffer must reparse clean, lower to
      the same module, and keep the same comment multiset (`main.rs:117-127`) — no edit otherwise.
- [ ] `textDocument/rangeFormatting` is advertised and implemented for a selection that covers whole
      statements.
- [ ] Failing-first tests: (a) a commented flow with ragged interior spacing formats canonically and
      keeps every comment; (b) a module with `op` / `flow` / `op` in that source order round-trips in
      that order; (c) idempotence — formatting twice equals formatting once, over the shipped
      `examples/*.flux` corpus.

## Progress
- (not started)

## Notes
- Check whether the order-preserving representation belongs in `flux-lang` (a CST-level formatter
  usable by `flux fmt` and `fluxlang` too) rather than in the server — if so, the L0 crate is the
  right home and the LSP just calls it.
- `reindent`/`cst_has_comment`/`comment_multiset` (`main.rs:1298-1386`) are the fallback this
  replaces; keep the guard, retire the indentation-only path.
