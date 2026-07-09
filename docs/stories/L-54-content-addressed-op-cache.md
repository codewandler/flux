---
id: L-54
title: Content-addressed cache for deterministic read-only ops
pillar: Language
status: ready
priority: 10
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
- [ ] Cache keys include operation identity, normalized input value, operation schema or
      implementation version, and an explicit invalidation domain such as workspace tree,
      git revision, datasource snapshot, or plugin-reported freshness token.
- [ ] The cache integrates with `ValueStore` / `FlowStore` / runtime dispatch so repeated
      deterministic reads across repair rounds or resumed turns can bind the cached value
      without re-running guarded IO.
- [ ] Writes, workspace snapshot changes, datasource freshness changes, and plugin
      invalidation metadata evict or bypass affected cache entries.
- [ ] Model calls, non-idempotent calls, write calls, approval-sensitive calls, and calls
      with unknown effects are never cached.
- [ ] Tests cover repeated read reuse, write invalidation, non-cacheable operation bypass,
      and audit evidence that distinguishes cache hit from fresh execution.

## Progress
- (implementation not yet started)

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- The cache must preserve the authorization -> approval -> guarded IO envelope: a cache hit
  is allowed only when the original operation would still be admissible in the current
  caller/trust context.
