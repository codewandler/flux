---
id: L-97
title: "Flux Glyph — an indented opcode projection for agents"
pillar: Language
status: ready
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

- [ ] The design vocabulary is implemented exactly: `F`, `=`, `^`, `?`, `?=`, `?~`, `|`, `|*`,
      `&`, `||`, `??`, `!?`, `!!`, and `~=`; `@{...}` carries a compact raw JSON node.
- [ ] Failing-first tests parse the shared triage fixture to the same AST as canonical Flux and
      format that AST back to the pinned Glyph representation.
- [ ] Native-core plus escape property tests satisfy `parse_glyph(format_glyph(ast)) == ast`.
- [ ] Indentation, unknown opcode, invalid arm placement, duplicate branch, and malformed escape
      diagnostics report source locations and never guess.
- [ ] Conversion requires an explicit Glyph source kind; `.flux` loading and runtime semantics are
      unchanged.

## Progress

- (not started)
