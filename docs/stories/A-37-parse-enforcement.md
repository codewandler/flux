---
id: A-37
title: "Structural enforcement: clippy ban on bare serde parses in flux-providers + malformed-envelope corpus"
pillar: Agent
status: done
epic: stream-resilience
design: docs/designs/stream-resilience.md
note: "the anti-whack-a-mole layer: crate-local clippy.toml disallowed-methods (verify it fires under --workspace; fallback = codegate source-scan) + envelope_corpus test (truncation at every offset, junk injection, frame corruption across all codecs) + AGENTS.md invariants bullet"
---

# Structural enforcement for the stream-resilience invariant

## Goal

Make "provider bytes never error a chunk stream" self-enforcing: a bare `serde_json::from_*` added
to flux-providers fails clippy at the gate, and a corpus test proves the runtime invariant holds
for every codec under systematic corruption. Design: [stream-resilience](../designs/stream-resilience.md).

## Acceptance

- [x] `crates/flux-providers/clippy.toml` bans `serde_json::from_str/from_slice/from_value/from_reader`
      via disallowed-methods; tolerant helpers live in one allow-listed module; VERIFIED to fire
      under `cargo clippy --workspace --all-targets -- -D warnings` (if per-crate config doesn't
      resolve there, fall back to a flux-codegate source-scan test — record which in Progress).
- [x] Failing-first corpus (`#[cfg(test)] mod envelope_corpus`, mappers `pub(crate)`):
      `chat_stream_survives_truncation_at_every_offset`,
      `responses_stream_survives_junk_frame_injection`,
      `messages_stream_survives_single_frame_corruption`,
      `bedrock_stream_errors_are_always_classified` (any `Err` is `StreamDecode`).
- [x] Corpus module carries an "add your codec here" registry comment.
- [x] AGENTS.md invariants gain one bullet: provider bytes never error a chunk stream.
- [x] Full workspace gate green.

## Progress

- 2026-07-04 filed (stream-resilience epic).
- 2026-07-04 **DONE.** Implemented both enforcement layers.

  **Mechanism chosen: crate-local `clippy.toml`, not the codegate fallback.** Verified it actually
  resolves per-crate under `--workspace`: added a temporary unannotated
  `serde_json::from_str("{}")` call to `flux-providers/src/lib.rs` and confirmed
  `cargo clippy --workspace --all-targets -- -D warnings` failed on it with the exact `reason`
  string from `clippy.toml`; removed the probe and confirmed the gate went green again. No
  flux-codegate fallback was needed.

  **Scope beyond the anticipated file list.** Re-grepping the current source (as instructed, since
  line numbers had drifted) turned up bare calls to the four banned methods in `codex.rs` too (not
  just openai.rs/messages/mod.rs/bedrock.rs): `is_terminal_event`, the WS first-frame kind-sniff
  inside `CodexWsTransport::connect`, and two fixture-parsing assertions in its `mod tests`. All
  three already degrade gracefully (`.ok()`/`.expect()` on trusted test data) and got the same
  targeted `#[allow(clippy::disallowed_methods)]` treatment. `realtime/event.rs` also has a bare
  `?`-propagating parse (`ServerEvent::parse`), but that module is behind the `realtime` feature,
  which nothing in the workspace enables by default — it isn't compiled/linted by the standard gate
  and was left untouched (out of scope for this pass; flagged here for whoever builds that feature
  with clippy on).

  Functions annotated (7 total, each with a reason comment at the call site):
  `openai.rs::map_chat_stream`, `::map_responses_stream`, `::parse_inline_call_body`;
  `messages/mod.rs::map_messages_stream_inner`; `bedrock.rs::frame_to_sse`, `::resolve_sso`,
  `::refresh_sso_token`, `::resolve_eks_pod_identity`; `codex.rs::is_terminal_event`,
  `CodexWsTransport::connect`, and its `mod tests`. `map_chat_stream`/`map_responses_stream` made
  `pub(crate)` (were private `fn`); `map_messages_stream_inner` got the allow but stayed private —
  the corpus drives the already-`pub` `map_messages_stream` wrapper instead; bedrock's
  `map_bedrock_event_stream` was already `pub`.

  **Corpus (`crates/flux-providers/src/envelope_corpus.rs`, 10 tests, all green):** one valid
  fixture turn per codec (chat/responses/messages SSE strings; bedrock AWS event-stream frames
  built by a local, test-only CRC32/frame encoder — duplicated rather than widening bedrock.rs's
  private test helpers' visibility). Sampling strategy: **truncation** is exhaustive over every
  byte offset (fixtures are a few hundred bytes, so exhaustive is cheap — a stride would matter only
  for a much bigger corpus); **junk-frame injection** inserts one deterministic junk frame at every
  frame boundary (`0..=n`, not every byte); **single-frame corruption** applies one deterministic,
  ASCII/UTF-8-safe corrupting edit per frame (an opening `{` flipped to a stray `}`, keeping the SSE
  framing intact so only the JSON envelope — not the SSE tokenizer — is exercised), iterated across
  every frame in the fixture, not every byte within it. Bedrock's single test
  (`bedrock_stream_errors_are_always_classified`) runs all three strategies internally since they
  share one assertion (`Err` ⇒ `StreamDecode`); the four contractual names from Acceptance are
  exact matches, plus 6 supplementary tests give full 3-strategy × 4-codec coverage.

  **Red-proof (failing-first, since the underlying tolerance already existed from A-34/35/36):**
  temporarily reverted `messages/mod.rs::map_messages_stream_inner`'s tolerant `match` back to a
  bare `serde_json::from_str(data)?` (the pre-A-35 shape) and re-ran the messages corpus tests: 2 of
  3 turned red (`messages_stream_survives_junk_frame_injection`,
  `messages_stream_survives_single_frame_corruption`, both panicking with the raw `Serde` error the
  bare `?` now propagated) while `messages_stream_survives_truncation_at_every_offset` stayed green
  — expected, since truncation alone can't produce a malformed *complete* frame (the SSE decoder
  only surfaces complete events; this matches the design doc's own framing argument). Reverted the
  change back immediately; reran to confirm all 10 corpus tests green again.

  **Gate (root workspace):** `cargo build --workspace` clean; `cargo test --workspace` all green
  (no `FAILED`/`error` across every crate, flux-providers 100/100 incl. the 10 new corpus tests);
  `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean (one
  formatting pass applied to the new file); `cargo test -p flux-codegate` 4/4 green (unused, since
  the clippy mechanism fired — run anyway per the task's gate list).

## Notes

- Runs LAST (lints the post-A-34/35/36 surface).
- The lint stops new bare parse sites at merge time; the corpus catches tolerant-looking helpers
  that still `?`-propagate. Both are needed.
