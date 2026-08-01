---
id: C-429
title: "The recipes page — argue from the architecture, name no competitor, back every claim with a command"
pillar: Core
status: backlog
design: docs/designs/flux-recipes.md
epic: flux-recipes
areas: [website, docs]
note: "the artifact the ask is really about, filed LAST because it is worthless without recipes underneath it. ⚠ Compares against the pattern — the transcript as the runtime contract — not against a named product: claims about a competitor's internals cannot be verified from here, age into misrepresentations, and are weaker than a command the reader can run"
---

# Make it click, without a comparison table

## Goal

One page that makes a reader understand *why flux is a different kind of thing* — and that survives
contact with a skeptic, because every claim on it is backed by a command in a recipe.

## The argument, and the shape it must take

The vision states it: *"Mainstream agents let an LLM's transcript become the runtime contract."*
Everything flux is proud of falls out of refusing that. The page's job is to make that concrete rather
than assert it.

⚠ **Argue against the architecture pattern, not a named product.** Three reasons, the third decisive:

1. Specific claims about another system's internals cannot be verified from here, and a wrong one is a
   liability that outlives the post.
2. Competitors ship. A page pinned to their behaviour this year ages into a misrepresentation.
3. **It is weaker.** *"Here is a run you can replay, fork and diff yourself — and here is why an agent
   whose contract is its transcript structurally cannot"* beats any comparison table, because the
   reader draws the conclusion. That is what "clicking" is.

## Acceptance

- [ ] A page listing the recipes, each stating **the guarantee it demonstrates** and **the command that
      verifies it**.
- [ ] ⚠ **Every claim on the page is backed by a command in a recipe.** A claim with no command is
      marketing, and one unbacked claim discredits the ones that are true. This is the acceptance
      criterion that decides whether the page is worth having.
- [ ] Names no competitor. Compares against the transcript-as-runtime pattern.
- [ ] The lead is a *task*, not a feature — someone should be able to see, in the first screen, a real
      thing flux did and why the way it did it matters.
- [ ] Commands on the page are pinned by a test, so a CLI change breaks CI rather than breaking the
      page silently. ⚠ The repo's own `website_in_sync` machinery is the precedent — a docs page that
      drifts from the binary is a defect this repo already knows how to catch.
- [ ] The honest caveats survive editing: which layer determinism covers
      ([C-426](C-426-the-determinism-proof.md)), and which recipes need a model or credentials. A page
      that quietly drops them to read better has become the thing it is arguing against.
- [ ] Full gate green, including the website checks.

## Notes

- **Filed last on purpose.** Blocked on [C-425](C-425-the-flagship-recipe-tracking-as-a-flux-app.md)
  and [C-426](C-426-the-determinism-proof.md) — the page is worthless without recipes underneath it,
  and writing it first would define what the recipes have to prove, which is backwards.
- Where it goes is undecided: `website/docs/` already carries `intro.md`, `getting-started.md`,
  `tutorial/` (first-flow, first-agent, first-app) and `language/examples.md`. This is not another
  tutorial — a tutorial teaches the tool, this argues the thesis — but it should not orphan itself
  from them either.
- ⚠ The pressure on this page is toward superlatives. The vision's own register is the model to match:
  it states the Improvement Loop pillar is *"currently aspirational, and this document says so
  honestly."* A positioning page that cannot admit a limitation is not credible about the guarantees.
- The screencast epic ([session-screencast](../designs/session-screencast.md)) is the natural supplier
  of visual assets here — a recipe that renders to a cast is the page's strongest possible artifact.
  Not a dependency in either direction.

## Progress

- Filed 2026-08-01 with the flux-recipes epic.
