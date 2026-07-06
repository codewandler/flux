---
id: A-42
title: Stream plan-mode gather rounds to the live sink
pillar: Agent
status: done
epic: multipass-agent-loop
note: "A-18 deferred this: plan-mode gather dispatch runs through a NullSink because reborrowing `&mut dyn AgentSink` per round hits an NLL wall — the fix is the loop host's ChannelSink/drain-loop shape; UX only, the envelope is unaffected"
---

# Stream plan-mode gather rounds to the live sink

## Goal
A-18's plan-mode gather executes read-only plans through an internal `NullSink`
(`FlowEngine::compile_with_gather`, crates/flux-flow/src/engine.rs), so the user sees silence
while gather rounds run instead of the live op/spinner stream normal mode shows. The borrow
structure (one `&mut dyn AgentSink` reborrowed per round inside the loop) hits a hard NLL wall
(E0499/E0505/E0597); the loop host solved the same problem with a `ChannelSink` + drain loop.
Give plan mode the same shape so gather is visible.

## Acceptance
- [x] Plan-mode gather rounds stream ops/results to the caller's sink live (CLI `flux plan` and
      REPL `/plan` both show them), mirroring normal-mode rendering (A-15 labels).
- [x] No change to what executes (read-only gather, shared budget) — rendering only.
- [x] Failing-first test on the seam (a recording sink observes gather-round events).

