# Realtime usage/cost capture (C-38)

Status: decided 2026-07-06 (dedicated design pass; every match site and the provider pricing sheet
verified); implementation same push. Story: `docs/stories/C-38-realtime-usage-capture.md`.

**Implemented 2026-07-07**: every section below landed as specified (full gate green; consumer-compat
verified to fail with EXACTLY the enumerated breakage). See the story's Progress note for the file
list.

## Problem

OpenAI Realtime's `response.done` event carries `response.usage` — input/output token totals plus
`input_token_details`/`output_token_details` with text/audio/cached splits (and cached sub-splits).
flux drops all of it at three layers:

1. `flux-providers/src/realtime/event.rs` — `ServerEvent::ResponseDone` is payload-less.
2. `flux-provider/src/realtime.rs` — `RealtimeEvent::ResponseDone` is payload-less.
3. `flux-flow/src/voice/{driver,sink}.rs` — `sink.response_done()` takes no arguments; `VoiceSink`
   has no usage callback.

The half-duplex text path emits one `EventKind::CallUsage { model, usage }` per provider call and
prices it through `PricingTable::cost`, rolled up by the `cost_summary` projection. Voice sessions
produce zero usage rows: structurally unbillable.

## Usage model: subset fields + surcharge rates

`Usage` (flux-core/src/stream.rs) gains two fields, mirroring the `reasoning_tokens` precedent:

```rust
#[serde(default)] pub audio_input_tokens: u64,   // subset of input_tokens (fresh prompt side)
#[serde(default)] pub audio_output_tokens: u64,  // subset of output_tokens
```

- Subsets: excluded from `total()` / `context_tokens()` (the provider already counts them in the
  parent fields). Cached audio lives inside `cache_read_input_tokens`.
- `accumulate()`: `audio_output_tokens` sums; `audio_input_tokens` replaces, inside the existing
  prompt-side gate.
- Serde: plain `#[serde(default)]` u64s — old persisted `CallUsage` payloads decode with zeros;
  old readers ignore the new fields.

`Rates` / `RateOverride` (flux-core/src/pricing.rs) gain **surcharge** tiers:

```rust
#[serde(default)] pub audio_input: f64,   // surcharge over `input` per audio input token
#[serde(default)] pub audio_output: f64,  // surcharge over `output` per audio output token
```

`cost()` stays a pure additive dot product (+2 terms). Surcharge — not absolute audio rates —
because the subset fields are already billed at the text rate by the parent terms; absolute rates
would force either a subtraction (underflow, non-additive formula) or a `0.0`-disables sentinel
(legit-zero rate inexpressible, every existing row would bill audio at $0).

Builtin rows (provider sheet verified 2026-07-06; re-verify at any future edit):

```
gpt-realtime / gpt-realtime-2:
  input 4.0, output 24.0, cache_write 4.0, cache_read 0.40,
  audio_input 28.0 ($32 audio − $4 text), audio_output 40.0 ($64 − $24)
```

Documented approximations: cached audio folds into `cache_read` (exact for this family — cached
text and cached audio both bill $0.40; would diverge for a mini-tier row); image input tokens bill
at the text input rate. All other existing full-struct-literal rows gain the two `0.0` fields.

## Wire parse + normalization

`event.rs` gets tolerant wire structs (every field `#[serde(default)]`): `WireUsage {
input_tokens, output_tokens, input_token_details { text_tokens, audio_tokens, image_tokens,
cached_tokens, cached_tokens_details { text_tokens, audio_tokens } }, output_token_details {
text_tokens, audio_tokens } }`.

Normalization mirrors the HTTP codec's convention (`openai.rs` usage mapping): the wire reports the
whole prompt with cached as a subset; flux's `input_tokens` is the **fresh** prompt:

```
input_tokens          = wire.input_tokens − cached            (saturating)
cache_read            = cached
audio_input_tokens    = wire.audio_tokens − cached_audio      (saturating; fresh audio subset)
audio_output_tokens   = wire.output_details.audio_tokens
```

