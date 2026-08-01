---
id: C-425
title: "The flagship recipe — the tracking framework as a flux app, where the runtime holds the invariants a prompt cannot"
pillar: Core
status: ready
priority: 12
design: docs/designs/flux-recipes.md
epic: flux-recipes
areas: [examples, docs]
note: "the epic's headline. `track` (codewandler/agentplugins) keeps its invariants in markdown a model is asked to honour, and we have first-person evidence of the drift — C-406 found epics with no tracker, a dangling C-330, 185 stale priorities, nine colliding ranks. Rebuilt in flux the mechanical half becomes authored flow. ⚠ It is a RECIPE, not a product"
---

# The tracker that cannot drift

## Goal

A runnable flux program that maintains a tracked backlog — and demonstrates, on a task the reader
already understands, what changes when the runtime owns the invariants instead of the prompt.

## Why this task, specifically

The `track` plugin maintains stories with frontmatter, a generated board, a CHANGELOG, epics and
designs. Exactly one component is deterministic (`gen_board.py`). **Everything else is a model
following markdown instructions**: keep frontmatter valid, regenerate the board after a status change,
file a story for unscoped work, keep the roadmap in sync.

⚠ **We have first-person evidence that this drifts, not a hypothesis.** Sessions running `track` on
this repo produced [C-406](C-406-the-board-has-epics-with-no-tracker-and-no-narrative.md): epic slugs
carrying open work with no tracker and no narrative; a story citing a `C-330` that was never filed; 185
non-`ready` stories carrying a stale `priority`; nine priority values shared by two or more `ready`
stories, so the rank does not rank. Not interesting model failures — **the predictable result of
putting invariants in prose and asking a model to honour them across a long context.**

That makes it the ideal flagship: the reader does not have to accept an abstract claim about runtimes,
because the failure mode is one they have lived.

## The split the recipe demonstrates

- **Runtime owns the mechanical half** — frontmatter validation, board regeneration, the epic-tracker
  audit, CHANGELOG sync, priority-collision detection. Authored flow with declared bounds. Does not
  get skipped when the context is long; runs in the same order every time.
- **Model owns the semantic half** — writing the story, judging whether a finding duplicates an
  existing one, classifying an epic as initiative or remediation bucket. Bounded jobs, typed in and
  out.

## Acceptance

- [ ] A runnable program under `examples/` in **program form** (`flux app run`), passing the existing
      whole-directory sweep in `crates/flux-eval/tests/examples_validate.rs` — which hand-picks
      nothing, so the recipe is gated the day it lands.
- [ ] It genuinely maintains a backlog: at minimum validate frontmatter, regenerate a board, and audit
      epics for missing trackers. A recipe that only *describes* the split demonstrates nothing.
- [ ] ⚠ **Readable end to end in one sitting.** The real `track` framework is large; the subset is a
      judgement call this story owns. A recipe nobody finishes reading demonstrates nothing either —
      when the two goals conflict, readability wins and the omission is stated.
- [ ] The header comment states what it needs — model, credentials, network — in the style
      `channels-app.flux` already uses (*"these journeys use only pure ops, so no model/credentials are
      needed"*). ⚠ If it needs a model, say so in the first five lines; a recipe that fails on a clean
      checkout because of an undeclared prerequisite is worse than no recipe.
- [ ] Each enforced invariant carries a one-line comment naming the drift it prevents, referencing the
      C-406 finding it comes from. That is what turns a program into an argument.
- [ ] ⚠ **It is explicitly a recipe, not a product.** A tracking app good enough to use is a
      maintenance burden with users. The header says it is illustrative and unsupported.
- [ ] Full gate green.

## Notes

- [C-426](C-426-the-determinism-proof.md) makes the determinism claim checkable and is deliberately a
  separate story — folded in here it becomes a README sentence nobody runs.
- Prior art in the tree: `examples/channels-app.flux` is the only existing program-form example, and
  the sweep gives program-form files parse + structural checks (their external ops live outside
  flux-eval's in-process registry). Read `examples/README.md` on the two documented exceptions before
  choosing which ops to use.
- ⚠ Honest scope on determinism: model-authored stages are **not** deterministic. The *shape* of the
  run is — order, bounds, which checks ran. Do not let the recipe imply more.
- The `track` framework lives at `codewandler/agentplugins`; this recipe mimics it, and is not a port
  of it. Do not vendor its files.

## Progress

- Filed 2026-08-01 with the flux-recipes epic.
