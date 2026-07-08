# Flux-Lang agent speed (epic)

**Status:** proposed 2026-07-09 · **Pillar:** Language · **Epic slug:** `flux-lang-agent-speed`

This epic tracks the four highest-leverage Flux-Lang features that would make every agent
turn faster without weakening the core contract: the LLM emits a typed plan, and the
deterministic runtime executes it through authorization, approval, and guarded IO.

The common theme is simple: do less model work, do more independent runtime work in
parallel, and reuse deterministic evidence whenever it is still valid.

## KF1 - whole-flow dependency scheduler

Current optimization is intentionally conservative and mostly local. It should become a
whole-flow scheduler that builds a symbol dependency graph across nested blocks, object
and list templates, conditions, loops, gathers, and call arguments. Independent read-only
work can then run in parallel behind effect and approval fences while preserving the same
observable result and trace order.

This speeds up every agent that gathers repo, tool, or datasource evidence before asking
a model to reason over it.

Tracked by [L-53](../stories/L-53-whole-flow-dependency-scheduler.md).

## KF2 - content-addressed op cache

Flux already has typed plans, explicit effects, operation metadata, and durable stores.
Use that structure to cache deterministic read-only operation results by content address:
operation identity, normalized inputs, op version/schema, workspace or datasource
snapshot, and invalidation domain.

This avoids paying the same IO and parsing cost across repair rounds, retries, forks,
sub-agents, and resumed turns.

Tracked by [L-54](../stories/L-54-content-addressed-op-cache.md).

## KF3 - plan-delta emission

Repair rounds and iterative agents often need to change one or two nodes, not re-emit an
entire plan. Add a safe delta format that patches the previous AST, materializes back to a
full `DraftAst`, and then passes through the existing analyzer, policy, and audit gates.

This cuts planner tokens, reduces malformed full-plan retries, and makes repairs more
stable without introducing a second execution path.

Tracked by [L-55](../stories/L-55-plan-delta-emission.md).

## KF4 - automatic context slicing

The HIR already knows symbol dependencies, and op schemas know what each operation reads.
Use that to compute the minimum context a planner or model-op needs for the next decision.
Large observations should be sliced to the referenced symbols, fields, and evidence
windows before they reach the model.

This improves speed, cost, and accuracy because agents stop rereading unrelated evidence
on every reasoning call.

Tracked by [L-56](../stories/L-56-automatic-context-slicing.md).

## Guardrails

- These features optimize the deterministic runtime; none may bypass the safety envelope.
- Effects, approval fences, and secret redaction define hard scheduling and caching
  boundaries.
- Every shipped slice needs failing-first tests and trace/audit coverage proving the
  optimized path is observationally equivalent to the unoptimized path.
