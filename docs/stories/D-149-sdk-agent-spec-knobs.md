---
id: D-149
title: AgentSpec knobs on ClientBuilder — groups, ambient signals, compaction, context budget
pillar: Agent
status: done
priority: 8
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 2 — evidence-gated tool surfacing + compaction control for embedders"
---

# AgentSpec knobs on ClientBuilder

## Goal
Pass through the `AgentSpec` knobs the CLI already uses but the SDK hardcodes:
`groups` (evidence-gated tool groups), `ambient_signals`, `compact_threshold_chars`,
`context_budget`.

## Acceptance
- [x] Failing-first: with a gating `ToolGroup`, an op is not advertised until its signal fires
      (catalog-capturing mock provider, mirror the evidence-gated surfacing tests).
- [x] `with_compaction`/`compact_threshold_chars` honored (compaction event recorded when the
      threshold trips on a long mock conversation).
- [x] `flux_evidence::ToolGroup` + `Observation` nameable via `flux_sdk::observe`.

## Progress
- **Done (unreleased).** `ClientBuilder` gained four `AgentSpec` pass-throughs
  (`crates/flux-sdk/src/lib.rs`): `groups(impl IntoIterator<Item = ToolGroup>)`,
  `ambient_signals(...)`, `with_compaction(chars)`, `context_budget(bytes)` — pure builder overlay
  onto `self.spec.*`, no engine change (Notes' `AgentSpec` fields unchanged).
- `flux_sdk::observe` extended with `SignalMatch` + `KIND_SIGNAL` (a gating `ToolGroup` — in the
  `groups` signature — can't be built without them; `ToolGroup`/`Observation` were already there).
- Two failing-first tests (`crates/flux-sdk/src/lib.rs`):
  `groups_gate_an_op_until_its_ambient_signal_surfaces` (a `WidgetTool` op `zzquux_probe` gated by a
  `ToolGroup`; a `SystemCaptureMock` captures the advertised op catalog — absent when gated, present
  once `ambient_signals(["widgets_on"])` surfaces the group) and
  `with_compaction_trips_and_records_a_context_compacted_observation` (`with_compaction(10)` + three
  prose turns trip compaction; asserts a `context.compacted` observation with `from > to` messages).
- CHANGELOG + WHATS-NEW updated (+ website mirror). Gate green (workspace test 2156 / clippy / fmt /
  codegate). **Not yet committed or released.**

## Notes
- `AgentSpec` fields at `crates/flux-agent/src/lib.rs:131-161`; builder overlay only — no engine
  changes. Depends on D-143 (envelope.rs).
