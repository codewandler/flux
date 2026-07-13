---
id: A-76
title: Harden the adaptive outer loop after the default cutover
pillar: Agent
status: done
design: docs/designs/adaptive-loop-hardening.md
note: "repeatable decisions, session-scoped surfacing, bounded calls, semantic integration routing, correlated telemetry, and a multi-path E2E matrix"
---

# Harden the adaptive outer loop after the default cutover

## Goal
Make the evidence → capability → exploration loop reliable across long-lived/shared engines and
real integration requests, while making its latency and model payloads directly auditable.

## Acceptance
- [x] Failing-first flow tests prove a `DecisionRequest` before or after an execution report parks on
  the same durable `agent.decision` await, can park repeatedly, and resumes without re-executing an
  already completed action.
- [x] Failing-first engine tests prove monotonic surfaced groups are isolated by session on one
  shared `FlowEngine`.
- [x] Failing-first staged tests prove one logical adaptive run has a default 12-call ceiling across
  intent repairs, exploration, and decision resumes, and exhausts with an honest error.
- [x] A compact routing index is derived only from live, wired integration groups; exact aliases,
  semantic capabilities, and URL hosts can route a request without loading every operation schema;
  multiple deterministic matches produce a `DecisionRequest` instead of choosing silently.
- [x] `AgentSpec`, project config, and the CLI expose the logical-run call ceiling. Built-in intent
  and exploration stages accept same-provider model, effort, token, and call-cap overrides, with
  inheritance as the default and cross-provider overrides rejected before a wire call.
- [x] Every built-in model call emits a durable, redacted `model.call` observation correlated by
  session/turn/stage/round, including duration, TTFT, usage, operation/schema size, and repair count;
  approval wait and batch execution durations are observable. `--show-loop` renders a compact view,
  while full request bodies remain behind the existing explicit sensitive trace opt-in.
- [x] Public docs describe fresh non-cacheable reads accurately and document routing, budgets,
  stage policy, decision resume, and telemetry. `CHANGELOG.md`, `WHATS-NEW.md`, and the
  self-improvement status are updated.
- [x] Hermetic coverage spans support retrieval, live time, semantic capability expansion,
  gather/action/approval/execution, decisions before and after execution, partial failure without
  replay, and shared-engine session isolation. The live matrix script supports three trials per
  model and reports per-stage call/latency data.
- [x] The full repository dev gate is green.

## Progress
- 2026-07-13: story/design opened after live regressions exposed one-shot decisions, engine-global
  capability stickiness, and insufficient stage-level latency evidence.
- 2026-07-13: failing-first scripted coverage closed repeated/pre- and post-execution decisions,
  partial-failure no-replay, session isolation, logical-call exhaustion, stage policy, deterministic
  integration routing, and correlated timings. Public/SDK/config/CLI surfaces and both documentation
  audiences were updated.
- 2026-07-13: an installed OpenRouter DeepSeek V4 Flash Nitro turn routed a live-time request to
  `core`, gathered `now`, and presented the observed timestamp. The compact trace attributed its
  7.5-second total to intent plus two exploration calls and reported an 83% cache hit. The Codex live
  smoke stopped before inference on an expired local OAuth session and returned the documented
  re-authentication action.
- 2026-07-13: root build/test/clippy/fmt/codegate, generated-doc sync, the complete nested plugin
  test/clippy gate, and the exact `task install` command passed. The first reinstall attempt exhausted
  disk space after both Cargo workspaces had been built; cleaning only ignored `plugins/target`
  artifacts recovered the space and the unchanged command installed both binaries successfully.

## Notes
- The safety invariant is unchanged: routing and surfacing can only narrow or expose the live
  catalog. Authorization → approval → guarded IO remains mandatory for every real effect.
- The model never authors executable Flux; the shipped Flux-Lang program remains the outer loop.
