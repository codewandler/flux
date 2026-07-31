---
id: C-301
title: "tree-sitter does not lex duration suffixes, so canonical Flux shows errors in Helix, Neovim and Zed"
pillar: Language
status: done
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

- [x] The grammar lexes a duration literal — number plus `ms`/`s`/`m` — everywhere a number is valid,
      and `test/corpus/` gains cases with suffixes. **Cross-repo:** name the flux-tree-sitter commit.
- [x] A failing-first demonstration: `npx tree-sitter parse` on a snippet using `500ms` reports
      `ERROR` before, and zero errors after. Include both outputs.
- [x] ⚠ **Confirm the accepted suffix set against `flux-lang`, not against this story.** 0.39.0's
      WHATS-NEW names `ms`, `s` and `m`; the parser is the authority and may accept more. A grammar
      that lexes a subset trades one class of false error for another.
- [x] The corpus gains a case for **each** canonical construct L-96 emits with a duration
      (`retry … delay:`, `loop for … every:`, `await`, `throttle`, `debounce`), so the next syntax
      change to any of them is caught by that repo's own `tree-sitter test`.
- [x] `assets/example.svg` and `examples/*.flux` in that repo are checked for whether they now render
      without errors — C-300 left the SVG unregenerated because CI does not check it.
- [x] State whether flux's own CST highlighter and `flux-lsp` were ever affected. C-300's evidence
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

## Progress

**Done, across two repos.** The fix and its guards live in `codewandler/flux-tree-sitter`; the pin
that makes them reach users lives here.

- **`a197393`** (grammar) — `number` lexes an optional `ms`/`s`/`m` suffix, mirroring flux-lang's
  `take_duration` (`crates/flux-lang/src/cst_decode.rs:2289`) exactly: `ms` ×1, `s` ×1000, `m`
  ×60000, bare number = milliseconds. The suffix lives **inside** the `number` token rather than in a
  node of its own, so `(number) @constant.numeric` keeps highlighting `500ms` whole and no query
  changed. Suffix and fraction are mutually exclusive, because `take_duration` consumes only digits
  and `_` — so `1.5s` is not a flux duration and must not lex as one.
- **`7fcc64c`** (corpus) — a guard per duration-carrying canonical header: `retry … delay:`,
  `loop for … every:`, `throttle … per:`, `debounce … wait:`, and `race` (positional and
  `timeout:`). Five of the six go **red** against the pre-fix rule; that was verified by reverting
  the `number` rule in place, regenerating, and re-running, then restoring byte-for-byte.
- **`9ea9890`** (merge) also landed two improvements that had been written and then reached nobody:
  L-96's named-option headers and `permissions` declarations. Both sat on unmerged/unpushed branches
  while `.helix/languages.toml` pinned `29cff6c`, which predated them.
- **The pin here moved `29cff6c` → `9ea9890`.** That is the step that actually reaches editors, and
  it is why [C-334](C-334-tree-sitter-corpus-check.md) exists: nothing verified that the pinned rev
  parses the canonical corpus, so a mirror could land and go nowhere indefinitely.

**`await` is deliberately not a duration guard.** `lower_await` accepts exactly one option, `when`,
and returns `unknown_option` for anything else — so `await` never carries a duration. Its corpus case
is committed as a *construct* guard with that stated, rather than being passed off as part of the
duration set.

**flux's own CST highlighter and `flux-lsp` were never affected — confirmed, not inherited.** No flux
crate links tree-sitter at all (zero hits across every manifest and lockfile). flux-lang's lexer never
attempts `500ms` as one token: `500` is `NUMBER`, `ms` is a plain `IDENT`, and duration positions are
captured verbatim rather than routed through `expr(0)` (`crates/flux-lang/src/parser.rs:452` says so
outright), with semantics resolved later on raw text. Already pinned by `cst_agreement.rs:435-441`,
which runs `wait: 300ms`, `delay: 500ms`, `for 10s, every: 1s`, `per: 1s` and `race timeout: 5s`
through a zero-ERROR assertion.

**`assets/example.svg` deliberately not regenerated.** No file in `examples/` contains a duration
suffix, so C-301 does not touch it. A fresh render *does* differ — but in the wrong direction, and
for an unrelated reason: it would import a live regression where a named-argument value renders as
punctuation. Filed as [C-336](C-336-named-argument-values-highlight-as-punctuation.md), which owns
regenerating the SVG once fixed.
