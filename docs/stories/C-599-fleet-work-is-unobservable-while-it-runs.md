---
id: C-599
title: "Make a running Fleet worker observable — activity on the rail, transcript on demand"
pillar: Core
epic: fleet-harness-throughput
status: ready
priority: 20
areas: [flux-tui, flux-cli]
note: "state.json is written twice per wave, so the TUI repaints zero times while workers run"
---

# Make a running Fleet worker observable

## Goal

Let an operator watching `flux tui --fleet` see that work is happening and drill into what a worker
is actually doing, without having to interrogate the coordinator — which today is both the only way
to ask and a way to destroy the wave ([C-598](C-598-a-concurrent-fleet-mutation-destroys-a-running-wave.md)).

## Acceptance — stage 1, post-hoc transcript (data already exists)

- [ ] `FleetWorkerView` carries the worker's session store path, derived from `agent_store_path`
      (deterministic; does not wait for a receipt).
- [ ] The Workers tab gains a transcript view over the worker's own `<store>/events.db`, opened
      read-only. Failing first, a test proves it renders tool calls and results for a finished
      worker, and reports a worker that has produced no events yet without erroring.
- [ ] A free key in the operations overlay opens it (`t`, `f`, `s`, `v` and `F3`–`F12` are all
      unbound today); `Esc` unwinds one level as the existing detail pane does.
- [ ] The transcript view is read-only and cannot dispatch, cancel or steer — the surface's authority
      posture in `docs/designs/board-fleet-tui.md` is unchanged.

## Acceptance — stage 2, live activity

- [ ] Worker NDJSON is observable while the turn runs. `guarded_agent_run_async` is the only place
      that sees the bytes as they arrive (it polls `child.read_output()` every 50 ms into an
      in-memory buffer and writes nothing until exit).
- [ ] The attention rail shows per-worker activity — at minimum the current operation and an idle/
      stalled age — while a wave runs.
- [ ] The refresh path repaints during a wave. `fleet_tui_refresh_token` hashes `state.json`,
      `fleet.toml`, `board.toml`, roadmap/decision files and git refs; none of them change between a
      wave's two writes, so the 1-second tick currently produces zero snapshots for the whole wave.

## Notes

- **Why nothing appears today.** During a wave, `state.json` is written exactly twice: once at
  `wave.agent-turns.delivered` before any worker starts, and once at
  `wave.agent-turns.completed|failed` after they all join. The TUI's 1s tick only snapshots when the
  refresh token changes, and the token is a hash over those files — so the entire wave is one silent
  interval. This is the whole answer to "why can't I see anything happening".
- **The data is live and already safe to read.** Each worker runs with
  `--store <git-dir>/flux-fleet/sessions/<id>`, and its `events.db` is appended op-by-op *during* the
  turn. `EventStore` is SQLite in mandatory WAL mode, explicitly designed for concurrent readers
  while one process writes. So stage 1's reader also works live — the gap is the refresh trigger, not
  the data.
- **`ChatState::project_session(events, session_id)` already renders an arbitrary session** from an
  arbitrary store into full tool cards. Reuse it rather than writing a second renderer. Caveat:
  `projection::historical_observation_entry` covers 5 of 26 observation kinds — story C-422 ("the
  render projection", `ready`) is the funded fix for that fidelity gap.
- **Two unrelated things are called "fleet" in the TUI.** The operations overlay (`operations.rs`,
  durable Fleet/Board state, `F2`) and the sub-agent pane (`fleet.rs`, live `WorkerRow`s with
  `running · read` / `stalled`). The second is exactly the projection this story wants — and it is
  structurally unreachable for Fleet workers, because it is fed by in-process `SpawnActivity` from
  the coordinator's *own* `task` children, two process hops away from a Fleet worker. Feeding it
  cross-process is stage 2's real design question.
- The per-worker detail pane that exists today (`Enter` on the Workers tab) is a static field dump
  plus the last 20 events of the last *completed* turn, each collapsed by `event_log_summaries` to
  `"{type} · {subject} · {outcome}"`. The receipt it reads from also carries `answer`, `usage`,
  `cost`, and every tool input and result body — all currently discarded by the view.
- `want —` / `drain —` on the rail are hardcoded `None` in both snapshot builders, pinned by a test.
  That is C-583 (`backlog`), not this story.
