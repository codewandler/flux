---
id: A-34
title: "OpenAI-wire envelope tolerance: skip unparseable SSE frames instead of killing the stream"
pillar: Agent
status: done
epic: stream-resilience
design: docs/designs/stream-resilience.md
note: "kills the two bare `?` envelope parses (openai.rs:269 chat, :870 responses) — the exact source of the user's `serialization error:` turn deaths; skip+count+StreamDiagnostic; declared error events stay fatal"
---

# OpenAI-wire envelope tolerance

## Goal

An unparseable `data:` frame on the chat or Responses SSE stream is skipped and counted — never an
`Err` stream item; drops surface as one end-of-stream `Chunk::StreamDiagnostic`. Declared provider
errors (`response.failed`, error events) keep their fatal semantics.
Design: [stream-resilience](../designs/stream-resilience.md).

## Acceptance

- [x] Failing-first: `chat_malformed_envelope_frame_is_skipped_and_the_stream_survives`.
- [x] Failing-first: `chat_content_after_a_junk_frame_still_arrives`.
- [x] Failing-first: `chat_dropped_frames_surface_a_stream_diagnostic`.
- [x] Failing-first: `responses_malformed_envelope_frame_is_skipped`.
- [x] Failing-first (guardrail pin): `responses_declared_error_events_stay_fatal`.
- [x] Both bare `?` sites (openai.rs:269, :870) are gone; `tracing::warn!` on drops.
- [x] Full workspace gate green.

## Progress

- 2026-07-04 filed (stream-resilience epic).
- 2026-07-04 implemented. `map_chat_stream` and `map_responses_stream`
  (`crates/flux-providers/src/openai.rs`) each got a `dropped_frames: u32` +
  `first_drop_detail: Option<String>` pair; the two bare
  `serde_json::from_str(data)?` envelope parses became `match` arms that on `Err` increment the
  counter, remember the first error message, emit `tracing::warn!(error, frame, "…skipping
  unparseable envelope frame")` (frame text truncated to 200 chars), and `continue` the loop
  instead of propagating. After the event loop ends (before any of the codec's normal terminal
  yields — recovered-tool-call handling / trailing text-and-tool blocks / `Done`), each function
  yields one `Chunk::StreamDiagnostic { dropped_frames, detail }` iff `dropped_frames > 0`. The
  `response.failed` / `"error"` declared-error arms in `map_responses_stream` are untouched — they
  run on the already-successfully-parsed `Value` and still `Err(Error::Provider(..))?`.
  `flux-providers/Cargo.toml` gained a direct `tracing.workspace = true` dependency (it was only
  transitive via `flux-provider` before; edition-2021 extern-prelude rules require a direct
  dependency to call `tracing::warn!`) — this is the one unavoidable non-`openai.rs` touch, exactly
  the kind of "tiny wiring" the story anticipated (it named `flux-core` re-exports, which turned out
  to already be `pub`; the actual tiny gap was this crate dependency instead).

  Failing-first tests (red confirmed against unmodified code, then green after implementing):
  1. `chat_malformed_envelope_frame_is_skipped_and_the_stream_survives` — red with
     `Serde(Error("expected ident", …))` panicking out of the stream; green after the fix.
  2. `chat_content_after_a_junk_frame_still_arrives` — same red cause; green, and the post-junk
     `"lo"` text arrives (`"Hello"` collected in full).
  3. `chat_dropped_frames_surface_a_stream_diagnostic` — red (same); green with exactly one
     `StreamDiagnostic { dropped_frames: 2, .. }` for two injected junk frames.
  4. `responses_malformed_envelope_frame_is_skipped` — red (same); green, `"Hi"` still collected.
  5. `responses_declared_error_events_stay_fatal` (guardrail pin) — **passed unchanged both before
     and after the change**, as specified: this is a pin, not a red→green test. A well-formed
     `response.failed` frame still yields `Err` carrying the `"boom"` message.

  Gate: `cargo build --workspace` clean; `cargo test --workspace` all green (zero failures across
  every crate, incl. the new 5 tests + the pre-existing 15 in `openai::tests`); `cargo clippy
  --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean for every file this
  story touched (`openai.rs`, `Cargo.toml`); `cargo test -p flux-codegate` green (4/4). Note: two
  transient full-workspace hiccups were observed and are **not from this story** — `bedrock.rs` and
  `messages/mod.rs`/`messages/wire.rs` are being actively edited by the concurrent A-36/A-35 agents
  in this same session (confirmed via `git diff --stat`: those files carry unrelated in-flight
  changes, and one workspace-test pass caught `bedrock.rs` mid-edit with a real compile error that
  cleared itself moments later on retry). Per this story's explicit constraint those files were not
  touched or fixed; the gate was re-run clean afterward with all crates compiling.

## Notes

- File-scoped: `crates/flux-providers/src/openai.rs` only (parallel-safe with A-35/A-36).
- Reuse the A-32 SSE-fixture test harness (raw `data:` strings → `futures::stream::once` →
  `map_chat_stream`/`map_responses_stream`).
- Depends on A-33's `Chunk::StreamDiagnostic`.
