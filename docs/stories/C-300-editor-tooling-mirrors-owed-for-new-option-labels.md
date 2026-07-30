---
id: C-300
title: "Two new Flux-Lang option labels ship without their editor-tooling mirrors"
pillar: Language
status: done
priority: 7
areas: [website, flux-lang]
note: "L-96 added `max` and `wait` as canonical header option labels; AGENTS.md mandates mirroring new language vocabulary into four grammars and nothing checks it, so editors highlight the language as it was before L-96"
---

# Two new option labels ship without their editor-tooling mirrors

## Goal

L-96 made canonical control headers use call-like named options and introduced two genuinely new
option labels — **`max`** and **`wait`**. The workspace `AGENTS.md` requires new language vocabulary to
be mirrored into the editor grammars, and **nothing mechanical enforces it**, so the labels are
highlighted correctly by `flux-lsp` and incorrectly by everything else.

L-96's implementor found this and left it deliberately: `website/**` was outside its write set, and
two of the four targets are separate repositories.

## Acceptance

- [x] `max` and `wait` are added to `website/src/theme/prism-include-languages.js`. The other labels
      L-96 uses (`risk`, `backoff`, `delay`, `until`, `for`, `every`, `per`) are already listed there —
      confirm that list against L-96's actual emitted vocabulary rather than against this story, in case
      a third label was added after filing.
- [x] The `codewandler/flux-tree-sitter` grammar is updated. **Cross-repo:** name the commit.
- [x] The TextMate and IntelliJ grammars are updated. The IntelliJ plugin is at
      `~/projects/flux-editors/intellij/` (JDK 21, needs `FLUXLANG_BIN` exported). **Cross-repo:** name
      the commits.
- [x] ⚠ **The durable half, and the reason this story is worth more than four one-line edits:** add a
      guard so the next label cannot ship unmirrored. The vocabulary lives in `flux-lang`'s highlighter
      (`highlight.rs::option_class`), which is the natural source of truth; a test that reads the
      Prism grammar and asserts every label the highlighter classifies also appears there would close
      the in-repo half. State plainly what such a guard can and cannot cover — it cannot reach the two
      external repos, and pretending otherwise is worse than saying so.
- [x] Full gate green.

## Progress

- 2026-07-31: Landed. **The vocabulary was read off the code, not off this story.** `option_class`
  is *structural* — it calls the first `IDENT` of a `HEADER_OPTION` a keyword and has no label list
  — so the emitted vocabulary is the union of the decoder's `match option_label(…)` arms in
  `cst_decode.rs`: **nine** labels (`backoff`, `delay`, `every`, `max`, `per`, `risk`, `until`,
  `wait`, `when`). Of those, exactly `max` and `wait` were missing from the Prism grammar — so this
  story's list was right, and there was **no third label**. `timeout` is accepted on `race` but
  never emitted (a race's timeout is its primary operand), and `each`'s `flat` goes through the
  arrow, so neither is an option label.
- The guard lives in `crates/flux-lang/tests/named_option_headers.rs` rather than a new file,
  because that file already owns `HEADERS` — L-96's canonical corpus. Putting the guard beside it
  means the corpus has **one** copy: adding a construct's option label there is what makes the
  guard demand a mirror. A separate test file would have needed its own copy of the corpus, which
  is the drift this story is about.
- The vocabulary reaches the test through a new `highlight::header_option_labels(src)`, which runs
  `option_class` over every `HEADER_OPTION` token. It deliberately does **not** restate the "first
  `IDENT` is the label" rule — a second copy would agree with itself rather than with the
  highlighter (the failure mode of C-248/C-259/C-264/C-279's guards).
- ⚠ **The guard covers one of four mirrors.** Its doc comment says so, and `AGENTS.md`'s ⚠ was
  rewritten from "NO drift guard" to name exactly what is and is not now checked. `flux-tree-sitter`
  and the TextMate/IntelliJ grammars stay manual and unguarded from this repo.
- A second test, `the_canonical_corpus_spells_the_option_labels_we_expect`, pins the nine labels so
  the mirror guard cannot pass *vacuously* on an empty vocabulary.
- **Scope call on the un-canonical leftovers: separate story, not this one.**
  `crates/flux-flow/assets/agent-loop.flux:27` and the `website/docs/language/*.md` pages parse and
  lower identically, so they are cosmetic; canonicalising a shipped runtime asset plus a batch of
  docs pages is a sweep with its own review surface and no bearing on the mirror guard installed
  here. Left untouched.

## Notes

- **This is the "guard that does not exist" variant of a defect class this repo has hit repeatedly.**
  C-248, C-259, C-264 and C-279 were each a guard that existed and did not run. This one is a rule
  written in `AGENTS.md` with no mechanism at all, which is why it went unnoticed until an implementor
  volunteered it.
- ⚠ Scope check before starting: L-96 also left `crates/flux-flow/assets/agent-loop.flux:27` on the
  legacy `until $done` body form, and `website/docs/language/{reliability,control-flow,examples,tour}.md`
  showing legacy header spellings. Those all still **parse** — the website contract test passes — so
  they are un-canonical rather than broken. Decide whether canonicalising them belongs here or in its
  own story; do not silently expand into a docs sweep.
- Related: [L-96](L-96-canonical-named-option-headers.md) introduced the labels and named this gap.
