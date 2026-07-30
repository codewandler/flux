---
id: C-243
title: "`fleet.start` + `ProcessRuntime` — flux never spawns flux, so no wave can be larger than one"
pillar: Core
status: done
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
      → `a_worker_is_confined_to_a_checkout_outside_the_workspace_root` (the arrangement
      `fleet.isolate` actually produces) + `a_workers_cwd_is_the_checkout_it_was_given` (the child's
      real cwd) + `a_worktree_must_be_an_existing_absolute_directory`. The `context_id` is minted per
      item and read back by `fleet.worker_status`. The worktree is a **parameter** the coordinator
      passes from `fleet.isolate`, not a call into it.
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

**2026-07-30 (rework round 1).** Independent review returned three blocking findings; all three were
real and all three are fixed. Recorded here because each one is a property, not a patch:

- **B1 — the op could not accept a `fleet.isolate` worktree.** It resolved the caller's path through
  `Workspace::resolve`, the *write*-path resolver, which admits only the primary and `@named` roots —
  while `allocate_worktree_dir` creates its parent *outside* every workspace root on purpose. So the
  documented composition errored at step 2. The resolve is gone; `System::rerooted`
  (`Workspace::with_root`: canonicalizes, must exist, does not require containment) is the guard, and
  what keeps it from being a blank cheque is the new authority contract — the checkout is a named
  `workspace.write` subject an operator approves — plus a hard absolute-path requirement. The original
  test missed this by building its checkout *inside* the test root; the regression test now mirrors
  `allocate_worktree_dir`'s sibling relationship.
- **B2 — under the C-262 unattended posture the returned endpoint was unreachable.** Unattended
  surfaces get `FLUX_SANDBOX=require` + `FLUX_SANDBOX_NET=0`; `spawn_background` is
  `Confinement::Sandboxed`, and `bubblewrap_argv` then adds `--unshare-net`, so the worker bound
  `127.0.0.1` inside its own netns while `await_ready` reported `live`. Two fixes: `start` now
  **refuses** when the coordinator's sandbox is active with the network closed, naming the remedy; and
  the sandbox posture is forwarded to the worker (`FLUX_SANDBOX`, `_NET`, `_WRITABLE`, `FLUX_BWRAP_BIN`,
  `FLUX_SANDBOX_EXEC_BIN`) while `FLUX_SANDBOXED` is withheld, mirroring `flux_eval`'s
  `SANDBOX_CHILD_ENV_KEYS` — without it a worker resolved its posture from an *empty* environment and
  ran unconfined while the operator demanded `require`. Proven in **both** lanes; see the report for
  what the ideal fix would need and why it is out of scope.
- **B3 — a worker's own catalog contained `fleet.start`, auto-approved.** `--yes` installs an
  `AllowApprover`, so the coordinator's first start was gated on `process.exec` and every start below
  it was not. Bounded by `FLUX_FLEET_DEPTH` (default 1 generation, matching `LocalSpawner`'s
  `max_depth`) plus a concurrent-worker cap. The marker travels only through the runtime's explicit env
  override, and `build_command` clears the child's environment first, so it cannot be forged.

Also fixed: the workers mutex is no longer held across the readiness wait (a stalling start blocked
`fleet.stop`/`fleet.worker_status` on every *other* worker); `with_base_port` actually works; the
cleanup claim at the construction site now states the `SIGKILL`/`process::exit` gap; `fleet.status`
carries the back-reference to `fleet.worker_status`; and `fleet.start` names the
`--allow-private-net` requirement in both its description and its result.

**2026-07-30 — landed (PARTIAL).** The port and both implementations are in, the ops are registered
in the production catalog, and a worker is a real guarded child process. What a resuming agent needs
to know:

1. **The status verb is `fleet.worker_status`, not `fleet.status`.** `fleet.status` already exists
   (A-116) and reads a *task* on a worker; a registry cannot hold two ops of one name, and the two
   questions have genuinely different answers — a task can be `completed` on a worker that has since
   died. The `AgentRuntime` port's method is still `status`, as the Acceptance specifies.
2. **The worktree is a parameter, not a `fleet.isolate` call.** `fleet.start { worktree }` takes the
   absolute path C-241 hands back, and composes:
   `fleet.isolate → fleet.start(worktree:) → fleet.dispatch(worker:, item:)`. Verified against C-241 as
   landed on `main`: it returns `<allocate_worktree_dir()>/checkout`, outside every workspace root.
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
7. **A network-isolated coordinator cannot start workers at all**, by design (see B2). The ideal fix —
   exempt the worker spawn from wrapping the way the local-eval child flux host is described as being,
   and let it apply the posture to its own descendants instead — needs a `spawn_background` counterpart
   to `System::run_with_env_exempt`, i.e. a change in `flux-system`. Out of scope here; the refusal is
   the honest interim.
