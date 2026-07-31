---
id: C-385
title: Let staging target only receipt-owned changes
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "GitStageInput is paths: Vec<String> only — there is no way to express 'stage what I changed' in a worktree that also holds the user's work, which is exactly the AGENTS.md rule the agent is asked to honour"
---

# Let staging target only receipt-owned changes

## Goal

Turn "assume uncommitted changes are user-owned unless you made them" from a prompt rule into an
operation the runtime can enforce.

## Acceptance

- [ ] `GitStageInput` accepts an optional `ownership: "this_session"`, resolved through C-384's
      receipts plus `git_hunks`, staging only receipt-covered hunks.
- [ ] It **refuses** — never widens to the whole file — when a path's hunks are not fully covered by
      receipts, and refuses outright when no receipts exist.
- [ ] Failing-first: one file with one user hunk and one agent hunk yields a staged diff containing
      only the agent hunk; with no receipts the op refuses rather than staging.
- [ ] Depends on C-384.

## Progress

- 2026-08-01 — filed from validation of GIT-02.

## Notes

- Hunk-level staging already exists (`git_hunks`/`git_stage_hunks`, C-92) with deterministic
  content-hash ids, so the mechanism is present — nothing writes the ownership record.
