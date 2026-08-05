---
id: C-242
title: "`fleet.integrate` — one ordered wave, one unskippable final gate"
pillar: Core
status: backlog
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-tools, flux-runtime]
note: "F5 — assemble story commits in order, gate the final combined tree once, and make publication on red impossible"
---

# `fleet.integrate` — one ordered wave, one unskippable final gate

## Goal
The fleet needs cross-story validation without multiplying the most expensive gate by every child
commit. Each story has one writer and isolated worktree, proves its own failing-first and targeted
checks, and hands off one story-sized commit. `fleet.integrate` assembles at most ten accepted commits
in dependency order on one dedicated wave branch, then runs the configured full gate exactly once on
the final combined tree.

The op owns the publication fence. Green permits the combined branch, pull request and completion
bookkeeping to advance. Red retains the exact failed candidate for diagnosis and returns structured
failure evidence, but publishes nothing and marks no story done. Conflicting commits and overlapping
write sets are serialized or rejected before the final gate; no second writer is started for the
same story.

## Acceptance
- [ ] **Failing-first test**, both directions: a two-commit wave that passes its targeted story
      checks but fails only when combined runs the full gate once, returns red, preserves the exact
      candidate SHA, publishes nothing and records no completion; a green wave becomes publishable.
      Prove the op is absent at the merge base.
- [ ] The full gate is not skippable and is invoked exactly once after the final accepted commit: no
      parameter, config or error path can publish an ungated wave. A gate that cannot run is a
      refusal, not a silent pass.
- [ ] Inputs are capped at ten story commits. Each input carries one story id, isolated-worktree
      identity, targeted-check evidence and exact commit SHA; duplicate story ids or two writers for
      one story are rejected.
- [ ] Integration follows declared dependency order. A conflict names the affected story and files,
      leaves a recoverable candidate, and does not launch a competing writer or silently half-merge.
- [ ] Red preserves history and the exact candidate tree for diagnosis — never `reset`, never a
      rewrite — while withholding branch publication, pull-request creation and done bookkeeping.
- [ ] Accurate `effects`/`access`/`intents` and concrete `permission_subjects`; `Risk::High`,
      consistent with `git_merge`.
- [ ] Standard gate green in both workspaces.

## Notes
- Depends on **F3 (C-238)** for `git_merge`/`git_revert` and on **F4 (C-241)** for the isolated
  branch to integrate. Backlog until both land.
- The gate command set is *configured*, not hardcoded — flux does not know a consumer's gate. The op
  takes it from the Program's datasource/config; the enforcement is that it runs *whatever* gate was
  configured, once on the final combined candidate.
- This op is where the `WaveCoordinator` contract stops being a design idea and becomes a test.
