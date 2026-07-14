---
id: D-178
title: Resurrect — transparent mid-turn crash recovery
pillar: Agent
status: backlog
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 4 — 'Temporal for agents'; depends on D-175 (Resume scope)"
---

# Resurrect — transparent mid-turn crash recovery

## Goal
Transparently finish a turn killed mid-execution (OOM / redeploy / crash) from the exact crash point:
zero model re-spend (the plan is durable source), no duplicate side effects for any op with a
recorded cassette cell, and a loud stop if the world diverged.

## Acceptance
- [ ] `Session::interrupted() -> Option<InterruptedTurn>` detects a `TurnStarted` with no `TurnEnded`
      (`turns()` `ended_at_ms == None`).
- [ ] `Session::resurrect(sink) -> Option<ResurrectReport>` finishes the open turn in-place on the
      same session: re-parse the plan (`plans_by_key`), fast-forward `StatementCompleted` prefix via
      `ResumeLedger::from_interrupted` (fold without an open `PlanHalted` latch), install
      `CassetteScope::Resume`, run `execute_flow_resumable_with_composites`, then `end_turn`.
- [ ] Failing-first exactly-once: kill a turn mid-op with a **counting fake op**; `resurrect`; assert
      the side-effect count did not increase for any op that had a recorded cell (served from tape),
      and the crash-tail op ran live exactly once.
- [ ] The live tail still gates through the **real** approver; a served cell whose re-derived
      `input_hash` mismatches latches `ReplayDiverged` loudly.
- [ ] `ClientBuilder::auto_resurrect(bool)` (default on for `Storage::dir`) runs `resurrect()` on
      `open_session`/`latest_session`; always surfaced through the report/sink (never silent).
- [ ] Docs state the honest at-least-once window (op fired before its cell was appended), mirroring
      Temporal activity semantics.

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- New: `crates/flux-flow/src/resurrect.rs` (~200 LOC, modeled on `replay.rs`/`fork.rs`),
  `ResumeLedger::from_interrupted` in `crates/flux-lang/src/runtime.rs` (`:676` fold pattern),
  `Session::interrupted/resurrect` + `ClientBuilder::auto_resurrect`.
- Depends on D-175 (`Resume` scope + `serve_nonlatching`).
