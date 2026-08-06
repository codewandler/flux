---
id: C-625
title: "Board planning mutations commit their writes"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "an uncommitted implementation (create --no-commit, board_fleet_cmd.rs + board_fleet_cli.rs, 184 insertions) sits in the working tree; finish it under this story and extend to transition/done. Roadmap dogfood needed a git-amend loop per created story."
---

# Board planning mutations commit their writes

## Goal

Every planning mutation that writes a file also commits it, because items are read with
`ls-tree`/`show` at the member's canonical ref: an uncommitted planning document is invisible to
every read path, so `create` without a commit is a silent no-op that reports success. The roadmap
dogfood (2026-08-06) had to wrap every `board create`/`board transition` in a git-amend loop
because each mutation refused on the dirt the previous one left. An implementation of the `create`
half (commit by default, `--no-commit` escape, `commit_new_planning_document`) already sits
uncommitted in the working tree — finish it under this story.

## Acceptance

- [ ] `board create`, `board transition`, `board done` and `board update` commit their own writes by default, path-scoped to the files they touched, with `--no-commit` as the escape hatch.
- [ ] A dirty checkout no longer blocks a planning mutation on unrelated files (the mutation commits only its own writes).
- [ ] `--dry-run` continues to write and commit nothing.
- [ ] The in-flight working-tree diff (BoardAction::Create `no_commit`, tests in board_fleet_cli.rs) lands as part of this story, not as an orphan.
