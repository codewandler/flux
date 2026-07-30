---
id: C-243
title: "`fleet.start` + `ProcessRuntime` — flux never spawns flux, so no wave can be larger than one"
pillar: Core
status: in-progress
priority: 4
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-orchestrate, flux-tools, flux-runtime]
note: "F6 — absorbs A-120/A-121/A-122. Not an optimization: FlowEngine's turn_gate means one worker serves one turn, so this is the prerequisite for parallelism at all"
---

# `fleet.start` + `ProcessRuntime` — flux never spawns flux, so no wave can be larger than one

## Goal
Nothing starts a worker. `flux` never spawns `flux` (`agent-fleet-runtime.md:13`), and `FlowEngine`'s
`turn_gate` means one worker serves one concurrent turn
(`crates/flux-flow/src/engine.rs:172`). So a "wave" today is a wave of one, and every downstream
story in this epic is gated on fixing that. **`ProcessRuntime` is a prerequisite, not a performance
improvement.**

Land the `AgentRuntime` port and its first implementation: `fleet.start` spawns an agent as a
subprocess through `flux-system`'s guarded spawn, scoped to the `fleet.isolate` worktree, bound to a
per-item `context_id`, and registers its A2A endpoint on the board. `ExternalRuntime` covers an
already-running worker.

This story absorbs A-120, A-121 and A-122 from the `agent-fleet-runtime` epic.

## Acceptance
- [x] **Failing-first test**: start a worker, `fleet.status` it, `fleet.stop` it — a start/stop/status
      round-trip. Prove the ops are absent at the merge base.
      → `the_fleet_worker_lifecycle_ops_are_registered_and_named_by_worker`
      (`crates/flux-cli/src/execution.rs:2450`) fails at the base with "`fleet.start` is not
      registered"; the round trip itself is `a_worker_round_trips_start_status_stop`
      (`crates/flux-orchestrate/src/worker.rs`). The status verb is **`fleet.worker_status`**, not
      `fleet.status` — see Progress.
- [x] An `AgentRuntime` port with `start`/`stop`/`status`/`endpoint`, plus `ProcessRuntime` and
      `ExternalRuntime` as its two implementations. The port is what A-124/A-125 later implement, so
      nothing container-specific leaks into it.
      → `crates/flux-runtime/src/agent_runtime.rs`; impls in `crates/flux-orchestrate/src/worker.rs`.
- [x] Spawning goes through `flux-system`'s guarded spawn — argv-only, no shell string, workspace-
      pinned. No second `Command::new`.
      → `System::spawn_background` only; `scripts/check-no-direct-io.sh` and
      `no_raw_process_command_outside_system` both green.
- [x] The worker is scoped to the worktree `fleet.isolate` returned and bound to the item's
      `context_id`, so a later `fleet.dispatch` resumes the same session (A2A continuity on
      `contextId` is already implemented — `crates/flux-server/src/a2a.rs:88`).
      → `a_worker_is_confined_to_the_checkout_it_was_given` asserts the child's real cwd via
      `System::rerooted`; the `context_id` is minted per item and read back by
      `fleet.worker_status`. The worktree is a **parameter**, because C-241 is not on `main` yet.
- [ ] Its A2A endpoint is registered on the board item, so a restarted coordinator can re-derive
      in-flight workers.
      → NOT landed as a ledger write. `fleet.start` returns the endpoint and the following
      `fleet.dispatch item=…` records it as the item's `runner`; a first-class
      `DispatchLedger::record_worker` needs `BoardLedger` in `flux-capabilities`, outside this
      story's write set. See Progress.
- [x] A worker that dies is reported as dead by `fleet.status` rather than appearing live — a test
      kills the subprocess and asserts the status.
      → `a_killed_worker_is_reported_dead_rather_than_live` plus
      `a_worker_that_exits_reports_its_exit_code_and_output`.
- [x] The layering rule holds (`cargo test -p flux-codegate`); classify any new crate in
      `flux-codegate`'s `layer()` map. → no new crate; `cargo test -p flux-codegate` green (18/18).
- [ ] Standard gate green in both workspaces.
      → green except the two **fenced** operation-reference files, which now owe three rows each (the
      coordinator lands them). Everything else green, with and without a sandbox backend.

## Notes
- Depends on **F4 (C-241)** for the worktree to scope a worker to.
- `A2aSpawner` and `LocalSpawner` already implement `Spawner`; the `task` op's authority contract is
  the precedent to follow (`crates/flux-orchestrate/src/lib.rs:1077-1090`) — and the one A-116 got
  wrong, so read A-130's fix before designing the authority surface here.
- Deliberately later, against this same port: `DockerRuntime` (A-124), `KubernetesRuntime` (A-125),
  NDJSON-stdio transport, endpoint-broker discovery (A-123/A-126).
- A spawned worker is a real OS process with real authority. Its policy must be the *narrow* one —
  path-scoped write authority confined to its worktree
  (`crates/flux-policy/src/lib.rs:298-304`, `:589`, `:640`), which is what makes the fenced-ledger
  rule structural rather than instructional.

## Progress

**2026-07-30 — landed (PARTIAL).** The port and both implementations are in, the ops are registered
in the production catalog, and a worker is a real guarded child process. What a resuming agent needs
to know:

1. **The status verb is `fleet.worker_status`, not `fleet.status`.** `fleet.status` already exists
   (A-116) and reads a *task* on a worker; a registry cannot hold two ops of one name, and the two
   questions have genuinely different answers — a task can be `completed` on a worker that has since
   died. The `AgentRuntime` port's method is still `status`, as the Acceptance specifies.
2. **The worktree is a parameter, not a `fleet.isolate` call.** C-241 is still `ready` on `main`, so
   there was nothing to call. `fleet.start { worktree }` takes the path C-241 will hand back, and
   composes: `fleet.isolate → fleet.start(worktree:) → fleet.dispatch(worker:, item:)`.
3. **Board endpoint registration is owed.** `fleet.start` returns the endpoint but writes nothing.
   The composed path already registers it — `fleet.dispatch item=…` records the endpoint as the
   item's `runner` through `BoardLedger` — but a *first-class* `fleet.start` write needs a new
   `DispatchLedger` method whose only implementation lives in `flux-capabilities`, outside this
   story's write set. That is the next slice, and it is small.
4. **How a worker becomes reachable.** `flux app run --serve=127.0.0.1:<port> --yes`, port offered by
   the parent from `8790..+64` and *proven* by the child's own `bind` (this crate opens no socket —
   it is a model-facing operation crate). Readiness is the worker's own `flux server listening on
   http://…` line on stderr, so the ops declare no network access at all.
5. **A locally-spawned worker is on loopback, which `fleet.dispatch` refuses by default.**
   `fleet_private_net()` is `PrivateNetAllow::None`, so dispatching to a `ProcessRuntime` worker
   needs the operator's private-network opt-in. Nothing here widens that, and it is the first thing
   to hit when wiring F7/F9.
6. **`ExternalRuntime` is implemented but not wired into the CLI** — naming already-running workers
   is operator configuration that does not exist yet.
