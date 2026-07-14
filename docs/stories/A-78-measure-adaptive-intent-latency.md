---
id: A-78
title: Measure and reduce adaptive intent latency
pillar: Agent
status: done
design: docs/designs/adaptive-intent-latency.md
note: "3+5 cross-model funnel; adopt a lean intent default only with correctness and call-count parity"
---

# Measure and reduce adaptive intent latency

## Goal
Use the shipped redacted stage telemetry to reduce the mandatory intent-stage delay without trading
away grounding, routing correctness, or cross-model reliability.

## Acceptance
- [x] A reproducible evaluator records total/startup time, stage duration and TTFT, calls, repairs,
      usage/cache, schema bytes, approval wait, and execution time without persisting full prompts
      or private reasoning.
- [x] Three-trial screening compares baseline, 512-token, and low-effort/512-token intent policy on
      Codex gpt-5.5, Gemini 3.5 Flash, DeepSeek V4 Flash Nitro, and GPT-5-mini.
- [x] Baseline and the provisional winner receive five fresh trials per model on pure conversation,
      live time, and adversarial support retrieval; evaluator turns remain explicitly capped at 12.
- [x] A universal default changes only when the exact confirmation/Slack matrix is present, every
      answer passes, repairs/calls do not increase, median intent latency improves at least 20%,
      greeting/time improve at least 10%, and support latency does not regress by more than 5%.
- [x] Gemini 3.5 Flash as an OpenRouter-only intent override is screened under a DeepSeek parent,
      advances only if it qualifies, and never becomes an automatic cross-provider default.
- [x] A Bitcoin-to-Slack approval-denial smoke runs on every model, selects Slack, and never executes
      a write; a pre-approval provider-schema failure is recorded rather than hidden.
- [x] Results and caveats are recorded in the design and self-improvement status; an unqualified
      candidate is rejected without changing defaults.

## Progress
- 2026-07-14: protocol fixed before implementation; A-77 budget coherence runs first so hidden
  ceilings cannot contaminate latency attribution.
- 2026-07-14: 36/36 screening turns passed. The paired 120-turn confirmation rejected the
  512-token candidate: no model reached the 20% intent target, GPT-5-mini quality regressed, and
  several end-to-end medians worsened. Shipped intent defaults are unchanged.
- 2026-07-14: all Slack smokes selected Slack and executed no write; Gemini failed before approval
  because its endpoint rejected valid-but-nonportable array schemas. Follow-up A-81 owns that
  structural provider issue.
- 2026-07-14: post-release review made matrix completeness a precondition for the keep gate; exact
  per-key coverage rejects missing, duplicate, stale, and header-only confirmation/Slack results.

## Notes
- This is targeted shipped-harness hardening, not a restart of the paused autonomous improvement
  loop and not a terminal-bench headline-gain claim.
- Design: [adaptive-intent-latency.md](../designs/adaptive-intent-latency.md).
