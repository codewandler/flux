---
id: A-54
title: Addressable tasks — task-state projection, non-blocking send, server-side tasks/get
pillar: Agent
status: done
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
note: "Tier-3 foundation: everything else (cancel/resubscribe/push) builds on retained, addressable tasks"
---

# Addressable tasks — projection, non-blocking send, tasks/get

## Goal
Make an A2A `Task` addressable after the turn ends: fold a stream's events into a `Task` projection
(state + [A-52] history/artifacts), honor non-blocking `message/send` by running the turn in the
background and returning `submitted`/`working` immediately, and implement server-side `tasks/get`
(realm-scoped) so a client can poll a task to completion. This is the foundation the rest of Tier 3
builds on.

## Why (evidence)
- `crates/flux-server/src/a2a.rs` runs every send synchronously and returns a `completed` `Task`
  whose id no later call can act on; `configuration.blocking` is ignored.
- `tasks/get` is client-only (`crates/flux-a2a/src/client.rs`); the server has no handler.
- The substrate exists: `events.db` projections ([event-store-unification]), the D-69 realm key, the
  C-18 session tag + lazy TTL sweep. See [a2a-stateful-task-model](../designs/a2a-stateful-task-model.md).

## Acceptance
- [ ] A `task(events) -> Task` projection (state from lifecycle/turn events; history/artifacts from A-52),
      realm-scoped like the conversation projection.
- [ ] Non-blocking `message/send` (blocking absent/false) returns `submitted`/`working` + `task-id`
      immediately and runs the turn on a background task that records its lifecycle to `events.db`.
- [ ] The blocking fast path (`blocking: true`) is preserved bit-for-bit (no regression).
- [ ] Server-side `tasks/get` resolves `task-id` → current `Task` within the caller's realm;
      unknown/other-realm/swept id → `-32001 TaskNotFound`.
- [ ] Failing-first tests: a non-blocking send returns non-terminal immediately then `tasks/get`
      observes it reach `completed`; a blocking send is unchanged; `tasks/get` on an unknown id is
      `-32001`; a cross-realm `tasks/get` is `-32001` (not distinguishable from unknown).

## Progress
- 2026-07-08 — done. `Task` projection = `project_task`/`project_stored_task` in
  `crates/flux-server/src/a2a.rs`: registry-first (live state), else a fold over the engine's own
  `turn_started`/`turn_ended` events (no second store; `cancelled`→canceled, `error`→failed,
  else completed; started-without-ended → optimistic `working` for cross-replica truthfulness;
  no turn events → `submitted`), realm-scoped exactly like the conversation projection (non-A2A
  and cross-realm ids = constant `-32001`). Non-blocking `message/send` (spec default; shared
  `flux_a2a::server::blocking_requested`) answers `submitted`+id immediately and drives the turn
  via `run_background` (gate → `enter_turn` identity swap → working → `run_turn_cancellable` →
  terminal transition). Blocking fast path preserved (same completed-Task shape; existing tests
  now opt in via `blocking: true`). Server-side `tasks/get` wired through the shared
  `dispatch_rpc` (single + multi mounts). **C-29 generalized:** every A2A run (blocking,
  streaming, non-blocking) registers in the new in-process `TaskRegistry` under ONE lock hold
  with the mint + TTL sweep (`mint_and_register`), and the sweep excludes live tasks via the new
  `EventStore::prune_inactive_excluding` — non-blocking mints made mid-turn sweeps possible, so
  the gate-held-mint rule alone no longer protects queued/running sessions. Realm for non-turn
  ops/mint = new `crate::caller_realm` (documented narrow relaxation of the D-69 coupling; the
  swap still happens gate-held before any turn). Tests: conformance
  (`non_blocking_send_returns_submitted_then_get_observes_completed`,
  `tasks_get_unknown_and_non_a2a_ids_are_not_found`, multi-mount `tasks/get`) + principal-mode
  `task_surface_is_realm_scoped_with_constant_not_found`. NOT a breaking signature change in the
  end (registry is internal state) — but the non-blocking DEFAULT is a wire-behavior change →
  minor bump.
- 2026-07-14 — A-87 supersedes the mutable identity wording above: A2A turns now pass an immutable
  `TurnIdentity` through the engine-owned `run_turn*_as` entry points after acquiring the turn gate;
  there is no `enter_turn` identity swap.

## Notes
- `input-required`/`auth-required` (resume-on-`taskId` via the suspend/resume seam) rides this story's
  async model; split it out if the resume seam proves large. **Still open after this story — the
  one remaining Tier-3 slice.**
- One context runs one task at a time (task id = session id): a concurrent second send on a live
  context is refused with a clear error; blocking sends queue on the gate as before.
- Epic: [a2a-conformance](../designs/a2a-conformance.md).
