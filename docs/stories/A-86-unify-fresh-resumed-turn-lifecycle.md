---
id: A-86
title: Unify fresh and resumed turn lifecycle
pillar: Agent
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: suspended-flow resume bypasses cancellation and the deterministic op-cache turn boundary
---

# Unify fresh and resumed turn lifecycle

## Goal

Run fresh, authored, and suspension-resumed turns through one lifecycle that consistently installs
cancellation, starts a cache generation, records telemetry, and leaves provider-valid history.

## Acceptance

- [x] One internal turn runner owns message recording, `begin_cache_turn`, turn/event creation,
      lexical runtime context, cancellation racing, usage flush, assistant finalization, and cleanup
      for both fresh and resumed paths.
- [x] Failing-first `read → await → external edit → resume → read` coverage proves the resumed read
      observes the post-suspension file contents rather than a prior-turn cache entry.
- [x] Pre-cancelled and mid-operation cancellation tests on a resumed flow return promptly and leave
      a provider-valid session with a defined cancellation terminal state.
- [x] Suspension consumption is transactional or explicitly recoverable: cancellation/failure cannot
      silently delete the only continuation checkpoint without an auditable terminal disposition.
- [x] Resumed `ai_segment`/sub-agent activity retains correct session, sink, usage, and audit context;
      no user-after-user or split tool-use/result history is possible on any termination branch.
- [x] Existing fresh-turn, decision-resume, process-restart, and flow-driven voice behavior remains
      green.

## Progress

- 2026-07-14 — Unified adaptive, authored, and resumed execution in one turn lifecycle. Fresh-cache
  resume, pre/mid-operation cancellation, recoverable checkpoint, valid-history, usage, restart, and
  voice-flow tests cover the shared path.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Residual of the durable-resume and cache work (C-26/A-16/L-54), not a change to Flux-Lang `await`
  semantics.
- A-87 may reuse the same internal turn guard, but this story owns lifecycle parity.
