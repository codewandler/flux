---
id: L-57
title: SyntaxKind + lossless layout-aware lexer for the CST front-end
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "CST foundation — GATED on flux-lang front-end isolation (no worktree). Lossless token stream: comments/newlines as trivia, `\"\"\"` one STRING token, significant NEWLINE/INDENT/DEDENT."
---

# SyntaxKind + lossless layout-aware lexer for the CST front-end

## Goal
Introduce the rowan `SyntaxKind` alphabet and a lossless, layout-aware lexer as stage one of the CST
front-end: a flat token stream preserving every byte (comments + newlines as trivia, `"""…"""` as
one STRING token) that carries the indentation grammar as significant `NEWLINE`/`INDENT`/`DEDENT`
tokens.

## Acceptance
- [ ] New `crates/flux-lang/src/syntax.rs` — `#[repr(u16)]` `SyntaxKind` covering every token kind +
      `ERROR`/`TRIVIA` + layout tokens (node-kind variants may be stubbed until L-58).
- [ ] New `crates/flux-lang/src/lexer.rs` producing tokens with `TextRange`s.
- [ ] Failing-first `lexer_is_lossless`: concatenating token texts reproduces the source byte-for-byte
      over a corpus with comments, blank lines, and `"""` blocks.
- [ ] Failing-first `layout_tokens_track_nesting`: INDENT/DEDENT/NEWLINE correct on nested blocks;
      tabs-in-indent still errors (parity with `preprocess`).
- [ ] `rowan` + `text-size` added to `crates/flux-lang/Cargo.toml` (flag as new L0 deps).

## Progress
- (not started — gated on isolation)

## Notes
- **Gated** on flux-lang front-end isolation — see [flux-lang-cst.md](../designs/flux-lang-cst.md)
  "Isolation gate". No worktree; land in-place on `main` once `parse.rs`/`format.rs` are quiescent.
- The `"""`→escaped-JSON re-encode moves OUT of the lexer into `cst_to_draft` (L-59).
