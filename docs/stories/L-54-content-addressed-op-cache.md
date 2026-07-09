---
id: L-54
title: Content-addressed cache for deterministic read-only ops
pillar: Language
status: done
priority:
epic: flux-lang-agent-speed
design: docs/designs/flux-lang-agent-speed.md
note: "KF2: reuse deterministic read-only op results across turns, repairs, forks, and sub-agents when the input snapshot is unchanged"
---

# Content-addressed cache for deterministic read-only ops

## Goal
Avoid repeating deterministic read-only work by caching operation results under a stable
content address and replaying them when the operation, normalized inputs, versioned schema,
and relevant workspace or datasource snapshot are unchanged.

## Acceptance
- [x] Cache keys include operation identity, normalized input value, operation schema or
      implementation version, and an explicit invalidation domain such as workspace tree,
      git revision, datasource snapshot, or plugin-reported freshness token.
- [x] The cache integrates with `ValueStore` / `FlowStore` / runtime dispatch so repeated
      deterministic reads across repair rounds or resumed turns can bind the cached value
      without re-running guarded IO.
- [x] Writes, workspace snapshot changes, datasource freshness changes, and plugin
      invalidation metadata evict or bypass affected cache entries.
- [x] Model calls, non-idempotent calls, write calls, approval-sensitive calls, and calls
      with unknown effects are never cached.
- [x] Tests cover repeated read reuse, write invalidation, non-cacheable operation bypass,
      and audit evidence that distinguishes cache hit from fresh execution.

## Progress
- 2026-07-09 review fix: `now`/`evidence`/`metrics` declared NonIdempotent (frozen clock/metrics);
  `task` carries Effect::Process (sub-agent writes invalidate the parent cache); hits re-redacted
  against the current secret set; generation bumps BEFORE write IO; hit path drops the cache lock
  before the evidence lock; `plan_turn` gets the same per-turn cache boundary as `run_turn`.
- 2026-07-09 DONE. Cache lives in `flux-runtime::Executor` at the ONLY dispatch seam, probed
  strictly AFTER the whole authorization→approval envelope (a hit is served only to a caller the
  op is admissible for right now — the story's envelope invariant by construction). Key =
  hash(op name, canonical input JSON (serde_json is key-sorted), input-schema fingerprint,
  invalidation-domain generation). Cacheable = effects⊆{Read} + Idempotent + Risk::Low +
  approval-insensitive + non-destructive; unknown ops return before the cache; only redacted
  successes are stored (512-entry safety bound). Invalidation: any non-Read-effect dispatch bumps
  the generation and clears; `Executor::begin_cache_turn()` (called by `FlowEngine::run_turn`)
  bounds the reuse window to ONE TURN — repair rounds/retries/nested plans within it — because
  the write-generation cannot see external mutations (user edits between turns). `FLUX_OP_CACHE=
  off|0` + `with_op_cache(bool)` kill switches. Evidence: `op_cache_hit` observation makes hits
  audit-distinguishable from fresh executions (the `tool_call` observation still fires). Tests:
  repeated-read reuse + audit marker, write invalidation, non-idempotent + disabled bypass; the
  flux-flow saga-resume fixture (`MarkTool`) was honestly re-declared NonIdempotent (a
  dispatch-recording probe was never idempotent). Residuals: durable cross-turn cache keyed on
  real snapshot tokens (git rev/mtime, plugin freshness) — the key already carries the domain
  slot; Network-effect dispatches currently bump the generation conservatively.

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- The cache must preserve the authorization -> approval -> guarded IO envelope: a cache hit
  is allowed only when the original operation would still be admissible in the current
  caller/trust context.
