---
id: C-240
title: "Board correctness — a retry leaves a stale runner, nothing can write evidence, and Blocked→Ready bypasses the attempt budget"
pillar: Core
status: in-progress
priority: 2
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-capabilities, flux-datasource]
note: "F2 — the sweep after a retry chases a dead run: transition() never clears runner/task_id, so worker-b's re-claim still points at worker-a's corpse"
---

# Board correctness — a retry leaves a stale runner, nothing can write evidence, and Blocked→Ready bypasses the attempt budget

## Goal
Four defects in the board make the loop unsafe to run unattended, all in the same seam:

1. **A retry leaves a stale runner.** `transition` never clears `runner`/`task_id`
   (`crates/flux-capabilities/src/datasource/memory_board.rs:181-186`), so after `Failed→Ready` the
   next sweep reads worker-a's dead `task_id` and chases it. `assignee` must *not* be cleared — the
   holder persists — but the run identity must.
2. **Nothing can write `evidence`.** `Item::evidence: Vec<Reference>` exists and round-trips through
   the markdown format (the fixture stores `commit/<sha>` and a PR URL) but no op writes it. This is
   the same defect A-130 fixed for `runner`/`task_id`, and it is the diff-handoff channel F7 needs.
3. **No reassign path.** `claim` conflicts for a non-holder, so there is no way to move an item from
   a dead worker to a live one.
4. **`Blocked→Ready` bypasses the 2-round budget** — it does not bump `attempts`, so a story can
   cycle through blocked forever without ever exhausting its rework budget.

## Acceptance
- [ ] **Failing-first test**: dispatch an item, fail it back to `Ready`, re-read it — a fresh sweep
      sees no `runner` and no `task_id`, while `assignee` is unchanged. Impossible today.
- [ ] `board.reassign` moves an item to a new assignee; a subsequent `claim` by that new assignee
      succeeds where it conflicts today.
- [ ] `board.record_evidence` appends a `Reference` to `Item::evidence`, round-tripping through both
      backends and the markdown format.
- [ ] `Blocked→Ready` bumps `attempts`, so the budget cannot be laundered through `blocked`.
- [ ] Every property above is pinned **for both backends** in the shared contract suite, not just for
      `MemoryBoard` — the port is the contract.
- [ ] Standard gate green in both workspaces.

## Notes
- Seam: `crates/flux-capabilities/src/datasource/board.rs` (the `WorkBoard` port + ops),
  `memory_board.rs` and the markdown backend, `crates/flux-datasource/src/board.rs`
  (`validate_transition` — the single-sourced transition rules live beside it).
- Ordering: this story shares files with **C-236 (F1)**, which is in flight. Run it after C-236
  integrates, and expect to merge main in first.
- `flux-datasource` is a protocol-line crate and `codewandler-flux-capabilities` is published — a
  port change here obliges a version decision. Do not edit versions yourself; report the surface
  change and run `scripts/check-crate-versions.sh`.
- The plan's framing, worth keeping: **the retry window is worse than a stale runner.** `assignee`
  is never cleared by *any* code path, so a re-claim by worker-b can leave `runner`/`task_id`
  pointing at worker-a's dead run — the coordinator then reports progress on a process that no
  longer exists.
