---
id: D-178
title: Resurrect — transparent mid-turn crash recovery
pillar: Agent
status: done
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
- [x] `Session::interrupted() -> Option<InterruptedTurn>` detects a `TurnStarted` with no `TurnEnded`
      (`turns()` `ended_at_ms == None`).
      — Engine driver: `flux_flow::resurrect::interrupted(events, session) ->
      Result<Option<InterruptedTurn>>` (the `Result` is stricter than `Option`: a crash during
      *planning*, with no accepted plan to resume from, is a loud `Err`, never a silent `None`);
      `Session::interrupted()` wraps it verbatim (read-only, opens no store, dispatches nothing).
- [x] `Session::resurrect(sink) -> Option<ResurrectReport>` finishes the open turn in-place on the
      same session: re-parse the plan (`plans_by_key`), fast-forward `StatementCompleted` prefix via
      `ResumeLedger::from_interrupted` (fold without an open `PlanHalted` latch), install
      `CassetteScope::Resume`, run `execute_flow_resumable_with_composites`, then `end_turn`.
      — flux-flow driver `flux_flow::resurrect::resurrect(events, store, executor, session,
      composites, sink) -> Result<Option<ResurrectReport>>`, wrapped by `Session::resurrect(sink)`
      (takes the SDK operation guard, so a resurrection is ordered against turns/replay/fork like
      every other session operation).
- [x] Failing-first exactly-once: kill a turn mid-op with a **counting fake op**; `resurrect`; assert
      the side-effect count did not increase for any op that had a recorded cell (served from tape),
      and the crash-tail op ran live exactly once.
- [x] The live tail still gates through the **real** approver; a served cell whose re-derived
      `input_hash` mismatches latches `ReplayDiverged` loudly.
- [x] `ClientBuilder::auto_resurrect(bool)` (default on for `Storage::dir`) runs `resurrect()`;
      always surfaced through the report/sink (never silent).
      — **Firing point moved from `open_session`/`latest_session` to the turn entries
      (`Session::send`/`send_with`), deliberately**: both openers are sync `fn`s and `resurrect` is
      async, so honoring the design sketch literally would have meant making the whole resume seam
      async (breaking, and it would drag IO into what is documented as a cheap handle mint). Running
      it as the first step of the next turn is the same guarantee — an interrupted turn is always
      finished before new input is processed — with no API break. Surfaced on the new
      `TurnOutput::resurrected` (boxed; `None` on the clean path) and streamed live to the caller's
      own sink in `send_with`. Default derived from `Storage::is_durable()`, so `Storage::custom`
      (typically Postgres) gets it too, and `Storage::in_memory` — where a crash takes the store with
      it — does not.
- [x] Docs state the honest at-least-once window (op fired before its cell was appended), mirroring
      Temporal activity semantics.

