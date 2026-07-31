---
id: C-383
title: git_uncommit — a history-preserving undo for an unpushed HEAD
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "the model-facing family has 15 git ops and no mixed-reset equivalent; git_revert appends an inverse commit, and the only git_reset in the repo is flux-eval's Risk::Destructive reset --hard + clean -fd, which destroys the patch"
---

# `git_uncommit` — a history-preserving undo for an unpushed HEAD

## Goal

Give the agent a way back from a mistaken local commit that keeps the work, instead of a choice
between an inverse commit and a blanket restore.

## Acceptance

- [ ] A `git_uncommit` op runs the mixed-reset equivalent bounded to `HEAD`, returning
      `{removed_commit, index_state, working_tree_status, upstream_divergence}`.
- [ ] It fails closed on every ambiguity: HEAD reachable from `@{upstream}`, a two-parent HEAD, the
      root commit, or an index holding changes that did not come from HEAD.
- [ ] It is added explicitly to the pinned set in `crates/flux-tools/tests/git_tree_policy.rs` —
      that scan selects ops by matching `--abort`/`--hard`/`-fd`, so a `--mixed` op is **not**
      selected automatically and would otherwise be silently unpinned.
- [ ] Failing-first: five hermetic temp-repo cases, one per refusal plus the success path; and an
      assertion in `git_tree_policy` that a reset-capable op absent from the pinned set reds the suite.
- [ ] Risk tier and effects are declared honestly and the published reference row carries the tier
      (C-368's contract).

## Progress

- 2026-08-01 — filed from validation of GIT-01. `git_uncommit` exists nowhere in the tree.

## Notes

- The `TreePrecondition` / `require_tree_precondition` seam this needs already exists
  (`crates/flux-tools/src/lib.rs:2490-2620`, pinned by C-249).
