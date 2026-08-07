---
id: C-641
title: "fleet handoff derives the write set and the owning worker from the worktree"
pillar: "Core"
status: ready
priority: 10
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "both are already recorded; requiring them by hand invites a wrong write set, which is false evidence rather than a typo"
---

# fleet handoff derives the write set and the owning worker from the worktree

## Goal

`fleet handoff` should not ask for facts the fleet already recorded. `--from-worktree` derives the
write set from the story worktree's `base..HEAD` range, and the owning worker is derived from the
agent that wave assigned to that worktree.

## Acceptance

- [ ] `flux fleet handoff <wave> <item> --commit <sha> --from-worktree` is accepted without
      `--write-set` and records the write set observed in `base..HEAD`.
- [ ] `--from-worktree` and `--write-set` are mutually exclusive, so one handoff never carries both a
      derived and a hand-typed write set.
- [ ] The owning worker is derived from the wave's story worktree, so another wave holding an attempt
      at the same item no longer makes the worker identity ambiguous.
- [ ] An empty observed write set, a mismatched hand-typed claim, and every other handoff
      verification stay exactly as they were.
