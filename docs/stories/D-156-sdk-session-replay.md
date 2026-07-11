---
id: D-156
title: Session::replay — hermetic time-machine replay in the SDK
pillar: Agent
status: backlog
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
- [ ] Failing-first: a cassette-recorded `Storage::dir` session replays hermetically (deny-all
      executor proves nothing dispatches live); the report matches the recorded plans.
- [ ] A non-replayable (pre-cassette / in-memory) session errors with an honest message.
- [ ] `ReplayReport` re-exported.

## Progress
- (pending)

## Notes
- `crates/flux-flow/src/replay.rs:118`. Depends on D-142.
