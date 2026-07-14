---
id: A-77
title: Make adaptive model and outer-loop budgets coherent
pillar: Agent
status: done
design: docs/designs/adaptive-budget-coherence.md
note: "remove the hidden 12-round clamp; default logical model calls and outer-loop iterations to 50 with CLI/config control"
---

# Make adaptive model and outer-loop budgets coherent

## Goal
Replace overlapping hard-coded adaptive limits with one visible logical model-call budget, while
making the separate authored decision/batch repeat configurable on every normal agent surface.

## Acceptance
- [x] Failing-first staged coverage proves a normal adaptive run configured for 50 calls may use
      one intent call plus 49 exploration calls, and refuses a 51st provider request.
- [x] Failing-first `ai_segment` coverage proves `max_rounds: 50` is honored exactly instead of
      failing at the hidden 12-round clamp.
- [x] The logical model-call default is 50 through `AdaptiveLoopPolicy`, CLI, config, SDK, roles,
      apps, and sub-agents; per-stage `max_calls` can still narrow it.
- [x] The authored outer-loop repeat defaults to 50 and is configurable through
      `[agent] max_iterations`, `--max-iterations`, `AgentSpec`, and the SDK builder, with
      CLI > project > user > default precedence.
- [x] Zero/overflow values fail before a provider request or Flux-Lang execution, and decision
      suspension/resume cannot reset either logical budget.
- [x] Public docs, both changelogs, and self-improvement status distinguish model calls,
      `ai_segment.max_rounds`, and outer-loop iterations.
- [x] The complete repository gate and exact `task install` pass.

## Progress
- 2026-07-14: opened after inspection showed that normal exploration and `ai_segment` both clamp
  native rounds to 12 even when the public logical budget or authored node requests more.
- 2026-07-14: removed the duplicate clamp, raised both visible defaults to 50, added
  `--max-iterations` / `[agent] max_iterations`, and pinned the 50/51 and authored-segment bounds in
  failing-first tests.
- 2026-07-14: workspace build/test, clippy with warnings denied, formatting, the architecture
  layering gate, and exact `task install` all pass. The install run replaced both `flux` and
  `flux-lsp`; all 110 `flux-system` tests passed in its library-test phase.

## Notes
- The safety envelope is unchanged. Larger bounds permit more cognition; they do not grant an
  operation, bypass approval, or weaken guarded IO.
- Design: [adaptive-budget-coherence.md](../designs/adaptive-budget-coherence.md).
