---
id: C-562
title: "Fleet status stays small and tells the operational truth"
pillar: Core
status: ready
priority: 3
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli]
depends_on: [C-560]
note: "dogfood stop-line — status reached 2,694,752 bytes by embedding historical turn events and tool payloads"
---

# Fleet status stays small and tells the operational truth

## Goal

Make the default status and dashboard projections bounded, fast and useful even after repeated
failed/continued turns, while preserving detailed evidence behind explicitly bounded inspection.

## Acceptance

- [ ] Failing first, a hermetic state shaped like the 2026-08-05 roadmap dogfood run produces a
      multi-megabyte `fleet status` because `last_turn`, intake receipts and historical event arrays
      are copied into the default projection. The fixed JSON and human projections remain below a
      reviewed fixed byte budget independent of retained history count and payload size.
- [ ] Default status reports main state, active/attention worker counts, wave/item state, exact
      BoardRefs, repositories, current sessions, last transition/error summaries and current
      revision without embedding answers, tool events, diffs or repository contents.
- [ ] `inspect worker|result|activity|wave` remains the route to detail, honors its explicit item and
      byte bounds, and returns omission metadata/references when retained evidence is larger.
- [ ] Dashboard and status agree on active workers. A completed, failed, cancelled or interrupted
      process is never counted as active merely because an old receipt said `working`.
- [ ] Redaction happens before projection budgeting; adversarial secrets and one huge event cannot
      appear in the default output or its omission metadata.
- [ ] Stable JSON fixtures cover empty, one active worker, five concurrent workers, repeated failures
      and a long-lived state file; human output names the next useful inspect/recovery command.

## Notes

- Observed output: 2,694,752 bytes at roadmap Fleet revision 150 on 2026-08-05.
