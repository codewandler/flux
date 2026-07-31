---
id: C-299
title: "A configured resource ceiling reaches neither sub-agents nor the `flux` binary"
pillar: Core
status: in-progress
priority: 5
areas: [flux-cli, flux-orchestrate, flux-runtime]
note: "C-290 built the ceiling and could reach neither consumer — flux-cli and flux-orchestrate were both fenced. Until this lands, `[limits]` is inert for the binary and `task`-delegated work is unbounded, while the SDK doc says the ceiling binds"
---

# A configured resource ceiling reaches neither sub-agents nor the `flux` binary

## Goal

C-290 gave an embedding host a real concurrency ceiling and a real retained-result ceiling, enforced
in the funnel every in-process tool call traverses. Two consumers never got wired to it, both because
they were fenced off by a concurrent story rather than by a design decision. Until this lands the
feature is narrower than its own documentation says.

**1. Sub-agents run unbounded.** `LocalSpawner::spawn` builds the child with
`Executor::new_with_authorization(...)` (`crates/flux-orchestrate/src/lib.rs:395-401`), which defaults
to `ResourceLimits::new()` — `grep -rn "ResourceLimits\|resource_limits" crates/` returns **zero**
hits in flux-orchestrate. So `task`-delegated work runs unbounded **in the same process**, while
`ClientBuilder::resource_limits` documents that the ceiling "binds for this in-process client"
(`crates/flux-sdk/src/lib.rs:750-752`). A host that sets a ceiling and then delegates has the ceiling
silently not apply to the delegated half.

**2. The `flux` binary ignores `[limits]`.** The new `max_concurrent_tool_calls`,
`tool_call_queue_timeout_ms` and `max_retained_result_bytes` keys are consumed today only by an
embedder calling `ResourceLimits::from_config`. `flux-cli`'s executor assembly never reads them, so a
configured key does nothing for anyone running the shipped binary — C-290's implementor documented
this rather than working around its fence, which was right, but it is real debt.

## Acceptance

- [x] A failing-first test: a runtime with `max_concurrent_tool_calls(N)` that delegates through
      `task` exceeds N in flight across parent and child. That is the defect; assert on observed
      occupancy, not on configuration.
      → **Reframed, with proof — the hypothesised state is unreachable.** See "Why the stated
      failing-first test cannot be written" below. The failing-first test that *does* exist asserts
      observed occupancy and is the CLI half's:
      `crates/flux-cli/src/execution.rs::cli_resource_ceiling_wiring::a_configured_limits_table_binds_for_the_cli_executor`
      — 3 tool calls in flight under a configured ceiling of 1, at the merge base.
- [x] Sub-agent executors inherit the parent's ceiling. **Decide and state whether the ceiling is
      shared or per-child.**
      → **PER-CHILD, decided deliberately and stated at all five doc sites.** Each child gets
      `ResourceLimits::independent_copy()` — same configured numbers, its own semaphore. A shared
      budget was implemented first, proven to deadlock (below), and abandoned.
      `max_concurrent_tool_calls = N` therefore bounds *each agent* at N; with k live sub-agents the
      process may run up to N×(k+1) tool calls. Every doc site says exactly that rather than implying
      a whole-process cap.
- [x] ⚠ **Check the deadlock boundary before sharing anything.**
      → Checked by building the shared shape and watching it deadlock, not by argument.
      `crates/flux-sdk/tests/resource_limits.rs::a_delegated_child_is_bounded_but_never_starved_by_its_parent`
      drives a real conversational turn (real `SpawnTaskSupervisor`, real `tokio::spawn`). Swap
      `independent_copy()` for `clone()` in `LocalSpawner::spawn` and it fails with `runs == 0` at a
      ceiling of 1 with a *single* delegation. The hazard is also **wider than the story assumed** —
      see below.
- [x] `flux-cli` reads `[limits]` and applies it at executor assembly, so a configured key binds for
      the shipped binary.
      → `cli_resource_limits` + the `resource_limits` parameter on
      `assemble_cli_execution_environment` (`crates/flux-cli/src/execution.rs`), resolved once in
      `build_agent_with` and shared with the sub-agent spawner.
- [x] `website/docs/reference/config.md` lists the new keys.
      → All four (`max_concurrent_tool_calls`, `tool_call_queue_timeout_ms`,
      `max_retained_result_bytes`, `max_evidence_payload_bytes`), plus an explicit note that these
      are per-agent and multiply under delegation fan-out.
- [x] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.

## Notes

- Both gaps were found by C-290's independent review, not by its implementor — and neither was
  avoidable in that story: flux-cli and flux-orchestrate were fenced for C-213 and C-277 respectively.
  This is the cost of a wide wave, paid deliberately.
- ⚠ Sequencing: the `flux-cli` half is one call site and cheap. The sub-agent half is a design
  decision with a deadlock hazard attached. They can ship separately, and if this story is split, ship
  the CLI wiring first — it is the one an operator can observe.
