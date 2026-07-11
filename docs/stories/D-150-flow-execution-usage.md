---
id: D-150
title: ExecutionResult.usage — flow runs report token spend
pillar: Agent
status: ready
priority: 9
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 2 — flux-cognition drops per-call usage today; surface + sum it"
---

# ExecutionResult.usage — flow runs report token spend

## Goal
A `FlowClient` execution reports what the model calls inside it cost: cognition ops
(`ai.extract/rank/judge/reason`, `synth`) surface per-call `Usage`, and
`ExecutionResult` gains a summed `usage: Option<Usage>`.

## Acceptance
- [ ] Failing-first: a flow calling `ai.extract` twice reports summed non-zero `usage`; a
      pure-ops flow reports `None`.
- [ ] `flux-cognition` records a usage observation per provider call (today it is dropped
      entirely — verify and fix at the source, not by re-parsing transcripts).
- [ ] `ExecutionResult` marked `#[non_exhaustive]` (MINOR; flag in CHANGELOG alongside the
      field).

## Progress
- (pending)

## Notes
- Touches `crates/flux-cognition/src/lib.rs` + `crates/flux-sdk/src/flow.rs`
  (`ExecutionResult`, `crates/flux-sdk/src/flow.rs:587`).
- Accumulation convention: follow `flux_core::Usage::accumulate` (output summed,
  input/cache last-wins) only for same-conversation calls; independent cognition calls SUM
  inputs — document the choice in the field's rustdoc.
