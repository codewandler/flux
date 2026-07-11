---
id: D-149
title: AgentSpec knobs on ClientBuilder — groups, ambient signals, compaction, context budget
pillar: Agent
status: ready
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
- [ ] Failing-first: with a gating `ToolGroup`, an op is not advertised until its signal fires
      (catalog-capturing mock provider, mirror the evidence-gated surfacing tests).
- [ ] `with_compaction`/`compact_threshold_chars` honored (compaction event recorded when the
      threshold trips on a long mock conversation).
- [ ] `flux_evidence::ToolGroup` + `Observation` nameable via `flux_sdk::observe`.

## Progress
- (pending)

## Notes
- `AgentSpec` fields at `crates/flux-agent/src/lib.rs:131-161`; builder overlay only — no engine
  changes. Depends on D-143 (envelope.rs).
