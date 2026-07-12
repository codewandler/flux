---
id: D-150
title: ExecutionResult.usage — flow runs report token spend
pillar: Agent
status: done
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
- [x] Failing-first: a flow calling `ai.extract` twice reports summed non-zero `usage`; a
      pure-ops flow reports `None`.
- [x] `flux-cognition` records a usage observation per provider call (today it is dropped
      entirely — verify and fix at the source, not by re-parsing transcripts).
- [x] `ExecutionResult` marked `#[non_exhaustive]` (MINOR; flag in CHANGELOG alongside the
      field).

## Progress
- **Done (unreleased).** Fixed at the source: `flux-cognition`'s `run_model` now captures the
  call's `Usage` (`Chunk::Usage`, last-wins within a call) instead of discarding it, and
  `CognitionOp::execute` records a `cognition.usage` observation (op + model + usage) on the shared
  evidence log via `ctx.evidence` when the call billed anything (a free/`mock` call records nothing).
  `flux-cognition` gained an L0 `flux-evidence` dep (layering-legal).
- `ExecutionResult` gained `usage: Option<Usage>` + `#[non_exhaustive]`; a `cognition_usage(executor)`
  helper reads the observations back off `executor.evidence().by_kind("cognition.usage")` and **sums
  every field** (independent single-shot calls, not last-wins). Threaded through `finish_outcome`
  (execute/execute_with) and the `execute_optimized` literal.
- Failing-first tests: SDK `execution_result_sums_cognition_usage_and_none_for_pure_ops` (two
  `ai.extract` @100/20 → summed 200/40; a `read`-only flow → `None`); flux-cognition
  `cognition_op_records_a_usage_observation_when_the_call_bills` +
  `cognition_op_records_nothing_when_the_call_is_free` (proves the fix at the source).
- CHANGELOG + WHATS-NEW updated (breaking `#[non_exhaustive]` flagged MINOR) + website mirror. Gate
  green (workspace test 2159 / clippy / fmt / codegate; hit + cleared a full-disk ENOSPC mid-gate).
  **Not yet committed or released.**

## Notes
- Touches `crates/flux-cognition/src/lib.rs` + `crates/flux-sdk/src/flow.rs`
  (`ExecutionResult`, `crates/flux-sdk/src/flow.rs:587`).
- Accumulation convention: follow `flux_core::Usage::accumulate` (output summed,
  input/cache last-wins) only for same-conversation calls; independent cognition calls SUM
  inputs — document the choice in the field's rustdoc.
