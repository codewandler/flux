---
id: C-562
title: "Fleet status stays small and tells the operational truth"
pillar: Core
status: done
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli]
depends_on: [C-560]
note: "dogfood stop-line — status reached 2,694,752 bytes by embedding historical turn events and tool payloads"
done_override: "Acceptance boxes were written by the contracting session, not by the worker that delivered it; the auditable evidence is the accepted tag, the green repository gate and the merge commit named in the evidence entry."
---

# Fleet status stays small and tells the operational truth

## Goal

Make the default status and dashboard projections bounded, fast and useful even after repeated
failed/continued turns, while preserving detailed evidence behind explicitly bounded inspection.

## Acceptance

- [x] Failing first, a hermetic state shaped like the 2026-08-05 roadmap dogfood run produces a
      multi-megabyte `fleet status` because `last_turn`, intake receipts and historical event arrays
      are copied into the default projection. The fixed JSON and human projections remain below a
      reviewed fixed byte budget independent of retained history count and payload size.
- [x] Default status reports main state, active/attention worker counts, wave/item state, exact
      BoardRefs, repositories, current sessions, last transition/error summaries and current
      revision without embedding answers, tool events, diffs or repository contents.
- [x] `inspect worker|result|activity|wave` remains the route to detail, honors its explicit item and
      byte bounds, and returns omission metadata/references when retained evidence is larger.
- [x] Dashboard and status agree on active workers. A completed, failed, cancelled or interrupted
      process is never counted as active merely because an old receipt said `working`.
- [x] Redaction happens before projection budgeting; adversarial secrets and one huge event cannot
      appear in the default output or its omission metadata.
- [x] Stable JSON fixtures cover empty, one active worker, five concurrent workers, repeated failures
      and a long-lived state file; human output names the next useful inspect/recovery command.

## Notes

- Observed output: 2,694,752 bytes at roadmap Fleet revision 150 on 2026-08-05.
- Evidence: `cargo test -p flux-cli fleet_` (35 passed) plus
  `cargo test -p flux-cli --test board_fleet_cli fleet_run_launches_a_real_local_story_agent_in_its_child_worktree`.
  The hermetic dogfood fixture retains more than 2,500,000 bytes of state while the default
  projection stays inside 65,536 bytes and the human projection inside 4,096 bytes.


## Evidence

- Delivered by a fleet worker in wave-346, gated green by flux's full release gate, accepted as fleet/accepted/wave-346/flux and merged into main as d68abd2e. Its bounded projection supersedes an earlier one in main — the same story implemented twice because this record still read `ready` while the work was committed. C-562's version is a superset (byte budget, omission records, next_command) and its worker-liveness derivation now also carries the supervisor check.
