---
title: Realtime voice (experimental)
description: "Experimental realtime voice model seam, session lifecycle, and safety model for full-duplex sessions."
---

# Realtime voice (experimental)

Realtime support is an SDK-level experimental surface for full-duplex voice models. It is not the
normal text agent loop, and in its default mode it does not turn each utterance into a Flux-Lang
plan. It exists for embedders that need a long-lived audio session while still routing effectful
tool calls through the guarded executor. Since 0.15.0 the driver has a second, **flow-driven** mode:
an authored flow owns the call and the realtime model is only the acoustic front-end (see
[Flow-driven voice](#flow-driven-voice) below).

The public shape is `RealtimeProvider`, a sibling of the half-duplex [`Provider`](./providers.md).
The concrete OpenAI implementation is behind the `realtime` cargo feature on `flux-providers`.

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
  `VoiceSink` (your callbacks for audio frames, transcripts, tool events, barge-in, and — since
  0.15.0 — `session_ended` when a flow-driven call completes). Beside the default model-driven
  entry it exposes `run_flow_turns`, where a `VoiceTurnHandler` on your side owns each turn.
- **The same safety envelope.** Every tool call the voice model makes is dispatched through the
  runtime's `Executor::dispatch` — the identical [permission / approval / redaction
  chain](./safety.md) a text turn uses, with no bypass path. Tools are declared to the model once,
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

## Flow-driven voice

Since 0.15.0 an **authored flow can drive the call** instead of the model: the driver speaks the
flow's authored prompts (TTS via the realtime channel), the caller's reply resumes the flow's
suspension, and the model does cognition only where the flow explicitly delegates a bounded
segment (`ai_segment` — see [durability and sessions](../language/durability.md)). Classic-IVR
determinism over the same voice stack: the deterministic skeleton makes **zero** model planning
calls, and when the flow completes the driver speaks the final line, fires
`VoiceSink::session_ended` (your hangup/handoff hook), and ends the session.

The SDK entry point is `Session::run_voice_flow(provider, config, flow, sink, cancel)` — the voice
counterpart of `Session::start_flow` and the flow-driven sibling of the model-driven
`FlowClient::run_voice_session`. It needs the persistent engine, so it lives on `Session` rather than
`FlowClient`. Under the hood it drives `VoiceSessionDriver::run_flow_turns` with an
`EngineVoiceHandler` — a `VoiceTurnHandler` backed by a `FlowEngine`, which server/embedding hosts
can also drive at the driver level directly. Each turn the handler
returns a `VoiceReply`: `Continue(text)` (speak this, await the caller) or `Complete(text)` (speak
this final line, end the call). Ops a voice-driven flow dispatches traverse the engine's shared
executor — the same envelope as a text turn — and barge-in is unchanged.

**Breaking in 0.15.0:** `VoiceTurnHandler::turn` returns `VoiceReply` instead of `String` — return
`VoiceReply::Continue(text)` for the old behavior. The new `start()` (speak first) and
`VoiceSink::session_ended` hooks have defaults, so existing implementations only adjust `turn`.

**Breaking again:** `turn` now takes the speaker as well —
`turn(&self, speaker: &Speaker, user_text: &str)`. A phone line has exactly one candidate, so the
realtime driver passes `Speaker::sole()` and behavior is unchanged; the parameter exists because a
[many-party room](../channels/inventory.md#room) has N speakers and a handler cannot decide whether it
was addressed without knowing who spoke. An existing implementation adds the parameter and ignores it.

## Status and limits

- **Experimental.** The traits and event shapes are pre-1.0 and have already changed in breaking
  ways between releases; expect more.
- **SDK-only.** There is no CLI surface for realtime voice today — no `flux` subcommand opens a
  voice session, and `.flux` programs cannot declare one.
- **One provider.** OpenAI Realtime is the only implementation; the seam is provider-shaped so
  others can land, but none have.
- **In the lower-level model-driven mode, the realtime model owns the turn.** There is no authored
  outer-loop round-trip per utterance. flux's guarantee here is narrower and
  deliberate: every *effectful action* still crosses the guarded executor envelope, and the audit
  trail still records it. The [flow-driven mode](#flow-driven-voice) inverts this: the flow owns the
  turn, and the model speaks only inside a bounded, tool-scoped segment.
- **Audio transport is yours.** flux does no telephony, WebRTC, or device IO; it exchanges
  model-native audio bytes and leaves capture, playout, and resampling to the embedding application
  (with `flux-audio` available as a utility).

## Related docs

- [Providers and models](./providers.md) — text-provider routing and credentials.
- [Safety and approvals](./safety.md) — the executor envelope realtime tool calls still use.
