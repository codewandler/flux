# Explicit adaptive policy for spawned sub-agents

**Status:** implemented ([A-82](../stories/A-82-configure-sub-agent-adaptive-policy.md)) ·
**Layer:** L3 (`flux-orchestrate`) with the existing L6 SDK attachment seam

## Problem

`AgentSpec` owns the complete adaptive cognition contract: one logical model-call ceiling and
independent intent/explore model, reasoning effort, output-token and call limits. A
`LocalSpawner`, however, creates a fresh child `AgentSpec` from role frontmatter and currently sets
only inherited all-call reasoning plus `SpawnLimits.max_tokens` and `max_iterations`. The child's
`adaptive_policy` therefore always remains `AdaptiveLoopPolicy::default()`.

That makes a host unable to give a fast routing model and a deeper exploration model to specialists,
or to bound their native-stage calls independently from the parent. `SpawnLimits` is not the right
home: its token field is the child's ordinary per-request fallback, its iteration field bounds the
authored outer loop, and its wall clock bounds the whole child. Adaptive stage limits are a separate
logical budget already represented by `AdaptiveLoopPolicy`.

## API and compatibility

`LocalSpawner::with_adaptive_policy(policy)` stores an explicit policy and assigns it to the child
`AgentSpec` before `into_engine` validates/resolves it. `SubAgents` keeps its existing public shape;
instead of adding a field (which would break exhaustive struct literals), it gains
`into_spawner_with_adaptive_policy(system, policy)`. Existing `into_spawner(system)` delegates with
`AdaptiveLoopPolicy::default()`.

Both SDK attachment seams mirror that additive shape:

```rust
flow_client.with_sub_agents_policy(sub_agents, child_policy);
let client = Client::builder()
    .with_sub_agents_policy(sub_agents, child_policy)
    .build(provider, root)?;
```

Existing `FlowClient::with_sub_agents(sub_agents)` and `ClientBuilder::with_sub_agents(sub_agents)`
delegate with the default policy. No existing constructor, method call, or public struct literal
changes. Direct `LocalSpawner` consumers can opt in through its builder method; `SubAgents`
consumers choose the policy when materializing the spawner.

## Propagation and invariants

The policy is cloned into `LocalSpawner::at_depth`, so bounded grandchildren cannot silently regain
larger default cognition budgets. Each spawn clones it into the role-derived `AgentSpec` before
provider/model resolution and engine assembly. Invalid zero/cross-provider stage configuration keeps
failing at the existing `AgentSpec::into_engine` pre-wire boundary.

This is cognition configuration, not authority. It does not alter role tool intersection, inherited
authorization/identity, approvers, cancellation, audit correlation, or guarded IO.

## Verification

A capturing provider drives real child adaptive turns through the raw spawner and high-level
conversational builder. The regressions observe both native requests and assert the configured stage
model, effort and output ceilings. A second capped run proves the logical total prevents the next
provider request. Default construction is pinned to `AdaptiveLoopPolicy::default()`, and
nested-spawner construction is checked in-module so descendants retain the explicit policy.
