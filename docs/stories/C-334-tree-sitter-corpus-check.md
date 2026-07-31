---
id: C-334
title: "Nothing verifies that the pinned tree-sitter rev parses canonical Flux"
pillar: Language
status: ready
priority: 5
areas: [flux-lang, plugins]
note: "C-301's root cause, not its symptom — the grammar repo had TWO landed improvements that never reached anyone because `.helix/languages.toml`'s pin stayed at 29cff6c for both. AGENTS.md:172 concedes only 1 of 4 editor mirrors is guarded, and the guarded one (Prism) is the only one that cannot produce ERROR nodes"
---

# Nothing verifies the pinned tree-sitter rev parses canonical Flux

## Goal

Close the gap `AGENTS.md:172` states outright:

> ⚠ **Editor-tooling mirrors are manual, and only ONE of the four is guarded** … **Grep the other
> three after syntax work — nothing else will tell you.**

The asymmetry is the sharp part: the guarded mirror (website Prism) is the only one that **cannot
break parsing** — it merely mis-colours. The three unguarded ones are the three that produce `ERROR`
nodes in a real editor.

**This is not theoretical.** C-301 shipped a grammar that reported `500ms` as a syntax error in
Helix, Neovim and Zed for multiple releases, because 0.39.0 made duration suffixes canonical. And
the deeper finding from fixing it: the grammar repo already contained **two** landed improvements —
L-96 named-option headers and `permissions` declarations — that reached nobody, because
`.helix/languages.toml`'s pinned rev never moved. The mirror work was done. It landed nowhere, and
nothing noticed.

So the failure mode to guard is not only "nobody mirrored the change" but **"the pin does not
reflect the mirror"**.

## Acceptance

- [ ] `scripts/check-tree-sitter-corpus.sh`: resolve the rev pinned in `.helix/languages.toml`,
      fetch and build that exact grammar, parse **every** `examples/*.flux`, and fail on any `ERROR`
      or `MISSING` node, naming the file and the construct.
- [ ] **Failing-first, and it must be demonstrated against history rather than contrived:** run the
      script against rev `29cff6c` (the rev flux pinned until 2026-07-31) and show it **failing** on
      a duration suffix. A script that is green on its first run against every rev has not been
      demonstrated. Then show it green against the current pin.
- [ ] `--self-test`, and the `exit 2 = unreachable = CI skips` convention `check-release-tags.sh`
      already uses — this check needs the network, so it must degrade to a skip rather than a red
      when GitHub is unreachable.
- [ ] Wired into CI as a **nightly or release-gated** job, explicitly **not** a PR gate (network
      dependency), with `timeout-minutes` set.
- [ ] The corpus it parses is the one that already exists and is already swept mechanically —
      `examples/`, consumed by `crates/flux-lang/tests/cst_agreement.rs` (frozen AST SHA-256) and
      `crates/flux-eval/tests/examples_validate.rs` (a real `read_dir` sweep, so a *new* example is
      guarded by default). Do not invent a second corpus that can drift from the first.
- [ ] State what this does **not** cover: the TextMate and IntelliJ mirrors in
      `codewandler/flux-editors`. They can only mis-colour, not fail to parse, which is why they rank
      below this — but say so rather than leaving the reader to assume four-way coverage.

## Notes

- Found while fixing [C-301](C-301-tree-sitter-does-not-lex-duration-suffixes.md), which is where the
  "two improvements landed nowhere" evidence comes from. Related: C-300 (the Prism mirror check, the
  one guard that exists), C-320 (will owe the same four-way propagation the moment it changes the
  grammar), L-42 (the website node-kind tables, the same class one layer over — and that one *did*
  get a generated drift guard).
- ⚠ **We own `codewandler/flux-tree-sitter`** (it is checked out at `~/projects/flux-tree-sitter`),
  so this is a check we can act on rather than a request to a third party. That also makes the
  stronger option viable if you want it: a job in *that* repo which parses this repo's `examples/`
  corpus on every grammar change, so the two ends meet in the middle. Weigh it — the pin check here
  catches "the pin is stale", the corpus check there catches "the grammar regressed", and they are
  not the same failure.
- The pin comment in `.helix/languages.toml` now records why moving it matters; keep that comment
  accurate if this story changes the mechanism.
