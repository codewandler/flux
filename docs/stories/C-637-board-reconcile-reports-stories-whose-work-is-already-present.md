---
id: C-637
title: "board reconcile reports stories whose work is already present"
pillar: "Core"
status: done
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "a story implemented in main while its status read ready was dispatched again and a worker turn reproduced committed code"
---

# board reconcile reports stories whose work is already present

## Goal

Make "this item's work is already in the tree" a question the board can answer. A story implemented
in `main` while its status still read `ready` was dispatched again, and a full worker turn reproduced
code committed hours earlier; nine more were repaired by hand with `board start` + `board done
--override-reason` pairs. The board already records everything needed to notice. Detection is the
whole value — the fix is a transition anyone can make once they know — so `board reconcile` reports
and never repairs.

## Acceptance

- [x] `flux board reconcile` reports every item whose status says the work is outstanding while
      evidence of that work is already present, and writes nothing.
- [x] Two independent signals count as evidence and each finding names which fired: an
      `implementation-landed` commit reachable from `HEAD` that names the item and touches paths
      outside the board's own item directory, and an `acceptance-complete` section whose every
      checkbox is ticked.
- [x] A commit that touches only the board's own documents — adding the item, flipping its status,
      re-rendering the marker region — is never mistaken for implementation.
- [x] A `done` item is never a finding, and an item id matches only as a whole token, so `C-63` is
      not found inside `C-637`.
- [x] Each finding names the profile-valid transition path that would close it, and every step of
      that path is accepted by the planning state machine rather than restated beside it.
- [x] The verb is read-only wherever the board API exposes it: it appears in `board schema`, `board
      call reconcile` is not classed as a mutation, and the session backend refuses it rather than
      answering from a backend that has neither history nor acceptance text.
- [x] History reading is bounded and says so: the scan depth is fixed, and a scan that reaches the
      ceiling warns that older implementation is not visible to the report.
- [x] Failing first, tests prove a landed-but-`ready` item is reported, a board-only commit is not
      evidence, a `done` item is never a finding, and the fully-ticked acceptance signal stands on
      its own.

## Progress

- 2026-08-08 — implemented as `flux board reconcile` in `crates/flux-cli/src/board_fleet_cmd.rs`:
  `already_present_evidence` is the pure predicate, `read_reconcile_history`/`parse_history_records`
  the single bounded `git log` pass, and `reconcile_board` the read-only verb. Ten failing-first unit
  tests in the crate's `tests` module cover both signals, the board-only-commit exclusion, token
  matching, the `done` exclusion, the transition path against `valid_planning_transition`, and the
  read-only wiring.

## Notes

- Evidence is deliberately two independent signals. A commit naming the item is the strong one but
  depends on message discipline; a fully ticked `Acceptance` section is weaker but needs no history
  at all, so a repository with neither convention nor a readable `.git` still gets an answer.
- Workspace scope reads each member's own history: a workspace id is `member/ID`, and the commit that
  implemented it lives in that member's repository, not in the workspace root.
- Reporting only is the point. `board reconcile` never transitions an item, so it stays safe to run
  from a coordinator loop.
