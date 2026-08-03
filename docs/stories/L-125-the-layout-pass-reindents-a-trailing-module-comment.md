---
id: L-125
title: "The layout pass re-indents a trailing module-level comment into the last flow's body — on input with no legacy spelling at all"
pillar: Language
status: done
priority: 9
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang, flux-lsp]
note: "found by L-103 and recorded-not-fixed there. Pre-existing in `format_cst`'s layout pass, so the LSP does it on every save today; `fluxlang fmt` is merely the first thing to apply it to files on disk. It gates L-104, which gates L-106 — the canonicalize quick-fix"
---

# A comment after the last declaration gets adopted by that declaration

## Goal

Stop `format_cst`'s layout pass moving a module-level comment that follows the last declaration into
that declaration's body.

A comment at column 0 after the final `flow` is re-indented to column 2, which reads as though it
documents the last statement of that flow rather than the module. It happens on input containing
**no legacy spelling at all** — zero canonicalization splices are produced — so it is purely the
layout pass. Formatting is idempotent afterwards, which is what has kept it invisible.

## Why it matters more than it looks

- ⚠ **The LSP already does this on every save.** `crates/flux-lsp/src/format.rs` calls
  `flux_lang::format_cst::format_module`, so any editor with format-on-save has been silently
  re-anchoring these comments. `fluxlang fmt` (L-103) is not the cause — it is the first thing to
  apply the same pass to files on disk, where the result is committed.
- ⚠ **It gates [L-104](L-104-migrate-the-corpus.md).** L-104 rewrites the shipped corpus with the
  formatter. Doing that before this is fixed bakes the re-anchoring into every file it touches, and
  a comment silently changing what it appears to document is exactly the kind of diff nobody reviews
  line-by-line in a 21-file mechanical migration.
- L-104 gates [L-106](L-106-deprecation-diagnostics.md), which is where the LSP gains its
  **canonicalize quick-fix**. So this small bug sits at the head of that chain.

## Acceptance

- [x] **Failing-first**: a test formatting a module whose last declaration is followed by a
      column-0 comment, asserting the comment stays at column 0 — failing at the merge base.
- [x] The fix is in the layout pass, not in `canonicalize` — L-103 deliberately left
      `format_source`/`format_module` untouched so the LSP's format-document and
      `website_contract.rs`'s fixed-point assertions keep their meaning. Do not move the boundary
      without saying why.
- [x] Comments in the neighbouring positions are pinned in the same test or beside it: before the
      first declaration, between two declarations, and trailing on a statement. ⚠ A **multiset**
      assertion cannot see this class — the comment survives, it moves — so the pin must assert on
      output *text* or column, as L-103's own re-anchor test had to.
- [x] `cargo test -p flux-cli --test website_contract` still passes, including
      `public_flux_examples_are_canonical_formatter_fixed_points`.
- [x] Full gate green, including `bash scripts/build-portable-wasm.sh`. The child-owned portable
      WASM build and parity tests pass; the wave parent owns the one full repository gate after
      integration.

## Notes

- Second finding from the same seam, worth deciding at the same time: `format` renders string lists
  compactly (`["a","b"]`) where `format_cst` puts a space after the comma — so `with_tools` output is
  canonical in *spelling* but not a byte-level `format` fixed point. Either reconcile them or record
  which one is authoritative.
- L-88 (done) is the story that built this formatter; its own note records that it downgrades a
  commented flow to an indentation-only reindent, which is the neighbourhood this bug lives in.
- L-103's rework fixed an ordering bug in its splice applier (a zero-width insertion sharing a start
  offset with a replacement was dropped). That was in `canonicalize`, not here — different code, same
  class of silent-loss bug, worth reading before touching this one.

## Progress

- Filed 2026-08-01 from L-103's "recorded, not fixed" note, after the owner asked whether the LSP
  should support the new formatter. It does support *a* formatter (L-88); the canonicalize quick-fix
  is L-106; this is the bug in front of both.
- Implemented 2026-08-03 in the layout pass. The exact-text regression failed first with the final
  comment rendered as `  # after the last declaration`; the same test pins a comment before the
  first declaration, between declarations, and trailing on a statement. A terminal comment-only
  line now keeps an explicitly column-zero source position when no following statement exists to
  establish its scope.
- The string-list secondary finding is decided without expanding this fix: `format_cst` is the
  authoritative byte-level layout for authored files and therefore for the LSP and `fluxlang fmt`.
  `format` remains the semantic AST projection; `website_contract` deliberately compares its
  significant tokens rather than whitespace. Its compact `with_tools` list is valid canonical
  spelling and need not be a byte-level `format_cst` fixed point.
- Child verification: all 441 `codewandler-flux-lang` library tests plus the feature-gated CLI
  targets, all 53 LSP library tests plus integration tests, `flux-cli --test website_contract` (33),
  both relevant Clippy runs with `-D warnings`, `cargo fmt --all -- --check`, the changelog mirror
  golden check, and `scripts/build-portable-wasm.sh` all pass. The wave parent will run the full
  workspace gate once after integration.
- 2026-08-04: the final integrated wave passed the complete repository gate.
