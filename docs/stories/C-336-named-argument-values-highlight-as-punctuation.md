---
id: C-336
title: "A named-argument value highlights as punctuation, and swallows the comma after it"
pillar: Language
status: in-progress
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

- [x] **Failing-first**: a test over the highlighter's own output asserting that the value of a
      named argument classifies as a type/identifier and **not** as punctuation, red before the fix.
      The existing highlight tests are the place to put it — this must be observed by a named test,
      not by eyeballing an SVG.
- [x] The comma is its own token again. Assert the run boundary explicitly, because that is the half
      that reveals the real defect; a fix that recolours the identifier while still absorbing the
      comma has addressed the symptom.
- [x] `flux-lsp`'s semantic tokens are checked for the same input — the classification reaches a
      different consumer there, and `Punct` emitting nothing means the regression is *larger* in the
      editor than in `flux render`.
- [x] Cover the sibling spellings, not just `schema:`. Named options carry values in control headers
      too (`per:`, `wait:`, `max:`, `until:`, `every:`, `when:`, `timeout:`); check whether they
      share the defect and say which do.
- [x] ⚠ **`assets/example.svg` in `codewandler/flux-tree-sitter` is a committed generated artifact
      rendered by this highlighter, and it is currently stale-but-correct.** A fresh render today
      would *import* this regression. Once this story lands, regenerate it there
      (`flux render examples/readme-example.flux -o assets/example.svg`) and say so — that repo's CI
      deliberately does not check it, so nothing else will.
- [x] Full gate green in both workspaces.

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

## Progress

**Root cause — one missing match arm, and it is not L-96.** `classify_ident` decides an `IDENT` by
its *parent node kind*, and it had no arm for `SyntaxKind::VAR_EXPR`, so every sigil-less identifier
in expression position fell through to the `_ => C::Punct` catch-all
(`crates/flux-lang/src/highlight.rs:122`). The fix is the one arm
(`crates/flux-lang/src/highlight.rs:107`): `K::VAR_EXPR => C::Op`, because the parser wraps a bare
ident in a `VAR_EXPR` (`parser.rs:1454-1458`) and `cst_decode` lowers it to `Node::Var`
(`cst_decode.rs:1226`) — a bare *name reference*, which reads like every other reference. A `$sym`
never reaches this arm: a `VAR` token is classified before `classify_ident` is called.

**L-96 is exonerated.** The lead was plausible but wrong, and named arguments only made the defect
visible. `git log -S` puts both the `NAMED_ARG` parser node and the bare-ident→`VAR_EXPR` wrapping in
**`9960dd72 feat(flux-lang)!: make compact source canonical`**, not in L-96's `b5b6fd01`/`7160de7a`.
The defect is also not specific to named arguments — it reproduced identically for `$x = CallerSlots`,
`[CallerSlots, Other]` and `extract(CallerSlots, 2)`, all positions that predate named-option work.

**The comma absorption was a consequence, not a second fault.** `highlight` always emitted
`CallerSlots` and `,` as two separate spans; the SVG writer coalesces *adjacent same-coloured chars*
into one `<tspan>` (`crates/flux-tools/src/render.rs:225-228`), and both spans were `Punct`, hence one
run. The run boundary is therefore asserted as "the two spans are adjacent **and** classify
differently" — the property that makes coalescing impossible — rather than as mere span separation,
which was already true at the base and would not have been failing-first.

**Sibling spellings.** Only the *expression-valued* header options share the defect. `header_options`
takes an `expr_valued` list and there are exactly two entries across all four call sites:
`until` (`parser.rs:1005`, `repeat`/`loop`) and `when` (`parser.rs:1198`, `await`). Those parse their
value with `self.expr(0)`, so a bare ident lands in a `VAR_EXPR` and was mis-classified. Every other
option value — `per:`, `wait:`, `max:`, `every:`, `timeout:`, `risk:`, `backoff:` — goes through
`eat_option_value`, which bumps raw tokens straight into the `HEADER_OPTION`, where `option_class`
calls the value `Punct`. That is **deliberate** (it matches the space-keyword spelling L-96 replaced)
and is left unchanged; the new test pins both halves so the distinction cannot drift.

**`assets/example.svg` (flux-tree-sitter) — done, and it needed no commit.** The committed artifact
already renders `<tspan fill="#61afef">CallerSlots</tspan>` and `<tspan fill="#61afef">Intent</tspan>`,
i.e. it predates the regression, which independently confirms `Op`/`#61afef` is the correct target
class. A fresh `flux render examples/readme-example.flux` from this branch is **byte-identical** to
the committed file (`diff` clean), so the artifact is now current-and-correct rather than
stale-but-correct and nothing is owed there. That repo was also dirty and 11 commits ahead of
`origin/main` at the time of checking, so it was deliberately not written to.

Note the story's colour vocabulary: `#61afef` is `OP` in `render.rs`'s palette, not `TYPE`
(`TYPE` is `#e5c07b`). The hex in the report — and the pre-regression artifact — both point at `Op`.
