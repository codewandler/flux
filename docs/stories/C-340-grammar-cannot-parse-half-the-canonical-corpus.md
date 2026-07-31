---
id: C-340
title: "The tree-sitter grammar cannot parse 7 of 15 canonical examples — 166 ERROR nodes on constructs it never supported"
pillar: Language
epic: road-to-stable
status: done
areas: [flux-lang]
note: "measured by C-334's new check against the CURRENT pin — bare-identifier binds, typed binds, ctx blocks, `+=`, col-0 goal, and optional field access all produce ERROR nodes. C-301 was the tip: durations were one construct, this is six more. The grammar repo's own 3-file corpus is 100% clean at the same rev, which is how it stayed invisible"
---

# The grammar cannot parse half the canonical corpus

## Goal

Make `codewandler/flux-tree-sitter` parse the Flux that flux itself ships as canonical.

[C-334](C-334-tree-sitter-corpus-check.md) built the check that measures this, and its first real run
against the **current** pin (`9ea9890`) reports **166 defect nodes across 7 of 15 examples**:

| example | defects |
|---|---|
| `examples/release.flux` | 73 |
| `examples/improve-tbench.flux` | 26 |
| `examples/improve-synthetic.flux` | 24 |
| `examples/improve-multi.flux` | 22 |
| `examples/cognition-research.flux` | 18 |
| `examples/eval-smoke.flux` | 6 |
| `examples/eval-synthetic.flux` | 4 |

Six construct families the grammar has **never** supported:

- **bare-identifier binds** — `src = grep(...)` (only `$src = …` parses)
- **typed binds** — `need: Need = need({…})`
- **`ctx` blocks** — `ctx pack` / `purpose` / `budget` / `include a, b`
- **compound assignment** — `pack += more`
- **a column-0 `goal "…"` line under a `flow` header** — breaks the whole declaration and alone
  accounts for all 73 defects in `release.flux`
- **optional field access** — `$x.field?`

[C-301](C-301-tree-sitter-does-not-lex-duration-suffixes.md) was the tip of this. Durations were one
construct; this is six more, and the blast radius is the same — every editor that grammar backs
(Helix, Neovim, Zed) shows idiomatic Flux as a syntax error.

**Why it stayed invisible, which is the part worth internalising:** the grammar repo has its own
3-file `examples/` corpus, and its CI parses *that* one. At the identical rev that corpus is 100%
clean while flux's 15-file corpus is 47% broken. A second corpus did not merely permit the drift —
**it certified it.**

## Acceptance

- [x] `scripts/check-tree-sitter-corpus.sh` passes against the pin, with **no allowlist**. C-334's
      implementor deliberately refused to add one, and it was right: silencing half the corpus
      rebuilds the blind spot in a new place.
- [x] **Failing-first is already built** — run C-334's check before you start and paste the 166-defect
      baseline, then again at the end. Work construct-family by construct-family and report the
      defect count after each, so the sequence shows which change bought what.
- [x] Each construct family gains a `test/corpus/` case in the grammar repo, **verified to fail
      against the pre-fix grammar** — the standard C-301 set. A case blessed from the parser without
      that check records current behaviour instead of guarding it.
- [x] ⚠ **Decide the `goal "…"` question rather than assuming.** `examples/release.flux` puts `goal`
      at column 0 under a `flow` header, unlike every other example, and it alone accounts for 73 of
      the 166 defects. flux-lang accepts it. Confirm whether that is canonical or an accident in the
      corpus — if it is an accident, the corpus is what should change, and that is a flux-side fix,
      not a grammar one.
- [x] The grammar repo's own 3-file corpus is **retired or explicitly demoted** to a smoke set, with
      a note saying why — it is the artifact that certified the drift. Ideally that repo's CI parses
      *this* repo's `examples/` instead. See the Notes.
