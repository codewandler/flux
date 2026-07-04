---
id: A-35
title: "Messages-wire envelope tolerance: skip bad event JSON and unknown event types"
pillar: Agent
status: done
epic: stream-resilience
design: docs/designs/stream-resilience.md
note: "replaces the fatal `messages SSE: bad event JSON` at messages/mod.rs:381 with skip+count+StreamDiagnostic; also fixes a latent kill found during design — a well-formed vendor event with an unknown `type` is fatal today (StreamEvent has no catch-all)"
---

# Messages-wire envelope tolerance

## Goal

The Messages SSE codec (anthropic, bedrock, openrouter-anthropic, ollama) skips unparseable event
frames and tolerates unknown event `type`s instead of erroring the stream; drops surface as one
end-of-stream `Chunk::StreamDiagnostic`. `StreamEvent::Error` (a declared provider error) stays
fatal. Design: [stream-resilience](../designs/stream-resilience.md).

## Acceptance

- [x] Failing-first: `messages_bad_event_json_is_skipped_and_the_stream_survives`.
- [x] Failing-first: `messages_unknown_event_type_is_tolerated`.
- [x] Failing-first: `messages_dropped_frames_surface_a_stream_diagnostic`.
- [x] Failing-first (guardrail pin): `messages_declared_error_event_stays_fatal`.
- [x] The `map_err(…)?` at messages/mod.rs:381 is gone; `tracing::warn!` on drops.
- [x] Full workspace gate green.

## Progress

- 2026-07-04 filed (stream-resilience epic).
- 2026-07-04 implemented. `map_messages_stream_inner`'s single
  `serde_json::from_str::<StreamEvent>(data)` call now matches its `Result` instead of `?`-
  propagating: on `Err` it increments a `dropped_frames: u32` counter, records the first error's
  detail, `tracing::warn!`s, and `continue`s to the next frame. Because `StreamEvent` is an
  internally-tagged enum (`#[serde(tag = "type")]`), a well-formed frame with an unrecognized
  `type` produces the *same* `serde_json::Error` (serde's "unknown variant" error) as syntactically
  broken JSON — so one match arm tolerates both failure shapes named in the Acceptance without any
  `wire.rs` catch-all variant. After the byte stream ends, if `dropped_frames > 0`, one
  `Chunk::StreamDiagnostic { dropped_frames, detail }` is yielded before the generator completes.
  The guardrail (`StreamEvent::Error` → still `Err(Error::Provider(...))?` in its own match arm,
  unchanged) was verified fatal both before and after the change.
  Red→green: wrote all 4 tests, then temporarily reverted the two implementation edits (kept
  tests) and ran `cargo test -p flux-providers --lib messages::tests::messages_` — 3 of 4 failed
  with the pre-fix `Error::Provider("messages SSE: bad event JSON …")` (bad-JSON and unknown-variant
  cases), 1 (`messages_declared_error_event_stays_fatal`) already passed pre-fix, confirming the
  guardrail pin. Reapplied the implementation → all 4 pass, plus the other 15 `messages::` tests
  unaffected.
  Gate: `cargo build --workspace` green; `cargo test --workspace` green (327 test binaries, 0
  failed — the 5 `bedrock::tests::*` failures seen mid-session were the concurrent A-36 sibling
  story's own failing-first tests landing in real time, resolved by the time of the final run, not
  caused by or fixed in this story); `cargo clippy --workspace --all-targets -- -D warnings` clean;
  `cargo fmt --check` — my two touched files (`messages/mod.rs`, `messages/wire.rs`) verified
  fmt-clean directly via `rustfmt --check`; `cargo test -p flux-codegate` green (4/4). No deviation
  from the story's scope: only `messages/mod.rs` was touched; `wire.rs`'s only diff is pre-existing
  C-34 (reported-cost) work, untouched by me.

## Notes

- Files: `crates/flux-providers/src/messages/mod.rs` (+ `wire.rs` if the unknown-type tolerance
  lands as a serde catch-all variant). Parallel-safe with A-34/A-36.
- Depends on A-33's `Chunk::StreamDiagnostic`.
