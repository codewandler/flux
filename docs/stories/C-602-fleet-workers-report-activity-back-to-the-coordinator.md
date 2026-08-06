---
id: C-602
title: "Stream Fleet worker activity back to the coordinator and persist it centrally"
pillar: Core
epic: fleet-harness-throughput
status: ready
priority: 18
areas: [flux-cli, flux-orchestrate, flux-events]
note: "keep isolated worker stores (they must survive containerisation); centralize a bounded activity projection the way SpawnActivity already does in-process"
---

# Stream Fleet worker activity back to the coordinator

## Goal

Give Fleet workers the same live-activity channel in-process sub-agents already have, so the
coordinator can persist a bounded projection of what every worker is doing into the shared store —
without requiring workers to share a database.

## Why not simply share one store

A Fleet worker is a separate OS process (verified: `flux fleet run` spawns
`flux … run --stream-json` via `System::spawn_background`, distinct PID and address space), whereas a
`task` sub-agent is a tokio task inside the parent. That difference is deliberate and worth
preserving: a worker is intended to become containerisable — Docker, k8s, a remote runner. A remote
worker cannot append to the coordinator's SQLite file at all, so "everyone writes the shared store"
is a design that stops working at exactly the point the isolation was for.

Write contention is *not* the reason to keep them separate. Measured 2026-08-06:

| worker | events | span | rate |
|---|---|---|---|
| `wave-275-worker-1` | 724 | 797 s | 0.91/s |
| `wave-281-worker-1` | 353 | 1250 s | 0.28/s |

~4.5 small appends/sec at `max_workers = 5` — orders of magnitude below what WAL SQLite sustains, and
nowhere near the 5s busy timeout `docs/designs/event-store-concurrent-use.md` R1 warns about. The
separate store is justified by *location independence*, not by contention.

## Acceptance

- [ ] A worker's activity reaches the coordinator process while the worker runs, over a transport
      that does not assume shared local storage.
- [ ] The coordinator persists a bounded projection of that activity into the shared store, so
      cross-worker analysis and reactive UI read one place. Bounded means: structural identity plus
      operation name and status — not tool inputs or result bodies.
- [ ] Failing first, a test proves activity from `max_workers` concurrent workers is recorded in
      order per worker, and that a worker crashing mid-turn leaves its prior activity intact.
- [ ] The worker's own full-fidelity history stays in its own store; the central projection is a
      pointer plus summary, never a wholesale copy (the `subagent.trace` precedent).
- [ ] `flux fleet` surfaces the projection without deriving a per-worker filesystem path.

## Notes

- **The in-process precedent to copy.** A-79 `SpawnActivity`
  (`crates/flux-runtime/src/lib.rs`) carries `spawn_id`, `role`, `child_session_id`,
  `parent_session`, `depth` and a closed `SpawnActivityEvent` set (`Planning`, `ToolCall`,
  `ToolTiming`, `ToolResult`, `Observation`, `Finished`), transported as a `subagent.activity`
  observation through the parent's `AgentSink`. It deliberately excludes child text, thinking and
  tool-result content: *"a worker's secrets cannot leak through a field that is never read."* Fleet
  needs the cross-process equivalent of exactly this, not something richer.
- **The bytes already exist and are already discarded.** `guarded_agent_run_async`
  (`crates/flux-cli/src/board_fleet_cmd.rs`) polls the worker's `--stream-json` stdout every 50 ms
  into an in-memory buffer and writes nothing until the process exits. It is the only place that sees
  worker events as they arrive, and today the entire wave produces exactly two `state.json` writes —
  one before the first worker starts, one after the last joins.
- **Consequence for the UI**: this is what makes
  [C-599](C-599-fleet-work-is-unobservable-while-it-runs.md) stage 2 possible, and it is the
  transport that keeps working when a worker moves off-host.
- Superseded alternative: an earlier draft of this story proposed writing worker sessions directly
  into the shared store. Rejected — it is the option that breaks first under containerisation, which
  is the reason the process boundary exists.
