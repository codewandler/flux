---
id: L-118
title: "The tree-sitter grammar parses canonical Flux; the nightly red goes green"
pillar: Language
status: done
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

- [x] Upstream `flux-tree-sitter` parses all 15 `examples/*.flux` with zero `ERROR`/`MISSING`
      nodes (the C-334 script's own criterion), including the five named construct gaps.
      **Already delivered by [C-340](C-340-grammar-cannot-parse-half-the-canonical-corpus.md)**,
      not by this story. Re-verified here at the pinned rev: `scripts/check-tree-sitter-corpus.sh`
      reports `PASS grammar rev 2dbec53a… parses all 16 canonical example(s)` — sixteen now, not
      fifteen, because an example was added after the review measured.
- [x] The pin in `.helix/languages.toml` moves to the fixed rev; `scripts/check-tree-sitter-corpus.sh`
      passes locally and the nightly lane is green. **Already delivered by C-340** (`b56f1057`
      moved the pin `9ea9890` → `2dbec53`). Verified: the script passes locally against the pin, and
      the nightly lane's run `30687146208` (2026-08-01T06:07Z, `schedule`) is `success`.
- [x] The workflow's "WHY IT IS RED" preamble is replaced with the green-state contract, and this
      story ID is referenced there so a future red has a named owner pattern to follow.
      → `.github/workflows/tree-sitter-corpus.yml:17-49`. **This was the only item still owed**,
      and it is guarded rather than trusted: `crates/flux-lang/tests/tree_sitter_lane_contract.rs`
      fails on any PR if the lane stops naming the pinned rev or points at a story with no file.
      The same now-false red claim in `scripts/check-tree-sitter-corpus.sh:63-67` is corrected too.
- [x] The upstream repo's own 3-file corpus is replaced by or synced to flux's (the review's
      second-corpus-certified-the-drift observation — one corpus, per C-334's rule).
      **Already delivered by C-340.** Verified by cloning `codewandler/flux-tree-sitter@2dbec53`:
      `examples/README.md` there demotes the three files to "a smoke set, **not** the corpus
      contract", and its `ci.yml` step "Parse flux's canonical corpus (no allowlist)" runs
      `scripts/check-flux-corpus.sh`, which fetches flux's own `examples/`.

## Progress

- **This story was largely already done when it was picked up, and the residue was small.** It was
  filed from review F6, which measured against pin `9ea9890`; C-340 had already fixed the grammar
  upstream and moved the pin to `2dbec53` before L-118 was worked. Three of the four Acceptance
  items were satisfied at the merge base (`cdba401f`) and are ticked above with the evidence used
  to confirm each, not on the story's say-so. **The nightly red does not reproduce**: the corpus
  check passes at the merge base, and the nightly lane's own last scheduled run is green.
- What was still owed is the fourth item, and it is the interesting one. The lane's preamble was
  still announcing a standing red that had been fixed — the exact hazard the story's note names
  ("nightly-only red with no owner"), one level up from the grammar: **the lane's contract had
  drifted from the pin it audits**, and nothing observed it, because a nightly lane blocks no push,
  PR or cut and its header is plain text.
- So the preamble is now the green-state contract (whole corpus, no allowlist, no exempted file;
  what a red means; that the fix is upstream *then* move the pin), it records the C-340 history as
  the pattern to follow, and it names L-118. **It is guarded**:
  `crates/flux-lang/tests/tree_sitter_lane_contract.rs` asserts the lane names the revision
  `.helix/languages.toml` currently pins, that the pin is a full 40-char sha (so the substring match
  cannot weaken), and that every story ID the lane names resolves to a real file in `docs/stories/`.
  It deliberately does not forbid *historical* revs appearing — banning them would push authors to
  delete the record instead of updating it.
- Failing-first, at the merge base with only the test added:
  `the_nightly_lane_names_the_grammar_revision_it_audits` fails with ".github/workflows/
  tree-sitter-corpus.yml never names the grammar revision .helix/languages.toml pins
  (2dbec53a34a840a56d0c07899a4fd4c17225f51f)"; green after the preamble rewrite.
- **These are documentation-coherence guards and nothing more.** They cannot tell you whether the
  pinned grammar parses anything — only `scripts/check-tree-sitter-corpus.sh` can, it needs the
  network, and it stays nightly for the reasons its own header records.

## Notes

- Coordinate with the syntax epic (L-102): if L-107 later removes legacy spellings, the grammar
  shrinks — do not spend upstream effort perfecting legacy forms that are scheduled for deletion;
  canonical-dialect support is the priority.
- TextMate/IntelliJ remain unguarded after this story (review F6-adjacent); a guard for them is
  deliberately out of scope — grep them after syntax work per crate AGENTS.md.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F6.
