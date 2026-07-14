---
id: A-88
title: Supervise sub-agent cancellation through durable cleanup
pillar: Agent
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: parent cancellation drops the child future without the bounded cleanup used by deadline expiry
---

# Supervise sub-agent cancellation through durable cleanup

## Goal

Ensure parent cancellation cannot orphan a child provider/tool task or leave an audited sub-agent
session without a deterministic terminal record.

## Acceptance

- [x] Parent cancellation signals the child token, bounded-awaits the same cleanup/finalization path
      as deadline expiry, and only aborts/reaps after the documented grace period.
- [x] Failing-first hanging-provider and hanging-tool tests prove no child task/process remains after
      parent cancellation and the parent returns within the bound.
- [x] Audited child sessions flush usage and append their cancellation/turn terminal exactly once;
      history remains provider-valid with no empty assistant or split tool-use/result pair.
- [x] Nested-child cancellation (where explicitly enabled) propagates transitively and remains
      bounded; leaf/default-depth behavior is unchanged.
- [x] Cancellation, timeout, provider error, and successful completion share one supervised task
      owner and cannot race to double-finalize.

## Progress

- 2026-07-14 — Added one supervised child-task owner with cooperative cancellation, bounded cleanup,
  and abort/reap backstop. Hanging-provider, hanging-tool, usage/audit finalization, and transitive
  nested-cancellation tests prove bounded cleanup and exactly one provider-valid terminal.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Follow-up to D-05's cancellation/audit hardening. Sequence after A-86 where practical.
