---
id: C-725
title: "A worker turn records whether it left uncommitted work behind"
pillar: "Core"
status: ready
priority: 2
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "C-722 acceptance 4: a turn's recorded status must not claim more than its worktree can show. doctor now reports a dirty story worktree and fleet capture recovers it, but the turn record itself still reads success or failure with no signal that work was left uncommitted"
---

# A worker turn records whether it left uncommitted work behind

## Goal

A worker that does real work and never commits it is indistinguishable, to the turn record, from a
worker that did nothing. `doctor` now reports a dirty story worktree (C-722) and `fleet capture`
recovers it, but the turn's own record still reads `success` or `failed` with no signal that work
was left behind — so the one place a coordinator looks first says nothing about it.

## Acceptance

- [ ] A worker turn that ends leaving uncommitted changes in its story worktree records that as a
      distinct, queryable outcome, not as an unqualified success or failure.
- [ ] The record carries what `doctor` already computes — the worktree, the story, and the number of
      files — so a reader does not have to run a second command to learn whether anything was left.
- [ ] The outcome is queryable through `flux fleet inspect`, so a coordinator can find every turn in
      that state without reading transcripts.
- [ ] A turn's recorded status never claims more than its worktree can show. This is the same rule
      C-721 applies to `applied`.
- [ ] Regression test: a turn that ends with an untracked file in its story worktree is recorded
      with the distinct outcome and its file count, and a turn that ends clean is not.
