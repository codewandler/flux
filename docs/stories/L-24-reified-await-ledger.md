---
id: L-24
title: Reified-await ledger fold — loop-side checkpoint∘await (keep the prefix across an await)
pillar: Language
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: today a reified await inside a loop plan abandons all post-await work; with the ledger, the post-await re-emission fast-forwards the completed prefix — the loop-side case of the deferred checkpoint∘await composition
---

# Reified-await ledger fold

## Goal
When a loop plan hits a top-level `await`, it is reified (not a turn suspension) and today the
completed prefix is lost to the follow-up plan. Fold awaits into the halt-latch machinery: append
`PlanHalted{kind:"awaiting"}` with the same statement ledger, so when the model re-emits the plan
with the awaited information incorporated, the completed prefix fast-forwards instead of re-running.

## Acceptance
- [x] A reified await in resumable mode records the halt latch + ledger; the follow-up re-emission
      fast-forwards the matching prefix. Failing-first:
      `post_await_reemission_keeps_completed_prefix`.
- [x] The engine's pre-authored await-suspension path (suspensions table) is untouched.
- [x] Gate green.

## Progress
- **2026-07-06 — implemented, done.** Folded the reified await into the SAME halt-latch machinery
  A-16 built for a failing top-level statement:
  - `crates/flux-lang/src/ast.rs`: added `FailureKind::Awaiting` (serializes `"awaiting"`) — the one
    `FailureKind` variant not classified from a `FlowError` (it is reified directly on hitting a
    top-level `await`), documented as such; deliberately excluded from `is_fatal()`'s match arm (so
    it defaults `false`) and never matched by the denial re-emission guard.
  - `crates/flux-lang/src/runtime.rs` (`run_top_level_resumable`'s `Node::Await` branch, ~line 1240):
    alongside the existing `RunEvent::Awaiting` audit event, now builds a `PlanHalt{kind: Awaiting,
    op: None, stmt: stmt_hash16(&body[i]), message: "plan paused: awaiting `{source}`"}`, appends
    `RunEvent::PlanHalted`, and returns it as `FlowOutcome.failure` (keeping `suspension` set too, as
    before). This opens the halt latch `ResumeLedger::fold`/`FlowStore::open_halted_plan` already
    fold over — no new store table, no new fold logic. The strict driver `run_top_level` (used by
    `execute_flow`/`resume_flow`) is untouched — a completely separate function.
  - `crates/flux-flow/src/loop_host.rs` (`halt_guidance`): added the required `FailureKind::Awaiting`
    match arm (the exhaustive match forced this) with kind-specific guidance: keep the prefix
    byte-identical, incorporate the now-available input, continue from the next step; an unchanged
    `await` at that position is a free pass-through in the ledger walk, so re-emitting it costs
    nothing.
  - No changes to `state.rs`'s `open_halted_plan`/`OpenHalt`, `resume_context_message`, the denial
    guard, or `prospective_skip_len` — all are already generic over `FailureKind`, so the new kind
    flows through them for free (`Awaiting` is simply not in the `Denied | ConfirmDenied` set).
  - **Why the fast-forward works even though the ledger treats `Node::Checkpoint | Node::Await` as
    a free pass-through:** the ledger-matched prefix (0..await_index-1) skips via normal hash
    matching; the await's own position is a free pass-through (matches unconditionally, whether or
    not the re-emitted plan repeats the exact same `await`); the walk then breaks at the first
    post-await statement (no ledger entry exists there — the original run never reached it), so
    execution resumes exactly at the first divergence. If the model's follow-up plan drops the
    `await` node entirely (info now in hand), the walk breaks one position earlier for the same
    reason and the new statement there runs fresh — same effect, no special-casing needed.
  - **Failing-first proof:** wrote `post_await_reemission_keeps_completed_prefix`
    (`crates/flux-flow/src/loop_host.rs`, after `unrelated_next_plan_consumes_latch_with_zero_skips`)
    against a `CountingTool` op standing in for the pre-await effectful statement. Verified it failed
    for the right reason BEFORE implementing: temporarily reverted the `ast.rs`/`runtime.rs` halt
    reification and the `loop_host.rs` guidance arm (test kept), reran — failed at the
    `!failure.is_null()` assertion with `"failure":null` in the round-1 output (today's actual bug:
    the reified await opens no latch), i.e. exactly the story's premise. Then restored the
    implementation and reran — green. Round 1: 1 dispatch of `counter`, latch opens (`failure.kind ==
    "awaiting"`, `fatal == false`). Round 2 (byte-identical prefix + await, plus a new post-await
    statement): `steps == 1` (only the new statement ran), `counter` dispatch count stays at 1 (never
    double-runs across the await), latch consumed (`open_halted_plan` → `None`).
  - **Acceptance #2 proof (pre-authored path untouched):** (a) `run_top_level` (the function
    `execute_flow`/`resume_flow` call) was never edited — only its sibling `run_top_level_resumable`
    was; (b) `crates/flux-app/src/app.rs` (the `flux app run` journey path) calls exclusively
    `execute_flow`/`execute_flow_with_composites`/`resume_flow`/`resume_flow_with_composites` — never
    the resumable variants — so this story's change is unreachable from there; (c)
    `crates/flux-flow/src/engine.rs`'s own `resume_suspended` (the interactive suspensions-table
    resume) calls `resume_flow_with_composites`, also strict-mode only; (d) existing tests stay green
    unmodified: `await_suspends_then_resumes_without_rerunning_the_prefix` (flux-lang, strict mode),
    `await_inside_a_plan_is_reified_not_a_turn_suspension` (flux-flow engine.rs, the pinned reified-
    await test — the engine's own suspension latch (`take_suspension`) is still `None` after the
    turn), `suspensions_round_trip_take_once_and_replace` (flux-flow state.rs).
  - **Gate:** `cargo build -p flux-lang -p flux-flow` clean; `cargo test -p flux-lang --all-targets`
    241+1+3+2 passed, 0 failed; `cargo test -p flux-flow --all-targets` 228+3+1 passed, 0 failed
    (227→228: the one new test); `cargo test -p flux-app --lib` 24 passed (downstream sanity, not
    required by the story's gate list but touches `FlowOutcome`); `cargo clippy -p flux-lang
    --all-targets -- -D warnings` clean; `cargo test -p flux-codegate` 4 passed; `cargo fmt` made no
    further changes to the touched files. **One deviation:** `cargo clippy -p flux-flow --all-targets
    -- -D warnings` currently fails on the WHOLE package — but the single reported error is at
    `crates/flux-flow/src/compile.rs:1597` (`needless_lifetimes` in `skeleton_as_call`), a function
    that is entirely new, uncommitted work already present in this working tree before this story
    started (confirmed via `git diff` — every line of that function is an addition) and out of this
    story's file boundary (compile.rs is explicitly L-23's, not to be touched). No other clippy
    diagnostic appears anywhere in the package; every file this story touched
    (`ast.rs`/`runtime.rs`/`loop_host.rs`) is clippy-clean.

## Notes
- Depends on L-22 + A-16. The cross-suspension-latch composition for authored flows stays deferred
  (evolution-impl-plan "Deferred (v1)").
- Current reified-await behavior pinned at `await_inside_a_plan_is_reified_not_a_turn_suspension`
  (`crates/flux-flow/src/engine.rs:1449-1491`) — still green, unmodified.
