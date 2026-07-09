---
id: C-28
title: "Harden the codex WS transport so the guaranteed HTTP fallback always engages"
pillar: Core
status: done
priority:
epic: review-hardening
design: docs/designs/review-hardening.md
note: "three ways the codex WS transport defeats its own fail-fast contract: a non-char-boundary byte slice panics on a >300-byte error payload; connect() waits for the first frame with no timeout so a blackholing proxy hangs the turn; and a clean Close before the terminal event silently truncates the response"
---

# Harden the codex WS transport so the guaranteed HTTP fallback always engages

## Goal
Make `CodexWsTransport` honor the `StreamTransport` contract that connect "must fail (rather than hang or
panic) on any connect-time problem so the HTTP fallback can take over." Three defects in the pre-commit
connect/stream path each defeat that fallback:
1. **Panic on truncation** — the pre-data `error` event slices `&t[..t.len().min(300)]`
   (`crates/flux-providers/src/codex.rs:232`), a raw byte range that panics when byte 300 is not a UTF-8
   char boundary (a >300-byte error payload with a multibyte char straddling the offset). Unwind instead
   of `Err` → no fallback.
2. **No connect timeout** — the first-frame wait `loop { ws.next().await … }` (`:220`) has no timeout, so a
   proxy that accepts the upgrade then blackholes the socket pends forever and the whole turn hangs.
3. **Silent truncation on early close** — `Ok(Message::Close(_)) => break` (`:263`) ends the turn cleanly on
   a close received *before* the terminal event, so a policy-close (1008) after the first data frame surfaces
   partial text as a completed turn, contradicting the adjacent doc's "a reset before the terminal event
   still surfaces as a stream error."

## Acceptance
- [x] Failing-first test (1): an `error` event payload >300 bytes with a multibyte char across offset 300
      returns `Err` (fallback), not a panic. Use the repo's char-boundary-safe truncation helper.
- [x] Failing-first test (2): a connect that receives no frame within a bounded timeout returns `Err` so the
      HTTP-SSE path engages, rather than pending indefinitely.
- [x] Failing-first test (3): a `Close` frame received before the terminal event surfaces as a stream error
      (real truncation), not a clean end-of-turn.
- [x] The happy path (terminal event received) is unchanged; existing codex transport tests pass.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🔴 robustness. Three distinct fixes in one seam
  (`connect` + the pre-commit frame loop); folded into one story as they share the fail-fast contract and
  one PR. The byte-slice panic and the early-close truncation were each independently reported and verified.
- 2026-07-03 implemented, all three defects fixed in `crates/flux-providers/src/codex.rs`:
  1. Added a local char-boundary-safe `truncate_char_boundary` helper (no shared public helper was
     reachable without adding a cross-crate dependency edge from `flux-providers` to `flux-core`'s
     private `context::truncate_str` or `flux-plugin`'s private `truncate_on_char_boundary` — both are
     module-private in crates `flux-providers` doesn't depend on, so the established idiom was
     reproduced locally rather than newly hand-rolled) and used it in place of the raw
     `&t[..t.len().min(300)]` slice.
  2. Added a `first_frame_timeout: Duration` field on `CodexWsTransport` (production default
     `DEFAULT_FIRST_FRAME_TIMEOUT` = 30s, configurable via the new `oauth_at_timeout` constructor used
     by tests) and wrapped the first-frame wait loop in `tokio::time::timeout`, returning `Err` on
     elapse. Promoted `tokio` from an optional (`realtime`-feature-only) dependency to a hard dependency
     of `flux-providers` in `Cargo.toml` (removed the now-redundant `[dev-dependencies] tokio` entry).
  3. Changed the mid-stream `Ok(Message::Close(_)) => break` arm to surface
     `Error::Provider("ws closed before terminal event: …")` via `?` instead of ending the turn cleanly.
  New tests: `ws_error_event_over_300_bytes_with_multibyte_char_does_not_panic`,
  `ws_first_frame_timeout_falls_back_to_http`, `ws_close_before_terminal_event_surfaces_as_stream_error`
  (plus two new hermetic stub servers, `ws_blackhole_server` and `ws_close_before_terminal_server`).
  Verified each test fails for the intended reason pre-fix (panic / outer-timeout-guarded hang /
  assertion failure) by temporarily reverting just the three fix hunks, then restored. Gate green:
  `cargo test -p flux-providers` (71 passed), `cargo clippy -p flux-providers --all-targets -- -D
  warnings` (clean), `cargo fmt -p flux-providers --check` (clean); also spot-checked
  `cargo build -p flux-providers --features realtime` still builds after the Cargo.toml change.

## Notes
- Evidence: `crates/flux-providers/src/codex.rs:220` (no connect timeout), `:232` (byte-slice panic),
  `:263` (Close-before-terminal). StreamTransport contract doc in the same module.
- Residual of [C-07](C-07-codex-websocket-transport.md). Design: [review-hardening](../designs/review-hardening.md).
