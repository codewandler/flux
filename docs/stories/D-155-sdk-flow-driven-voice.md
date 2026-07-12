---
id: D-155
title: Flow-driven voice front door — Session::run_voice_flow
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 3 — the deferred D-132 SDK seam; unblocked by D-142's engine-holding Session"
---

# Flow-driven voice front door — Session::run_voice_flow

## Goal
The engine-owned voice loop (D-132) reaches the SDK: `Session::run_voice_flow(provider, config,
flow, sink, cancel)` assembles `EngineVoiceHandler` over the session's engine and drives
`VoiceSessionDriver::run_flow_turns` — the flow speaks first, caller turns resume suspensions,
flow completion hangs up.

## Acceptance
- [x] Failing-first: SDK-level port of the driver's mock-realtime test — authored prompt spoken
      at `SessionReady`, a caller turn resumes the suspension, completion triggers
      `VoiceSink::session_ended`.
- [x] Voice types (`VoiceSink`, `VoiceReply`, `RealtimeProvider`, `RealtimeConfig`) nameable via
      `flux_sdk::voice`.
- [x] The existing model-driven `FlowClient::run_voice_session` is unchanged and its docs
      contrast the two modes.

## Progress
- **Done (unreleased).** `Session::run_voice_flow(provider, config, flow, sink, cancel)`
  (`crates/flux-sdk/src/session.rs`): connects the realtime provider, assembles
  `EngineVoiceHandler::new(self.engine, self.id, flow)`, and drives
  `VoiceSessionDriver::run_flow_turns` — reusing `self.engine.executor` (unused in flow mode, per the
  driver). Not feature-gated (the `RealtimeProvider` trait + `RealtimeConfig` are always available;
  only the concrete openai-realtime provider lives behind flux-providers' `realtime` feature).
- New `flux_sdk::voice` module re-exports `VoiceSink`/`VoiceReply` (from `flux_flow::voice`) +
  `RealtimeConfig`/`RealtimeProvider` (from `flux_provider`) — the D-146-deferred module.
- `FlowClient::run_voice_session` unchanged; its doc now contrasts model-driven vs the flow-driven
  `Session::run_voice_flow`.
- Failing-first integration test `tests/voice_flow.rs` (SDK-level port of the engine's
  `flow_driven_voice_session_speaks_authored_prompts_and_hangs_up`): a `Client`+`Session` with a
  registered `echo` op, a mock realtime scripting `SessionReady` → two `InputTranscriptDone`, a
  two-`await` interview flow. Asserts the three authored prompts are spoken in order (`send_text`) and
  completion fires `session_ended("Booked!")`. `NeverProvider` proves zero planner calls.
- CHANGELOG + WHATS-NEW + website mirror updated. Gate green (workspace 2165; SDK all-features;
  clippy all-features / fmt / codegate). **WAVE 3 (D-152..D-155) COMPLETE.** Not committed/released.

## Notes
- Seams: `EngineVoiceHandler::new(Arc<FlowEngine>, session_id, flow)` + `run_flow_turns`
  (`crates/flux-flow/src/voice/driver.rs:224,:329`). Depends on D-142 + D-147.
- Voice-test gotcha: the voice-module `EchoTool` has an empty schema — pass a lone object arg in
  voice-crate flow tests (see flow-driven-session notes).
