---
id: D-06
title: Realtime voice-to-voice as a first-class flux provider
pillar: Agent
status: done
priority:
theme: downstream-managed-services
design: docs/designs/realtime-voice-provider.md
---

# Realtime voice-to-voice as a first-class flux provider

## Goal
Give flux a **sibling, session-oriented provider seam** for full-duplex voice-to-voice models (OpenAI
Realtime), so a voice model's tool calls flow through the **same `Executor::dispatch` envelope** as text
agents and its tools are declared **once** from the live `ToolRegistry`. Lets downstream services
voice surface delete its parallel model stack (bespoke WS client + double tool-declaration + scattered
keys) and run on flux's safety/audit guarantees.

## Why (downstream managed services)
Some downstream services run the realtime model **entirely outside flux** (own WS client,
`crates/audio`, `channel-rtvbp` acoustic loop), reaching flux only behind the function-call seam — one-shot
`FlowClient::execute` per tool call. The cost: a parallel
model stack flux would otherwise own, tools declared twice and hand-synced (`realtime::Tool` in `spec.rs`
vs. `register_op` in `presets.rs`), and the OpenAI key read in three places. flux's `Provider` is
**half-duplex** (`stream(Request) -> ChunkStream`) and cannot model a persistent bidirectional audio
session — so this is a new seam, not a tweak.

## Acceptance

**Phase 1 — the provider primitive** (each behavioural criterion names its hermetic test — mock
`RealtimeProvider`, no API key — all passing):
- [x] **L0 vocabulary.** `flux_core::audio` — `AudioFormat`/`AudioEncoding` (+ `OPENAI_PCM16` /
      `TELEPHONY_ULAW` consts). (`ContentBlock::Audio` deferred.)
- [x] **L1 seam.** `flux_provider::realtime` — `RealtimeProvider`/`RealtimeSession`/`RealtimeEvent`/
      `RealtimeConfig`/`TurnDetection`; `AudioDelta`/`send_audio` carry **decoded bytes** and `ToolCall`
      carries only strings (no L2 type).
- [x] **L1 impl.** `flux_providers::realtime` (feature `realtime`) — OpenAI-Realtime `RealtimeProvider`
      ported from a downstream realtime client; GA endpoint, no beta header; private
      `Secret { ApiKey | OAuth(Arc<dyn TokenSource>) }`; one `openai_realtime(...)` constructor. Tests:
      `realtime::event::*`, `realtime::config::*`.
- [x] **Tools through the envelope.** `tool_call_routes_through_executor` — a scripted `ToolCall` reaches
      `Executor::dispatch`; result fed back via `send_tool_result` + `create_response`. `denied_tool_is_gated`
      — a destructive op is **gated** (never executes; model gets an error) — proves no bypass.
- [x] **Single declaration.** `tools_declared_once` — `tool_defs_from_registry(&ToolRegistry)` yields the
      registry's `ToolSpec`s as `ToolDef`s (removes downstream double-declaration). Re-proven end-to-end at
      the SDK seam by `run_voice_session_routes_a_tool_call_through_the_envelope`.
- [x] **Barge-in + debounce.** `barge_in_cancel_is_idempotent` (idle `SpeechStarted` doesn't cancel/error;
      both barge-ins surface) + `create_response_debounced` (a turn with two tool calls fires exactly one
      `create_response`).
- [x] **Driver + sink + SDK seam.** `flux_flow::voice::{VoiceSessionDriver::run, VoiceSink,
      tool_defs_from_registry}` (off-loop dispatch); surfaced as `FlowClient::run_voice_session`.
- [x] Full gate green (`cargo build/test --workspace`, `clippy -D warnings`, `fmt`,
      `cargo test -p flux-codegate`, plus `cargo {clippy,test} -p flux-providers --features realtime`).

**Phase 2 — engine-owned voice turns (spike):**
- [x] `VoiceSessionDriver::run_flow_turns` + a `VoiceTurnHandler` seam (a flux-side decider that, in
      production, wraps `FlowEngine::run_turn`) — each completed user turn (`InputTranscriptDone`) drives one
      reply, the model is STT/TTS. Proven by `flow_owns_two_voice_turns`. Per-turn `run_turn`, **not**
      cross-turn `await` (a single suspendable flow owning the whole call stays future work).

## Progress
- **Done — landed on `main`.** Phase 1 + the Phase 2 spike. **Built as modules, not new crates** (the
  user's "prefer single crate" preference + consolidation precedent; resolves the design's crate-vs-module
  fork): L0 `flux_core::audio`, L1 `flux_provider::realtime` (seam) + `flux_providers::realtime` (OpenAI
  impl, feature `realtime`), L3 `flux_flow::voice`, SDK `FlowClient::run_voice_session`. Zero new crates →
  `flux-codegate::layer()` + workspace members untouched, layering green by construction.
- 11 new tests (6 in `flux_flow::voice`, ~4 lifted/extended in `flux_providers::realtime`, 1 SDK seam).
  Gate green: 667 workspace tests, clippy `-D warnings` (default + `realtime` feature), fmt, codegate.
- **Deferred (intentional):** the full single-suspendable-flow voice mode (cross-turn `await`);
  `ContentBlock::Audio`; downstream rewiring is a separate pass outside this repo.
- **Publish note:** `flux-providers` gains an **optional** WS dep set behind the `realtime` feature;
  `flux-sdk` gains `tokio-util`. Fold into `PUBLISHING.md` before a release.

## Notes
- **Sibling, not replacement:** the half-duplex `Provider` and the session-oriented `RealtimeProvider`
  coexist. The win is routing realtime tool calls through `Executor::dispatch` with a single tool
  declaration — same permission/approval/redaction/evidence guarantees as text agents.
- **Audio boundary:** flux speaks model-native format only; telephony/WebRTC resampling stays in the
  consumer/channel (`g711_ulaw` lets OpenAI resample server-side, so `crates/audio` becomes optional and
  stays in the downstream consumer).
- **Couples with** [D-01](D-01-flow-input-seeding.md) (the flow a Phase-2 voice flow runs on) and
  [D-02](D-02-tenant-event-substrate.md) (voice tool calls become audited events once the substrate is
  tagged). Mirrors [D-05](D-05-sub-agent-hardening.md)'s "lift the proven downstream pattern into flux"
  shape.
- **Open forks** (in the design): trait name (`RealtimeProvider` vs. `DuplexProvider`/`LiveProvider`);
  separate `flux-realtime` crate vs. a `flux-providers` module; D-06's rank in the downstream track.
- **Non-goals:** new sandbox boundary; in-tree resampling; remote/distributed realtime; replacing the
  half-duplex `Provider`.
