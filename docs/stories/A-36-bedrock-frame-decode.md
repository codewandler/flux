---
id: A-36
title: "Bedrock frame decode: skip payload garbage, classify integrity failures as StreamDecode"
pillar: Agent
status: done
epic: stream-resilience
design: docs/designs/stream-resilience.md
note: "chunk-payload garbage (non-JSON / bad base64 / bad utf8) → skip+count+warn; CRC mismatch / header overrun / truncated tail stay errors but reclassify `Error::StreamDecode` so the A-33 backstop retries the call"
---

# Bedrock frame decode resilience

## Goal

Bedrock's AWS event-stream deframer never emits a fatal error for garbage payload bytes, and its
genuine integrity failures are classified `StreamDecode` so the planner backstop converts them into
a retried step instead of a dead turn. Design: [stream-resilience](../designs/stream-resilience.md).

## Acceptance

- [x] Failing-first: `bedrock_non_json_chunk_payload_is_skipped`.
- [x] Failing-first: `bedrock_bad_base64_chunk_is_skipped`.
- [x] Failing-first: `bedrock_crc_mismatch_is_a_classified_stream_decode_error`.
- [x] Failing-first: `bedrock_truncated_tail_is_a_classified_stream_decode_error`.
- [x] Bedrock `exception` frames keep their fatal semantics (existing behavior pinned).
- [x] Full workspace gate green.

## Progress

- 2026-07-04 filed (stream-resilience epic).
- 2026-07-04 implemented. `frame_to_sse` now returns a local `FrameOutcome { Sse, Skip, Garbage }`
  instead of `Result<Option<String>>`: the chunk arm's four payload-decode failures (not JSON,
  missing `bytes`, bad base64, not UTF-8) return `FrameOutcome::Garbage(detail)` instead of
  `Err(..)`; `map_bedrock_event_stream`'s loop matches on it — `Sse` yields as before, `Skip` is a
  no-op (unchanged, non-chunk events), `Garbage` increments a `dropped_frames` counter and
  `tracing::warn!`s, then continues deframing. Every genuine integrity-failure site was reclassified
  from `Error::Provider` to `Error::StreamDecode` (message text unchanged, only the variant):
  `buffered_frame_len`'s implausible-length and prelude-CRC-mismatch checks; `parse_event_headers`'s
  four structural errors (truncated header block, non-utf8 name/value, unknown value-type tag);
  `frame_to_sse`'s message-CRC-mismatch, header-block-overrun, and missing-`:message-type` checks;
  and `map_bedrock_event_stream`'s trailing-bytes (truncated tail) check. The `exception`/`error`
  frame arm in `frame_to_sse` (a *declared* provider failure) was left completely untouched —
  still `Err(Error::Provider(...))` — per the guardrail; the pre-existing
  `event_stream_surfaces_exception_frames` test pins this and passed unmodified throughout.
  Red→green: added `bedrock_non_json_chunk_payload_is_skipped` and `bedrock_bad_base64_chunk_is_skipped`
  (new); renamed+strengthened `event_stream_rejects_corrupt_prelude_crc` →
  `bedrock_crc_mismatch_is_a_classified_stream_decode_error` and
  `event_stream_errors_on_truncated_tail` → `bedrock_truncated_tail_is_a_classified_stream_decode_error`,
  each now also asserting `matches!(err, Error::StreamDecode(_))`; also added one bonus test,
  `bedrock_message_crc_mismatch_is_also_classified_stream_decode_error`, for the distinct
  message-CRC integrity path (not required by Acceptance but exercises a separate reclassified
  code path). All 5 confirmed RED against the prior code (4 panicked on `Error::Provider(...)`
  where `StreamDecode` or a skip was expected; one on the classification assertion) before
  implementing, then GREEN after. Fixture construction: reused the file's existing test
  infrastructure — `encode_frame` (hand-rolled AWS event-stream frame encoder: prelude
  length+CRC, string headers, payload, message CRC — the deframer's test-side inverse, itself
  independently pinned by `crc32_matches_check_value` against the standard CRC-32/IEEE check
  value) and `chunk_frame` (wraps one Anthropic SSE event JSON as a valid `{"bytes":...,"p":...}`
  chunk frame) plus `byte_stream_from` (splits a byte vec into a mock `ByteStream`). No new harness
  needed — corrupted garbage-payload frames were built by calling `encode_frame` directly with a
  non-JSON or non-base64 payload (bypassing `chunk_frame`'s valid-wrapper construction); CRC
  corruption flips a single byte in an otherwise-valid `chunk_frame` output (prelude-CRC region for
  the prelude test, the trailing 4-byte message-CRC region for the message-CRC test); the
  truncated-tail test appends only the first 10 bytes of a second valid frame after a complete
  first one. Gate: `cargo build --workspace` clean; `cargo test --workspace` 88 test binaries
  green, 0 failed; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check`
  clean (one auto-fixable wrap via `cargo fmt -p flux-providers`, re-verified clean+green after);
  `cargo test -p flux-codegate` green (4/4). No deviations from scope: only
  `crates/flux-providers/src/bedrock.rs` touched for code; SSO/credential-path JSON parses in the
  same file were left untouched. `openai.rs`/`messages/mod.rs`/`messages/wire.rs` are modified in
  the working tree by concurrent sibling stories (A-34/A-35), not by this story.

## Notes

- File-scoped: `crates/flux-providers/src/bedrock.rs` (parallel-safe with A-34/A-35).
- The deframer outputs a `ByteStream` (not `Chunk`s), so skips are warn-logged + counted only; the
  downstream Messages codec's own diagnostics (A-35) cover content accounting.
- Depends on A-33's `Error::StreamDecode`.
