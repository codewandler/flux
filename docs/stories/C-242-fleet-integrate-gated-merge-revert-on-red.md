---
id: C-242
title: "`fleet.integrate` — gate-after-every-merge becomes impossible to skip, and red reverts itself"
pillar: Core
status: backlog
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-tools, flux-runtime]
note: "F5 — the sharpest instance of host-enforces: the op gates and merges or does neither, so the rule stops being an instruction a model can skip"
---

# `fleet.integrate` — gate-after-every-merge becomes impossible to skip, and red reverts itself

## Goal
The `track` loop's most-violated rule is "run the full gate after **every** merge, not once at the
end of the wave" — because two stories that each compile alone can fail together with no git conflict
at all, and gating per merge attributes that failure for free. As prose, a model skips it. As an op,
it cannot.

`fleet.integrate` is reversible by construction: `--no-ff` merge the item's branch, run the
configured project gate on the integration branch, and **on red, `git revert -m 1` and return red**.
Never `reset`, never a rewrite. There is no code path through this op that merges without gating.

## Acceptance
- [ ] **Failing-first test**, both directions: integrating a branch that breaks the gate leaves the
      integration branch at its pre-merge tree (verified by a tree diff) and returns red; integrating
      one that passes lands it. Prove the op is absent at the merge base.
- [ ] The revert is `git revert -m 1` of the merge commit. A test asserts no `reset` and no history
      rewrite occurred — the merge and its revert are both still in the log.
- [ ] The gate is not skippable: no parameter, no config, and no error path merges without gating.
      A test pins this (e.g. a gate that cannot run is a refusal, not a silent pass).
- [ ] Accurate `effects`/`access`/`intents` and concrete `permission_subjects`; `Risk::High`,
      consistent with `git_merge`.
- [ ] A conflict is a clean recoverable error naming the conflicting files, leaving the tree
      consistent — not a silent half-merge.
- [ ] Standard gate green in both workspaces.

## Notes
- Depends on **F3 (C-238)** for `git_merge`/`git_revert` and on **F4 (C-241)** for the isolated
  branch to integrate. Backlog until both land.
- The gate command set is *configured*, not hardcoded — flux does not know a consumer's gate. The op
  takes it from the Program's datasource/config; the enforcement is that it runs *whatever* gate was
  configured, always.
- This op is where the `WaveCoordinator` contract stops being a design idea and becomes a test.
