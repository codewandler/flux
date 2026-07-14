---
id: A-80
title: Propagate cancellation and session lineage through nested adapter runtimes
pillar: Agent
status: done
priority: 2
epic: live-sub-agent-activity
note: "nested adapter runtimes inherit live parent cancellation and session correlation without changing standalone one-shot behavior"
---

# Propagate cancellation and session lineage through nested adapter runtimes

## Goal

When a guarded adapter tool opens a nested one-shot runtime, preserve the active parent turn's
cancellation and session lineage so a sub-agent spawned there cancels with the served request and records
the real parent correlation instead of behaving like an unrelated `flux-sdk` run.

## Acceptance

- [x] Failing-first regression drives `parent FlowEngine -> adapter Tool -> nested FlowClient -> TaskTool`
      and proves cancelling the parent reaches the child without waiting for its wall-clock deadline.
- [x] The nested `TaskTool` receives the real active parent session id, and a shared child audit stream
      records it as `correlation_id`.
- [x] Turn context inheritance is lexical and concurrency-safe: parallel parent turns cannot exchange
      cancellation tokens or session ids, retained contexts cannot keep obsolete turn state, and a cloned
      context temporarily installing a nested reporter restores the outer reporter on exit.
- [x] Direct one-shot `FlowClient` execution outside a parent turn retains its current no-cancel/no-parent
      behavior.
- [x] Public runtime/SDK comments document the inheritance boundary; full workspace gates are green.

## Progress

- 2026-07-14 discovered while validating A-79 against ai-agent-platform's served manager route. Live
  activity now crosses the nested adapter through a lexically scoped reporter, but `FlowClient` still
  constructs a fresh context with no parent cancellation token or real session id.
- 2026-07-14 A-79 now pins a fresh one-shot context's reporter before streamed execution crosses
  `tokio::spawn`. This story still owns scoped restoration for deliberately shared/cloned context slots
  alongside cancellation and session lineage.
- 2026-07-14 failing-first regression
  `cargo test -p codewandler-flux-sdk nested_streamed_task_inherits_parent_cancel_and_session_lineage -- --nocapture`
  failed after 2.08s because parent cancellation did not reach the parked child before its 30-second
  deadline (`Elapsed(())`). The same command passes after the implementation.
- 2026-07-14 added one future-local `RuntimeTurnContext` for cancellation, parent session lineage and
  live child reporting; turn drivers scope it lexically, guarded adapters inherit it, and a fresh
  `FlowClient` context pins the snapshot before streamed execution crosses `tokio::spawn`. Added parallel,
  retained-context, nested-reporter and direct-one-shot negative coverage; child audit correlation uses
  the real parent session.
- 2026-07-14 adversarial surface review found the direct/resumable `flux flow run` paths discarded the
  reporter returned while installing a turn, so they preserved cancellation/session lineage but lost live
  child activity. Both CLI paths now execute inside the complete runtime-turn scope, with a regression that
  observes the real session and reporter. `set_turn` is marked `must_use`; its returned reporter is a public
  API signature change, so the next release must be a pre-1.0 minor (0.23), not a patch.
- 2026-07-14 verification is green: `cargo fmt --all -- --check`, `cargo build --workspace`,
  `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test -p flux-codegate`, and
  `UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync` (3/3).

## Notes

- Keep this separate from A-79: live reporting is observational. Cancellation/session propagation changes
  lifecycle and persisted correlation semantics and needs its own failing-first coverage.
- Runtime-turn state is future-local rather than process-global or stored on a shared engine context;
  higher-level `EngineLoopHost` model-stage accounting retains its independent serialization contract.
