---
id: A-90
title: Keep host channel operations available after adaptive routing
pillar: Agent
status: done
priority: 1
design: docs/designs/adaptive-ambient-operations.md
areas: [flux-flow, flux-orchestrate]
---

# Keep host channel operations available after adaptive routing

## Goal
Let an embedding host mark a deliberately small operation group as adaptive-loop ambient so intent
routing can narrow the functional catalog without removing channel-owned presentation or progress
facilities.

## Acceptance
- [x] A failing-first adaptive-loop test selects one realistic functional family and proves ambient
      operations remain provider-visible during exploration while the ambient group is absent from
      intent and capability-signal family choices.
- [x] Ambient operations remain subject to registration, permission, authored-tool-ceiling, native
      operation-count/schema budgets, approval, and normal executor dispatch.
- [x] The contract reaches role-derived child runtimes without a separate mutable or process-global
      channel.
- [x] Flux tests, clippy, formatting, and the architecture gate are green.

## Progress

- 2026-07-14: failing-first `ambient_operations_survive_single_family_intent_routing` exposed the
  reserved host group as an intent choice and then omitted its operations when the router selected
  only `reporting`.
- 2026-07-14: `flux.ambient` is excluded from semantic intent/signal choices and unioned into each
  live exploration catalog. Permission, active `with_tools`, authored ceilings, operation/schema
  budgets, approval, and ordinary dispatch remain authoritative. The marker lives on `ToolSpec`, so
  role-derived children inherit it without mutable host state.
- 2026-07-14: all 136 `codewandler-flux-flow` tests, strict package clippy, formatting, and the four
  `flux-codegate` architecture tests pass.