## Progress
- **2026-07-27 (engine slice, flux-lang + flux-flow only — SDK deferred).** Implemented per the
  approved plan (`.flux/plans/deterministic-agent-lab.md`, Wave 2b):
  - `flux-lang`: `ResumeLedger::from_interrupted(events: &[RunEvent], plan: &str) -> Option<Self>`
    (`crates/flux-lang/src/runtime.rs`, directly under `fold`) — gathers `StatementCompleted` for
    `plan` WITHOUT requiring an open `PlanHalted` latch; resets on node-index non-increase (restart)
    or a consuming `PlanResumed{prior==plan}`. 3 tests (no-latch gather where `fold` regression-pins
    `None`; last-execution-only on restart; equals `fold` when the latch is open).
  - `flux-flow`: new `crates/flux-flow/src/resurrect.rs` — `InterruptedTurn`, `ResurrectReport`
    (both `#[non_exhaustive]`), `pub fn interrupted(events, session)`, `pub async fn
    resurrect(events, store, executor, session, composites, sink)`. Crash-tail slice = `OpRecorded`
    cells strictly after the last `StatementCompleted`/`PlanHalted` row (empty when there is no
    statement row at all). Installs `CassetteScope::Resume`, runs the shipped
    `execute_flow_resumable_with_composites` on the SAME session, closes the turn via
    `EventStore::end_turn` + one assistant message (mirrors `finish_turn_lifecycle`'s ordering),
    resets the cassette scope on every path (a `Drop` guard, including `?` early-return). 10 tests
    (detection; none-when-closed; loud-err-crash-during-planning; the exactly-once headline;
    stale-prefix-cell exclusion; deny-approver halts the live tail; leftover-cell divergence latches;
    truncated-cell refuses without re-firing; crash-after-last-statement zero-dispatch close;
    awaiting-crash closes suspended with the suspension persisted).
  - Small precursor refactor: extracted `Cell::collect(trace)` in `cassette.rs` out of
    `ReplayTape::from_trace` so the crash-tail slice reuses the identical extraction (no duplicated
    match arm); made `engine::suspension_prompt` `pub(crate)` so the driver reuses the exact live
    suspension text.
  - Gate (scoped): `cargo test -p codewandler-flux-lang` (361 passed), `cargo test -p
    codewandler-flux-flow --lib` (all green except one pre-existing failure in the untracked,
    concurrently-edited `whatif.rs` — D-176 Tune, another agent's in-flight work, not touched by
    this story), `cargo clippy -p codewandler-flux-lang -p codewandler-flux-flow --all-targets -- -D
    warnings` (clean), `cargo fmt --all` (clean; only this story's own files show diffs — the
    workspace's other in-flight staged/uncommitted changes were unaffected), `cargo test -p
    flux-codegate` (11 passed). One unrelated pre-existing gate failure noted, not fixed (out of
    scope, orchestrator-owned): `website_customer_changelog_is_in_sync` — `CHANGELOG.md` is
    currently mid-edit (uncommitted) by another agent.
  - SDK slice deferred at this point — `crates/flux-sdk/` was another agent's active file.
- **2026-07-28 (SDK slice — story complete).** `Session::interrupted()`/`Session::resurrect(sink)`
  wrap the driver; `Session` carries an `auto_resurrect` flag set from
  `ClientBuilder::auto_resurrect` (defaulted from the new `Storage::is_durable()`), and
  `send`/`send_with` run an `auto_resurrect_step` before the new turn. `TurnOutput` gained
  `resurrected: Option<Box<ResurrectReport>>` (boxed — the report is large and almost always
  absent); `ResurrectReport`/`InterruptedTurn` gained `Clone` so it can ride on the cloneable
  `TurnOutput`. Every throwaway `Session` the SDK mints internally (a `Fork`, a `Counterfactual`, a
  `Scenario` work dir) sets `auto_resurrect: false` — none of them is a crashed production session.
  Deliberate: an `interrupted()` **error** inside the auto step (a crash during planning, with no
  durable plan to finish) is not fatal to the new turn — there is nothing to resurrect, and refusing
  new input because of an old unfinishable turn would be worse than the crash. Explicit
  `Session::resurrect`/`interrupted` still report it loudly.
  5 new tests in `crates/flux-sdk/tests/resurrect.rs`, seeding a crash exactly as the engine-level
  suite does but through the public `event_store()`/`engine()` escape hatches (no test-only
  backdoor): the exactly-once headline under a **never-called provider** (the completed statement is
  fast-forwarded, its side effect never re-fires, only the tail runs live); auto-resurrect on by
  default for `Storage::dir` and reported on `TurnOutput` without polluting the new turn's text;
  `auto_resurrect(false)` leaving the turn open for the embedder; in-memory defaulting off; and
  `interrupted()` being a clean `None` on a healthy session.
  Gate green in both workspaces (build/test/clippy `-D warnings`/fmt), plus `flux-codegate` and
  `codewandler-flux-sdk` on both the default and `test-kit` feature configurations.

## Notes
- New: `crates/flux-flow/src/resurrect.rs` (~290 LOC, modeled on `replay.rs`/`fork.rs`),
  `ResumeLedger::from_interrupted` in `crates/flux-lang/src/runtime.rs` (directly under `fold`).
  `Session::interrupted/resurrect` + `ClientBuilder::auto_resurrect` are the follow-up story's job.
- Depends on D-175 (`Resume` scope + `serve_nonlatching`) — landed.
