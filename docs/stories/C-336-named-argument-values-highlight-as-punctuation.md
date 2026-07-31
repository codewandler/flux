---
id: C-336
title: "A named-argument value highlights as punctuation, and swallows the comma after it"
pillar: Language
epic: road-to-stable
status: ready
priority: 6
areas: [flux-lang, flux-lsp]
note: "found while checking whether C-301 affected flux's own highlighter — it does not, but this does: `schema: CallerSlots` renders CallerSlots as punct grey (#828997) instead of type blue (#61afef) and absorbs the trailing comma into the same token. Reproduced on 0.43.0. Worse in flux-lsp, where Punct emits no semantic token at all"
---

# A named-argument value highlights as punctuation

## Goal

Restore correct classification for the value half of a named argument.

Reproduced on 0.43.0 with `flux render`:

```flux
flow main
  $x = ai.extract({ from: $doc, schema: CallerSlots, ask: "q" })
```

renders as

```
<tspan fill="#828997">CallerSlots,</tspan>
```

`#828997` is the **punctuation** colour; a type identifier should be `#61afef`. Two distinct faults
in one token:

1. **Misclassification** — `CallerSlots` is classified as punctuation rather than as a type.
2. **Token-run absorption** — the following `,` is swallowed into the *same* `tspan`, so the
   highlighter is emitting one run where there should be an identifier followed by a separator.

The second is the more diagnostic of the two: a classifier that merely guessed the wrong kind would
still break the run at the comma. Absorbing it says the token run itself is being built wrong, which
points at the named-option/named-argument handling rather than at `classify_ident` in isolation.

**This is not cosmetic in `flux-lsp`.** The LSP's semantic tokens are produced from the same walk,
and `Punct` emits **no token at all** — so an editor driven by flux-lsp loses the identifier
entirely rather than merely colouring it oddly.

## Acceptance

- [ ] **Failing-first**: a test over the highlighter's own output asserting that the value of a
      named argument classifies as a type/identifier and **not** as punctuation, red before the fix.
      The existing highlight tests are the place to put it — this must be observed by a named test,
      not by eyeballing an SVG.
- [ ] The comma is its own token again. Assert the run boundary explicitly, because that is the half
      that reveals the real defect; a fix that recolours the identifier while still absorbing the
      comma has addressed the symptom.
- [ ] `flux-lsp`'s semantic tokens are checked for the same input — the classification reaches a
      different consumer there, and `Punct` emitting nothing means the regression is *larger* in the
      editor than in `flux render`.
- [ ] Cover the sibling spellings, not just `schema:`. Named options carry values in control headers
      too (`per:`, `wait:`, `max:`, `until:`, `every:`, `when:`, `timeout:`); check whether they
      share the defect and say which do.
- [ ] ⚠ **`assets/example.svg` in `codewandler/flux-tree-sitter` is a committed generated artifact
      rendered by this highlighter, and it is currently stale-but-correct.** A fresh render today
      would *import* this regression. Once this story lands, regenerate it there
      (`flux render examples/readme-example.flux -o assets/example.svg`) and say so — that repo's CI
      deliberately does not check it, so nothing else will.
- [ ] Full gate green in both workspaces.

## Notes

- Found on 2026-07-31 while completing [C-301](C-301-tree-sitter-does-not-lex-duration-suffixes.md).
  That story asked whether flux's own CST highlighter and `flux-lsp` were affected by the duration
  bug; the answer is a well-evidenced **no** (no flux crate links tree-sitter at all, and the
  highlighter classifies by token kind plus parent node rather than re-lexing) — but the check turned
  this up instead.
- **Suspected origin: L-96's named-option work**, which is the change that introduced named
  `name: value` runs into control headers and arguments. That is a lead, not a conclusion — confirm
  it against the history rather than assuming.
- Related: the highlighter is `crates/flux-lang/src/highlight.rs` (walks the real parser's token
  stream, classifying by token kind + parent node kind); `flux-lsp`'s adapter is
  `crates/flux-lsp/src/semantic.rs`.
- Adjacent, from the same investigation and worth its own consideration: **no file in `examples/`
  uses a duration suffix**, so the tree-sitter repo's CI zero-error example parse does not exercise
  C-301's fixed lexer at all — its corpus is the only guard layer. Adding `timeout 10s` to a canonical
  example would make CI a second, independent guard. See also [C-334](C-334-tree-sitter-corpus-check.md).
