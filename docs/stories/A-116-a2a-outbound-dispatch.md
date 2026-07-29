---
id: A-116
title: Outbound A2A dispatch — client cancel, A2aSpawner, and fleet.dispatch/status/cancel
pillar: Agent
status: ready
priority: 4
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-a2a, flux-orchestrate]
note: "the A2A server task surface is already done (A-53…A-57 in flux-server); this is the missing client half — A2aClient has no cancel, and only the flux a2a REPL can reach it"
---

# Outbound A2A dispatch — client cancel, A2aSpawner, and fleet.dispatch/status/cancel

## Goal
Let a journey or op dispatch work to a **remote** flux agent. Today the A2A client
(`crates/flux-a2a/src/client.rs:43`) exposes `send` / `get_task` / `await_task` / `stream` but no
cancel, and its only callers are the `flux a2a` REPL (`crates/flux-cli/src/a2a_cmd.rs:131`, `:218`).
The server side is already complete — non-blocking send, `tasks/get`, `tasks/cancel`,
`tasks/resubscribe` all live in `crates/flux-server/src/a2a.rs` (A-53…A-57) — so this is purely the
client half.

Remote workers are what make the fleet survive a coordinator restart and let each worker own its own
repo checkout.

## Acceptance
- [ ] `A2aClient::cancel_task(id)` wraps `tasks/cancel`, closing the client/server asymmetry.
- [ ] `A2aSpawner: Spawner` in `flux-orchestrate` (L3 → L1, a legal edge — no new crate, no
      `flux-codegate` `layer()` change): `spawn(SpawnRequest, cancel)` maps onto
      `send(blocking = true)`, and the passed `CancellationToken` fires `cancel_task`. Failing-first
      test: cancelling the token cancels the **remote** task, proven against a stub server.
- [ ] The existing `task` op works over `A2aSpawner` **verbatim** — zero new op surface for the
      blocking-delegate case, and every existing depth / cap-scope bound still applies.
- [ ] `fleet.dispatch` / `fleet.status` / `fleet.cancel` ops cover fire-and-**track**, which
      `Spawner`'s fire-and-await signature cannot express: `dispatch` wraps `send(blocking = false)`
      and returns the `task_id`, `status` wraps `get_task`, `cancel` wraps `cancel_task`.
- [ ] Accurate `effects`, `Risk`, `Idempotency` and concrete `permission_subjects` on all three ops —
      the dispatch target (the worker's guarded origin) is the subject, never `*`.
- [ ] Documented: fleet workers must be served by `flux serve` / flux-server —
      `flux_a2a::server::is_unsupported_a2a_method` (`crates/flux-a2a/src/server.rs:195`) still
      classifies `tasks/cancel` and friends as unsupported in the *embeddable* reduced dispatch.

## Progress
- (not started)

## Notes
- Design: [fleet-coordinator.md §4](../designs/fleet-coordinator.md).
- Layering checked: `flux-a2a` is L1 (`flux-codegate/src/lib.rs:41`), `Spawner`/`SpawnRequest`/
  `SpawnOutcome` are L2 (`crates/flux-runtime/src/lib.rs:573`, `:624`), `flux-orchestrate` is L3
  (`lib.rs:46`).
- Depends on A-113 only for the write-back target (`task_id`, `runner` on the board `Item`); the
  client and spawner work is independent.
