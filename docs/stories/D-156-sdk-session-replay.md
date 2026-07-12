---
id: D-156
title: Session::replay — hermetic time-machine replay in the SDK
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 4 — replay_session over the injected store; CLI-only no more"
---

# Session::replay — hermetic time-machine replay in the SDK

## Goal
`Session::replay(turn, sink)` wraps `flux_flow::replay::replay_session` over the session's
`(events, executor)` so an embedder can hermetically re-run a recorded session — zero live
dispatches — and inspect the `ReplayReport`.

## Acceptance
- [x] Failing-first: a cassette-recorded `Storage::dir` session replays hermetically (deny-all
      executor proves nothing dispatches live); the report matches the recorded plans.
- [x] A non-replayable (pre-cassette / in-memory) session errors with an honest message.
- [x] `ReplayReport` re-exported.

## Progress
- **Done (unreleased).** `Session::replay(turn, sink)` (`crates/flux-sdk/src/session.rs`) wraps
  `flux_flow::replay::replay_session(&engine.events, &engine.executor, id, turn, sink)` (mirrors the
  CLI's `run_replay`), mapping the `FlowError` to `flux_core::Error`. `ReplayReport` re-exported at the
  crate root.
- Failing-first tests (`crates/flux-sdk/src/lib.rs`): `session_replays_a_recorded_plan_hermetically`
  (client A records a `write`-plan turn on `Storage::dir`; client B built with a **`NeverMock`
  provider** — panics if the model is hit — reopens the session and replays: `report.plans` non-empty,
  `report.diverged` None, no panic → hermetic) and `replay_of_a_non_recorded_session_errors_honestly`
  (a chat-only prose turn has no cassette cells → error contains "not replayable").
- Cassette recording is gated only on `FLUX_CASSETTE` (default on), not storage — so a chat-only turn
  is the honest non-replayable case. CHANGELOG + WHATS-NEW + website mirror updated. Gate green
  (workspace 2167; clippy all-features / fmt / codegate). **Not committed/released.**

## Notes
- `crates/flux-flow/src/replay.rs:118`. Depends on D-142.
