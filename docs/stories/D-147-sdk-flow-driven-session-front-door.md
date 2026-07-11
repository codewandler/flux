---
id: D-147
title: Flow-driven sessions front door — Session::start_flow + suspended
pillar: Agent
status: done
priority: 6
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 2 — the D-131 differentiator reaches the SDK; durable await across restarts"
---

# Flow-driven sessions front door — Session::start_flow + suspended

## Goal
Expose the flow-driven session primitive (D-131) through the SDK: `Session::start_flow(&DraftAst)`
runs an authored flow to its first top-level `await` and surfaces the flow's authored prompt;
`send()` resumes; suspension state is queryable — durable human-in-the-loop that survives a
process restart.

## Acceptance
- [x] Failing-first: an authored flow with two `await`s — `start_flow` returns the authored
      prompt with `suspended: true`; `send("answer")` resumes to the next prompt; completing the
      flow flips `suspended: false`.
- [x] A process restart between the two awaits (drop client, rebuild over `Storage::dir`)
      still resumes via `open_session` + `send`.
- [x] `TurnOutput` gains `suspended: bool` and `#[non_exhaustive]` (MINOR; flag in CHANGELOG).
- [x] `FlowClient`'s `finish_outcome` suspension error retargets its message at
      `Session::start_flow` (`crates/flux-sdk/src/flow.rs`).

## Progress
- **Done (unreleased).** `Session::start_flow(&DraftAst)` + `Session::suspended()` added
  (`crates/flux-sdk/src/session.rs`); both delegate to the engine's `start_flow_turn` /
  `FlowStore::has_suspension` — no new dispatch path. `TurnOutput` gained `suspended: bool` and
  `#[non_exhaustive]`; a shared private `finalize` seam stamps the post-turn suspension state onto
  every turn door (`send`/`send_with`/`stream`/`start_flow`), so the flag is uniform.
  `FlowClient::finish_outcome`'s one-shot suspension error retargeted at `Session::start_flow`.
- Two failing-first tests in `crates/flux-sdk/src/lib.rs`:
  `start_flow_suspends_surfaces_prompt_and_send_resumes` (two-await interview, prompt + suspended
  flags across all three turns) and `suspended_flow_survives_a_process_restart` (drop the whole
  client mid-interview, rebuild over the same `Storage::dir`, `open_session` + `send` resumes). A
  `NeverMock` provider proves the deterministic skeleton never calls the planner.
- CHANGELOG + WHATS-NEW updated; breaking `#[non_exhaustive]` flagged MINOR. Gate green
  (workspace test/clippy/fmt/codegate). **Not yet committed or released.**

## Notes
- Engine seams: `start_flow_turn` (`crates/flux-flow/src/engine.rs:1095`), suspension-first
  resume branch (`:302-306`), `FlowStore::has_suspension`. Depends on D-142.
