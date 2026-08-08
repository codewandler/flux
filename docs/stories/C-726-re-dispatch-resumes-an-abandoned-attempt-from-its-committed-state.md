---
id: C-726
title: "Re-dispatch resumes an abandoned attempt from its committed state"
pillar: "Core"
status: ready
priority: 3
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "C-724 acceptance 4, second half: a released item re-dispatches from the canonical ref, discarding the abandoned attempt's commits. plan_wave_topology pins base_commit from the canonical ref and handoff verification requires the cited test to fail at that pinned base, so seeding from an old attempt's branch inverts that evidence and needs a design decision"
---

# Re-dispatch resumes an abandoned attempt from its committed state

## Goal

When a claim is released because its supervisor died (C-724), the story's committed work survives on
its branch — but re-dispatch starts a fresh worktree from the canonical ref, so the abandoned
attempt's commits are silently left behind and the work is done twice.

## Acceptance

- [ ] Re-dispatch of an item whose previous attempt was abandoned resumes from that attempt's
      committed state rather than from the canonical ref.
- [ ] The evidence contract still holds. `plan_wave_topology` pins `base_commit` from the canonical
      ref and handoff verification requires the cited test to fail *at that pinned base*; resuming
      from a branch that already carries a failing-first test inverts that evidence, so this story
      must say explicitly what the new base and the new failing-at-base proof are.
- [ ] An abandoned attempt that holds no commits re-dispatches from the canonical ref exactly as
      today.
- [ ] The operator can tell the two apart: the dispatch record names the attempt it resumed and the
      commit it resumed from.
- [ ] Regression test: a wave released by C-724's abandoned-claim path re-dispatches its item from
      the abandoned branch's head, and its handoff still proves the cited test fails at the base it
      declares.
