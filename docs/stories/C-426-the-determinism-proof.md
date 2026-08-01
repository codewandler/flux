---
id: C-426
title: "Make the determinism claim checkable — the reader runs it, rather than believing it"
pillar: Core
status: backlog
design: docs/designs/flux-recipes.md
epic: flux-recipes
areas: [examples, docs]
note: "BLOCKED on C-425. Deliberately not folded into the flagship: an unverified determinism claim on a page arguing FOR determinism is the worst available failure, and folded in it becomes a README sentence nobody runs. ⚠ Must state precisely which layer the claim covers — model-authored stages are not deterministic, the shape of the run is"
---

# A claim the reader can check in one command

## Goal

Turn the flagship recipe's central claim into something a skeptic verifies from a clean checkout in
under a minute — and state exactly what it does and does not cover.

## Why it is its own story

The whole epic argues that flux is a different kind of thing because a run is a deterministic artifact.
⚠ **An unverified determinism claim, on a page whose subject is determinism, is the worst available
outcome** — it invites exactly the reader we most want to convince to test it and find the edge we
never checked.

Folded into [C-425](C-425-the-flagship-recipe-tracking-as-a-flux-app.md) this becomes one sentence in a
README that nobody ever runs. Its own story means it has to actually work.

## Acceptance

- [ ] The same input produces the same output twice, shown by a command the reader can run — not
      asserted in prose.
- [ ] The run replays offline through `flux replay`, model-free, and the recipe page shows the command
      and what to look for.
- [ ] A deliberate change surfaces in `flux diff` as a **plan change** rather than as noise — the
      distinction C-44 exists to draw, and the one that makes a diff worth having.
- [ ] ⚠ **The scope of the claim is stated precisely and prominently**: model-authored stages are not
      deterministic; the *shape* of the run is — order, bounds, which checks ran, which effects were
      proposed. The first reader who gets two different stories out of it must find that caveat before
      they conclude the page is marketing.
- [ ] The commands are pinned by a test, so a change to the CLI surface breaks CI rather than breaking
      the docs silently. ⚠ Prose describing a command is not a pin — the repo already knows what
      happens when a doc claims a behaviour nothing tests.
- [ ] The comparison is drawn against the **architecture pattern** — an agent whose contract is its
      transcript has no artifact to replay — and names no competitor. See the design for why that is
      both safer and more persuasive.
- [ ] Full gate green.

## Notes

- **Blocked on C-425**: there must be a recipe before there is a proof about it.
- The vision already states the property this story demonstrates: *"Because a run is a deterministic
  artifact, flux delivers what no LLM-as-runtime framework can: hermetic replay, fork-at-any-decision,
  and run-diff."* This story is that sentence made executable.
- A-45 (`flux replay`), A-46 (`flux fork`), C-44 (`flux diff`) are all shipped — this story consumes
  them, it does not build them. If one of them cannot do what this needs, that is a finding worth its
  own story rather than a workaround here.
- ⚠ Beware of proving determinism on a run so trivial the claim is empty. The proof must exercise the
  recipe's real path, including at least one model-authored stage, so the caveat above is demonstrated
  rather than merely written.

## Progress

- Filed 2026-08-01 with the flux-recipes epic.
