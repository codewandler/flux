---
id: A-25
title: "Make with_tools cap-scope transitive across nested sub-agent delegation"
pillar: Agent
status: done
priority:
epic: review-hardening
design: docs/designs/review-hardening.md
note: "under opt-in with_max_depth>=2 a grandchild is spawned with an empty cap-scope stack (active_cap_scope()==None) and its spawner re-subsets the FULL base_registry, so an ancestor with_tools ceiling isn't enforced two hops down and TaskTool is re-registered unconditionally — a cap-scope escape on the opt-in nested path (default depth=1 keeps every child a leaf)"
---

# Make with_tools cap-scope transitive across nested sub-agent delegation

## Goal
Enforce the documented narrow-only-on-descent invariant across the sub-agent boundary before D-05's
multi-tenant nested-delegation use case relies on it. In `LocalSpawner::spawn`
(`crates/flux-orchestrate/src/lib.rs:267`) the child gets a fresh `ToolContext` with an empty cap-scope
stack (`active_cap_scope() == None`), and the installed `at_depth` spawner clones the **full**
`base_registry` (`:195-209`) rather than the parent's narrowed `subset(effective_tools)` (which is used
only for this child's own executor). So under opt-in `with_max_depth ≥ 2`, a grandchild's tool set is
computed as `grandchild_role.tools ∩ None = grandchild_role.tools` (or the entire base registry) — the
ancestor `with_tools` ceiling is not carried down. `TaskTool` is also re-registered unconditionally
(`:268-272`) even when `task ∉ effective_tools`. `SubAgentApprover` auto-approves non-destructive calls,
so non-destructive tools the `with_tools` block excluded become reachable two hops down.

## Acceptance
- [x] Failing-first test (extend `max_depth_bounds_nested_delegation`, `crates/flux-orchestrate/src/lib.rs:2299`):
      spawn the delegator at `max_depth = 2` with `cap_scope: Some(["task","read"])`, excluding `ping` (the
      grandchild role's only tool); assert `GRANDCHILD.marker` is **absent**. Today the grandchild is spawned
      with `cap_scope = None` and the marker is written.
- [x] Fix (two parts): (a) when `child_can_delegate`, push `effective_tools` onto the child `ToolContext`'s
      cap-scope stack and/or build the `at_depth` spawner over the narrowed registry, so descendants inherit
      the ceiling; (b) gate the `TaskTool` re-registration on `task ∈ effective_tools`.
- [x] Default (`max_depth = 1`) behaviour is unchanged; the depth-count bound still holds.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded **security-but-opt-in-only** (Opus). The escape is
  unreachable through the shipped CLI/`flux app run`/default SDK (all build `SubAgents` at the default
  `max_depth = 1`, `:152`, keeping every child a leaf); only a Rust embedder calling `with_max_depth(≥2)`
  reaches it. Destructive ops remain blocked by `SubAgentApprover`; the leak is auto-approved non-destructive
  tools. Filed as forward-looking hardening, not a default-reachable hole.
- 2026-07-03 fixed. `LocalSpawner::at_depth` now takes an explicit `base_registry` (the caller's own
  `base_registry.subset(effective_tools)`, not `self.base_registry.clone()`), so the depth-next spawner
  can only ever draw from a pool an ancestor's `with_tools` ceiling has already narrowed — transitive
  across any number of hops, since each hop rebuilds the next one's base the same way. `child_can_delegate`
  now also requires `task ∈ effective_tools` (`child_has_task`, via `Option::is_none_or`), so `TaskTool` is
  no longer re-registered unconditionally when a role/scope excludes it; the depth bound (`child_depth <
  max_depth`) is unchanged, just `&&`-combined with the new check. Failing-first test: extended
  `max_depth_bounds_nested_delegation` with a third scenario (`sys3`/`depth-nested-scoped`) that spawns
  `delegator` at `max_depth = 2` with `cap_scope: Some(["task","read"])`; confirmed it failed for the right
  reason (`GRANDCHILD.marker` written) against the pre-fix code before implementing, then passed after.
  Also updated the `delegator` test-fixture role's `tools` to `[task, ping]` (previously `[ping]`) since
  `task` is now gated like any other tool — a delegating role must declare it. Gate: `cargo test
  -p flux-orchestrate` (25 passed), `cargo clippy -p flux-orchestrate --all-targets -- -D warnings` (clean),
  `cargo fmt -p flux-orchestrate --check` (clean).

## Notes
- Evidence: `crates/flux-orchestrate/src/lib.rs:267` (fresh ctx), `:195-209` (`at_depth` clones full base),
  `:258` (local narrowed registry), `:268-272` (unconditional TaskTool), `:729` (TaskTool reads child scope),
  `:152` (default depth 1), `:214-218` (narrow-only doc); `docs/designs/strict-review-flows.md:292-299`.
- Residual of [L-11](L-11-strict-review-scoped-capabilities.md) / [D-05](D-05-sub-agent-hardening.md).
  Design: [review-hardening](../designs/review-hardening.md).
