---
id: C-243
title: "`fleet.start` + `ProcessRuntime` — flux never spawns flux, so no wave can be larger than one"
pillar: Core
status: ready
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
- [ ] **Failing-first test**: start a worker, `fleet.status` it, `fleet.stop` it — a start/stop/status
      round-trip. Prove the ops are absent at the merge base.
- [ ] An `AgentRuntime` port with `start`/`stop`/`status`/`endpoint`, plus `ProcessRuntime` and
      `ExternalRuntime` as its two implementations. The port is what A-124/A-125 later implement, so
      nothing container-specific leaks into it.
- [ ] Spawning goes through `flux-system`'s guarded spawn — argv-only, no shell string, workspace-
      pinned. No second `Command::new`.
- [ ] The worker is scoped to the worktree `fleet.isolate` returned and bound to the item's
      `context_id`, so a later `fleet.dispatch` resumes the same session (A2A continuity on
      `contextId` is already implemented — `crates/flux-server/src/a2a.rs:88`).
- [ ] Its A2A endpoint is registered on the board item, so a restarted coordinator can re-derive
      in-flight workers.
- [ ] A worker that dies is reported as dead by `fleet.status` rather than appearing live — a test
      kills the subprocess and asserts the status.
- [ ] The layering rule holds (`cargo test -p flux-codegate`); classify any new crate in
      `flux-codegate`'s `layer()` map.
- [ ] Standard gate green in both workspaces.

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
