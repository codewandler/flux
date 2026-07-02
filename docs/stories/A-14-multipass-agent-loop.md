---
id: A-14
title: Multi-pass agent-loop.flux — orient → bounded gather → execute, with host threading
pillar: Agent
status: done
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
- [x] New `agent-loop.flux` ships as THE loop (no flags). A trivial prompt (chat) and a simple
      actionable prompt (full plan) make exactly as many provider calls as today —
      `orient_fast_path_adds_no_provider_calls` (mock provider call-count assertion).
- [x] A gather-tagged plan sets `settled: ""` and routes through the gather loop; results bind as
      ordinary FlowStore symbols; `turn.gather` observations emitted —
      `gather_pass_binds_symbols_and_observes`.
- [x] The brief is host-carried per turn (reset in `set_turn`) and prepended to every subsequent
      planner feedback message — `brief_prepended_to_followup_plan_calls`.
- [x] Budget exhaustion degrades gracefully: a leftover gather plan runs as the first execute
      iteration (`gather_budget_exhaustion_degrades_to_execute`).
- [x] `plan` op (`crates/flux-tools/src/reflect.rs` PlanInput + `loop_host.rs`) accepts `phase`;
      phase-less calls behave as `execute` (old ejected loops byte-compatible — loop round-trip
      test updated, `engine.rs:1239-1253`); `flux loop eject` emits the new text.
- [x] `PlanAttempt` (flux-events) gains `phase`; C-15 projections keep summing.
- [x] Docs truth: `docs/agent-loop.md` (listing + `--show-loop` sample), `docs/usage.md`,
      `crates/flux-flow/docs/ops-reference.md` updated.
- [x] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)
- 2026-07-02: Implemented end-to-end. `agent-loop.flux` restructured into orient / `unless $settled
  { repeat 3 until $settled { … } }` gather / `repeat 25 until $done` execute (see Notes for the
  `unless` deviation). `Phase::from_wire` (compile.rs) parses the wire string; `EngineLoopHost::plan`
  resolves it at entry, emits a `loop.phase` observation, threads it into `compile_turn`, computes
  `settled` (`""` only for an accepted `gather: true` plan), stores/observes (`flow.brief`) the brief
  the moment it's accepted, and prepends it to every later feedback message via a new `brief:
  Mutex<Option<Brief>>` field (reset in `set_turn`). `PlanAttempt`/`EventKind::PlanAttempted` gained
  `phase` (serde-default); populated at every `record_plan_attempt` call site in `loop_host.rs`
  including `run_plan`'s "rejected" outcome (via a `last_phase` field). `PlanInput` (reflect.rs) gained
  `phase`. Added 6 tests in `loop_host.rs` (`orient_fast_path_adds_no_provider_calls`,
  `gather_pass_binds_symbols_and_observes`, `brief_prepended_to_followup_plan_calls`,
  `gather_budget_exhaustion_degrades_to_execute`, `phase_less_plan_call_behaves_as_execute`,
  `old_ejected_loop_text_still_runs`) + 1 in `engine.rs`
  (`plan_attempts_are_phase_stamped_across_the_multipass_loop`); all pre-existing tests (148 in
  flux-flow, incl. the loop round-trip test at `engine.rs`) pass unchanged, confirming call-count
  parity with the pre-phased loop. Docs updated: `docs/agent-loop.md`, `docs/usage.md`,
  `crates/flux-flow/docs/ops-reference.md`. Gate green: `cargo build/test/clippy -D warnings` for
  `flux-flow`/`flux-events`/`flux-tools` (+ `flux-cli` build/test as a downstream sanity check),
  `cargo fmt --all`.

## Notes
- Depends on A-13 (protocol). A-15 renders what this emits.
- The completion fast-path (A-06) is untouched: a successful `complete`-carrying plan makes the next
  `plan()` return chat regardless of phase.
- **Deviation from the literal design sketch:** `repeat … until $cond` is a *post-check* guard in the
  flux-lang runtime (the body always runs at least once; `until` is evaluated after each iteration,
  not before) — a bare `repeat 3 until $settled { run_plan… }` for Pass 2 would therefore always
  execute one gather round even when orient already settled, breaking the "zero added latency"
  requirement. Fixed by wrapping Pass 2 in `unless $settled { repeat 3 until $settled { … } }`: the
  `unless` gate (a plain conditional, checked once) skips the whole gather block when orient already
  settled, and once inside, the `repeat`'s post-check semantics are exactly right (a gather round
  always needs to run before checking whether the *new* plan settled). Verified by the passing
  `orient_fast_path_adds_no_provider_calls` test and unaffected by all 148 pre-existing tests.
- `settled` is carried on the wire `Plan` JSON as a string (`""`/`"true"`), matching the file's
  existing `$done = fmt("true")` idiom and the design doc's literal comment, rather than a native JSON
  boolean.
