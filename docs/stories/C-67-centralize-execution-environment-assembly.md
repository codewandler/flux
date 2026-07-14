---
id: C-67
title: Centralize execution-environment assembly
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: CLI, App, AgentSpec, and SDK independently wire workspace, policy, plugins, events, and context
---

# Centralize execution-environment assembly

## Goal

Provide one mechanical assembly path for an explicit workspace/system, registry, policy/identity,
approver, events, plugins/endpoints, and context while leaving surface-specific policy decisions at
L6.

## Acceptance

- [x] An `ExecutionEnvironment`/builder (name may differ) takes one explicit `Workspace/System` and
      all mandatory C-60/C-62 authority inputs, then builds an executor/engine without consulting
      `current_dir()` again.
- [x] CLI agent construction, `run_app`, `AgentSpec::assemble`, SDK clients, and App agent/journey
      construction delegate their shared mechanics to it; no second plugin/endpoint/datasource audit
      wiring path remains.
- [x] Failing-first tests change process cwd between construction and lazy agent creation and prove
      context detection, role/skill lookup, guarded IO, and event attribution retain the original
      workspace root.
- [x] Nominally fallible `try_*` constructors return errors for invalid workspace/configuration and
      contain no downstream `expect`/panic on that path.
- [x] Cross-surface parity tests assert the same requested tool set produces the same registry,
      policy requests, identity, redactor, and guarded system in CLI, App, and SDK assembly.
- [x] Public behavior and layer direction remain unchanged; deprecated constructor shims delegate to
      the builder and have a documented removal plan.

## Progress

- 2026-07-14 — Centralized mechanical assembly in `ExecutionEnvironment` and migrated CLI, App,
  AgentSpec, orchestration, and both SDK clients. The production conformance test exercises all four
  public paths with one typed contract, identity, redactor, and explicit guarded root.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Depends on C-60 and C-62 so consolidation targets the repaired safety contract.
- This story owns App's split-root/fallible-constructor issue; C-71 owns behavior-neutral file/module
  extraction after assembly converges.
