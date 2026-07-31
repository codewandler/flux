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

- [x] **Failing-first test**: a `[limits] max_concurrent_tool_calls = 1` table binds for the
      `flux app run` executor. It fails today. Assert on **observed occupancy** inside `Tool::execute`,
      the way C-299's `a_configured_limits_table_binds_for_the_cli_executor` does — not on
      configuration having been read.
      → `app_run_resource_ceiling_wiring::a_configured_limits_table_binds_for_the_app_run_executor`
      (`crates/flux-cli/src/app_cmd.rs`); red as `left: 3, right: 1`.
- [x] **A second failing-first test for the sub-agent half**: `flux app run strict-review`'s reviewer
      children inherit the configured ceiling. Removing the wiring must turn it red — C-299's review
      caught precisely this class by mutating the wiring line and observing that no test name changed,
      so hold this one to that bar.
      → `app_run_resource_ceiling_wiring::app_run_strict_review_reviewers_inherit_the_configured_ceiling`;
      mutation performed and recorded under `## Progress`.
- [x] The ceiling is resolved **once** per assembly. C-299's own recorded risk is that a second
      `cli_resource_limits` call mints a second semaphore and executors silently stop sharing a budget.
      → one `cli_resource_limits` call per assembly in `run_app`, `run_review` and `record_client`;
      every consumer takes a `clone()` of that one value.
- [x] The per-child semantics C-299 settled are preserved and not silently re-litigated: each agent
      gets `ResourceLimits::independent_copy()`, so `N` bounds *each* agent and a process with k live
      children may run up to N×(k+1). Do **not** introduce a shared semaphore here — C-299 reproduced
      a real deadlock in that shape (a parent holding a permit while awaiting a child that queues on
      the same semaphore, bounded only by the queue timeout).
      → no new semaphore sharing; the sub-agent test asserts `in_flight == 2` (one permit for the
      parent, one for the child) precisely so a "fix" that shared one budget reads `1` and fails.
- [x] Audit for any *other* surface that assembles an `ExecutionEnvironment` without
      `with_resource_limits`, and either wire it or record why it is exempt. Two holes found by
      accident suggests the seam, not the call sites, is the problem — consider whether assembly can
      be made to fail closed instead of defaulting to unbounded.
      → see `## Progress`; two more surfaces wired, one recorded as exempt.
- [x] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace`.
      → green in both postures except one pre-existing, unrelated failure recorded under `## Progress`.

## Progress

**What changed.** `run_app` resolves `cli_resource_limits(&cfg)` once, immediately after it loads the
config, and hands that one value to both halves. The inline `ExecutionEnvironment::new(..)` is gone:
program mode now goes through a new `assemble_app_execution_environment`, a thin wrapper over C-299's
existing `assemble_cli_execution_environment` that layers on the app's `WorkspaceContext`. That is the
seam-level answer to "make assembly fail closed": `resource_limits` is a **required** parameter of the
shared seam, so a surface can no longer end up unbounded by simply not mentioning it — which is exactly
how both of this story's holes were opened. `build_review_sub_agents` likewise takes a required
`ResourceLimits` and installs it on the bundle.

**Mutation proof (the bar the Acceptance sets).** Reverting `build_review_sub_agents` to its bare
`SubAgents::new` — i.e. deleting `.with_resource_limits(resource_limits)`, `crates/flux-cli/src/review.rs` —
while leaving everything else wired turns exactly one test name red:

```
test app_cmd::app_run_resource_ceiling_wiring::app_run_strict_review_reviewers_inherit_the_configured_ceiling ... FAILED
  assertion `left == right` failed: expected the parent and exactly ONE reviewer child in flight, saw 4
  left: 4   right: 2
```

`a_configured_limits_table_binds_for_the_app_run_executor` stayed green in that run, so the two tests
are independently attributable rather than one wiring line covered twice.

**Surface audit (Acceptance 5).** Every other CLI entry point (`run`, `plan`, `tui`, `serve`,
`flow run`, `preset`, the session commands, `stream-json`) reaches its runtime through
`build_agent`/`build_agent_lazy` → `build_agent_with`, which C-299 already wired. Beyond those,
grepping `ExecutionEnvironment::new` and the SDK client builders left three call sites:

- `flux review` (`run_review`) — assembles through `flux_sdk::FlowClient`, never `build_agent_with`.
  **Wired**: `.resource_limits(..)` on the builder, plus the ceilings on its reviewer children.
- `flux record` (`record_client`) — `flux_sdk::Client`, a real live turn. **Wired**.
- `flux test` (`offline_client`) — **deliberately exempt**, reasoning recorded on the function itself:
  it is a regression gate whose verdict must depend only on the fixture, and a queue-timeout refusal
  under a locally-configured ceiling is a tool error, i.e. a red test on one machine and green on
  another. There is also no runaway workload to cap — the provider is never called.

A stronger fail-closed shape (making `ExecutionEnvironment::new` itself demand ceilings) was considered
and **not** attempted here: it is a published L2 API with call sites across `flux-app`, `flux-agent`,
`flux-sdk` and the tests, so it is a design change and deserves its own story rather than riding along
on a wiring fix.

**Gate.** `cargo test --workspace` (and the `FLUX_BWRAP_BIN=/nonexistent/bwrap` posture), clippy,
fmt in both workspaces, and `cargo test -p flux-codegate` were all run. One failure, pre-existing and
untouched by this diff: `flux-codegate`'s `roadmap_status_line_matches_the_workspace_version` — the
0.42.1 cut did not restamp `docs/roadmap.md`, which still says `0.42.0`. Both inputs it reads are
byte-identical to `origin/main`, and `docs/roadmap.md` is a coordinator-owned file, so it is left alone.

## Notes

- Related: [C-299](C-299-resource-ceilings-do-not-reach-sub-agents-or-the-cli.md) built the wiring and
  the per-child decision; [C-290](C-290-runtime-resource-limits.md) built `ResourceLimits`.
- ⚠ Worth reading before starting: C-299's review also established that the ceiling **barely binds
  inside a conversational turn** at all, because `execute_batch` is itself a dispatched op holding one
  permit and the identity-keyed `HELD_SLOTS` exemption then covers everything nested beneath it. So
  `max_concurrent_tool_calls` mostly binds dispatches *not* nested in another op's execution. That is
  pre-existing C-290 coarseness rather than this story's to fix, but an operator reading the config
  docs would expect more, and it deserves its own story rather than being quietly inherited here.