Parse arm: `"response.done"` extracts `response.usage` via `serde_json::from_value(..).ok()` —
missing or malformed usage yields `None`, never a stream error (A-33 discipline, test-pinned).

## Seam change (deliberate breaking change, flagged in CHANGELOG)

- `ServerEvent::ResponseDone(Option<Usage>)` (crate-private enum — no external impact).
- `RealtimeEvent::ResponseDone { usage: Option<Usage> }`.
- `VoiceSink::response_done(&mut self, usage: Option<&Usage>)` — default no-op body retained.

No parallel `Usage` event / separate sink callback: usage arrives atomically on `response.done`,
and a split would force the driver and every sink to correlate two callbacks per response.
`RealtimeEvent` is not `#[non_exhaustive]`, so even a new variant would break the exhaustive driver
matches — there is no non-breaking option; the field is the cheapest true shape.

Match-site inventory (verified): constructions — realtime codec `map_event` (the producer), 4 in
`flux-flow/src/voice/tests.rs`, 1 in `flux-sdk/src/flow.rs` (test script); destructuring — the two
driver loops only. Sink implementors overriding `response_done`: none in-repo (test sinks use the
default); one downstream (enumerated in its adoption story).

## CallUsage emission: on the driver

The driver is the voice analogue of the engine, and on the text path the **engine** emits
`CallUsage` — so voice usage persistence lives on the driver, not on every consumer's sink:

```rust
pub struct UsageRecording {
    pub events: Arc<EventStore>,
    pub session_id: String,
    pub model_spec: String,   // canonical_model_spec(Some("openai"), model) — C-15 attribution
}
impl VoiceSessionDriver {
    pub fn with_usage_recording(self, rec: UsageRecording) -> Self;
}
```

`record_usage`: non-fatal append (`let _ =`), zero-usage responses skipped (no placeholder rows,
mirroring the engine), rows are **un-turn-scoped** `NewEvent::new(EventKind::CallUsage { .. })` —
`EventStore::record_call_usage` can't be reused (it no-ops without a turn id; voice has no turns),
and `cost_summary` folds `CallUsage` stream-wide regardless. The sink still receives the usage in
`response_done` for consumer-side surfaces. Rejected alternative: sink-side appends — every
consumer re-implements it and flux's own future voice surfaces get nothing.

Model-spec note: stamp with a **known** provider prefix (`"openai"`); an unknown prefix would be
dropped by canonicalization and break attribution.

## Ergonomics pack (same files, same story)

- `TranscriptAccumulator` (`flux-flow/src/voice/transcript.rs`, plain helper — not a sink
  decorator): `push_input` / `push_output` / `is_empty` / `flush() -> Vec<Message>` (user first,
  then assistant; empty side skipped; idempotent). Flush on `response_done` **and once at session
  close** — a hangup can land before the final `response.done`. Documented caveat: the driver
  collapses input-transcript delta and done events into one callback; if input transcription is
  ever enabled with both event kinds, append would double-count — the eventual fix is a distinct
  done-callback, out of scope here.
- `RealtimeConfig::{with_voice, with_temperature}` builders.
- `pub use config::default_model` from the realtime codec module (the id `connect` falls back to).

## Test plan

1. `Usage::accumulate` folds audio (sum vs replace; totals exclude subsets).
2. `cost` applies surcharge tiers; builtin realtime row prices a mixed text/audio/cached usage to
   hand-computed dollars (proves prefix-strip resolution too).
3. flux-events: old `call_usage` payload decodes with zero audio fields; audio round-trips.
4. Wire: full GA fixture (cached splits) → exact normalized Usage; bare/garbage usage → `None`.
5. `map_event` threads the usage through.
6. Driver: usage reaches the sink in both loops; `UsageRecording` appends exactly one row for a
   usage-bearing response, skips zero-usage, and `cost_summary` prices the stream (end-to-end).
7. Accumulator segmentation (turn boundaries, output-only, empty flush).
8. Config builders.
