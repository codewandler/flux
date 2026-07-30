---
id: C-240
title: "Board correctness — a retry leaves a stale runner, nothing can write evidence, and Blocked→Ready bypasses the attempt budget"
pillar: Core
status: done
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
- [x] **Failing-first test**: dispatch an item, fail it back to `Ready`, re-read it — a fresh sweep
      sees no `runner` and no `task_id`, while `assignee` is unchanged. Impossible today.
- [x] `board.reassign` moves an item to a new assignee; a subsequent `claim` by that new assignee
      succeeds where it conflicts today.
- [x] `board.record_evidence` appends a `Reference` to `Item::evidence`, round-tripping through both
      backends and the markdown format.
- [x] `Blocked→Ready` bumps `attempts`, so the budget cannot be laundered through `blocked`.
- [x] Every property above is pinned **for both backends** in the shared contract suite, not just for
      `MemoryBoard` — the port is the contract.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — **done.** Recovered as an orphan: the implementor was killed mid-task by a coordinating
  session crash, having left the work uncommitted but crate-test green. Its branch was preserved
  verbatim first, then reviewed independently, then integrated. Gate green on the integration branch:
  3128 tests across 159 suites, clippy `-D warnings`, fmt in both workspaces, flux-codegate,
  check-crate-versions.
  The failing-first evidence had to be reconstructed, since a killed implementor files no `BASE_PROOF`.
  All four properties were proved unreachable or oppositely pinned at the merge base, and item 4 is the
  strongest of them: the base did not merely lack the behaviour, it **pinned the opposite** —
  `assert_eq!(requeued.attempts, 0, "blocking is not a retry")` in `memory_board.rs` plus the matching
  "Blocking is not a retry" contract lap. `reassign` and `record_evidence` did not exist at the base at
  all, so the contract's calls could not compile there.
  What makes "the port is the contract" true rather than asserted: both trait methods are declared with
  **no default bodies**, so a backend cannot pass by not implementing them, and
  `assert_work_board_contract` is called from both `tests/memory_board.rs` and `tests/markdown_board.rs`.
  **Scope note, deliberate and wider than Acceptance item 4 asks:** `Blocked→Ready` was implemented by
  folding the edge into `is_retry`, so it also clears `runner`/`task_id` rather than only bumping
  `attempts`. Coherent with defect 1's rationale and pinned by the contract either way, but F3/F4 authors
  should know the sweep now sees a cleared run identity after an unblock too.
  Version decision discharged at integration: `codewandler-flux-datasource` 1.2.0 → **1.3.0** (MINOR).
  `is_retry`'s semantics and the public `EDGE_DIAGRAM` const's text changed, but no signature was removed
  and the set of legal edges is unchanged, so it is not wire-breaking. `codewandler-flux-capabilities`
  rides `version.workspace = true`, so its breaking trait change — two required methods, no default
  bodies — is carried by the next workspace MINOR rather than needing a hand bump here.
  Five customer-facing pages were corrected in the same round; `website/docs/agent/fleet.md:76` had begun
  stating the *opposite* of shipped behaviour ("No other edge touches it").

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
