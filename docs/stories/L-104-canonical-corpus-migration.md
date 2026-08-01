---
id: L-104
title: "Migrate the corpus to the canonical dialect"
pillar: Language
status: backlog
priority: 16
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang, flux-flow]
note: "P2+P3 — agent-loop.flux, examples/*.flux, doc snippets, skill examples; plus the hand-fixes a formatter can't do (fmt(\"\") noise, fmt-pre-binds)"
---

# Migrate the corpus to the canonical dialect

## Goal

The corpus is what models imitate and users copy, and today it teaches the legacy dialect: the
flagship `crates/flux-flow/assets/agent-loop.flux` and every `examples/*.flux` use `$` sigils,
braced-object calls, body-line `until`, and legacy `await … when`. Run `fluxlang fmt` (L-103) over
all of it, then hand-fix the idioms a formatter can't see.

## Acceptance

- [ ] `agent-loop.flux`, all `examples/*.flux`, `docs/syntax.md`/`reference.md` snippets, and the
      `skill.rs` examples are canonical-dialect; `fluxlang fmt --check` passes over the corpus
      (add that check to the gate so it stays true).
- [ ] Hand-fixes applied: `$answer = fmt("")` → `answer = ""`, `$done = fmt("true")` → `done = true`
      (both verified parsing at 0.45.0), fmt-pre-binds inlined as interpolated literals where the
      value is used once (interpolation is implicit in every string literal — reference.md §
      literals).
- [ ] Zero semantic drift: for each migrated file, the pre- and post-migration AST are equal
      (modulo the deliberate hand-fixes, which get their own before/after execution assertion via
      the mock provider for agent-loop.flux).
- [ ] The examples-validate gate (`crates/flux-eval/tests/examples_validate.rs`) stays green.

## Progress
-

## Notes

- Depends on L-103. agent-loop.flux is safety-adjacent (the one loop on every text surface,
  AGENTS.md) — migrate it last, with the full gate and the session-shape tests green.
- The tree-sitter mirror currently *fails* on canonical spellings (L-118) — sequence the pin move
  so editor users aren't left with a fully-red corpus in Helix/Neovim/Zed.
