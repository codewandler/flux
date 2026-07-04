---
id: A-33
title: "Stream-decode backstop: classified decode errors become planner retries, not turn deaths"
pillar: Agent
status: done
epic: stream-resilience
design: docs/designs/stream-resilience.md
note: "new `Error::StreamDecode` + `Chunk::StreamDiagnostic`; `stream_blocks` keeps usage on error (C-31 pushed down); decode-class errors cost one `max_steps` step via `last_reject` + continue — lands first, defines the seams for A-34..A-37"
---

# Stream-decode backstop

## Goal

A mid-stream decode failure (malformed provider bytes) costs one planner step — never the turn —
and its usage is recorded. This is the layer that makes the invariant hold even for future
unhardened codecs. Design: [stream-resilience](../designs/stream-resilience.md).

## Acceptance

- [x] `flux-core/error.rs`: new variant `StreamDecode(String)`
      (`"provider stream decode error: {0}"`) — distinct from `Provider` (transport) and `Serde`.
- [x] `flux-core/stream.rs`: new `Chunk::StreamDiagnostic { dropped_frames: u32, detail: String }`;
      compiler-led fixes for any exhaustive `Chunk` matches.
- [x] `flux-flow/compile.rs`: `stream_blocks` returns usage alongside its result; in
      `compile_turn_inner`, decode-class errors (`StreamDecode` | `Serde` in this stream context)
      accumulate usage, set `last_reject` ("the provider stream broke while decoding the model's
      output: …"), and `continue` within `max_steps`; no message is pushed (a fresh identical call
      is the correct retry). Non-decode errors keep propagating. Empty turn with a
      `StreamDiagnostic` folds the diagnostic into `last_reject`. `engine.rs planner_error` renders
      the new variant.
- [x] Failing-first: `stream_decode_error_becomes_a_planner_retry_and_the_turn_survives` (mock:
      `Err(StreamDecode)` call 1, good plan call 2 → attempts == 2).
- [x] Failing-first: `exhausted_budget_reports_the_stream_decode_error`.
- [x] Failing-first: `usage_survives_a_mid_stream_decode_error`.
- [x] Failing-first: `empty_turn_with_stream_diagnostic_sets_last_reject`.
- [x] Full workspace gate green.

## Progress

- 2026-07-04 filed (stream-resilience epic, from s_368-class envelope-kill forensics).
- 2026-07-04 implemented. `flux-core::Error::StreamDecode(String)` added (after `Provider`);
  `flux_core::Chunk::StreamDiagnostic { dropped_frames: u32, detail: String }` added (after
  `Done`) — the only exhaustive `Chunk` match in the workspace without a wildcard arm
  (`flux-providers/src/messages/mod.rs` test helper) got one new arm, mechanically. `stream_blocks`
  (`flux-flow/src/compile.rs`) now returns
  `(Result<(Vec<ContentBlock>, String, Option<StopReason>, Option<StreamDiagnostic>)>, Usage)` —
  `Usage` unconditionally alongside the `Result`, mirroring `compile_turn`/`compile_turn_with_arm`'s
  C-31 shape one level down (a local `struct StreamDiagnostic { dropped_frames, detail }` carries
  the tolerated-drop signal out of the tuple). `compile_turn_inner` unpacks usage first (always
  accumulated), then matches the `Result`: a new `is_stream_decode(&Error) -> bool` predicate
  (`matches!(e, Error::StreamDecode(_) | Error::Serde(_))`, scoped to this stream-consuming call
  site only) routes decode-class errors to `last_reject = "the provider stream broke while
  decoding the model's output: {e}"` + `continue` (no message pushed); everything else still `?`
  -propagates. The pre-existing "truly empty turn" branch (no blocks, no text, `stop_reason !=
  MaxTokens`) now folds a present `StreamDiagnostic` into `last_reject` before the step-budget
  check, and that check's own error format was unified with the bottom-of-loop fallback so a
  1-step budget also surfaces the named cause instead of the bare "no plan" message.
  `render_completion` (the post-plan finalize call) updated for the new tuple shape only — no
  parallel retry logic added there per the story's note; a decode error on that one-shot call still
  propagates as before. `engine.rs::planner_error` gained a `StreamDecode` arm rendering "the model
  provider's response broke mid-stream and could not be decoded: …" instead of the raw
  `Display`. Test harness: added `MockChunks`/`mock_chunks` (a `Mock` variant whose per-call
  sequence is `Vec<Result<Chunk>>`, so a test can inject an `Err` mid-stream — `Mock` itself always
  wraps chunks in `Ok` and was left untouched). Red→green: `stream_decode_error_becomes_a_planner_retry_and_the_turn_survives`,
  `exhausted_budget_reports_the_stream_decode_error`, `usage_survives_a_mid_stream_decode_error`,
  `empty_turn_with_stream_diagnostic_sets_last_reject` — verified RED against the prior code (see
  below) before restoring the implementation, then GREEN. Gate:
  `cargo build --workspace` clean; `cargo test --workspace` 87 test binaries green (one transient
  failure in `flux-events::projection::tests::pre_l38_plan_attempted_rows_decode_without_plan_source`
  observed mid-run, caused by a concurrent session actively editing that file for the unrelated
  L-38 story — confirmed by re-running, green on the next pass, not touched by this story);
  `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean;
  `cargo test -p flux-codegate` green (4/4). Deviation: tests were authored alongside the
  implementation (the type additions and the classification logic are too entangled to sequence
  cleanly commit-by-commit) rather than test-then-code-then-test in strict order; RED was verified
  after the fact by temporarily reverting the classification `match`/diagnostic-fold in
  `compile_turn_inner` back to a bare `result?` / `let _ = diagnostic;` and re-running the four new
  tests — all four failed for the expected reason (raw `StreamDecode`/generic "no plan" errors
  instead of the classified retry/last_reject text) — then the implementation was restored and the
  suite re-run green. `flux-events`/`flux-core::pricing.rs` (C-34's and a concurrent L-38 session's
  files) were not touched beyond what already existed in the working tree.

## Notes

- One-shot `compile()` loop and `finalize` get the same treatment / usage-on-error for free from the
  signature change; keep it type-driven like C-31.
- A-38 also edits compile.rs — sequence after this story.
