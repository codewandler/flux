---
id: C-639
title: "fleet park and unpark make parking a recorded lifecycle state"
pillar: "Core"
status: ready
priority: 11
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "parking lives in a driver-owned text file, invisible to fleet status, so a parked wave was re-decided every minute and unparking meant editing text"
---

# fleet park and unpark make parking a recorded lifecycle state

## Goal

Make pausing a wave a recorded lifecycle state of that wave instead of a line in a driver-owned text
file. `flux fleet park <wave> --reason` records why the wave is paused, `flux fleet unpark <wave>`
returns it to the state it held, and `fleet status` reports the pause and its reason — so a parked
wave is not re-decided every minute and unparking is a verb rather than an edit.

## Acceptance

- [x] `flux fleet park <wave> --reason TEXT` sets the wave's status to `parked` and records the
      reason, the status it is returning to, and the revision it was parked at, on the wave itself.
- [x] `flux fleet unpark <wave>` restores the recorded previous status and clears the park record. A
      wave parked by rework escalation, which carries no park record, returns to `awaiting-handoffs`.
- [x] Parking an already-parked wave and unparking a wave that is not parked are typed
      `conflict/precondition` failures; an unknown wave is `not-found`.
- [x] `flux fleet status` reports the park and its reason on the wave row in both the JSON and human
      projections, within the existing status byte budget.
- [x] Both verbs are journalled (`wave.parked` / `wave.unparked`), bump the revision, and appear in
      `flux fleet schema` as mutations supporting `--dry-run`, `--if-revision` and
      `--idempotency-key`.
