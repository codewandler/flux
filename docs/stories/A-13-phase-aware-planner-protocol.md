---
id: A-13
title: Phase-aware planner protocol — emit_plan gather/brief + compile_turn phase segments
pillar: Agent
status: ready
priority: 3
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
- [ ] `EmitPlanInput` parses `gather`/`brief` like `complete` (`crates/flux-flow/src/compile.rs`,
      tool-input branch ~410-469); both surface on the returned `TurnOutput::Plan`.
- [ ] A `gather: true` plan is validated read-only: every called op — composites included, via the
      registry's transitive effect metadata (`crates/flux-flow/src/registry.rs:211-307`) — must be
      effect-clean of write/destructive intent; violations are repair feedback (same shape as
      hidden-op rejection). Failing-first test: `mutating_gather_plan_is_repair_feedback`.
- [ ] Gather plans are capped (~12 call nodes); oversize is repair feedback
      (`oversize_gather_plan_is_rejected`).
- [ ] In `phase: "execute"`, a `gather: true` emission is rejected with repair feedback
      ("gather budget spent") — `gather_tag_rejected_in_execute_phase`.
- [ ] Per-phase instruction segments are separate, byte-stable cached segments appended after
      segment A (A-03 discipline holds — assert segment A bytes unchanged across phases:
      `phase_segments_do_not_perturb_segment_a`). The "WHOLE task in one plan" instruction
      (`compile.rs:951`) is rescoped to the execution plan.
- [ ] Phase-less calls compile with `phase: "execute"` semantics, byte-compatible with today
      (existing compile tests unchanged).
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- C-17 gates untouched and still run on every phase's every step.
- A-14 consumes this; independent of the L-22 resume track.
