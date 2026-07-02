---
id: C-14
title: Durable evidence trail — persist observations, record plan attempts, carry signal provenance
pillar: Core
status: ready
priority: 6
note: the plan graph reaches NO audit trail (flow.plan is sink-only; record_plan_attempt has zero production callers) and /evidence is an in-memory Vec lost on exit — while vision.md claims evidence is "recorded as auditable events" on event-sourced sessions
---

# Durable evidence trail

## Goal
Close the gap between the auditability claims (README:14 "a turn is a readable graph";
agent-loop.md "inspect its evidence with `/evidence`"; vision.md:97 "tool calls, destructive
markers, skill activations, and compaction are recorded as evidence") and reality, verified
2026-07-02: there are two unrelated audit systems — the in-memory `flux_evidence::EvidenceLog`
(what `/evidence` reads; lost on exit; admitted in agent-loop.md:85) and the durable
`flux-events::EventStore`. The compiled plan graph reaches NEITHER: the `flow.plan` observation
goes to the display sink only (`loop_host.rs:507`), and `EventStore::record_plan_attempt`
(`events/store.rs:553`) has zero production callers. Signal→group provenance is also dropped —
only active group names are recorded, never the signals that justified them (`engine.rs:472,798`).

## Acceptance
- [ ] **Failing-first:** `turn_evidence_persists_to_event_store` (flux-flow) — a mock turn against
      an in-memory store leaves `observation`-kind events (incl. `tool_call`) on the session stream.
- [ ] New `EventKind::Observation(flux_evidence::Observation)` (flux-events gains the
      flux-evidence dep — L2→L0, codegate-clean); `EventStore::record_observation` non-fatal.
      Emission = watermark flush in `FlowEngine` at BOTH turn-termination paths (cancel +
      completion); batched per turn (crash-loss documented — turn-granular audit is the goal, not
      crash forensics). First flush from watermark 0 also captures startup observations.
- [ ] **Failing-first:** `plan_attempts_recorded_with_fingerprint_and_text` — the loop host records
      `PlanAttempted` for: accepted (with AST fingerprint = the existing `transcript_hash`, plus
      `render_pretty` plan text capped at 8k — the human-auditable graph, round-trippable via
      `parse`), chat, compile_error, and user-rejected. `PlanAttempted` gains
      `#[serde(default)] fingerprint/plan_text` (old logs decode); `record_plan_attempt` takes a
      `PlanAttempt` struct.
- [ ] **Failing-first:** `groups_active_observation_carries_signals` — the per-turn `groups.active`
      observation carries the detected signal names alongside the resolved groups (one cheap record
      per turn; `detect_signals` already runs exactly once per turn).
- [ ] `/evidence` keeps reading the in-memory log (identical content for the live session); new
      `flux_events::projection::observations()` serves offline/programmatic reads. No duplication
      of `TurnStarted`/`TurnEnded`/`CallUsage`.
- [ ] `docs/agent-loop.md:85` persistence note updated to the new reality.
- [ ] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P6 of the round).

## Notes
- Layering: flux-evidence stays pure L0 (never sees flux-events); the engine (L3) is the tee point.
- Compile-internal repair rounds stay invisible (compile_turn doesn't expose them) — recorded
  decision, not scope creep.
- A-08 (sub-agent audit) builds directly on this story's event kinds.
