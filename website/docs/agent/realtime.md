---
title: Realtime voice (experimental)
---

# Realtime voice (experimental)

flux has an **experimental** seam for full-duplex, voice-to-voice models: `RealtimeProvider`, a
sibling of the half-duplex [`Provider`](providers.md) used for text turns. Where a text provider
answers one request with one stream, a realtime session is a long-lived connection that carries
input audio and output audio, transcripts, and tool calls concurrently, with the model — not flux —
driving acoustic turn-taking (server-side VAD) and barge-in. The concrete implementation is
feature-gated behind the `realtime` cargo feature on `flux-providers`, and the whole surface is
SDK-level and subject to change.

## What exists

- **The seam** — `RealtimeProvider` / `RealtimeSession` / `RealtimeEvent` / `RealtimeConfig` in
  `flux-provider`. A session exposes `send_audio` / `send_text` / `cancel_response` (barge-in) /
  `send_tool_result`; the event stream yields audio deltas, input/output transcripts, speech
  start/stop, tool calls, and per-response token usage.
- **One concrete provider** — OpenAI Realtime, in `flux-providers::realtime` behind the
  `realtime` feature: a WebSocket client built with `openai_realtime(api_key)`,
  `openai_realtime_from_env()` (reads `OPENAI_KEY`, then `OPENAI_API_KEY`), or
  `openai_realtime_oauth(token)`. The default model id is `gpt-realtime`.
- **The driver** — `VoiceSessionDriver` in `flux-flow` runs the session event loop and feeds a
  `VoiceSink` (your callbacks for audio frames, transcripts, tool events, barge-in).
- **The same safety envelope.** Every tool call the voice model makes is dispatched through the
  runtime's `Executor::dispatch` — the identical [permission / approval / redaction
  chain](safety.md) a text turn uses, with no bypass path. Tools are declared to the model once,
  from the same registry the executor gates. Dispatch runs off the audio loop, so a slow tool never
  stalls audio or barge-in.
- **Audio helpers** — the dependency-free `flux-audio` crate provides PCM16 encode/decode, stateless
  and streaming resampling (phase carried across packets), and frame re-chunking. flux itself speaks
  the model-native format only (`AudioFormat::OPENAI_PCM16`, i.e. PCM16 24 kHz mono; G.711 µ-law
  8 kHz is also expressible); resampling to your transport's rate is your side of the boundary.

## How to enable

The SDK entry point is `FlowClient::run_voice_session` — build a [`FlowClient`](../sdk/flow-client.md)
with your ops and policies as usual, then hand it a realtime provider, a config, and a sink:

```toml
[dependencies]
flux-sdk = "*"
flux-providers = { version = "*", features = ["realtime"] }
```

```rust
use flux_flow::VoiceSink;
use flux_provider::RealtimeConfig;
use flux_providers::realtime::openai_realtime_from_env;
use tokio_util::sync::CancellationToken;

let provider = openai_realtime_from_env()?; // OPENAI_KEY / OPENAI_API_KEY
let config = RealtimeConfig::voice_agent("gpt-realtime", "You book appointments. Be brief.");

let cancel = CancellationToken::new();      // trigger to end the session (e.g. hangup)
// `client` is an assembled FlowClient; `sink` is your VoiceSink implementation.
client.run_voice_session(&provider, config, &mut sink, &cancel).await?;
```

`run_voice_session` declares the client's registered ops to the model, builds the executor, and
drives the session until `cancel` fires or the connection ends. Your `VoiceSink` receives the output
half; you push caller audio through the session handle.

## Status and limits

- **Experimental.** The traits and event shapes are pre-1.0 and have already changed in breaking
  ways between releases; expect more.
- **SDK-only.** There is no CLI surface for realtime voice today — no `flux` subcommand opens a
  voice session, and `.flux` programs cannot declare one.
- **One provider.** OpenAI Realtime is the only implementation; the seam is provider-shaped so
  others can land, but none have.
- **The model owns the turn.** Unlike text turns, there is no plan/DAG indirection per utterance —
  sub-second turn-taking cannot round-trip a planner. flux's guarantee here is narrower and
  deliberate: every *effectful action* still crosses the guarded executor envelope, and the audit
  trail still records it.
- **Audio transport is yours.** flux does no telephony, WebRTC, or device IO; it exchanges
  model-native audio bytes and leaves capture, playout, and resampling to the embedding application
  (with `flux-audio` available as a utility).
