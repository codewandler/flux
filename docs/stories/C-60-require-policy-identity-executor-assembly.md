---
id: C-60
title: Require policy and identity in public executor assembly
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: release blocker — AgentSpec, SDK, and App can construct an Executor with no authorization floor
---

# Require policy and identity in public executor assembly

## Goal

Make authorization a construction-time invariant of every production `Executor`, so auto-approval
can never mean execution without a policy floor or caller identity.

## Acceptance

- [x] Failing-first SDK, App, and `AgentSpec::assemble` tests prove an action denied by policy remains
      denied even when the surface uses auto-approval.
- [x] Production executor construction requires an `AuthorizationPolicy`, `Caller`, and `Trust`,
      either explicitly or through one documented local-default profile; `policy: None` is not a
      reachable production state.
- [x] CLI, App, SDK, `AgentSpec`, orchestration, realtime/voice, and direct authored-flow assembly
      migrate to the protected constructor and retain their intended permission/approval behavior.
- [x] Any unchecked constructor is conspicuously test-only (or unsafe-in-name), unavailable to normal
      downstream construction, and covered by a structural construction audit.
- [x] The duplicated synthetic local identity is replaced by one lower-layer identity/profile helper
      without adding an inner-to-outer dependency.
- [x] Architecture and SDK documentation truthfully describe policy, approval, and guarded IO after
      the migration; the full workspace and architecture gates pass.

## Progress

- 2026-07-14 — Added mandatory `ExecutionAuthorization`/`ExecutionEnvironment` assembly and a shared
  local profile, then migrated production surfaces. SDK, App, and `AgentSpec` tests prove
  auto-approval cannot override an authorization denial.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- C-62 depends on this story's mandatory policy/identity seam.
- Primary evidence: `flux_runtime::Executor::new`, `AgentSpec::assemble`,
  `FlowClient::build_executor`, and `flux_app::build_executor`.
