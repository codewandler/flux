---
id: C-307
title: "`flux app run` ignores `[limits]`, and its review sub-agents are unbounded"
pillar: Core
status: in-progress
priority: 9
areas: [flux-cli]
note: "C-299 wired [limits] through build_agent_with, which covers run/plan/tui/serve. `flux app run` assembles its own ExecutionEnvironment and never calls with_resource_limits; build_review_sub_agents returns a bare SubAgents::new, so `flux app run strict-review`'s reviewer children run with no ceiling at all"
---

# `flux app run` ignores `[limits]`, and its review sub-agents are unbounded

## Goal

Make a configured `[limits]` table bind for **every** shipped entry point, not just the ones that
route through `build_agent_with`.

## Why this is a separate story

C-299 fixed the headline gap — `[limits]` now binds for `flux run`, `plan`, `tui` and `serve`, all of
which assemble through `build_agent_with`. Its independent review then found that `flux app run` does
not, and C-299's implementor disclosed it rather than widening its own scope. That was the right
call; this is the follow-up.

Two distinct holes, and the second is worse than the first:

1. **The app path builds its own environment.** `crates/flux-cli/src/app_cmd.rs` loads the config,
   then constructs `ExecutionEnvironment::new(...).with_workspace(...).with_redactor(...)` with **no**
   `with_resource_limits`. So a configured ceiling is silently inert for the whole `flux app run`
   surface.
2. **Its review sub-agents get nothing at all.** `build_review_sub_agents`
   (`crates/flux-cli/src/review.rs`) returns a bare `SubAgents::new`, so `flux app run strict-review`
   fans out to reviewer children that carry no ceiling — the exact defect C-299 exists to close, on
   the one path that fans out hardest.

## Acceptance

- [ ] **Failing-first test**: a `[limits] max_concurrent_tool_calls = 1` table binds for the
      `flux app run` executor. It fails today. Assert on **observed occupancy** inside `Tool::execute`,
      the way C-299's `a_configured_limits_table_binds_for_the_cli_executor` does — not on
      configuration having been read.
- [ ] **A second failing-first test for the sub-agent half**: `flux app run strict-review`'s reviewer
      children inherit the configured ceiling. Removing the wiring must turn it red — C-299's review
      caught precisely this class by mutating the wiring line and observing that no test name changed,
      so hold this one to that bar.
- [ ] The ceiling is resolved **once** per assembly. C-299's own recorded risk is that a second
      `cli_resource_limits` call mints a second semaphore and executors silently stop sharing a budget.
- [ ] The per-child semantics C-299 settled are preserved and not silently re-litigated: each agent
      gets `ResourceLimits::independent_copy()`, so `N` bounds *each* agent and a process with k live
      children may run up to N×(k+1). Do **not** introduce a shared semaphore here — C-299 reproduced
      a real deadlock in that shape (a parent holding a permit while awaiting a child that queues on
      the same semaphore, bounded only by the queue timeout).
- [ ] Audit for any *other* surface that assembles an `ExecutionEnvironment` without
      `with_resource_limits`, and either wire it or record why it is exempt. Two holes found by
      accident suggests the seam, not the call sites, is the problem — consider whether assembly can
      be made to fail closed instead of defaulting to unbounded.
- [ ] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace`.

## Notes

- Related: [C-299](C-299-resource-ceilings-do-not-reach-sub-agents-or-the-cli.md) built the wiring and
  the per-child decision; [C-290](C-290-runtime-resource-limits.md) built `ResourceLimits`.
- ⚠ Worth reading before starting: C-299's review also established that the ceiling **barely binds
  inside a conversational turn** at all, because `execute_batch` is itself a dispatched op holding one
  permit and the identity-keyed `HELD_SLOTS` exemption then covers everything nested beneath it. So
  `max_concurrent_tool_calls` mostly binds dispatches *not* nested in another op's execution. That is
  pre-existing C-290 coarseness rather than this story's to fix, but an operator reading the config
  docs would expect more, and it deserves its own story rather than being quietly inherited here.
