---
id: A-18
title: Multi-pass plan mode — read-only gather inside flux plan / REPL /plan
pillar: Agent
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: plan mode stays single-shot in the epic MVP; this brings gather to it — gather plans auto-run (non-mutating, same trust run_plan already grants), then the final execution plan is shown for approval
---

# Multi-pass plan mode

## Goal
Plan mode's contract is "show me the full plan before anything runs", which today forces the model
to plan blind (docs/usage.md: "Plan mode is single-shot per turn"). Let `flux plan` / REPL `/plan`
run read-only gather plans automatically (they are non-mutating — the same trust level `run_plan`
already grants without approval), then present the grounded final execution plan for approval.

## Acceptance
- [x] `compile_once`/`plan_turn` accept the orient/gather contract; gather plans execute (read-only
      enforced per A-13), the final execution plan is shown and NOT run.
- [x] Piped/`-o json|yaml` behavior stays print-and-exit (with gather having run).
- [x] docs/usage.md plan-mode section updated (the "single-shot" caveat retired).
- [x] Gate green.

## Progress

**2026-07-06 — implemented, gate green.**

**Seams found/changed** (`crates/flux-flow/src/engine.rs`):
- `FlowEngine::compile_once` (`engine.rs:324`) and `FlowEngine::plan_turn` (`engine.rs:523`) — the
  two real call sites named in the story (`compile_once`/`plan_turn`, not `plan_turn`'s inner
  `compile_turn` call directly) — now delegate to a new shared helper,
  `FlowEngine::compile_with_gather` (`engine.rs:388`), instead of calling `compile_turn` once with
  `Phase::Execute`.
- `compile_with_gather` drives the SAME phase state machine A-13/A-14 already ships in
  `agent-loop.flux`: `Phase::Orient` → (if `compiled.gather`) execute the plan
  (`execute_flow_resumable_with_composites`, read-only enforced already inside `compile_turn` by
  A-13's `gather_violation`) → feed `outcome.transcript` back as the next round's feedback message,
  brief-prefixed via the (now `pub(crate)`) `loop_host::format_brief`/`cap_loop_feedback` — → repeat
  up to `compile::GATHER_ROUND_BUDGET` (new `pub(crate) const = 3`, `compile.rs:1537`) times. Once
  the model settles (an ordinary plan or chat) OR the budget is spent, the SETTLED `TurnOutput` is
  returned; it is **never executed** by this function — the existing `flux plan`/`/run` approval
  flow in `main.rs` (unchanged) is what may run it later.

**Bounding gather**: no second budget was invented. `compile::GATHER_ROUND_BUDGET = 3` is the ONE
constant both surfaces read; a new test
(`engine::tests::agent_loop_flux_gather_budget_matches_the_shared_constant`) asserts
`agent-loop.flux`'s literal `repeat 3` (normal mode's Pass 2) still matches it, so the two surfaces
cannot silently drift apart.

