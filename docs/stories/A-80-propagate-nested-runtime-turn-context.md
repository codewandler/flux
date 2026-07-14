---
id: A-80
title: Propagate cancellation and session lineage through nested adapter runtimes
pillar: Agent
status: backlog
priority: 2
epic: live-sub-agent-activity
note: "a guarded adapter's one-shot FlowClient inherits live reporting now, but still loses parent cancel + session lineage"
---

# Propagate cancellation and session lineage through nested adapter runtimes

## Goal

When a guarded adapter tool opens a nested one-shot runtime, preserve the active parent turn's
cancellation and session lineage so a sub-agent spawned there cancels with the served request and records
the real parent correlation instead of behaving like an unrelated `flux-sdk` run.

## Acceptance

- [ ] Failing-first regression drives `parent FlowEngine -> adapter Tool -> nested FlowClient -> TaskTool`
      and proves cancelling the parent reaches the child without waiting for its wall-clock deadline.
- [ ] The nested `TaskTool` receives the real active parent session id, and a shared child audit stream
      records it as `correlation_id`.
- [ ] Turn context inheritance is lexical and concurrency-safe: parallel parent turns cannot exchange
      cancellation tokens or session ids, retained contexts cannot keep obsolete turn state, and a cloned
      context temporarily installing a nested reporter restores the outer reporter on exit.
- [ ] Direct one-shot `FlowClient` execution outside a parent turn retains its current no-cancel/no-parent
      behavior.
- [ ] Public runtime/SDK comments document the inheritance boundary; full workspace gates are green.

## Progress

- 2026-07-14 discovered while validating A-79 against ai-agent-platform's served manager route. Live
  activity now crosses the nested adapter through a lexically scoped reporter, but `FlowClient` still
  constructs a fresh context with no parent cancellation token or real session id.
- 2026-07-14 A-79 now pins a fresh one-shot context's reporter before streamed execution crosses
  `tokio::spawn`. This story still owns scoped restoration for deliberately shared/cloned context slots
  alongside cancellation and session lineage.

## Notes

- Keep this separate from A-79: live reporting is observational. Cancellation/session propagation changes
  lifecycle and persisted correlation semantics and needs its own failing-first coverage.
- The one-active-turn constraints on `ToolContext` still apply; use task/future-local inheritance rather
  than a process-global slot.
