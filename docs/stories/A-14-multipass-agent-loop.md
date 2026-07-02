---
id: A-14
title: Multi-pass agent-loop.flux — orient → bounded gather → execute, with host threading
pillar: Agent
status: ready
priority: 4
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: the loop text itself becomes phased (orient / repeat-3 gather / repeat-25 execute); plan op gains phase; brief is host-carried per turn; PlanAttempt gains phase; clean cutover — no flags
---

# Multi-pass agent-loop.flux

## Goal
Restructure `crates/flux-flow/assets/agent-loop.flux` into the phased loop (design Part 1 sketch):
one orient `plan()` call (three-way contract — chat / full plan / gather+brief), a `repeat 3`
read-only gather loop gated on `$plan.settled`, then the standard `repeat 25` execute loop. The
`plan` op gains a `phase` argument; the host threads brief/settled and stamps phase into the audit
trail.

## Acceptance
- [ ] New `agent-loop.flux` ships as THE loop (no flags). A trivial prompt (chat) and a simple
      actionable prompt (full plan) make exactly as many provider calls as today —
      `orient_fast_path_adds_no_provider_calls` (mock provider call-count assertion).
- [ ] A gather-tagged plan sets `settled: ""` and routes through the gather loop; results bind as
      ordinary FlowStore symbols; `turn.gather` observations emitted —
      `gather_pass_binds_symbols_and_observes`.
- [ ] The brief is host-carried per turn (reset in `set_turn`) and prepended to every subsequent
      planner feedback message — `brief_prepended_to_followup_plan_calls`.
- [ ] Budget exhaustion degrades gracefully: a leftover gather plan runs as the first execute
      iteration (`gather_budget_exhaustion_degrades_to_execute`).
- [ ] `plan` op (`crates/flux-tools/src/reflect.rs` PlanInput + `loop_host.rs`) accepts `phase`;
      phase-less calls behave as `execute` (old ejected loops byte-compatible — loop round-trip
      test updated, `engine.rs:1239-1253`); `flux loop eject` emits the new text.
- [ ] `PlanAttempt` (flux-events) gains `phase`; C-15 projections keep summing.
- [ ] Docs truth: `docs/agent-loop.md` (listing + `--show-loop` sample), `docs/usage.md`,
      `crates/flux-flow/docs/ops-reference.md` updated.
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- Depends on A-13 (protocol). A-15 renders what this emits.
- The completion fast-path (A-06) is untouched: a successful `complete`-carrying plan makes the next
  `plan()` return chat regardless of phase.
