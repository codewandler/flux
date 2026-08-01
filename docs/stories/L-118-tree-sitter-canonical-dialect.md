---
id: L-118
title: "The tree-sitter grammar parses canonical Flux; the nightly red goes green"
pillar: Language
status: ready
priority: 15
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "Review F6, MEDIUM — 7/15 canonical examples fail at the pin (bare binds, typed binds, ctx, +=, goal); nightly-only red with no owner until now"
---

# The tree-sitter grammar parses canonical Flux; the nightly red goes green

## Goal

The pinned `codewandler/flux-tree-sitter` grammar has never supported bare-identifier binds, typed
binds, `ctx` blocks, `+=`, or `goal` lines — 7 of 15 canonical examples fail, so Helix/Neovim/Zed
users see errors on exactly the spelling the formatter emits
(`.github/workflows/tree-sitter-corpus.yml:21-30`). The corpus lane is nightly-only and blocks
nothing, so the red persists by memory alone. This story owns it: land the grammar work upstream,
move the pin, lane goes green.

## Acceptance

- [ ] Upstream `flux-tree-sitter` parses all 15 `examples/*.flux` with zero `ERROR`/`MISSING`
      nodes (the C-334 script's own criterion), including the five named construct gaps.
- [ ] The pin in `.helix/languages.toml` moves to the fixed rev; `scripts/check-tree-sitter-corpus.sh`
      passes locally and the nightly lane is green.
- [ ] The workflow's "WHY IT IS RED" preamble is replaced with the green-state contract, and this
      story ID is referenced there so a future red has a named owner pattern to follow.
- [ ] The upstream repo's own 3-file corpus is replaced by or synced to flux's (the review's
      second-corpus-certified-the-drift observation — one corpus, per C-334's rule).

## Progress
-

## Notes

- Coordinate with the syntax epic (L-102): if L-107 later removes legacy spellings, the grammar
  shrinks — do not spend upstream effort perfecting legacy forms that are scheduled for deletion;
  canonical-dialect support is the priority.
- TextMate/IntelliJ remain unguarded after this story (review F6-adjacent); a guard for them is
  deliberately out of scope — grep them after syntax work per crate AGENTS.md.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F6.
