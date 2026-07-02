---
id: L-17
title: Runtime semantics hardening — fatality, eval-path unification, concurrency audit, real throttle/debounce
pillar: Language
status: done
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: statement-position jq diverges from bind-position; a denied confirm inside loop/composite IS retried; parallel/race drop completed-branch audit; checkpoint run/resume keys disagree; throttle counts entries not dispatches; debounce is a sleep stub
---

# Runtime semantics hardening

## Goal
Make the interpreter match the language's documented semantics under the epic's normative "target
semantics" section (design doc): one eval path for pure nodes, structural error fatality, honest
concurrency audit, unified checkpoint keys, and full `throttle`/`debounce` semantics.

## Acceptance
- [x] **Failing-first:** statement-position `jq` on a string-stored JSON value succeeds like the
      bind arm (F13); implemented by factoring one shared pure-node eval helper (not a point fix).
- [x] **Failing-first:** a `ConfirmDenied`/`AssertFailed` inside `loop` and inside a composite op
      is NOT retried by an enclosing `retry` (F14); fatality checked via `is_fatal()` across
      wrapping layers.
- [x] `parallel`: sibling failure preserves completed branches' buffered output/steps, merged in
      declaration order before the error propagates (F15 runtime half; test).
- [x] `race`: all-branches-failed yields a joined branch error distinct from timeout; losers'
      dispatched steps counted in transcript/step count (F16; tests for both).
- [x] Checkpoint: one `flow_key` (name + body hash) for `execute_flow` and `resume_flow`; an
      edited body does not fast-forward (F17; test).
- [x] `throttle` counts op dispatches (budget-style) with an atomic keyed bucket (F18; test).
- [x] `debounce`: per-`name` cross-turn last-trigger state in the session store; body runs only
      after `wait_ms` of quiet (F19; test).
- [x] Small-fix batch (F20): `trim_output` on `pipe`/`memo` transcript paths; `last_value` updated
      unconditionally in `when`/`unless`/`match`/`route`; `StepId` includes the op name; `each`
      item binding restored after the loop.
- [x] In-crate tests exist for retry-failure/backoff/fatal, race all-fail, parallel branch-failure
      (the review found zero in-crate constructions of these nodes). Full gate green; CHANGELOG
      entry left to the epic close (the file is shared across the parallel L-15..L-19 agents).

## Progress
- 2026-07-02 — all eight tasks implemented in `crates/flux-lang/src/{runtime,error}.rs`; 17 new
  in-crate tests, `cargo test -p flux-lang` (179+7) and the `flux-flow` canary (125) green; clippy
  `-D warnings` clean; fmt applied.
  - **F13** `eval_pure_node` is the single eval path for `expr`/`fmt`/`jq`/`parse`/`var`/`lit`/
    `obj`/`list` in BOTH bind and statement position (statement `jq` now re-parses string-stored
    JSON). Failing-first verified empirically (fix reverted → test fails).
  - **F14** `FlowError::is_fatal()` (error.rs); `loop` and the composite-op boundary propagate
    fatal errors structurally instead of stringifying; `retry` checks `is_fatal()`. Verified
    failing-first (loop wrap reverted → test fails). Policy denial is NOT structurally
    representable yet (it surfaces as an in-band `OpOutcome::is_error` string) — documented.
  - **F15** `parallel` uses `join_all`: every branch's buffered sink output/steps/transcript/
    binding merges in declaration order before the first (declaration-order) branch error
    propagates. Failed branches' partial audit merges too.
  - **F16** `race` drives branches via `FuturesUnordered` with per-branch state held OUTSIDE the
    futures, so cancelled losers' dispatched steps/transcript survive and merge; all-failed yields
    "`race` failed: all N branch(es) errored: `name`: err; …" — distinct from the timeout message
    (unchanged). Sink replay stays winner-only (a cancelled loser's buffer may end mid-op).
  - **F17** `flow_key` = declared name **plus** body hash (`name#<hash16>` / `h:<hash16>`), one
    derivation for run + resume; new `resume_flow_named` carries the name (public `resume_flow`
    delegates with `None` — signature unchanged so flux-flow keeps compiling; wiring the name
    through the engine's resume path is follow-up outside this story's file ownership).
  - **F18** `throttle` runs its body statement-at-a-time (budget-style): admit-check before each
    statement, then the statement's actual dispatch delta is appended to the keyed timestamp
    bucket; the load-evict-store cycle is atomic under a process-wide lock.
  - **F19** `debounce` records a unique per-trigger token under `__debounce_last_<name>` in the
    session store, sleeps `wait_ms`, and runs the body only if its token is still current — a
    re-trigger within the window supersedes (coalesces) the older trigger.
  - **F20** (a) `trim_output` on `pipe`/`memo` transcript pushes; (b) `last_value` unconditional
    in `when`/`unless`/`match`/`route`; (c) `StepId` = `step_<op>_<hash16>`; (d) `each` saves and
    restores an outer binding of the item symbol (with its original metadata) after the loop.

## Notes
- Findings F13–F20; normative semantics in docs/designs/flux-lang-v1-hardening.md §Target semantics.
