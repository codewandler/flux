---
id: D-132
title: Realtime/voice driver defers to flow suspensions (flow-driven voice)
pillar: Agent
status: in-progress
epic:
design: ../designs/flow-driven-voice.md
note: "downstream ask (ai-agent-platform R-20 voice half): a D-131 flow-driven session speaks its authored prompts over the realtime/voice provider and resumes on caller input"
---

# Realtime/voice driver defers to flow suspensions (flow-driven voice)

## Goal
Extend the D-131 flow-driven session mode to the **realtime/voice provider**: when a flow drives the
session, the voice driver **speaks the flow's authored prompts** (instead of letting the realtime
model improvise) and **resumes the suspension on caller input** — classic-IVR-shaped determinism over
the existing voice stack. Today `flux_flow::voice::VoiceSessionDriver` assumes the model owns the
conversation; a parked flow suspension has no voice pathway.

## Acceptance
- [x] A `mode`/entry on the voice driver runs a D-131 flow-driven session: the authored prompt at each
      suspension is spoken (TTS via the realtime channel), the caller's reply resumes the flow, and
      model cognition runs only where the flow calls it. `VoiceSessionDriver::run_flow_turns` +
      `EngineVoiceHandler` (a `FlowEngine`-backed `VoiceTurnHandler`); the driver speaks first at
      `SessionReady` and resumes via the engine's suspension-first routing. Failing-first:
      `flow_driven_voice_session_speaks_authored_prompts_and_hangs_up` — a two-`await` flow speaks its
      three authored prompts with a planner call-count of `0`.
- [x] Flow completion maps to the driver's existing terminal controls (hangup / handoff): a completed
      flow returns `VoiceReply::Complete`, the driver speaks the final line, fires the new
      `VoiceSink::session_ended` hook, and ends the session loop (`session.close()`). Asserted in the
      test.
- [x] Usage capture + event/turn projection work for flow-driven voice exactly as for model-driven:
      the handler drives through `FlowEngine::start_flow_turn`/`run_turn`, which already
      `begin_turn`/`end_turn` + `record_call_usage`. The test asserts three recorded first-class turns.
      Gate green.

## Progress
- 2026-07-10 — filed from the ai-agent-platform flows-arc design (R-20 covers text + voice together,
  per their product decision). Depends on D-131.
- 2026-07-11 — **DONE.** Design [flow-driven-voice.md](../designs/flow-driven-voice.md). The seam
  already existed (the test-only `run_flow_turns` + `VoiceTurnHandler` Phase-2 spike); D-132 made it
  real: `VoiceTurnHandler` gains `start()` (speak-first) and its `turn` now returns `VoiceReply`
  (`Continue`/`Complete`); `run_flow_turns` speaks the flow's opening prompt at `SessionReady` and
  ends the call on `Complete` via the new `VoiceSink::session_ended` hangup hook; `EngineVoiceHandler`
  bridges a `FlowEngine` (start/resume via a `PromptCapture` `AgentSink`), classifying continue-vs-
  complete with the new non-consuming `FlowStore::has_suspension`. Approval envelope + barge-in
  untouched (ops run through the engine's shared executor; the driver's own executor is unused in flow
  mode). One failing-first test; existing `flow_owns_two_voice_turns` updated to the `VoiceReply`
  shape. flux-flow 301/301, clippy + fmt clean. **SDK front door deferred** — `FlowClient` has no
  `EventStore` to assemble a `FlowEngine`; production voice callers (flux-server/flux-agent) already
  hold one, so the `pub` driver-level API is the deliverable (same shape as D-131's `start_flow_turn`).

## Notes
- The downstream consumer keeps its RTVBP boundary + audio bridge; this ask is only about the driver
  honoring a flow suspension as the source of the next assistant utterance.
- Downstream note: their `channel-rtvbp` currently constructs the backend before
  `session.initialize` — they track a deferred-construction change (R-18) that may land first on
  their side.
