---
id: A-116
title: Outbound A2A dispatch — client cancel, A2aSpawner, and fleet.dispatch/status/cancel
pillar: Agent
status: done
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
- [x] `A2aClient::cancel_task(id)` wraps `tasks/cancel`, closing the client/server asymmetry.
- [x] `A2aSpawner: Spawner` in `flux-orchestrate` (L3 → L1, a legal edge — no new crate, no
      `flux-codegate` `layer()` change): `spawn(SpawnRequest, cancel)` maps onto
      `send(blocking = true)`, and the passed `CancellationToken` fires `cancel_task`. Failing-first
      test: cancelling the token cancels the **remote** task, proven against a stub server.
- [x] The existing `task` op works over `A2aSpawner` **verbatim** — zero new op surface for the
      blocking-delegate case, and every existing depth / cap-scope bound still applies.
- [x] `fleet.dispatch` / `fleet.status` / `fleet.cancel` ops cover fire-and-**track**, which
      `Spawner`'s fire-and-await signature cannot express: `dispatch` wraps `send(blocking = false)`
      and returns the `task_id`, `status` wraps `get_task`, `cancel` wraps `cancel_task`.
- [x] Accurate `effects`, `Risk`, `Idempotency` and concrete `permission_subjects` on all three ops —
      the dispatch target (the worker's guarded origin) is the subject, never `*`.
- [x] Documented: fleet workers must be served by `flux serve` / flux-server —
      `flux_a2a::server::is_unsupported_a2a_method` (`crates/flux-a2a/src/server.rs:195`) still
      classifies `tasks/cancel` and friends as unsupported in the *embeddable* reduced dispatch.

## Progress
- All six Acceptance items implemented. `crates/flux-a2a/src/client.rs` gains `cancel_task`; the
  rest is a new `crates/flux-orchestrate/src/fleet.rs` (`A2aSpawner` + the three `fleet.*` ops).
  `flux-a2a` added to `flux-orchestrate`'s `[dependencies]` — the L3 → L1 edge the story sanctions;
  `flux-codegate`'s layering lint passes untouched.
- **Cancellation has one documented window.** `spawn` races the blocking send against the token, but
  flux-server mints the task id server-side (`crates/flux-server/src/a2a.rs:420` `mint_and_register`)
  and `Message.task_id` is not honored as a client-assigned id — so a cancel landing *before* the
  send returns can only drop the in-flight request, not stop the worker. Once the worker hands the
  id over, cancellation propagates via `tasks/cancel`. Recorded at the seam in `A2aSpawner`'s doc
  comment. Closing the window entirely is a protocol change (client-assigned task ids), not this
  story's business.
- **Not wired: the board write-back.** `fleet.dispatch` returns the `task_id` in its result but does
  not write `task_id` / `runner` back onto a board item — A-113 lands `WorkBoard` without those
  fields on `ItemDraft`. Design §5 ("the board is the run registry") needs either a seventh board op
  or an extension to `claim`; filed separately by the coordinator.
- **Not registered into any surface toolset.** The `fleet.*` ops ship as `pub` types with
  `group: None`. `TaskTool` is registered individually by `flux-cli` / `flux-app` / `flux-sdk` and
  audited by `flux-cli/src/catalog_coherence.rs`; putting remote-dispatch ops in every agent's
  default surface is a safety-posture decision, and the design has the fleet coordinator assembling
  its own registry as a `.flux` Program.
- Gate green: `cargo test --workspace` (144 suites, 0 failures), `clippy --all-targets -D warnings`
  clean, `cargo fmt --check` clean in both workspaces, `cargo test -p flux-codegate` 13/13.

## Notes
- Design: [fleet-coordinator.md §4](../designs/fleet-coordinator.md).
- Layering checked: `flux-a2a` is L1 (`flux-codegate/src/lib.rs:41`), `Spawner`/`SpawnRequest`/
  `SpawnOutcome` are L2 (`crates/flux-runtime/src/lib.rs:573`, `:624`), `flux-orchestrate` is L3
  (`lib.rs:46`).
- Depends on A-113 only for the write-back target (`task_id`, `runner` on the board `Item`); the
  client and spawner work is independent.
