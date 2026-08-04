---
id: C-528
title: "Execute independent native tool calls from one model response concurrently"
pillar: Core
status: in-progress
priority: 30
areas: [flux-flow, flux-runtime]
note: "Flux-Lang parallel branches and runtime concurrency ceilings already exist, but the native agent loop awaits every model-emitted call in serial order; one response with N independent reads therefore pays N tool latencies"
---

# Execute independent native tool calls from one model response concurrently

## Goal

When a provider returns several independent native tool calls in one assistant message, execute the
safe calls concurrently while retaining Flux's authorization, approval, resource-limit, transcript,
and deterministic-observation contracts.

This is not the authored Flux-Lang `parallel` feature. That path already overlaps branches. The gap
is the host code that consumes a provider response: both the model-stage gather loop
(`crates/flux-flow/src/staged.rs`, currently `for ... execute_flow_with_composites(...).await`) and
adaptive exploration await one call completely before starting the next. C-290's
`max_concurrent_tool_calls` can bound concurrency, but these loops never create any concurrency for
it to bound.

## Acceptance

- [ ] Add a failing-first test in `flux-flow` whose provider emits at least two independent,
      gather-safe calls in one assistant message. A blocking test tool proves that both calls become
      active before either is released (`max_active > 1`); elapsed-time-only evidence is not enough.
- [ ] Model-stage gather and adaptive exploration use one shared native-call batch executor rather
      than maintaining separate scheduling implementations. Any other native model-response path is
      inventoried and either adopts that executor or records why its semantics intentionally differ.
- [ ] Batch admission classifies every call before execution. Calls may overlap only when their
      resolved operation, staging disposition, intents, semantic effects, and approval posture prove
      them gather-safe and independent. Unknown, effectful, approval-sensitive, or conflicting calls
      remain ordered; there is no "parallel by default" escape from authorization or approval.
- [ ] The provider transcript remains valid and deterministic: one assistant message containing N
      `tool_use` blocks is followed by one user message containing exactly N matching `tool_result`
      blocks in the provider's original call order, regardless of completion order. Invalid and
      unavailable calls occupy their original result positions too.
- [ ] A sibling failure is returned in its own ordered result slot and does not discard or
      automatically cancel successful siblings. Explicit turn cancellation and deadlines still
      reach every in-flight call, and no task survives the turn that owns it.
- [ ] Every call retains the same immutable turn/session identity, runtime context, redactor,
      authorization and guarded-IO path as serial dispatch. Concurrent progress may reflect real
      completion order, while durable batch observations include stable call index/id so replay and
      diagnosis do not depend on scheduler order.
- [ ] `ResourceLimits::max_concurrent_tool_calls` remains the authoritative execution ceiling. A
      failing-first test with a batch wider than N proves observed active executions never exceed N,
      and a configured queue timeout produces an ordered, actionable refusal rather than a hang.
- [ ] A regression test proves effectful/conflicting calls retain the existing ordering and failure
      semantics (`max_active == 1`, with later fail-fast actions skipped where the approved action
      batch contract requires that). A model cannot gain concurrency by mislabelling arguments.
- [ ] The planner/native-tool guidance tells capable providers to emit independent gather calls in
      one response when useful; it does not tell them to parallelize writes or approval-sensitive
      work.
- [ ] Targeted tests and the full repository gate are green. Before the pull request,
      `scripts/build-embedded-docs.sh` is run, any generated
      `crates/flux-server/assets/public-docs.zip` delta is inspected and committed, and
      `scripts/build-embedded-docs.sh --check` plus website documentation tests pass.

## Progress

- 2026-08-04: filed from direct source inspection. `staged.rs` serializes model-stage gather calls at
  the loop beginning near line 811 and adaptive exploration calls at the loop beginning near line
  1329 on `7744f27d`. No existing story names or accepts native model-response batch concurrency.
- 2026-08-04: implementation started from dispatched `origin/main` commit `8d017935`; the primary
  checkout and its user-owned changes remain untouched in a dedicated story worktree.
- 2026-08-04: failing-first evidence recorded by
  `model_stage_native_batch_overlaps_independent_gather_calls`: the serial baseline reached
  `max_active == 1` and failed because the second read could not start before the first was
  released. One shared classifier/batch executor now serves model stages and adaptive exploration;
  the focused `flux-flow` suite and clippy are green. Pre-tool hooks, active cassettes,
  approval-sensitive calls, non-idempotent calls, and incomplete `Network`-without-`Read` metadata
  remain ordered or captured.

## Notes

- Related, not duplicate: [C-290](C-290-runtime-resource-limits.md) supplies the concurrency ceiling;
  it does not schedule native model-response calls. [C-451](C-451-the-head-to-head-benchmark.md)
  measures performance but does not close this implementation gap.
- Approved action batches currently preserve fail-fast order in
  `EngineLoopHost::execute_batch`. Keep that truth unless independence and failure semantics are
  specified and proven; the first useful slice is concurrent independent gathering.
- Do not implement this as `join_all` over unclassified calls. Scheduling is downstream of the
  runtime's effect/intent truth, not a provider hint.