**Deliberate deviation from normal mode's graceful degradation** (documented in
`compile_with_gather`'s doc comment): if the budget is spent while the model still tags
`gather: true`, normal mode folds that leftover plan into the very next execute-loop round (it just
runs, since `run_plan` doesn't care about the tag). Plan mode does NOT do this — the leftover plan is
discarded unrun and the next call is forced into `Phase::Execute` (which rejects `gather: true`
inside `compile_turn`'s own repair loop) to compel a real settlement. This makes plan mode's
guarantee crisper ("only gather ever auto-runs, capped at exactly `GATHER_ROUND_BUDGET` rounds") at
the cost of one extra planner call in the rare case the model won't stop gathering. Covered by
`engine::tests::compile_once_bounds_gather_to_the_shared_round_budget`.

**Scope-limiting deviation (UX only, not safety)**: gather-round execution does not stream through
the caller's live `AgentSink` — `compile_with_gather` uses an internal `NullSink` for the dispatch
call, and `sink.planning(true/false)` is bracketed once around the whole phased sequence at the two
call sites instead of per-round. Reason: `sink: &mut dyn AgentSink` is a plain, non-`'static`
borrowed reference (unlike the loop host's clonable `Arc<Mutex<dyn AgentSink>>`), and reborrowing it
more than once per loop iteration (once for `compile_turn`'s thinking-sink, again for the gather
dispatch) hits a well-known NLL limitation — `rustc` unifies the reborrow's lifetime with the
reference's own declared lifetime across the loop's back-edge (E0499), even though each reborrow is
used and dropped before the next is taken; a `Box::pin` recursive-call restructuring was also tried
and hits the same wall via a different route (`&mut dyn AgentSink`'s trait-object lifetime bound
can't shrink across a captured `async move` state machine either — E0505/E0597). Fixing this properly
would mean adapting plan mode onto the loop host's `ChannelSink`/drain-loop architecture
(`run_turn_cancellable`'s pattern) — a materially bigger change than this story's scope. Net effect:
`/plan`'s gather rounds run silently (no live "reading…" stream); the settled plan renders exactly as
before. Filing a follow-up story is possible if this UX gap bites in practice, but it is not a
correctness or safety issue.

**Safety review (explicitly requested)**: no new hole found. The read-only guarantee is enforced
entirely inside `compile_turn`'s existing `gather_violation`/`OpRegistry::mutating_ops_in` gate
(A-13, unchanged) — `Compiled.gather` can only be `true` if that gate already accepted the plan as
effect-clean (no `Write`/`Network`/`Browser`/`LocalSystem` effect, not `Risk::Destructive`) and
within the 12-call-node cap, so `compile_with_gather` executing every `gather: true` plan it receives
inherits that invariant by construction rather than re-implementing it. Verified through this NEW
seam (not just at the compiler level, where A-13 already covered it) by
`engine::tests::compile_once_rejects_a_mutating_gather_plan_via_the_same_a13_gate`, which registers a
real mutating tool (`Effect::Write`) and asserts its dispatch log stays empty even when the model's
first attempt tags it `gather: true`. No approval-prompt risk either: gather plans are `Risk::Low`
and non-mutating by construction, so `Executor::dispatch`'s per-op policy auto-approves them with no
prompt — identical to normal mode's gather (already covered by the pre-existing
`run_plan_skips_approval_for_a_read_only_plan`), not a new code path.

**Also changed**: `FlowEngine::advertised_registry` (the old helper `compile_once`/`plan_turn` used
for a single evidence-gated `OpRegistry` build) is now dead — both callers build their `advertised`
`HashSet` via `surfaced_op_names`/`surfaced_for_turn` once per turn instead (mirroring
`run_turn_cancellable`'s own pattern) and `compile_with_gather` rebuilds the `OpRegistry` itself each
round from that fixed set — so it was removed rather than left as `#[allow(dead_code)]`.

**Tests** (`crates/flux-flow/src/engine.rs`, all `#[tokio::test]` unless noted):
- `agent_loop_flux_gather_budget_matches_the_shared_constant` (`#[test]`) — drift guard for the
  shared budget constant.
- `compile_once_stays_single_shot_when_orient_settles_immediately` — single-shot behavior unchanged:
  exactly one provider call (via `CaptureProvider`) when the model settles on the full plan directly.
- `compile_once_runs_gather_then_shows_the_final_plan_unexecuted` — gather auto-executes (its symbol
  lands in the session), the settled plan is returned, and the settled plan's own op never ran.
- `plan_turn_runs_gather_then_returns_only_the_settled_plan` — same contract through the REPL `/plan`
  seam, plus the persisted-message shape.
- `compile_once_bounds_gather_to_the_shared_round_budget` — exactly `GATHER_ROUND_BUDGET` (3) gather
  plans execute; a 4th gather-tagged plan past the budget never runs.
- `compile_once_rejects_a_mutating_gather_plan_via_the_same_a13_gate` — the safety test above.

**Failing-first proof**: all 4 new behavioral tests (excluding the single-shot and drift tests, which
must hold both before and after) were run against the pre-A-18 `engine.rs` (git `HEAD`, `compile_once`/
`plan_turn` always calling `compile_turn` with `Phase::Execute`) via a scratch swap — all 4 failed for
the right reason (`gather round … executed` / `the gather round executed automatically` / `the
corrected, effect-clean gather plan ran instead` assertions failed because gather never ran), then
passed again once the real implementation was restored.

**Gate** (package-scoped, per the story's instructions):
- `cargo build -p flux-flow -p flux-cli` — clean.
- `cargo test -p flux-flow` — 225+3+1 passed (lib + `gather_effect_gate` + `skill_docs_in_sync`
  integration tests), 0 failed.
- `cargo test -p flux-cli` — 85 passed, 0 failed.
- `cargo clippy -p flux-flow -p flux-cli --all-targets -- -D warnings` — clean.
- `cargo fmt -p flux-flow -p flux-cli -- --check` — clean.
- `cargo test -p flux-codegate` — 4 passed (layering unaffected; no new crate deps).
- `git status --short` after the gate shows only `crates/flux-flow/src/{compile,engine,loop_host}.rs`,
  `docs/usage.md`, and this story file changed by this work — `docs/stories/README.md` and
  `examples/improve-{synthetic,tbench}.flux` were already modified before this session started (or by
  concurrent work) and were left untouched, per the file-boundary instructions.

## Notes
- Depends on A-13/A-14 shipping and proving the gather contract in normal mode first.