## Progress
- 2026-07-06 filed — the scope-limiting deviation recorded in A-18's Progress.
- **2026-07-06 — implemented, gate green.**

  **Approach** (`crates/flux-flow/src/engine.rs`, `crates/flux-flow/src/loop_host.rs`): applied the
  exact ChannelSink/drain-loop shape `run_turn_cancellable` already uses (`engine.rs:172`), not a
  reborrow of the caller's `&mut dyn AgentSink`:
  - `FlowEngine::compile_with_gather` (`engine.rs:414`) now takes an owned, clonable
    `sink: Arc<std::sync::Mutex<dyn AgentSink>>` instead of running gather-round dispatch through an
    internal `NullSink`. Each round clones the `Arc` into a fresh `crate::loop_host::SharedSink` —
    never a reborrow of a captured outer reference, so the NLL wall A-18 hit (reborrowing the same
    `&mut dyn AgentSink` binding more than once per loop iteration, across the loop's back-edge)
    never arises: every "sink use" inside the loop is a brand-new owned value.
  - The round's read-only plan dispatch (`execute_flow_resumable_with_composites`) is now given
    `SharedSink::new(sink.clone())` (`engine.rs:518`) in place of `NullSink` — this is the whole fix:
    tool_call/tool_result stream live for every gather round's ops.
  - A-15 phase-label parity, "where applicable": each round also emits a `loop.phase` observation
    onto the sink before its planner call (`engine.rs:461`, mirroring
    `EngineLoopHost::plan`'s own observation in `loop_host.rs:919`) and brackets ONLY the
    `compile_turn` call with `crate::loop_host::PlanningGuard` (`engine.rs:471`, widened to
    `pub(crate)` in `loop_host.rs` for reuse) — the spinner stops the instant the model replies, so
    the round's own tool_call/tool_result progress renders next, exactly like normal-mode `plan()` →
    `run_plan()`. `thinking_sink` stays `None` (unchanged from A-18): threading it would also flip
    `Request.thinking` on in `compile_turn_inner` (`compile.rs:535`), a real behavioral change this
    rendering-only story must not make.
  - `FlowEngine::compile_once` (`engine.rs:328`, the one-shot `flux plan` seam) gained a
    `sink: &mut dyn AgentSink` parameter — it previously had NONE, so `flux plan` rendered gather
    rounds nowhere at all. It now builds an `mpsc` channel + `ChannelSink`, hands the channel's
    `Arc<Mutex<dyn AgentSink>>` half into `compile_with_gather`, and drains the receiver onto the
    caller's borrowed `sink` in a `tokio::select!` loop racing the compile future — byte-for-byte the
    same shape `run_turn_cancellable` uses (`engine.rs:256`).
  - `FlowEngine::plan_turn` (`engine.rs:575`, the REPL `/plan` seam) already had a sink; its single
    `sink.planning(true)`/`planning(false)` bracket around the WHOLE phased sequence was removed
    (planning is now bracketed per round, inside `compile_with_gather`) and its `cancel`-racing
    `tokio::select!` was widened into the same three-way loop (`cancel` / `rx.recv()` / the compile
    future) as `compile_once`, so Ctrl-C still drops the in-flight round promptly.
  - `crates/flux-cli/src/main.rs`'s one CLI call site (`run_plan`, ~line 2373) now builds a
    `CliSink::new(0)` before calling `compile_once` and passes it — no cost table needed, since
    `compile_once` never calls `turn_end` on it (a compile, not a billed turn).

  **What did NOT change**: the gather round still runs through the SAME
  `execute_flow_resumable_with_composites` call, the SAME `compile_turn`/A-13 `gather_violation`
  read-only gate, and the SAME `compile::GATHER_ROUND_BUDGET` — only which sink its events reach
  changed. No new evidence is written (the `loop.phase` observation goes to the sink only, not
  `executor.observe`, since the phased Rust loop isn't the reflexive `plan()` op and this story is
  rendering-only).

  **Tests** (`crates/flux-flow/src/engine.rs`, `#[tokio::test]`):
  - `compile_once_streams_gather_round_ops_to_the_live_sink` (new) — a `CollectSink` (extended with
    a `results: Vec<String>` field + `tool_result` override) observes `["echo"]` in both `tools` and
    `results` after a gather round runs through `compile_once`.
  - `plan_turn_streams_gather_round_ops_to_the_live_sink` (new) — the same contract through the REPL
    `/plan` seam (`plan_turn`).
  - The 4 pre-existing `compile_once_*` tests (`compile_once_stays_single_shot_when_orient_settles_immediately`,
    `compile_once_runs_gather_then_shows_the_final_plan_unexecuted`,
    `compile_once_bounds_gather_to_the_shared_round_budget`,
    `compile_once_rejects_a_mutating_gather_plan_via_the_same_a13_gate`) needed the unavoidable
    signature adjustment (an added `&mut sink` argument, per the story's own allowance) but their
    assertions and behavior are byte-identical to before.

  **Failing-first proof**: implemented the fix, then did a scratch swap of the exact fix line
  (`engine.rs`'s gather-round `SharedSink::new(sink.clone())` reverted to `NullSink`) and reran the
  two new tests — both failed for the right reason (`left: [] right: ["echo"]` — the sink observed
  nothing) — then restored the real line and reran to green. Same scratch-swap technique A-18's
  Progress entry used.

  **Gate** (package-scoped, per the story's instructions):
  - `cargo build -p flux-flow -p flux-cli` — clean.
  - `cargo test -p flux-flow` — 227 lib + 3 + 1 passed (225 pre-existing + 2 new), 0 failed.
  - `cargo test -p flux-cli` — 85 passed, 0 failed (unchanged from A-18's baseline).
  - `cargo clippy -p flux-flow -p flux-cli --all-targets -- -D warnings` — clean (needed
    `#[allow(clippy::too_many_arguments)]` on `compile_with_gather`, now 8 params).
  - `cargo fmt -p flux-flow -p flux-cli -- --check` — clean (applied `cargo fmt` once, then reverified).
  - `cargo test -p flux-codegate` — 4 passed (layering unaffected; no new crate deps).
  - `git status --short` after the gate shows only `crates/flux-cli/src/main.rs`,
    `crates/flux-flow/src/{engine,loop_host}.rs`, and this story file changed by this work.
    (`crates/flux-flow/src/compile.rs`, `docs/designs/multipass-agent-loop.md`,
    `docs/stories/A-40-plan-emission-token-ceiling.md`, `docs/stories/README.md` — dirty at session
    start per another concurrent workstream — were committed by that workstream during this session,
    per `git log`: `01c1e06`/`9cdd39a`/`c5943b5` landed on top of A-18's `4491d87` while this story
    was in progress; none of them were touched here.)

## Notes
- See A-18's Progress entry for the exact NLL shape attempted (loop + Box::pin recursion both
  failed); the ChannelSink/drain-loop pattern in `loop_host.rs` is the known-good architecture.
