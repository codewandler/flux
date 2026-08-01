---
id: C-427
title: "What makes a recipe a recipe — the contract, and where recipes live"
pillar: Core
status: backlog
design: docs/designs/flux-recipes.md
epic: flux-recipes
areas: [examples, docs]
note: "generalized FROM the flagship, not before it — a recipe contract written with no recipe in hand is speculation. Key property to preserve: examples_validate.rs sweeps the whole directory with no hand-picked list, so a recipe is gated the day it lands"
---

# The contract, written after the first real one

## Goal

State what a recipe is, so the second through tenth are consistent with the first — and decide where
recipes live without losing the CI gate that keeps the current corpus honest.

## Why after the flagship

A contract written before any recipe exists is a guess about which rules will matter. Writing one real
recipe first ([C-425](C-425-the-flagship-recipe-tracking-as-a-flux-app.md)) surfaces the actual
questions: how much prose belongs beside the code, whether a model is acceptable, how a reader knows
what to look for. This story generalizes what that produced.

## The contract, as proposed (C-425 may amend it)

A recipe:

1. does a **real task** someone would actually want done — not a language sample;
2. **runs from a clean checkout**, with any prerequisite (model, credentials, network) stated in the
   first few lines;
3. names **which guarantee it demonstrates** and **the command that verifies it** — the property that
   separates a recipe from a demo;
4. is **readable end to end**;
5. is **CI-gated** on the day it lands.

## Acceptance

- [ ] The contract is written down where a contributor adding a recipe will find it —
      `examples/README.md` is the incumbent and already carries the corpus's rules.
- [ ] ⚠ **The whole-directory sweep survives.** `crates/flux-eval/tests/examples_validate.rs` sweeps
      every file under `examples/` with no hand-picked list — an unknown op, missing required param or
      type conflict fails CI. Whatever the directory decision, a recipe must be gated by default rather
      than by being added to a list. **A list is how a corpus rots**: the one file nobody added is the
      one that breaks.
- [ ] The directory question is **decided and justified**: recipes in `examples/` inherit the sweep for
      free; a separate directory lets a recipe carry a prose walkthrough beside the code but must
      re-earn its gate. Either is defensible; leaving it implicit is not.
- [ ] The two existing documented exceptions stay documented and do not quietly grow: program-form
      files get parse + structural checks, and `advanced-code-review.flux` is pinned at parse-only
      because it calls an out-of-process plugin op.
- [ ] ⚠ **Decide whether a recipe may depend on a plugin.** It weakens its own gate to parse-only —
      the precedent is already in the tree. Say yes with the cost stated, or no.
- [ ] Full gate green.

## Notes

- **Blocked on C-425**, deliberately. See above.
- The existing `examples/README.md` is a good model for tone: it states exactly what is gated and how,
  including its own exceptions. Extend it rather than replacing it.
- [C-428](C-428-the-example-coverage-census.md) is the other half — this says what a recipe *is*, that
  one says which are *missing*.
- Every `.flux` file in `examples/` is native Flux-Lang text, so the parser, formatter and LSP cover
  the same corpus. Anything that would break that property needs a much better reason than
  presentation.

## Progress

- Filed 2026-08-01 with the flux-recipes epic.