- [x] Full gate green in both repos: this repo's five commands, and the grammar repo's
      `npx tree-sitter generate` / `test` / `cargo test` / `bash test/install-helix.sh` / examples
      zero-error / every query compiling.

## Notes

- **We own the grammar repo** — it is checked out at `~/projects/flux-tree-sitter`, and this is
  therefore directly actionable rather than an upstream request.
- ⚠ **The shared-parser-cache trap, learned the hard way by C-334's implementor:** tree-sitter caches
  compiled parsers by grammar *name*, so `~/.cache/tree-sitter/lib/flux.so` is shared across every
  checkout. Its first probe reported the OLD pin as clean because it loaded a `flux.so` another
  checkout had built. Export a private `TREE_SITTER_LIBDIR` per run — the same class of mistake as
  sharing a `CARGO_TARGET_DIR` between worktrees, and it makes a check pass while proving nothing.
- The parser under test must be the rev's **committed** `src/parser.c`, not one regenerated locally.
  Those C files are what Helix and nvim-treesitter actually compile; regenerating would go green on a
  rev whose committed parser was never regenerated — i.e. broken in every editor.
- **The strongest structural fix is cross-repo**, and it is the one C-334 recommends: a job in
  `codewandler/flux-tree-sitter` that parses *this* repo's `examples/` corpus on every grammar
  change. C-334's check catches "the pin is stale"; that one catches "the grammar never supported
  it", which is this story. Both are needed and they are not the same failure.
- After this lands the pin must move again, and `.helix/languages.toml`'s comment explains why that
  step is load-bearing.

## Progress

**166 → 0.** All 15 canonical examples parse clean at pin `2dbec53`, verified by C-334's check —
which is independent of the fix (a different agent wrote it, and it self-tests to report 166 at the
old rev).

Seven commits, one per construct family, each written failing-first and re-confirmed red against a
reverted `grammar.js`:

| step | remaining defects |
|---|---|
| baseline `7fcc64c` | 166 |
| bare + typed binds | 76 |
| `ctx` packs | 74 |
| `+=` | 73 |
| bare field access | 72 |
| column-0 `goal` | 12 |
| JSON inside `"""…"""` | **0** |

**The `goal` question: canonical, and the grammar had it exactly backwards.** No flux-side change is
owed. flux-lang treats `goal` as a declaration keyword (`parser.rs:531`, `flow_decl`'s dedicated
column-0 loop at `:626`, `cst_decode.rs:34`/`:61`), and testing all four spellings against flux-lang
directly rather than reading it: column-0, module-level and after-body all parse; the **indented**
form — the only spelling this grammar accepted — is a parse *error*. Modelled as a top-level
declaration to match. Expressing it as a directive inside `flow_declaration` models ownership better
but needs a GLR `conflicts:` entry, and the grammar's design note commits to lookahead-only
declaration boundaries; that attempt was backed out.

**A seventh family the story never named:** `{"changelog": "…"}` inside a `"""…"""` prompt.
`_triple_content` admitted `"` only when a non-brace followed, so a quote against a brace matched no
token and errored the whole string. Those were the last 12 defects.

**The local corpus is demoted, which was the structural half.** All three of the grammar repo's own
examples were rejected by flux-lang (`contact-centre.flux` had an indented `goal`, now fixed).
`scripts/check-flux-corpus.sh` + a CI step now parse *flux's* corpus with no allowlist, and
`examples/README.md` records that the local set is smoke-only. That closes the thing that made this
invisible: a second corpus that certified the drift.

⚠ Left deliberately: other binder positions still require the sigil (`memo`, `each`, `catch`,
`scope`, `await`, `branch`, and every `-> $bind` arrow). No corpus file exercises them bare.
`examples/demo.flux` and `examples/readme-example.flux` remain rejected by flux-lang — the first
legitimately (aspirational `type`), the second arguably not, but fixing it would force regenerating
`assets/example.svg`, which [C-336](C-336-named-argument-values-highlight-as-punctuation.md) owns.
