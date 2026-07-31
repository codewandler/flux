---
id: C-318
title: "The refreshed catalog reaches a running session — the registry A-95 froze on purpose"
pillar: Core
status: ready
priority: 12
areas: [flux-cli]
note: "C-310's honest boundary, declared by its own implementor — the refresh mechanism and its operator surface exist, but Executor owns its ToolRegistry by value with no registry_mut, and execution.rs cites A-95 prompt-cache stability as the reason the surfaced set must not churn mid-turn"
---

# The refreshed catalog reaches a running session

## Goal

C-310 built catalog refresh: a plugin's operations can be re-projected without restarting flux, with
every load-time check re-run and any widening of the granted capabilities refused outright. What it
deliberately did **not** do is make a running session see the result.

`Executor` owns its `ToolRegistry` **by value**, behind an `Arc<Executor>`, with no `registry_mut`.
That is not an oversight — `crates/flux-cli/src/execution.rs:1703-1708` cites **A-95 prompt-cache
stability** as the reason the surfaced tool set must not churn mid-turn. A registry that changes
under a live turn invalidates the cached prompt prefix, which is a real cost, and it also means the
model's view of what it can call changes between its decision and its dispatch.

So the story C-310 delivers is: *the mechanism exists and an operator can invoke it*. The story here
is: *a session that is already running picks it up*, without paying the cost A-95 was protecting.

This is the behaviour the connectors seam ultimately wants — an op set that changes when the
operator authenticates a provider, observed by the agent that is already running, not by the next
one.

## Acceptance

- [ ] **The interior-mutability decision is made explicitly and written down**, because that is the
      whole story. The existing precedent in this tree is a **side-channel**
      (`DynamicComposites`/`EngineLoopHost`), not mutation of the registry — evaluate that against
      making the registry interior-mutable, and say which and why. A story that quietly adds a
      `registry_mut` has skipped the question A-95 asked.
- [ ] **Turn-boundary semantics are defined and tested.** A refresh must not change the surfaced set
      *during* a turn. State where the boundary is, and prove with a test that a refresh landing
      mid-turn is not visible until the boundary — the failure mode is a model that plans against one
      catalog and dispatches against another.
- [ ] The prompt-cache cost is measured, not assumed. State what a refresh costs the cached prefix
      and whether that is acceptable; A-95 exists because it was not.
- [ ] **Failing-first**: a test showing a running session still dispatching the pre-refresh catalog,
      red before the wiring and green after.
- [ ] A withdrawn op is not callable by a running session after the boundary, and the in-flight
      guarantee C-310 established still holds — a call already running completes under the spec it
      was authorized with.
- [ ] Full gate green in both workspaces.

## Notes

- Declared by C-310's implementor as "the honest boundary of the story", with the recommendation that
  it get its own story. Related: [C-310](C-310-plugin-catalog-refresh.md) built the mechanism.
- **Do not weaken C-310's refusal rule to make this easier.** Its literal containment check — a
  refreshed capability entry must appear verbatim in the granted list — is strict on purpose: a
  permissive error there is a privilege escalation, a strict one is a refusal a restart resolves, and
  "must already be in the list" cannot drift as grant grammars gain wildcards.
- Two smaller gaps C-310 also recorded, neither belonging here: a retained op's `idempotency` and
  `group` are not covered by the weakening check (an idempotency downgrade is caught by C-191's I3 as
  a *warning* only; a group move changes surfacing breadth, not authority), and
  `retained_op_weakenings` is O(ops²) over visible ops — fine at the ~50-op scale a real plugin has,
  wrong for a 1000-op connector catalog.
