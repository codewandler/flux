---
id: D-157
title: Session::fork + Fork::{inject, edit, diff}
pillar: Agent
status: done
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
- [x] Failing-first: fork at turn 1, inject a different user input → `diff` reports the diverged
      ops; the original session's log is untouched (assert head_seq unchanged).
- [x] `edit` divergence works on a bound node value.
- [x] `RunDiff` re-exported via `flux_sdk::observe`.

## Progress
- **Done (unreleased).** `Session::fork(at)` (`crates/flux-sdk/src/session.rs`) mints a fork session
  (`create_session_with_context` correlated to the source), copies the conversation, and replays the
  prefix via `flux_flow::fork::replay_prefix` into it (a `DiscardSink` for the hermetic prefix) —
  original untouched. Returns a `Fork { engine, id, prefix, turn_guard }` with `inject(value, sink)`
  (→ `diverge_inject`), `edit(ast, sink)` (→ `diverge_edit`; halts surface as `Err`), `diff(&Session)`
  (→ `flux_events::run_diff` over the two run traces), `session()`, and `id()`. The engine is shared
  across sessions, so no new engine is built.
- `Fork` re-exported at the crate root; `RunDiff`+`DiffRow` added to `flux_sdk::observe`.
- Failing-first tests (`crates/flux-sdk/src/lib.rs`): a `BindPlanMock` records `bind x = read(note);
  return x` on `Storage::dir`; `fork_inject_diverges_and_leaves_the_original_untouched` forks at
  stmt 0, injects `"injected"`, asserts `!diff.identical` AND `events.head_seq(sid)` unchanged;
  `fork_edit_diverges_on_a_bound_value` edits with an alternate bind-literal plan → diverges. A
  `NeverMock` provider on the fork client proves no live model dispatch.
- `diverge_inject` requires the fork point (`prefix.plan.body[at]`) to be a `bind`; fork `at` is a
  statement index into the recorded final plan. CHANGELOG + WHATS-NEW + website mirror updated. Gate
  green (workspace 2169; clippy all-features / fmt / codegate). **Not committed/released.**

## Notes
- `crates/flux-flow/src/fork.rs:81,:230,:286`; `crates/flux-events/src/projection.rs:737`.
  Depends on D-142 + D-156.
