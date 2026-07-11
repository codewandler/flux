---
id: D-140
title: TurnDetection::ServerVad needs a create_response:false knob for flow-driven voice
pillar: Agent
status: done
epic:
design: ../designs/flow-driven-voice.md
note: "downstream ask (ai-agent-platform R-20 voice, flows arc ask 6): D-132's run_flow_turns doc says 'server-VAD with response creation off', but the seam TurnDetection and the OpenAI wire TurnDetection carry no create_response — a live flow-driven session's model auto-reply races the flow's spoken prompt"
---

# TurnDetection::ServerVad needs a create_response:false knob for flow-driven voice

## Goal
A flow-driven voice session (D-132) needs server-side VAD for turn boundaries + input transcription
**without** the model auto-responding — the flow speaks via `send_text`, and an auto-created
response races it (both speak). `run_flow_turns`'s own doc prescribes "server-VAD with response
creation off", but neither the seam `TurnDetection::ServerVad` nor the OpenAI wire `TurnDetection`
struct exposes OpenAI's `turn_detection.create_response` (and `interrupt_response`) flags — so the
prescription is currently unconfigurable and live flow-driven voice double-speaks.

## Acceptance
- [x] `TurnDetection::ServerVad` (seam) gains `create_response: Option<bool>` (and
      `interrupt_response: Option<bool>`), mapped onto the OpenAI wire `turn_detection` object;
      `None` keeps today's provider default (additive).
- [x] `SemanticVad` gets the same treatment (the wire supports it there too).
- [x] Failing-first test: a session config with `create_response: false` serializes the wire flag;
      absent stays absent.
- [x] The D-132 `run_flow_turns` doc points at the actual knob.

## Progress
- 2026-07-12 — filed from ai-agent-platform R-20 (flows arc ask 6). Downstream's flow-driven voice
  backend (`ConversationBackend`) is complete and hermetically tested; LIVE OpenAI use waits on
  this knob.
- 2026-07-11 — implemented. `TurnDetection::ServerVad`/`SemanticVad` (seam,
  `crates/flux-provider/src/realtime.rs`) gain `create_response: Option<bool>` +
  `interrupt_response: Option<bool>`; the OpenAI wire `TurnDetection` (`crates/flux-providers/src/
  realtime/config.rs`) gains the same two fields (`skip_serializing_if = "Option::is_none"`, so
  `None` keeps the wire object exactly as before — additive); `to_session_config`
  (`crates/flux-providers/src/realtime/mod.rs`) now threads both flags through for both VAD kinds.
  Failing-first tests added in `crates/flux-providers/src/realtime/mod.rs` tests module
  (`server_vad_create_response_flag_serializes_when_set_and_omitted_when_absent`,
  `semantic_vad_create_response_and_interrupt_response_serialize_when_set`) — confirmed failing
  (asserted `false`, got `Null`) before the mapping fix, green after. Updated the `run_flow_turns`
  doc comment (`crates/flux-flow/src/voice/driver.rs`) to name the actual knob instead of the vague
  "server-VAD with response creation off". Gate (scoped): `cargo test -p codewandler-flux-provider
  -p codewandler-flux-providers` both with and without `--features codewandler-flux-providers/
  realtime` (145 / 128 passed), `cargo clippy` same scope `-D warnings` clean, `cargo fmt` scoped
  check clean, `cargo test -p flux-codegate` clean (no layering change).
