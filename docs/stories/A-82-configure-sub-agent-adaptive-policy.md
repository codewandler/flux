---
id: A-82
title: Configure adaptive cognition policy for spawned sub-agents
pillar: Agent
status: done
design: docs/designs/sub-agent-adaptive-policy.md
note: "let embedding hosts bound child intent/explore models, effort, output and calls without changing existing spawner defaults"
---

# Configure adaptive cognition policy for spawned sub-agents

## Goal
Give embedding hosts an explicit, additive way to set the complete `AdaptiveLoopPolicy` used by
every child assembled through `SubAgents` or `LocalSpawner`, so parent and specialist latency/cost
budgets can be tuned independently.

## Acceptance
- [x] Failing-first test `explicit_child_adaptive_policy_reaches_both_native_stages` proves a child
      sends the configured intent/explore model, effort, token ceilings and call ceilings, including
      the logical total call cap, rather than silently reverting to `AgentSpec` defaults.
- [x] Existing `LocalSpawner::new`, `SubAgents::into_spawner`, and
      `FlowClient::with_sub_agents` / `ClientBuilder::with_sub_agents` callers retain the current
      default `AdaptiveLoopPolicy`; the new API is additive and does not require exhaustive public
      struct literals to change.
- [x] Bounded nested delegation carries the same explicit policy into every descendant spawner.
- [x] Public SDK/design documentation names the child policy seam and distinguishes adaptive model
      calls from outer-loop iterations, token budget, and wall-clock limits.
- [x] Focused formatting, tests, clippy with warnings denied, architecture layering, and diff checks
      pass.

## Progress
- 2026-07-14: opened for downstream manager/specialist latency work. The parent `AgentSpec` already
  accepts a complete adaptive policy, but `LocalSpawner::spawn` constructs each child spec and leaves
  that field at its default; `SubAgents` exposes only outer iterations/tokens/wall-clock and inherited
  all-call reasoning.
- 2026-07-14: implemented additive `LocalSpawner`, `SubAgents`, `FlowClient`, and conversational
  `ClientBuilder` policy seams. Real child turns through both SDK attachment doors prove both stage
  request shapes plus pre-wire stage/logical call stops; an in-module regression pins defaults and
  bounded-descendant inheritance.
- 2026-07-14: all 34 orchestrate tests, the SDK's existing delegated-task test, focused clippy with
  warnings denied, formatting, the architecture layering gate, `git diff --check`, and the public
  website build pass. The website build reports its pre-existing `language/tooling` broken-anchor
  warning. An extra warnings-denied rustdoc probe remains red on pre-existing SDK/orchestrate links
  outside this story; the repository's required gate does not include that probe.

## Notes
- Design: [sub-agent-adaptive-policy.md](../designs/sub-agent-adaptive-policy.md).
- This changes cognition budgets only. Child tools still traverse the same authorization, approval,
  redaction, and guarded-IO envelope.
