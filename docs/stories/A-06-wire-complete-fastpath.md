---
id: A-06
title: Wire the dead `complete` fast-path — stop paying a full second planner round per turn
pillar: Agent
status: done
priority: 5
note: a completion-carrying plan that runs to success now ends the turn with ONE toolless catalog-less render instead of a full second planner round — armed only on success (rejection/error/suspension/identical-skip all fall through to normal re-planning), render usage counted, render failure never aborts the turn
---

# Wire the dead `complete` fast-path

## Goal
Reclaim the structural extra model round every turn pays. Verified 2026-07-02: `emit_plan`'s
optional `complete` directive is prompt-advertised (`crates/flux-flow/src/compile.rs:790-825`) and
parsed (`:314`, `parse_completion :477`), and `render_completion` (`:511`, a grounded no-tools
no-catalog render) is fully implemented — but has **zero callers**; the loop
(`assets/agent-loop.flux`) never reads `$plan.complete` (loop_host surfaces only a boolean,
`loop_host.rs:432-437`). So a working turn is always: planner round 1 (emit_plan) → run_plan →
planner round **2** (full catalog + conversation + symbols, uncached tail) just to say "done".
Serves vision:41 ("re-running … costs the fewest model calls") and per-turn cost/latency.

## Acceptance
- [x] **Failing-first:** `completion_directive_skips_the_second_planner_round` (flux-flow engine
      tests, request-recording provider) — queue `[emit_plan_complete(echo, "summarize"), prose]`;
      assert exactly 2 provider calls and the second has NO tools and NO "Operation catalog" in its
      system (fails today — it's a full planner round; the existing
      `plan_with_complete_renders_grounded_summary` cannot distinguish and keeps passing).
- [x] Mechanism (in the loop host via the existing `force_stop` seam — no loop-contract break for
      ejected loops): `run_plan` arms `pending_completion` ONLY after a successful, non-suspended
      run — every early return (plan-rejected, execute-error, suspension, identical-plan-skip)
      precedes arming, so a failed run never consumes the completion and the model sees feedback.
      The next `plan()` renders via `render_completion` and returns `{kind:"chat", text}`; the
      loop's existing `case "chat"` ends the turn.
- [x] `failing-first:` `failed_plan_run_does_not_consume_completion` and
      `identical_plan_skip_does_not_render_completion`.
- [x] `render_completion` returns `(String, Usage)`; the render call's usage lands in turn totals +
      `CallUsage` (today `_usage` is discarded at compile.rs:544). A render error falls through to
      a normal planner round (never abort the turn on a summary failure).
- [x] The plan value's `complete` field goes bool→object|null (clean cutover; the shipped loop and
      eject template never read it) — `crates/flux-flow/docs/ops-reference.md` updated.
- [x] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P5 of the round).
- Done 2026-07-02. `pending_completion: Mutex<Option<Completion>>` on `EngineLoopHost` (reset per
  turn in `set_turn`): `run_plan` re-parses the directive off the plan value (`parse_completion`,
  now pub(crate)) and arms it only after `execute_flow` succeeds without suspension; `plan()` takes
  it after conversation assembly and calls `render_completion` (toolless, catalog-less) — Ok(text)
  ends the turn as `{kind:"chat"}` with usage accumulated into turn totals + per-call attribution;
  an empty render still accounts its tokens then re-plans; a render error emits a
  `completion.render_error` observation and falls through to a normal planner round. The plan
  value's `complete` is now the full directive object (or null). 3 discriminating engine tests via
  a new request-capturing provider + a `boom` always-fails fixture op. Docs: ops-reference plan/
  run_plan rows + agent-loop.md `--show-loop` sample updated.

## Notes
- Net turn shape: plan + run + one grounded no-catalog render, replacing plan + run + full second
  planner round. Live A/B in C-16's verification (vs the A-05 baseline: $0.0137 / 2.1s rm task).
- Design decisions in `~/.claude/plans/wiggly-tumbling-salamander.md` §A-06.
