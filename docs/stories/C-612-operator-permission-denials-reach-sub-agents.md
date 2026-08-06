---
id: C-612
title: "Operator permission denials reach sub-agents"
pillar: "Core"
status: done
epic: agent-evidence-scope
areas: [flux-orchestrate, flux-cli, flux-app]
design: docs/designs/agent-evidence-scope.md
note: "a child executor gets an empty PermissionManager and no disabled ops, so deny and tools.disable stop at delegation"
done_override: "Implemented and tested in main (8728936e): operator permission rules are carried into every sub-agent and descend through nesting (flux-orchestrate/src/lib.rs:158, :219, :346). NOTE: the fleet separately re-implemented this in wave-385 because this story still read `ready` — that duplicated work is the cost of the gap this transition closes."
---

# Operator permission denials reach sub-agents

## Goal

Make an operator's two subtractive controls survive delegation. `docs/designs/agent-evidence-scope.md`
("Operator denials stop at the delegation boundary") records both halves: `LocalSpawner::spawn` built
the child executor with `PermissionManager::new()` — empty, so every child subject resolves to `Ask`
and `SubAgentApprover` allows anything non-destructive — and with **no disabled set at all**, so
`[tools] disable` bound the agent the operator was watching and nothing it delegated to. Both read as
enforced in the config reference while evaporating one `task` call deep. This carries them across the
boundary the same way `push_cap_scope` carries a tool ceiling: narrow-only, and transitive through
bounded nested delegation.

## Acceptance

- [x] `[permissions] deny` reaches a sub-agent's executor, and descends to a grandchild.
      → `LocalSpawner::with_permissions` / `SubAgents::with_permissions`, cloned in
      `LocalSpawner::at_depth`, installed at the `Executor::new_with_authorization` call in
      `LocalSpawner::spawn`. Denials only — never the allow list, which would widen a child past what
      the operator granted its parent. Test:
      `crates/flux-orchestrate/src/lib.rs::tests::a_permission_manager_reaches_children_and_descends_through_nesting`.
- [x] `[tools] disable` reaches a sub-agent's executor, and descends to a grandchild.
      → Failing-first, asserting on the **effect** rather than on configuration:
      `crates/flux-orchestrate/src/lib.rs::tests::disabled_ops_reach_the_sub_agent_and_descend_through_nesting`
      spawns a role whose model calls a disabled `ping` op that writes `PINGED.marker` iff it really
      executed. Before the fix the marker exists — `panicked at crates/flux-orchestrate/src/lib.rs:4324:
      an op the operator disabled must not execute inside a sub-agent`. After it, the marker is absent
      and the child turn still completes (the refusal is a tool-level error, not a dead turn).
- [x] The operator's **authored expressions** travel, not the parent's resolved names. A child's
      catalog is a different one (role ∩ cap_scope narrowing, plus its own agent ops), so `spawn`
      re-resolves them with `ToolRegistry::resolve_disabled` against the registry the child actually
      gets, and also keeps the raw patterns so an op introduced by a later catalog generation is
      disabled at that generation's turn boundary — the same pair the top-level executor installs.
- [x] Wired at the operator-facing surfaces: `flux-cli`'s `build_agent` assembly
      (`crates/flux-cli/src/execution.rs`, beside the `[permissions] deny` wiring) and
      `crates/flux-app/src/app.rs` (the `App`/journey spawner, sharing the `disabled` patterns it
      already resolves for its own executor).
- [x] Unconfigured stays unchanged: `permissions: None` and an empty pattern list skip installation
      entirely, so an embedder who configured neither sees the prior behaviour.
- [x] `website/docs/reference/config.md` says both controls bind delegated work, in the
      `[permissions]` and `[tools] disable` sections.

## Notes

- Design: [agent-evidence-scope.md](../designs/agent-evidence-scope.md), item "Operator denials stop
  at the delegation boundary". This story closes only that item; the evidence-scope narrowing
  (`Workspace` narrowing constructor, `SpawnRequest` read scope, the punctures) belongs to its
  siblings under the epic.
- Shape deliberately copied from C-299's `resource_limits`: an optional field on `LocalSpawner` and
  `SubAgents`, cloned in `at_depth`, applied once at child-executor construction.
- `SubAgents` gained a public field (`disabled_patterns`), breaking for struct-literal construction —
  every in-tree caller uses `SubAgents::new`. Same precedent as C-299's field addition.
- Not carried: the SDK's `FlowClient::with_sub_agents` surfaces set neither control automatically; an
  embedder calls `SubAgents::with_permissions` / `with_disabled_patterns`. Worth a follow-up if the
  SDK should mirror the CLI's automatic wiring.

## Progress

- Failing-first captured before any implementation: with the plumbing present but the child executor
  still built without a disabled set, the targeted test fails on the marker assertion (RED, quoted in
  Acceptance). Installing the resolved set + patterns turns it green.
- Targeted validation: `cargo test -p codewandler-flux-orchestrate
  disabled_ops_reach_the_sub_agent_and_descend_through_nesting`, `cargo test -p
  codewandler-flux-orchestrate`, `cargo check -p codewandler-flux-app`, `cargo check -p
  codewandler-flux-cli`, `cargo clippy` and `cargo fmt` on the touched packages. The full repository
  gate is the integrator's.
