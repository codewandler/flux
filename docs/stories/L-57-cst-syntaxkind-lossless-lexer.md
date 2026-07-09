---
id: L-57
title: SyntaxKind + lossless layout-aware lexer for the CST front-end
pillar: Language
status: done
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
- [x] New `crates/flux-lang/src/syntax.rs` — `#[repr(u16)]` `SyntaxKind` covering every token kind +
      `ERROR`/`TRIVIA` + layout tokens (node-kind variants stubbed to `ROOT` until L-58) + the
      `FluxLang` rowan `Language` binding (checked `u16` round-trip test).
- [x] New `crates/flux-lang/src/lexer.rs` producing tokens with `TextRange`s (raw scan + layout pass).
- [x] `lexer_is_lossless`: `reconstruct(src) == src` over a corpus with comments, blank lines,
      CRLF, `"""` blocks, and a newline-free tail.
- [x] `layout_tokens_track_nesting`: INDENT/DEDENT correct on nested blocks; blank/comment-only lines
      don't move the stack; `tabs_in_indentation_are_flagged` records the tab error (parity).
- [x] `rowan` + `text-size` added (workspace deps; new L0 deps flagged).

## Progress
- Done 2026-07-09: `syntax.rs` (SyntaxKind + FluxLang) and `lexer.rs` (lossless, layout-aware) landed
  in-place on `main` — the front-end was isolated (data-transforms shipped in `fc666ab`, tree clean).
  6 new tests pass; full `flux-lang` suite green (282 lib tests, round-trip invariant untouched);
  clippy + fmt clean. Node-kind SyntaxKind variants + the tolerant parser are L-58. **Uncommitted.**

## Notes
- **Gated** on flux-lang front-end isolation — see [flux-lang-cst.md](../designs/flux-lang-cst.md)
  "Isolation gate". No worktree; land in-place on `main` once `parse.rs`/`format.rs` are quiescent.
- The `"""`→escaped-JSON re-encode moves OUT of the lexer into `cst_to_draft` (L-59).
