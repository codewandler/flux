---
id: C-181
title: Make provider retries visible while they happen
pillar: Core
status: done
epic: turn-latency-visibility
design: docs/designs/turn-latency-visibility.md
note: "retries only tracing::warn! today (flux-provider/src/lib.rs:775-844) and no surface installs a subscriber — a 30s backoff is indistinguishable from a slow model"
---

# Make provider retries visible while they happen

## Goal
`NativeProvider::stream` retries transient 429/5xx and transport failures with exponential backoff
up to `DEFAULT_MAX_RETRIES` (6), and force-refreshes OAuth on a 401. Every one of those paths is
invisible: they emit `tracing::warn!` and no product surface installs a subscriber. A turn that
sleeps 30s through backoffs looks exactly like a turn where the model is thinking. Add a narrow
observer seam so a retry is reported live, and so its count survives onto the `model.call`
observation for after-the-fact attribution.

## Acceptance
- [x] `flux-provider` exposes `RetryEvent` / `RetryReason` / `RetryObserver` plus a
      `with_retry_observer` task-local scope (matching the `scope_runtime_turn` idiom,
      `flux-runtime/src/lib.rs:457`); no observer installed is a no-op.
- [x] The connect loop notifies **before** each backoff sleep, for all four paths: retryable status,
      transport error, forced OAuth refresh, and WS→HTTP transport fallback — failing-first test
      driving a stub transport that fails then succeeds, asserting the events and their ordering
      relative to the sleep.
- [x] The model stage installs an observer that forwards a `model.retry` observation to the live
      `AgentSink` **and** counts; the counts land in `ModelCallMetrics` and on the `model.call`
      observation, including when the call ultimately fails (no stream is ever returned on that path,
      so the count cannot ride the stream).
- [x] The TUI footer shows a live warn-styled `↻ retry 2/6 · 4s` while a backoff is pending, and
      clears it when the call resumes or ends.
- [x] Retry policy is unchanged — `DEFAULT_MAX_RETRIES`, `backoff_delay`, and `Retry-After` handling
      keep their existing tests green.

## Progress
- Implemented 2026-07-28. `crates/flux-provider/src/retry.rs` holds `RetryEvent`/`RetryReason`/
  `RetryObserver` + the `with_retry_observer` task-local and the public `report_retry` producer.
  The connect loop notifies before each sleep on all four paths; `flux-flow`'s `consult_model`
  installs a counting+forwarding reporter, folds the tallies onto `ModelCallMetrics`, and emits
  them on `model.call`. The three `stream_blocks` call sites in `staged.rs` now share it.
- Tests: `retryable_status_reports_each_retry_to_the_scoped_observer`,
  `a_retry_event_precedes_the_backoff_it_announces`,
  `a_forced_oauth_refresh_is_reported_on_the_retry_seam`,
  `a_transport_fallback_is_reported_on_the_retry_seam`,
  `retries_without_an_observer_are_unaffected` (flux-provider);
  `a_connect_retry_reaches_the_surface_live_and_is_tallied_on_the_call`,
  `retries_are_tallied_even_when_the_call_ultimately_fails` (flux-flow);
  `the_sink_turns_a_model_retry_observation_into_a_live_signal`,
  `a_pending_retry_takes_over_the_footer_from_the_model_timer` (flux-tui).
- The `model.retry` observation carries the reason's short LABEL only — never the raw transport
  error, which can embed an endpoint URL. The durable tally rides `model.call` instead.

## Notes
- A field on `NativeProvider`/`Request` was rejected: providers are built once per session and
  shared, the sink is per-turn — see the design's "why a task-local" note.
- `ModelTrace` already counts `http_attempts`/`oauth_refreshes`/`transport_fallback` but only prints
  to stderr under `FLUX_MODEL_TRACE`; that stays a developer path and is not the seam here.
- Consumed by [[C-180]] for the badge's `↻ N retries` suffix.
