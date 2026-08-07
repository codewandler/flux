---
id: C-642
title: "fleet quiesce and resume make installing a new binary safe by construction"
pillar: "Core"
status: done
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "stopping dispatch by hand went wrong twice in one evening, once corrupting a full workspace test run"
---

# fleet quiesce and resume make installing a new binary safe by construction

## Goal

Make "stop the fleet so a new binary can be installed" a verb instead of a procedure. `flux fleet
quiesce` records a durable maintenance window that refuses every dispatch and confirms nothing is in
flight; `flux fleet resume` lifts it. This serves the Core value that recorded truth is askable and
recovery is a verb: the hand-driven version — a process-table scan followed by an install — went
wrong twice in one evening, once corrupting a full workspace test run.

## Acceptance

- [x] `flux fleet quiesce [--reason TEXT]` records a durable quiesce on fleet state before it
      inspects anything, so no wave can be dispatched between the check and the install.
- [x] While quiesced, `fleet run`, `fleet spawn` and `fleet task` refuse with
      `conflict/precondition` naming the recorded reason; inspection, handoff, integration,
      acceptance and reclamation stay available.
- [x] `quiesce` fails with `conflict/precondition` while any worker turn is in flight, naming each
      live worker, and succeeds with `safe_to_install: true` once they settle. Liveness is the same
      derivation `fleet status` uses, so a stale `working` row is not reported as in flight.
- [x] `flux fleet resume` lifts the recorded window and reports what it lifted; dispatch works
      again afterwards.
- [x] `fleet status` reports the window under `quiesce`, and its human line reads `quiesced` rather
      than `running`.
- [x] Failing-first test:
      `fleet_quiesce_stops_dispatch_and_refuses_to_confirm_while_a_worker_is_in_flight` in
      `crates/flux-cli/tests/board_fleet_cli.rs`.

## Progress

- Implemented in `crates/flux-cli/src/board_fleet_cmd.rs`: `QuiesceRecord` on `FleetState`, the
  `Quiesce` verb, the single `fleet_action_dispatches` guard in `run_fleet_action`, `fleet_quiesce`,
  the `Resume` pairing, and the `quiesce` field on the bounded status projection.
- Documented in `website/docs/coding/fleet.md` ("Quiescing before an install") and the `fleet skill`
  guide.

## Notes

- Design: [recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven](../designs/recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven.md).
- The wider lifecycle epic (`fleet pause`/`shutdown`/startup reconciliation, in
  [a-fleet-pauses-resumes-and-recovers-from-shutdown-and-cancellation](../designs/a-fleet-pauses-resumes-and-recovers-from-shutdown-and-cancellation.md))
  is deliberately out of scope here: this story closes only the install race, and its window is a
  recorded flag rather than a drain-to-boundary state machine.

