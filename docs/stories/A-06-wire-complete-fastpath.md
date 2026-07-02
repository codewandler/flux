---
id: A-06
title: Wire the dead `complete` fast-path — stop paying a full second planner round per turn
pillar: Agent
status: ready
priority: 5
note: render_completion has ZERO callers and the loop never reads $plan.complete — the prompt instructs models to attach a completion directive that is pure dead weight, and every successful turn ends with a full planner round (catalog + conversation + uncached symbols) just to produce closing prose
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
- [ ] **Failing-first:** `completion_directive_skips_the_second_planner_round` (flux-flow engine
      tests, request-recording provider) — queue `[emit_plan_complete(echo, "summarize"), prose]`;
      assert exactly 2 provider calls and the second has NO tools and NO "Operation catalog" in its
      system (fails today — it's a full planner round; the existing
      `plan_with_complete_renders_grounded_summary` cannot distinguish and keeps passing).
- [ ] Mechanism (in the loop host via the existing `force_stop` seam — no loop-contract break for
      ejected loops): `run_plan` arms `pending_completion` ONLY after a successful, non-suspended
      run — every early return (plan-rejected, execute-error, suspension, identical-plan-skip)
      precedes arming, so a failed run never consumes the completion and the model sees feedback.
      The next `plan()` renders via `render_completion` and returns `{kind:"chat", text}`; the
      loop's existing `case "chat"` ends the turn.
- [ ] `failing-first:` `failed_plan_run_does_not_consume_completion` and
      `identical_plan_skip_does_not_render_completion`.
- [ ] `render_completion` returns `(String, Usage)`; the render call's usage lands in turn totals +
      `CallUsage` (today `_usage` is discarded at compile.rs:544). A render error falls through to
      a normal planner round (never abort the turn on a summary failure).
- [ ] The plan value's `complete` field goes bool→object|null (clean cutover; the shipped loop and
      eject template never read it) — `crates/flux-flow/docs/ops-reference.md` updated.
- [ ] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P5 of the round).

## Notes
- Net turn shape: plan + run + one grounded no-catalog render, replacing plan + run + full second
  planner round. Live A/B in C-16's verification (vs the A-05 baseline: $0.0137 / 2.1s rm task).
- Design decisions in `~/.claude/plans/wiggly-tumbling-salamander.md` §A-06.
