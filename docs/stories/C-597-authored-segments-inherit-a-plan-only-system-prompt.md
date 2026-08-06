---
id: C-597
title: "Stop telling an authored implementation segment that its actions are only captured"
pillar: Core
status: ready
priority: 10
areas: [flux-flow]
note: "EXPLORE_SYSTEM is a plan-then-hand-off contract; Fleet story workers inherit it verbatim and behave accordingly"
---

# Stop telling an authored implementation segment that its actions are only captured

## Goal

Give an operator-authored `ai_segment` a system prompt that matches its real execution semantics. A
Fleet story worker exists to write, test and commit, but it currently inherits the adaptive planner's
prompt, which tells it the opposite — and it complies.

## Acceptance

- [ ] Failing first, a test proves an authored segment's system segments do not instruct the model
      that its actions may only be captured, and do not direct it to `finalize_plan` in place of
      acting, while an ordinary adaptive turn's prompt is unchanged.
- [ ] The authored-segment prompt states the actual contract: effectful calls are executed through
      the approve/execute batch path, their results arrive on the following round, and the segment is
      expected to complete its assignment rather than hand a plan back.
- [ ] `explore_segments` selects the prompt by `ctx.authored_ceiling.is_some()`, the same signal
      `adaptive_explore` already uses for `authored_segment`.
- [ ] A Fleet story worker dispatched on the implementation profile attempts at least one effectful
      operation for a story whose Acceptance requires code.

## Progress

- Diagnosed, not fixed.

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
