---
id: C-180
title: Show LLM inference wait time in the TUI
pillar: Core
status: done
epic: turn-latency-visibility
design: docs/designs/turn-latency-visibility.md
note: "the measurements already exist on the model.call observation (duration_us/ttft_us) — the TUI sink drops them (controller.rs:188)"
---

# Show LLM inference wait time in the TUI

## Goal
The TUI attributes execution time to operations (`exec 1.2s` per tool card) but never to model
inference, so a `4 steps · 18.1s` turn gives no way to tell a slow model from a slow op. Spend the
per-call timings the engine already publishes on `model.call`: a per-round badge in the transcript,
a live model-call timer in the footer, and a turn-level `llm` split in the closing summary.

## Acceptance
- [x] `ChannelSink::observation` carries `duration_us`, `ttft_us`, and the C-181 retry count off the
      `model.call` observation into the UI event (today it extracts only `usage`/`model`/`stage`/
      `operations`, `controller.rs:188-215`) — failing-first test asserting the fields survive the
      hop.
- [x] A sealed thinking entry renders one dim badge line
      (`◇ model stage.explore #2 · 4.2s · ttft 0.9s`), and still renders it for a stage that emitted
      no thinking tokens — TestBackend test.
- [x] While a model call is in flight the footer shows its own elapsed beside the turn elapsed
      (`explore · 18.1s · model 3.2s`), stamped on `Planning(true)` and cleared on `Planning(false)`.
- [x] The end-of-turn footer segment splits wall clock: `4 steps · 18.1s · llm 12.4s`, accumulated
      across every model call in the turn and reset when the next turn starts.
- [x] Turns with no model call (a `/`-command, a resumed transcript) render exactly as today — no
      `llm 0s` segment, no empty badge row.

## Progress
- Implemented 2026-07-28. `ChannelSink` carries `duration_us`/`ttft_us`/`retries` off `model.call`
  into `UiEvent::CallUsage`; `ChatState::record_model_call` folds the wait and badges the round's
  thinking entry; the footer gained the in-flight `model Ns` segment and the `llm Ns` turn split.
- Tests: `a_sealed_thinking_entry_carries_its_model_call_latency`,
  `a_model_call_without_thinking_tokens_still_renders_its_latency`,
  `consecutive_model_calls_badge_their_own_rounds`, `the_footer_shows_the_in_flight_model_wait`,
  `the_turn_summary_splits_total_time_from_model_wait`,
  `a_turn_without_a_model_call_shows_no_llm_segment`,
  `the_sink_carries_model_call_latency_and_retries`,
  `model_wait_accumulates_across_every_round_of_the_turn` (flux-tui).
- Verified failing-first: with the badge render removed, the two badge tests fail.

## Notes
- Ordering is safe: `PlanningGuard` drops (→ `Planning(false)`, which seals the thinking entry)
  before `observe_model_call` runs — `crates/flux-flow/src/staged.rs:721-737`.
- The plain CLI already formats this line (`format_model_call`, `crates/flux-cli/src/rendering.rs:382`)
  but gates it behind `--trace-loop`; the TUI badge is unconditional.
- Retry count comes from [[C-181]]; land that first or leave the field `None` until it exists.
