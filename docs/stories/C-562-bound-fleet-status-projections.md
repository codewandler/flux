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

## Progress

- 2026-08-06 — added C-562 to the active Board program as a Fleet/Board stop-line before C-570.
  Default `status` and `dashboard` now share `flux.fleet-status/v1`: bounded coordinator transition
  metadata, current worker/wave counts and summaries, repository/source health, active schedule
  entries, decision refs and explicit targeted-inspect routes. A greater-than-2 MB hermetic state
  projects below 256 KiB without answers, tool events, intake bodies or secrets; terminal workers
  with stale errors no longer count as active or attention. The real roadmap revision-247 state
  projects to 13,342 bytes with 19 cancelled workers, zero active/attention workers and no retained
  `last_turn` or event arrays.
- 2026-08-06 — completed the targeted-inspect boundary. Every inspect view now redacts before a
  192 KiB data budget and a 256 KiB final response ceiling, preserves terminal identity/status/
  outcome/session fields first, and replaces oversized strings, arrays or objects with indexed
  omission references into `.flux/fleet/state.json` or `.flux/fleet/events.ndjson`. Worker, result,
  activity and wave regressions prove large answers/tool events/handoffs/reviews stay out while
  terminal facts survive. Deterministic empty, one-worker, five-worker, repeated-failure and
  long-lived-state fixtures complete the status matrix.
- 2026-08-06 — the mandatory release gate and installed-build dogfood are green. At roadmap Fleet
  revision 247, installed `fleet status` serialized to 13,369 bytes with 19 terminal workers, zero
  active/attention workers and no embedded turn/event arrays; bounded inspection serialized the
  main worker to 31,458 bytes, 100 activity events to 121,470 bytes and cancelled `wave-242` to
  1,577 bytes, all below the 256 KiB response ceiling without omission. The restarted Fleet TUI was
  connected with color, `auto-ok` approval mode and zero failure-rail items. A subsequent live main
  turn reached the authored `ai_segment` directly without approval; the provider then returned its
  external account-level HTTP 429 usage limit, so no model-dependent semantic assertion is claimed.
