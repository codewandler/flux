---
id: C-301
title: "tree-sitter does not lex duration suffixes, so canonical Flux shows errors in Helix, Neovim and Zed"
pillar: Language
status: ready
priority: 6
areas: [flux-lang]
note: "found by C-300 while mirroring L-96's option labels — `500ms`/`10s`/`1m` produce ERROR nodes, and 0.39.0's compact syntax made duration suffixes the canonical spelling, so the editors that use tree-sitter now show errors on idiomatic source"
---

# tree-sitter does not lex duration suffixes

## Goal

`codewandler/flux-tree-sitter` is what Helix, Neovim and Zed use to parse Flux. It does not lex
duration suffixes: `500ms`, `10s`, `1m` produce **`ERROR` nodes**. C-300's implementor verified the
distinction — parsing the same canonical header with a bare number (`60_000`) gives zero errors, so
the option-label structure is clean and the fault is purely the number lexer.

This is pre-existing and was invisible for a good reason: that repo's test corpus only ever used bare
numbers. What changed is the canonical spelling. **0.39.0 made `ms`/`s`/`m` suffixes idiomatic**, and
L-96 then put durations into canonical option tails (`retry 3, delay: 500ms`, `loop for 10s,
every: 1s`). So the shipped examples and the formatter's own output now trip it.

C-300 called this "the single biggest remaining editor-fidelity gap" and recorded it in that repo's
CHANGELOG rather than fixing it, because its own scope was option labels.

## Acceptance

- [ ] The grammar lexes a duration literal — number plus `ms`/`s`/`m` — everywhere a number is valid,
      and `test/corpus/` gains cases with suffixes. **Cross-repo:** name the flux-tree-sitter commit.
- [ ] A failing-first demonstration: `npx tree-sitter parse` on a snippet using `500ms` reports
      `ERROR` before, and zero errors after. Include both outputs.
- [ ] ⚠ **Confirm the accepted suffix set against `flux-lang`, not against this story.** 0.39.0's
      WHATS-NEW names `ms`, `s` and `m`; the parser is the authority and may accept more. A grammar
      that lexes a subset trades one class of false error for another.
- [ ] The corpus gains a case for **each** canonical construct L-96 emits with a duration
      (`retry … delay:`, `loop for … every:`, `await`, `throttle`, `debounce`), so the next syntax
      change to any of them is caught by that repo's own `tree-sitter test`.
- [ ] `assets/example.svg` and `examples/*.flux` in that repo are checked for whether they now render
      without errors — C-300 left the SVG unregenerated because CI does not check it.
- [ ] State whether flux's own CST highlighter and `flux-lsp` were ever affected. C-300's evidence
      says no (they classify by parent node), so this should be a tree-sitter-only fix — but confirm
      rather than inherit the claim.

## Notes

- ⚠ **This story cannot be closed from this repository.** The fix lives entirely in
  `codewandler/flux-tree-sitter`. It is filed here because that is where the language vocabulary is
  decided and where the story board lives, and because C-300 just installed the in-repo half of the
  mirror guard — but the guard **cannot see tree-sitter**, which is precisely why this gap survived.
- Related: [C-300](C-300-editor-tooling-mirrors-owed-for-new-option-labels.md) found it and mirrored
  the option labels; [L-96](L-96-canonical-named-option-headers.md) made duration-carrying option
  tails canonical.
- Worth knowing when working in that repo: it commits generated artifacts (`src/parser.c`,
  `src/grammar.json`, `src/node-types.json`) per its own `AGENTS.md`, so a grammar change produces a
  large diff that is mostly regenerated output. C-300's option-label commit was ~27k lines for that
  reason.
