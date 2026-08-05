---
id: C-545
title: "Do not retry quota-exhausted HTTP 429; surface the terminal limit directly"
pillar: Core
status: done
priority: 33
epic:
design:
areas: [flux-provider, flux-providers]
note: "429 is blanket-retryable today (flux-provider/src/retry.rs); a codex usageLimitExceeded with a days-away reset gets backoff-retried instead of failing fast to ready"
---

# Do not retry quota-exhausted HTTP 429; surface the terminal limit directly

## Goal

The provider layer distinguishes a *terminal* 429 — quota/usage exhaustion with a reset that is
minutes to days away (codex `usageLimitExceeded`, and other providers where the response carries an
explicit reset or "purchase more credits" marker) — from a *transient* rate-limit 429, and does not
retry the terminal kind: the error surfaces immediately with the reset time, so the caller returns
to ready at once instead of burning backoff cycles against a wall that will not move.

## Acceptance

- [x] `is_retryable_status` / the `RetryReason::Status(429)` path
      (`crates/flux-provider/src/retry.rs:32`, `:166` as of 2026-08-05) no longer treats every 429
      as retryable: a provider-classified terminal 429 bypasses retry entirely. A failing-first
      test drives a codex-shaped `usageLimitExceeded` 429 through the retry policy and proves zero
      retry attempts.
- [x] The codex adapter (`crates/flux-providers/src/codex.rs`) classifies its 429 body: a
      `usageLimitExceeded` (or equivalent reset-bearing) payload is terminal; a bare 429 stays
      transient/retryable. Other adapters (anthropic, openai, openrouter, bedrock, ollama) get the
      same hook with a per-provider classification where their error shape carries an explicit
      quota/credit marker (e.g. Anthropic "credit balance is too low"); unclassifiable 429s keep
      today's retry behavior.
- [x] The surfaced terminal error carries the provider's reset time / limit message verbatim so an
      operator (or fleet coordinator) can schedule around it; a test asserts the message and reset
      survive to the caller.
- [ ] The gate is green in both workspaces.

## Progress

- 2026-08-05: added failing-first coverage that drove a codex-shaped `usageLimitExceeded` 429
  through `NativeProvider`: before the fix it made two HTTP hits despite a one-retry budget
  (`left: 2`, `right: 1`); after the fix it makes one initial hit, emits zero
  `RetryReason::Status(429)` events, and returns the reset-bearing JSON body byte-for-byte.
- 2026-08-05: added `Credential::is_terminal_http_error` as the provider-specific body-aware seam.
  Codex classifies typed/reset-bearing usage limits; OpenAI, Anthropic, OpenRouter, and Bedrock
  classify their explicit quota/credit markers; both Ollama transports remain deliberately
  unclassified. A bare codex 429 still retries once and recovers.
- 2026-08-05: targeted gate is green:
  `cargo test -p codewandler-flux-provider -p codewandler-flux-providers` (27 + 192 passed, one
  live-key test ignored), targeted `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, and `cargo test -p flux-codegate` (51 passed). Per the dispatched
  wave contract, the integration parent owns the one final full gate across both workspaces.

## Notes

- Filed 2026-08-05 via /track:story. Motivating incident: the codex account hit
  "usage limit … try again at Aug 11" and flux's blanket-retryable 429 policy retries it as if
  transient, wasting wall-clock; the Anthropic "Credit balance is too low" failure the same morning
  is the sibling case (that one arrives as a 400-class `invalid_request_error` — classify by body
  marker, not status alone).
- Transient rate-limit 429s (per-minute throttles) must keep retrying — the split is
  terminal-vs-transient, not 429-vs-rest.
