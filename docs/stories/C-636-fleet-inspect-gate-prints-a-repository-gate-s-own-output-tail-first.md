---
id: C-636
title: "fleet inspect gate prints a repository gate's own output, tail first"
pillar: "Core"
status: ready
priority: 9
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "a failing gate's reason is the most-wanted fact in the system and currently requires knowing the shape of state.json"
---

# fleet inspect gate prints a repository gate's own output, tail first

## Goal

Make a failing gate's reason askable. `fleet inspect` already exposes every other recorded fact, but
the single most-wanted one — why a wave went red — still required knowing the shape of `state.json`
and writing a `jq`/Python expression over
`topology.repositories[].gate.evidence.stdout`. `flux fleet inspect gate <wave>` returns that
evidence directly, tail first, so the verdict survives both the view's `--limit` and its structural
byte budget.

## Acceptance

- [x] `flux fleet inspect gate <wave>` returns each repository's gate status, candidate, runs, argv,
      exit code and captured `stdout`/`stderr` from the wave topology, under the existing bounded,
      redacted `flux.fleet-inspect/v1` envelope.
- [x] Output is tail first: the last line the gate wrote is the first line reported, each stream
      reports `line_count` and `truncated`, and a gate that never ran reports its `reason` instead of
      evidence.
- [x] `--repository <id>` narrows a multi-repository wave to one gate and errors `not-found` for an
      id the wave's topology does not name; a missing wave and a missing target both error.
- [x] Failing first,
      `fleet_inspect_gate_prints_a_repository_gate_output_tail_first` proves the ordering and the
      narrowing, and `fleet_inspect_gate_keeps_the_failure_reason_inside_the_projection_budget`
      proves a 60 KB gate log still returns inside `FLEET_INSPECT_BUDGET_BYTES`.
- [x] `fleet status` names the new view as the next command for a red gate, and the CLI surface is
      covered by `scriptless_inspection_and_report_surfaces_are_bounded_and_deterministic`.

## Notes

- Design: [recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven](../designs/recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven.md).
- Implementation: `InspectView::Gate`, `gate_output_tail` and the `--repository` argument in
  `crates/flux-cli/src/board_fleet_cmd.rs`; documented in `website/docs/coding/fleet.md`.
