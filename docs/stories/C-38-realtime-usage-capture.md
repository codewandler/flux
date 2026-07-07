---
id: C-38
title: Realtime usage/cost capture end-to-end + voice-sink ergonomics pack
pillar: Cost
status: done
epic: consumer-gaps
design: docs/designs/realtime-usage-capture.md
note: "from the 2026-07-06 downstream-consumer review: the realtime voice path drops usage entirely (response.done's usage object is parsed away at three layers) — voice sessions are structurally unbillable while the text path emits CallUsage per call; DELIBERATE BREAKING CHANGE on the realtime seam, flagged in CHANGELOG"
---

# Realtime usage/cost capture

## Goal
Bring the realtime voice path to cost parity with the text path: per-response token usage parsed
from the wire, surfaced to the `VoiceSink`, priced (audio rates), and persisted as
`EventKind::CallUsage` rows — plus the sink-side ergonomics every voice consumer needs
(transcript accumulation, config builders, default-model export).

## Why (evidence)
OpenAI Realtime's `response.done` carries `response.usage` (input/output totals + text/audio/cached
splits). flux drops it at three layers: `ServerEvent::ResponseDone` is payload-less
(`flux-providers/src/realtime/event.rs:95`), `RealtimeEvent::ResponseDone` is payload-less
(`flux-provider/src/realtime.rs:101`), and `VoiceSink::response_done()` takes no args
(`flux-flow/src/voice/sink.rs`). The text path emits `EventKind::CallUsage { model, usage }` per
call and prices it via `PricingTable::cost`. Voice sessions produce zero usage rows.

## Design (decided 2026-07-06; full detail in the design doc)
- **Usage model**: `Usage` gains `audio_input_tokens` / `audio_output_tokens` as `#[serde(default)]`
  **subset** fields (mirror of `reasoning_tokens`: excluded from `total()`/`context_tokens()`;
  `accumulate` sums output-side, replaces input-side). `Rates`/`RateOverride` gain
  `audio_input`/`audio_output` **surcharge** tiers (cost stays an additive dot product; `0.0` =
  audio bills as text). Builtin rows for `gpt-realtime` + `gpt-realtime-2` (sheet 2026-07-06: audio
  $32 in/$64 out over text $4/$24 → surcharges 28.0/40.0; cached audio folds into `cache_read` at
  $0.40 — exact for this family, approximation documented).
- **Wire parse**: tolerant `WireUsage` serde structs (all `#[serde(default)]`); normalization
  mirrors the HTTP codec: fresh input = input−cached, fresh audio = audio−cached_audio
  (saturating). Missing/malformed usage → `None`, never a stream error (A-33 discipline).
- **DELIBERATE BREAKING CHANGE** (pre-1.0, flagged): `RealtimeEvent::ResponseDone { usage:
  Option<Usage> }` and `VoiceSink::response_done(&mut self, usage: Option<&Usage>)` (default
  no-op). Usage arrives atomically on response.done; a parallel event/callback would force every
  consumer to correlate two callbacks per response. In-repo inventory: 2 driver matches, 5 test
  constructions; downstream: 1 sink impl + test constructions (enumerated in its adoption story).
- **CallUsage emission lives on the driver** (voice analogue of "the engine emits CallUsage"):
  optional `UsageRecording { events: Arc<EventStore>, session_id, model_spec }` via
  `VoiceSessionDriver::with_usage_recording`; non-fatal append, zero-usage rows skipped,
  un-turn-scoped (`cost_summary` folds `CallUsage` stream-wide); model spec stamped via
  `canonical_model_spec(Some("openai"), model)` (C-15 attribution).
- **Pack**: `TranscriptAccumulator` helper (`flux-flow/src/voice/transcript.rs`; delta-buffers →
  whole per-turn `Message`s, flush on response_done AND at close, idempotent);
  `RealtimeConfig::{with_voice,with_temperature}`; `pub use config::default_model`.

## Acceptance
- [x] Usage + pricing model per design; `cost` applies audio surcharges; builtin realtime rows
      verified against the provider sheet at impl time; old `CallUsage` payloads decode (defaults).
- [x] Wire parse with pinned tolerance tests (full GA fixture with cached splits; bare
      response.done; garbage usage → None).
- [x] Seam change threaded end-to-end (event → provider seam → driver → sink); all in-repo match
      sites fixed; sink default keeps non-overriding implementors compiling.
- [x] `UsageRecording`: usage-bearing response appends exactly one CallUsage row with the canonical
      spec; zero-usage skipped; `cost_summary` prices the stream end-to-end (test proves dollars).
- [x] `TranscriptAccumulator` segmentation tests (turn boundaries, output-only turn, empty flush,
      close-flush); config builders + default_model export tested.
- [x] Full gate green. Consumer-compat: `cargo check` failure expected and must match EXACTLY the
      enumerated breakage (sink impl signature); anything beyond fails the step.

## Progress
- 2026-07-06 filed from the consumer review; design decided same day (dedicated design pass).
- 2026-07-07 implemented end-to-end: `Usage`/`Rates`/`RateOverride` audio subset+surcharge fields
  (`flux-core`), `gpt-realtime`/`gpt-realtime-2` builtin rows (sheet verified 2026-07-06), tolerant
  `WireUsage` parse + normalization (`flux-providers/src/realtime/event.rs`), the breaking seam
  change threaded through `RealtimeEvent::ResponseDone`/`VoiceSink::response_done` to both driver
  loops, `UsageRecording`/`with_usage_recording`/`record_usage` on `VoiceSessionDriver`,
  `TranscriptAccumulator` (`flux-flow/src/voice/transcript.rs`), `RealtimeConfig::{with_voice,
  with_temperature}`, and the `default_model` re-export. Full gate green (build/test/clippy/fmt,
  root + plugins). Consumer-compat check on the downstream workspace showed EXACTLY the expected
  controlled failure — one `VoiceSink` implementor's `response_done` signature — nothing else.

## Notes
- Adoption story (top priority) in the consumer's repo accompanies this: sink signature, accumulator
  swap, UsageRecording wiring, constant/builder cleanup.
- The consumer workspace will NOT compile against flux HEAD until its adoption story lands — the
  window is deliberate and communicated.