- Related: [C-290](C-290-runtime-resource-limits.md) built `ResourceLimits`;
  [C-298](C-298-evidence-log-is-the-dominant-unbounded-retention.md) is the other thing C-290 could
  not reach.

## The deadlock boundary is wider than `task`

The story warned that a parent holding a slot while awaiting a child is a deadlock. That is right,
and it is **not only `task`**. The chain for a delegation on the real conversational path is:

```
execute_batch   (a dispatched op — HOLDS a slot for the whole batch)
  └─ task       (a dispatched op — HOLDS a slot for the child's whole turn)
       └─ SpawnTaskSupervisor::spawn   ← tokio::spawn; HELD_SLOTS does not cross
            └─ child executor → child's first tool call → asks for a slot
```

With one shared semaphore the child queues behind **two** ancestors that are both blocked waiting for
it. At a ceiling of 1 nothing runs at all; in general every delegated child stalls until the queue
timeout refuses it. Reproduced, not reasoned:
`a_delegated_child_is_bounded_but_never_starved_by_its_parent` fails with `runs == 0`.

Marking the delegating ops as non-occupying was implemented and **does not close it**. The set of ops
that can transitively await a sub-agent is the whole nested-program family — `execute_batch`,
`explore`, `ai_segment`, `flow_run`, any authored model stage, plus `change_implement` — and it is
open-ended: any future op that runs an authored flow can contain a `task`. The invariant would be
unenforceable, and a regression would surface only under saturation *and* delegation, which is the
worst possible failure signature. A shared budget needs a structural mechanism (ancestry-keyed
permits, or releasing a slot across any nested dispatch), i.e. a design, not this story's wiring.

Hence per-child. It is the weaker guarantee, it is stated as such everywhere, and it is safe by
construction: parent and child hold different semaphores, so no ancestor can ever block a descendant.

## Why the stated failing-first test cannot be written

Acceptance 1 asked for a runtime that "exceeds N in flight across parent and child" at the base. That
state is unreachable, by conservation:

- Every in-flight `task` consumes exactly one parent slot (it is an ordinary op today), and
- the child agent loop executes its action batch **strictly sequentially**
  (`crates/flux-flow/src/loop_host.rs`, the `execute_batch` action loop), so one child contributes at
  most one concurrent execution.

So k concurrent delegations consume k slots and yield at most k concurrent child executions: real
work is bounded by N *by accident*. The unbounded child executor is masked 1:1 by the supervisor's own
slot. Three independent shapes were tried (parent-overlap, fan-out, nested depth) and all conserve.

That makes the documented gap a **bookkeeping** gap rather than a resource gap under today's
sequential child loop — worth fixing (the child now inherits real ceilings instead of reporting none,
and its op-cache/evidence bytes are genuinely bounded where they were not), but not observable as an
occupancy overrun. The failing-first test therefore attaches to the CLI half, where the defect *is*
directly observable: 3 in flight under a configured ceiling of 1.

## Progress

**2026-07-31 — landed on `impl/C-299`.**

- **CLI half (complete).** `cli_resource_limits(&Config)` is the one place `[limits]` becomes runtime
  ceilings; `assemble_cli_execution_environment` gained a `resource_limits` parameter.
  `build_agent_with` resolves it **once** and hands the same value to both the sub-agent spawner and
  the top-level environment — resolving twice would mint two semaphores and stop the top-level
  agent's own executors sharing one budget.
- **Sub-agent half (per-child).** `LocalSpawner` / `SubAgents` carry `resource_limits`; the child
  executor gets `independent_copy()`. It descends through nested delegation (`at_depth`) too. Wired
  automatically at all three surfaces — `ClientBuilder::build`, `FlowClient::try_with_sub_agents`,
  `try_with_sub_agents_policy` — so a host that sets `resource_limits` and then attaches sub-agents
  gets the descent without a second call.
- `ResourceLimits::independent_copy` is new public API on `flux-runtime` (additive).
  `SubAgents` gained a public field, which is breaking for struct-literal construction — every
  in-tree caller uses `SubAgents::new`, and this matches the precedent C-290 set with
  `flux_config::Limits`. Release-bump input.

### Owed, deliberately not done here

1. **The ceiling barely binds inside a conversational turn.** `execute_batch` holds one slot and
   `HELD_SLOTS` exempts every op nested beneath it, so `max_concurrent_tool_calls` mostly binds
   dispatches that are *not* nested inside another op's execution (the deterministic `FlowClient`
   path, direct SDK dispatch). This predates C-299 — it is C-290's exemption being coarse — but an
   operator reading the new config docs would reasonably expect more. Worth its own story.
2. **A whole-process concurrency bound** remains unavailable; see the deadlock section for what it
   would take.
3. **`flux app run`** (`crates/flux-cli/src/app_cmd.rs`) assembles its own environment and does not
   read `[limits]`. Out of this story's named areas; one call site when someone wants it.
