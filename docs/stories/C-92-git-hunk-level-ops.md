---
id: C-92
title: Add hunk-level git_* ops so an agent can stage part of a shared file
pillar: Core
status: backlog
priority:
note: "whole-file git_stage forces sweeping a coworker's in-flight hunks into an agent commit; the split-file case has no tool"
---

# Add hunk-level git_* ops so an agent can stage part of a shared file

## Goal
Let an agent stage and commit only its own changes when another author is concurrently editing the
same file. Today `git_stage` operates on whole file paths, so a file touched by two authors can only
be staged in full — the agent must either sweep the coworker's uncommitted hunks into its own commit
or hand the task back to the human. This serves Core's worktree-discipline invariant (protect the
user's worktree; make changes auditable) by giving the guarded git surface the granularity `git add
-p` has.

## Acceptance
- [ ] A new guarded op stages a subset of a file's changes at hunk granularity (the `git add -p`
      equivalent) — e.g. `git_stage_hunks { path, hunks: [<selector>] }` — routed through the same
      `flux_system` guarded process/IO path as the existing `git_*` ops (no second `Command::new`,
      workspace-pinned, argv-only). Name/shape decided in a short design note first.
- [ ] A read op surfaces the addressable hunks of a working-tree file (index vs worktree diff split
      into stable, selectable hunk units) so the staging op has something deterministic to reference.
- [ ] The op refuses cleanly (recoverable ToolResult error, not a plan-halting raw error) when a
      requested hunk no longer applies (the file changed underneath), mirroring the fs-tool guidance
      pattern (see C-32).
- [ ] Effects/`permission_subjects`/`intents` are accurate — staging is a workspace mutation scoped
      to the named path, so the approval gate sees the right subject (do not return empty subjects).
- [ ] A failing-first test proves the split-author case: a file with two independent hunks, stage
      only one, assert the index contains exactly that hunk and the other stays in the working tree.
- [ ] Catalog/docs kept in sync: `crates/flux-flow/docs/ops-reference.md` and the `git` group in
      `groups.rs` (+ the `builtins_register` expected-name list).

## Progress
- (not started)

## Notes
- Motivating incident: an agent implementing `/effort` had to commit into a tree a coworker was
  editing; `engine.rs` and a design doc were shared files, and whole-file `git_stage` would have
  pulled the coworker's D-175/D-178 hunks into the agent's commit. Whole-file staging cannot separate
  two authors editing one file; only hunk-level staging can.
- Relevant surface: the existing `git_*` ops in `flux-tools` (spec + `permission_subjects` +
  `intents` + `execute`, IO via `ctx.system`); the `git` tool group in `groups.rs`. All OS process
  creation must go through `flux_system::System` (`build_command`) — argv-only, no shell string.
- Design-first: the hunk-selector contract (index-based? context-hash? unified-diff patch bytes?) is
  the crux and belongs in `docs/designs/` before implementation.
- Non-goal: interactive `git add -p` prompting — the op takes an explicit hunk selection, since the
  guarded envelope has no interactive TTY.
