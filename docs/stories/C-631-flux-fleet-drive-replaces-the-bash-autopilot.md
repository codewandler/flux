---
id: C-631
title: "flux fleet drive replaces the bash autopilot"
pillar: "Core"
status: ready
epic: fleet-harness-throughput
areas: [flux-cli]
note: "the 582-line bash driver parses state three ways without --if-revision and cannot self-edit safely; its loops (planner/retro/scribe) are authored .flux files it invokes host-side, so the port inherits them unchanged"
priority: 0
---

# flux fleet drive replaces the bash autopilot

## Goal

The unattended driver is a 582-line bash script that parses `state.json` three ways with no
`--if-revision` guard, cannot be edited while running (bash re-reads by byte offset), needed a
production failure to find each of its ~17 defects, and holds the only closed control loop in the
system. Its judgment is already outside it — planner/retro/scribe are authored `.flux` loops it
invokes host-side — so a native `flux fleet drive` inherits them unchanged and replaces only tick
mechanics: status fingerprinting, wave-state advancement, handoff reconstruction, snapshot
accumulation, dispatch width.

## Acceptance

- [x] `flux fleet drive --tick` performs one deterministic tick (report, advance, accumulate, dispatch) and `--loop` runs it on an interval under a single-instance guard.
- [x] All state reads go through the native store with revision guards; no state.json text parsing.
- [x] Park/claim semantics implement the roadmap R-10..R-13 contracts (handoff-before-park, reasoned parks, claim release, budget reset on unpark).
- [x] Dispatch consults `board reconcile` and refuses to send a worker at a story whose implementation is already present, naming what it withheld rather than dropping it silently. `board next` answers a question about status, not about the tree: `wave-472` dispatched ten stories whose work was already in `main`, costing ten worker turns and ~29 GB of handoff verification for a wave whose every commit conflicted with the code it duplicated — and C-637, which shipped `reconcile`, was itself inside that wave. The check must **fail closed**: if reconcile cannot be read the tick does not dispatch, because an empty already-built set is indistinguishable from "nothing is already built".
- [ ] The bash autopilot is deleted from the roadmap repository once drive reaches parity, and AUTOPILOT.md documents the native command instead.

## Progress

- Tick mechanics (`--tick`/`--loop`, the durable single-instance lock, the fingerprint, and the
  fail-closed `board reconcile` gate) landed earlier in this story in
  `crates/flux-cli/src/board_fleet_cmd.rs`.
- Park/claim contracts closed here. R-10: `fleet park` harvests the committed work its worktrees
  already prove *before* it writes the pause, reports it as `data.harvested`, and journals
  `wave.park.harvested`. R-13: `fleet unpark` resets the rework budget the park exhausted — every
  story the park froze returns to its accepted handoff and `rework_attempts` is cleared
  (`data.budget_reset`) — so the human's answer buys real rounds. R-12: a park is not an ending, so
  the wave keeps its claim and `drive` withholds those items with `"reason": "parked"` and the
  recorded reason instead of reporting them as live work; the claim is released only when the wave
  provably ends (applied or cancelled). R-11 (reasoned parks) was already in place from C-639.
- Failing first: `unparking_resets_the_rework_budget_and_frees_the_stories_the_park_froze`,
  `unparking_never_invents_a_status_for_a_story_that_holds_no_accepted_handoff` and
  `drive_dispatch_names_the_park_that_holds_an_item_rather_than_a_generic_claim` in
  `crates/flux-cli/src/board_fleet_cmd.rs`, plus
  `parking_a_wave_harvests_committed_work_before_the_pause` in
  `crates/flux-cli/tests/board_fleet_cli.rs`.
- Outstanding: the final criterion is work in the roadmap repository (delete the bash driver, point
  `AUTOPILOT.md` at `flux fleet drive`). It cannot be performed from this repository.

## Notes

- Design: [a fleet pauses, resumes, and recovers](../designs/a-fleet-pauses-resumes-and-recovers-from-shutdown-and-cancellation.md)
  — R-10 (harvest before resume/park) and R-13 (single instance, counter hygiene) are subsumed by
  these native verbs.
- User-facing documentation: `website/docs/coding/fleet.md` — "Unattended driving" and "Parking a
  wave".
