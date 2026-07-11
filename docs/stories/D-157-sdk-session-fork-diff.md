---
id: D-157
title: Session::fork + Fork::{inject, edit, diff}
pillar: Agent
status: backlog
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 4 — counterfactual sessions for embedders"
---

# Session::fork + Fork::{inject, edit, diff}

## Goal
`Session::fork(at_turn)` wraps `fork::replay_prefix`; the returned `Fork` diverges via
`inject(input, sink)` / `edit(node, value, sink)` into a NEW session and `diff(&Session)` reports
what changed (`flux_events::run_diff`).

## Acceptance
- [ ] Failing-first: fork at turn 1, inject a different user input → `diff` reports the diverged
      ops; the original session's log is untouched (assert head_seq unchanged).
- [ ] `edit` divergence works on a bound node value.
- [ ] `RunDiff` re-exported via `flux_sdk::observe`.

## Progress
- (pending)

## Notes
- `crates/flux-flow/src/fork.rs:81,:230,:286`; `crates/flux-events/src/projection.rs:737`.
  Depends on D-142 + D-156.
