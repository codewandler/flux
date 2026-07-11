---
id: D-155
title: Flow-driven voice front door — Session::run_voice_flow
pillar: Agent
status: backlog
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
- [ ] Failing-first: SDK-level port of the driver's mock-realtime test — authored prompt spoken
      at `SessionReady`, a caller turn resumes the suspension, completion triggers
      `VoiceSink::session_ended`.
- [ ] Voice types (`VoiceSink`, `VoiceReply`, `RealtimeProvider`, `RealtimeConfig`) nameable via
      `flux_sdk::voice`.
- [ ] The existing model-driven `FlowClient::run_voice_session` is unchanged and its docs
      contrast the two modes.

## Progress
- (pending)

## Notes
- Seams: `EngineVoiceHandler::new(Arc<FlowEngine>, session_id, flow)` + `run_flow_turns`
  (`crates/flux-flow/src/voice/driver.rs:224,:329`). Depends on D-142 + D-147.
- Voice-test gotcha: the voice-module `EchoTool` has an empty schema — pass a lone object arg in
  voice-crate flow tests (see flow-driven-session notes).
