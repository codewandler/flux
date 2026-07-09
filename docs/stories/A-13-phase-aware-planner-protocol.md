---
id: A-13
title: Phase-aware planner protocol — emit_plan gather/brief + compile_turn phase segments
pillar: Agent
status: done
priority:
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: the protocol half of the phased loop — gather plans are compile-enforced read-only (effect metadata incl. transitive composites), ~12-node cap, gather-tag rejected once the budget is spent
---

# Phase-aware planner protocol

## Goal
Teach the planning path phases: `compile_turn` gains a `phase` parameter selecting a per-phase
instruction segment, and `emit_plan` (`EmitPlanInput`) gains optional `gather: bool` and
`brief: {goal, needs[]}` fields — the carrier for orient/gather semantics
(design: Part 1 normative semantics).

## Acceptance
- [x] `EmitPlanInput` parses `gather`/`brief` like `complete` (`crates/flux-flow/src/compile.rs`,
      tool-input branch ~410-469); both surface on the returned `TurnOutput::Plan`.
- [x] A `gather: true` plan is validated read-only: every called op — composites included, via the
      registry's transitive effect metadata (`crates/flux-flow/src/registry.rs:211-307`) — must be
      effect-clean of write/destructive intent; violations are repair feedback (same shape as
      hidden-op rejection). Failing-first test: `mutating_gather_plan_is_repair_feedback`.
- [x] Gather plans are capped (~12 call nodes); oversize is repair feedback
      (`oversize_gather_plan_is_rejected`).
- [x] In `phase: "execute"`, a `gather: true` emission is rejected with repair feedback
      ("gather budget spent") — `gather_tag_rejected_in_execute_phase`.
- [x] Per-phase instruction segments are separate, byte-stable cached segments appended after
      segment A (A-03 discipline holds — assert segment A bytes unchanged across phases:
      `phase_segments_do_not_perturb_segment_a`). The "WHOLE task in one plan" instruction
      (`compile.rs:951`) is rescoped to the execution plan.
- [x] Phase-less calls compile with `phase: "execute"` semantics, byte-compatible with today
      (existing compile tests unchanged).
- [x] Gate green.

## Progress
- 2026-07-02: shipped. `crates/flux-flow/src/compile.rs`: `EmitPlanInput`/`EmitPlanTextInput` gain
  optional `gather: Option<bool>` + `brief: Option<EmitPlanBriefInput>` (new struct, `goal`/`needs`),
  parsed tolerantly via new `parse_gather`/`parse_brief` (mirroring `parse_completion`) right where
  `complete` is parsed in the `emit_plan` tool-input branch. `Compiled` gains `gather: bool` +
  `brief: Option<Brief>` (new public `Brief{goal, needs}` type), populated on acceptance and `false`/
  `None` for the one-shot `compile()` path and the plain-text plan fallback (neither has a wrapping
  JSON object to read `gather`/`brief` off). Added a public `Phase` enum (`Orient`/`Gather`/
  `Execute`, `Execute` is `#[default]`) as a new trailing parameter on `compile_turn`/
  `compile_turn_with_arm`/`assemble_system_segments`; updated the 4 real call sites
  (`loop_host.rs::EngineLoopHost::plan`, `engine.rs::compile_once`, `engine.rs::plan_turn`,
  `flux-eval/tests/emission_ab.rs::run_arm`) to pass `Phase::Execute` — mechanical, byte-compatible
  for now (A-14 threads the real phases through the loop host). Segment layout: a NEW cached
  `phase_contract(phase)` segment is inserted directly after segment A (`build_planner_prompt`,
  untouched/byte-stable across phases) and before segment B/C — so every phase shares A's cache
  prefix per A-03. The "Put the WHOLE task in one plan" sentence was removed from segment A and
  rescoped into the phase segment (worded "one execution plan", present for `Orient`'s branch-2 and
  `Execute`; `Gather`'s segment instead says to keep gathering or settle). Gather enforcement lives
  in two new pieces: `OpRegistry::mutating_ops_in` (`registry.rs`, mirrors `hidden_ops_in`'s shape) —
  walks every `Node::Call` via `for_each_node` and flags an op whose signature has `Risk::Destructive`
  or `Effect::Write` (a composite's own declared, `analyze_composites`-validated effects/risk stand in
  for its body, so no separate expansion is needed — this is what makes the check transitive through
  composites); and `gather_violation`/`count_call_nodes`/`GATHER_NODE_CAP` (12) in `compile.rs`,
  called from the `emit_plan` branch right after the hidden-ops check whenever `gather == true` — a
  violation is repair feedback (same C-17 shape as the hidden-op rejection), and `gather: true` in the
  execute phase is rejected outright via a dedicated constant message before the effect/cap check even
  runs. Deliberately did NOT treat every non-`Read` effect as disqualifying (`Effect::Process` alone,
  e.g. `git_status`/`git_diff`/`git_log`, all `Risk::Low`, stays gather-eligible) — narrowed to
  `Write`/`Destructive` specifically after checking the real op catalog found a Low-risk `append` op
  that DOES declare `Effect::Write` (so risk-only would miss it) and several genuinely read-only
  `git_*` ops that only declare `Effect::Process` (so "any non-Read effect" would wrongly reject them).
  New tests (all green) in `compile.rs`'s test module: `mutating_ops_in_flags_write_effect_and_composite_transitively`
  (registry-level unit test, incl. the `git_status` non-disqualification check),
  `mutating_gather_plan_is_repair_feedback`, `gather_plan_calling_a_mutating_composite_is_rejected_transitively`,
  `oversize_gather_plan_is_rejected`, `gather_tag_rejected_in_execute_phase`,
  `gather_tag_in_execute_phase_is_rejected_even_on_the_final_step`,
  `clean_gather_plan_is_accepted_in_orient_phase`, `clean_gather_plan_is_accepted_in_gather_phase`,
  `emit_plan_captures_optional_brief`, `phase_segments_do_not_perturb_segment_a` (the story's named
  segment-A byte-compare), `execute_phase_is_byte_compatible_with_the_pre_phase_prompt`. Updated the
  pre-existing `system_segments_keep_the_static_prefix_stable_across_symbol_changes` test for the new
  4-segment layout (previously 3) — all previously-green tests confirmed still passing verbatim after
  adding a mandatory `Phase` argument at every existing call site (139 → 148 tests in `flux-flow`'s
  lib test binary).
  Gate (package-scoped, per the orchestrator's instruction for this parallel story):
  `cargo build/test/clippy -D warnings` clean for `flux-flow`, `flux-cli`, and `flux-eval` (the
  hermetic `corpus_is_valid` test; the live `emission_ab_live` stays `#[ignore]`d), plus a plain
  `cargo build` sanity check on `flux-sdk`/`flux-orchestrate`/`flux-app`; `cargo fmt -p flux-flow -p
  flux-eval` (scoped, since another agent has concurrent uncommitted work in `crates/flux-lang`).
  The orchestrator runs the full workspace gate (incl. `flux-codegate`) afterward.

## Notes
- C-17 gates untouched and still run on every phase's every step.
- A-14 consumes this; independent of the L-22 resume track.
