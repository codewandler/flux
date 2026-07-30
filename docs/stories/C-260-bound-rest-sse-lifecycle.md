---
id: C-260
title: "Cancel REST SSE turns on disconnect and bound event buffering"
pillar: Core
status: done
priority: 5
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-server]
note: "MEDIUM — detached REST stream work survives client disconnect and writes to an unbounded channel"
---

# Cancel REST SSE turns on disconnect and bound event buffering

## Goal

Make the REST streaming route stop provider/tool work when its consumer disappears and prevent a
slow reader from accumulating unbounded events.

## Acceptance

- [x] Failing-first tests prove dropping the REST SSE body does not currently cancel the turn and a
      stalled consumer can enqueue beyond a finite cap.
- [x] The route uses a bounded channel with documented backpressure/cancellation semantics.
- [x] A response-body drop guard cancels the request-owned `CancellationToken`; the spawned turn is
      joined or otherwise guaranteed not to continue approved effects after disconnect.
- [x] Normal completion, explicit cancellation, timeout exemption, and disconnect all leave a valid
      provider session history.
- [x] Buffered REST, webhook, and blocking A2A deadlines cancel and await their owning turn before
      `408`, including child cleanup and durable terminal finalization.
- [x] The REST path reuses the proven A2A stream-lifecycle primitive where practical.
- [x] `cargo test -p flux-server`, Clippy for the crate, and the standard gate are green.

## Progress

- REST SSE now owns a request `CancellationToken` through a response-body drop guard, uses a
  256-event bounded channel, and cancels when its synchronous sink sees a full or closed buffer.
  The producer owns its daemon work permit through cancellation/finalization, and cancelled turns
  are folded into provider-budget accounting on every exit path.
- Added route-level regression coverage for disconnect cancellation, durable `cancelled`
  finalization, and `ValidHistory`, plus a direct stalled-buffer test. Existing server coverage pins
  normal completion, A2A explicit cancellation, and timeout exemption.
- Scoped verification is green: `cargo test -p flux-server` (34 unit + all integration/doc tests),
  `cargo clippy -p flux-server --all-targets -- -D warnings`, package `cargo fmt --check`, and
  `cargo test -p flux-codegate`. The integrated workspace build/test/Clippy/format gate is also
  green.
- Closure review found the non-streaming timeout still dropped the live handler future. Protected
  REST, webhook, and blocking A2A requests now receive a request-owned cancellation token; on
  deadline the middleware cancels and awaits engine/child finalization before returning `408`.
  Regressions pin durable `cancelled` outcomes and `ValidHistory` on REST and blocking A2A.

## Notes

- Evidence: review B finding 2; compare `crates/flux-server/src/lib.rs` with the bounded A2A stream
  and its cancellation drop guard.
