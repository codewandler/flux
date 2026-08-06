---
id: C-597
title: "Stop telling an authored implementation segment that its actions are only captured"
pillar: Core
status: done
areas: [flux-flow]
note: "EXPLORE_SYSTEM is a plan-then-hand-off contract; Fleet story workers inherit it verbatim and behave accordingly"
---

# Stop telling an authored implementation segment that its actions are only captured

## Goal

Give an operator-authored `ai_segment` a system prompt that matches its real execution semantics. A
Fleet story worker exists to write, test and commit, but it currently inherits the adaptive planner's
prompt, which tells it the opposite — and it complies.

## Acceptance

- [x] Failing first, `an_authored_segment_is_told_its_effects_execute_not_that_they_are_captured`
      (`crates/flux-flow/src/staged.rs`) proves the authored contract omits every piece of the
      planner's deferral language ("may capture an action instead of executing it", "call
      finalize_plan", "Never claim a captured action already happened"), states that effects execute,
      keeps the evidence discipline verbatim, and leaves the adaptive prompt unchanged.
- [x] `SEGMENT_SYSTEM` states the actual contract: effectful calls are performed against the real
      workspace, their results arrive on the following round, nothing is deferred to a later approval
      step, and the segment carries its assignment to completion rather than handing back a plan.
- [x] `explore_segments` selects on `ctx.authored_ceiling.is_some()` — the same signal
      `adaptive_explore` uses for `authored_segment`.
- [x] The contract names `finalize_plan` and describes staging, so a worker can actually convert
      staged work into execution. Asserted directly by the test above.
- [ ] A Fleet story worker dispatched on the implementation profile produces a commit for a story
      whose Acceptance requires code. *(Pending the end-to-end wave.)*

## Progress

- Implemented, then **corrected** — the first version made things worse and the correction matters
  more than the original fix.
- v1 asserted that effects "REALLY EXECUTE" and told the model *"nothing is deferred to a later
  approval step … do not hand back a plan"*. That is false. `adaptive_explore` captures every
  non-gather-safe call into `state.proposed` with the tool result `captured as proposed action N;
  not executed`, and only `FINALIZE_PLAN` converts the batch into real execution via
  `approve_batch`/`execute_batch` in `EngineLoopHost::run_scoped_segment`.
- Consequence on Fleet wave-275: the worker made 68 calls, attempted `bash` four times, staged its
  writes, never called `finalize_plan`, and finished having changed nothing — strictly worse than the
  planner prompt it replaced, which at least taught the ritual. The worker's own report named the
  cause exactly: *"This runtime captured my write/edit/cargo/commit calls as approval-gated proposed
  actions rather than executing them."*
- v1's test passed because it asserted only the **absence** of planner language. Absence of the wrong
  contract is not presence of a working one. The test now asserts the ritual is present:
  `finalize_plan`, the word `STAGED`, and the literal `captured as proposed action` string the model
  will encounter.
- Evidence discipline is copied across unchanged in both versions, so neither can be mistaken for
  loosening grounding requirements.

## Notes

- `explore_segments` (`crates/flux-flow/src/staged.rs`) pushes `EXPLORE_SYSTEM` unconditionally, so an
  authored segment gets the planner contract verbatim:

  > "Flux may capture an action instead of executing it; the tool result will say so."
  > "If actions are captured, call `finalize_plan` by itself with instructions for the post-execution
  > answer. Never claim a captured action already happened."
  > "Batch only independent gather calls; never batch writes, destructive work, or calls that need
  > approval."

  That is correct for the adaptive CLI path, where a plan is surfaced for approval. It is wrong for a
  Fleet worker: `EngineLoopHost::run_scoped_segment` handles a `batch` step by calling
  `approve_batch` and then `execute_batch` (`loop_host.rs`), so effects really do execute.
- Live evidence, Fleet wave-263, `flux/C-542`, `claude/opus`, 22 tool calls — **`glob` ×4, `read` ×5,
  `grep` ×13, and not one effectful call attempted**. The worker then returned a question whose first
  clause is the prompt talking back:

  > "this session captures actions for approval instead of executing them, so I cannot compile,
  > observe a failing-first test, or run the gate before the commit is frozen"

  It had `write`, `edit`, `patch`, `bash`, `proc.run`, `git_stage` and `git_commit` in its admitted
  ceiling the entire time.
- This is a discouragement, not a hard block, which matches the observed inconsistency: in wave-257
  one of four workers (`flux/C-562`) pushed through and committed 661 insertions, one wrote 80 lines
  and never committed (`exchange/X-138`), and two produced nothing.
- Pairs with **C-570** (backlog): nothing yet *requires* a worker to signal terminal delivery, so a
  worker that plans instead of implementing still reports success. Fixing the prompt raises the
  delivery rate; C-570 makes the failure detectable.
