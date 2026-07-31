---
id: L-97
title: "Flux Glyph — an indented opcode projection for agents"
pillar: Language
status: done
priority: 20
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang]
note: "F/= /^ /? /?= /?~ /| /|* /& /|| /?? /!? /!! /~= with `@{...}` as the raw-AST escape"
---

# Flux Glyph — an indented opcode projection for agents

## Goal

Provide a compact, regular notation whose indentation and small opcode vocabulary map directly to
common `DraftAst` constructors and expand deterministically to canonical Flux.

## Acceptance

- [x] The design vocabulary is implemented exactly: `F`, `=`, `^`, `?`, `?=`, `?~`, `|`, `|*`,
      `&`, `||`, `??`, `!?`, `!!`, and `~=`; `@{...}` carries a compact raw JSON node.
- [x] Failing-first tests parse the shared triage fixture to the same AST as canonical Flux and
      format that AST back to the pinned Glyph representation.
- [x] Native-core plus escape property tests satisfy `parse_glyph(format_glyph(ast)) == ast`.
- [x] Indentation, unknown opcode, invalid arm placement, duplicate branch, and malformed escape
      diagnostics report source locations and never guess.
- [x] Conversion requires an explicit Glyph source kind; `.flux` loading and runtime semantics are
      unchanged.

## Progress

Built as `crates/flux-lang/src/glyph.rs` (`flux_lang::glyph::{format_glyph, parse_glyph, OPCODES}`),
spec in `crates/flux-lang/docs/glyph.md`, CLI `fluxlang glyph` / `fluxlang unglyph`.

- **No new node kind and no new `.flux` syntax.** Glyph is a projection of the existing `DraftAst`;
  the `UPDATE=1` skill/website regenerations produce an empty diff. The editor-tooling mirrors
  (Prism, tree-sitter, TextMate/IntelliJ) are therefore untouched — they describe `.flux`, which did
  not change.
- **`|` is the single arm opcode.** The design sketch spelled a parallel branch `+docs`, which would
  have been a fifteenth opcode; instead `|` means "arm of the enclosing construct" and the construct
  decides. Opcode and operand are always space-separated and arm bodies are always indented (the
  sketch's glued `|bug` / `!?medium` forms are gone). Expression operands are canonical Flux
  expressions, not a Glyph-specific spelling — `docs/designs/flux-notation-workbench.md` records all
  three.
- **Core plus escape.** Fourteen constructs are native; any node whose canonical spelling is one
  line passes through verbatim as a leaf; everything else takes `@{<compact json>}`. Writer and
  reader share one classifier, so a verbatim line is emitted only when the reader would read it back
  as one — that is what makes the round-trip total rather than best-effort.
- Diagnostics are Glyph-local for structure and remapped for expressions: the reader expands to
  canonical Flux with a line map and rewrites the canonical `line N:` prefix to the Glyph line.
- Adjacent, **not** fixed: `fluxlang`'s pre-existing `rail_reports_the_existing_parser_diagnostics`
  test fails at the merge base — it uses `confirm "y", risk: high` as malformed input, which L-96
  made valid. It is invisible to `cargo test --workspace` (it needs `--features cli`).

- 2026-07-31 — integrated. **No node kind and no new input syntax**, so the editor-grammar mirror
  obligation does not apply: Glyph is a projection of the existing `DraftAst` with its own sigils, no
  file extension and no editor integration, and both `UPDATE=1` regenerations produced an empty diff.
  ⚠ **A follow-up worth taking seriously before L-98/L-99:** a non-core container swallows its whole
  subtree into one escape line. `fluxlang glyph crates/flux-flow/assets/agent-loop.flux` renders the
  head of the loop and then collapses the entire `repeat` block — the bulk of the file, including core
  `match`/`when` nodes nested inside it — into a single multi-kilobyte `@{…}` line. That is *correct*
  per this story's contract (`repeat`/`each`/`try`/`saga` are outside the fourteen opcodes) and the
  round-trip is exact, but it means Glyph's compression is weak on exactly the real production flows
  it exists to compress. L-98 and L-99 should decide between widening the core and letting the escape
  carry a header with a native body instead of being all-or-nothing.
