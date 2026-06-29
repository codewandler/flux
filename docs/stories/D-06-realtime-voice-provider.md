---
id: D-06
title: Realtime voice-to-voice as a first-class flux provider
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design: docs/designs/realtime-voice-provider.md
---

# Realtime voice-to-voice as a first-class flux provider

## Goal
Give flux a **sibling, session-oriented provider seam** for full-duplex voice-to-voice models (OpenAI
Realtime), so a voice model's tool calls flow through the **same `Executor::dispatch` envelope** as text
agents and its tools are declared **once** from the live `ToolRegistry`. Lets the downstream managed-agents
voice surface delete its parallel model stack (bespoke WS client + double tool-declaration + scattered
keys) and run on flux's safety/audit guarantees.

## Why (managed-agents)
managed-agents runs the realtime model **entirely outside flux** (own `crates/realtime` WS client,
`crates/audio`, `channel-rtvbp` acoustic loop), reaching flux only behind the function-call seam — one-shot
`FlowClient::execute` per tool call (documented in managed-agents `behaviour-runner.md`). The cost: a parallel
model stack flux would otherwise own, tools declared twice and hand-synced (`realtime::Tool` in `spec.rs`
vs. `register_op` in `presets.rs`), and the OpenAI key read in three places. flux's `Provider` is
**half-duplex** (`stream(Request) -> ChunkStream`) and cannot model a persistent bidirectional audio
session — so this is a new seam, not a tweak.

## Acceptance

**Phase 1 — the provider primitive** (each behavioural criterion names the failing-first test, all
hermetic / no API key, against a mock `RealtimeProvider`):
- [ ] **L0 vocabulary.** `flux-core::audio` exposes `AudioFormat`/`AudioEncoding` (+ `OPENAI_PCM16` /
      `TELEPHONY_ULAW` consts). (`ContentBlock::Audio` deferred to Phase 2.)
- [ ] **L1 seam.** `flux-provider::realtime` exposes `RealtimeProvider`/`RealtimeSession`/`RealtimeEvent`/
      `RealtimeConfig`/`TurnDetection`; `AudioDelta`/`send_audio` carry **decoded bytes** and `ToolCall`
      carries only strings (no L2 type). Layering proven by `cargo test -p flux-codegate`
      (`flux-realtime` = 1, `flux-voice` = 3).
- [ ] **L1 impl.** `flux-realtime` provides an OpenAI-Realtime `RealtimeProvider` (lifted from managed-agents
      `crates/realtime`), GA endpoint, no beta header, a private `Secret { ApiKey | OAuth }` reusing
      `flux_provider::TokenSource`, and one `openai_realtime(secret)` constructor.
- [ ] **Tools through the envelope.** `tool_call_routes_through_executor` — a scripted `ToolCall` reaches
      `Executor::dispatch`, the result is fed back via `send_tool_result` + `create_response`, and a
      denied/destructive op is **gated by the envelope** (proves no bypass).
- [ ] **Single declaration.** `tools_declared_once` — `tool_defs_from_registry(&ToolRegistry)` yields the
      registry's `ToolSpec`s as `ToolDef`s and the session config carries exactly those (kills managed-agents'
      double-declaration).
- [ ] **Barge-in.** `barge_in_cancel_is_idempotent` — `SpeechStarted` with no active response is `Ok` and
      emits `barge_in()`; `create_response_debounced` — a turn with two tool calls fires exactly one
      `create_response`.
- [ ] **Driver + sink.** `flux-voice` provides `VoiceSessionDriver::run(conn, sink, cancel)` (off-loop
      dispatch so a slow tool never stalls audio) + `VoiceSink` (a symmetric cousin of `AgentSink`).
- [ ] Full gate green (`cargo build/test --workspace`, `clippy -D warnings`, `fmt`,
      `cargo test -p flux-codegate`).

**Phase 2 — engine-owned voice turns (spike):**
- [ ] A `VoiceSessionDriver` mode driving a **suspendable `FlowEngine` flow** across turns (the deferred
      cross-turn `await` from managed-agents `behaviour-runner.md`), proven by a 2-turn scripted flow against a
      mock `RealtimeProvider` — without committing the full engine-suspension work.

## Progress
- **Design committed; implementation deferred** (the D-05 pattern — sign off the design, build in a later
  pass). Design doc: [`docs/designs/realtime-voice-provider.md`](../designs/realtime-voice-provider.md).
- Scope designed: Phase 1 (provider primitive) + a Phase 2 engine-owned-turns spike.
- API claims grounded against real symbols: `flux_provider::{Provider, ToolDef, TokenSource}`,
  `flux_spec::ToolSpec`, `flux_runtime::{Executor::dispatch, ToolRegistry::specs}`, `flux-codegate::layer`.

## Notes
- **Sibling, not replacement:** the half-duplex `Provider` and the session-oriented `RealtimeProvider`
  coexist. The win is routing realtime tool calls through `Executor::dispatch` with a single tool
  declaration — same permission/approval/redaction/evidence guarantees as text agents.
- **Audio boundary:** flux speaks model-native format only; telephony/WebRTC resampling stays in the
  consumer/channel (`g711_ulaw` lets OpenAI resample server-side, so `crates/audio` becomes optional and
  stays in managed-agents).
- **Couples with** [D-01](D-01-flow-input-seeding.md) (the flow a Phase-2 voice flow runs on) and
  [D-02](D-02-tenant-event-substrate.md) (voice tool calls become audited events once the substrate is
  tagged). Mirrors [D-05](D-05-sub-agent-hardening.md)'s "lift the proven downstream pattern into flux"
  shape.
- **Open forks** (in the design): trait name (`RealtimeProvider` vs. `DuplexProvider`/`LiveProvider`);
  separate `flux-realtime` crate vs. a `flux-providers` module; D-06's rank in the downstream track.
- **Non-goals:** new sandbox boundary; in-tree resampling; remote/distributed realtime; replacing the
  half-duplex `Provider`.
