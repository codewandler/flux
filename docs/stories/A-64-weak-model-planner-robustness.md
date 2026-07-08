---
id: A-64
title: Weak-model planner/loop robustness — contract failure + repeated plugin reads
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-002 + F-004: `openai/gpt-4o-mini` failed the planner contract on a trivial plan after repair attempts (OpenRouter+Codex controls passed); a weak OpenRouter model called websearch.search then repeated the same op instead of answering — investigate + add a guardrail, not a hard weak-model guarantee"
---

# Weak-model planner/loop robustness

## Goal
Two weak-model failures the beta caught: (F-002) `openai/gpt-4o-mini` failed the planner contract on
a *trivial* plan even after validator repair attempts, while OpenRouter and Codex controls passed;
and (F-004) a weak OpenRouter model successfully called `websearch.search` but then repeated the
same plugin op instead of answering. Investigate both and add a guardrail / honest guidance — the
goal is graceful failure and a documented capability floor, **not** a guarantee that every
low-capability model passes.

## Why (evidence)
- Beta F-002: "A trivial plan request failed after validator repair attempts. OpenRouter and Codex
  controls worked."
- Beta F-004: "An OpenRouter model successfully called `websearch.search` but repeated the same
  plugin operation instead of answering. Codex completed the same task correctly."

## Acceptance
- [ ] F-004: the loop detects an unproductive repeat of an already-succeeded plugin read and either
      forces a progress/answer step or stops with an honest "could not make progress" instead of
      looping — reusing/extending the existing stall-guards
      ([A-27](A-27-identical-plan-skip-stall-guard.md)/[A-28](A-28-read-coverage-stall-guard.md))
      rather than a new mechanism. Failing-first test reproduces the repeat and asserts it halts.
- [ ] F-002: root-caused — capture *why* gpt-4o-mini's repair loop never converged on a trivial plan
      (which contract clause it kept violating). If a cheap tolerance/repair-hint closes it without
      regressing strong models, add it with a test; otherwise document the capability floor.
- [ ] Docs/provider guidance names a recommended capability floor for the planner and points weak
      models at the failure mode, so users aren't surprised.

## Progress
- 2026-07-08 **DONE** (guardrail + guidance; weak-model parity remains an explicit non-goal).
  - **F-004 (repeated op):** root cause was `classify_spec` (flux-flow) treating a **network** read
    (`Effect::Network`, e.g. `websearch.search`) as `Neutral` → invisible to the read ledger, so the
    A-27/A-28/A-29 stall guards never saw the repeat. Now network reads classify as `TrackedRead`
    (counted, never cache-served — remote results can legitimately change), so an unproductive
    identical repeat trips the no-new-evidence / read-breadth guards and the loop escalates then stops
    with an honest "could not make progress". Pure/reflexive ops (`run_plan`, cognition) stay `Neutral`.
    Test: `network_reads_are_tracked_not_neutral`; the guard-trip behavior on a `TrackedRead` repeat is
    already covered by the existing A-28/A-29 tests.
  - **F-002 (planner contract):** documented a **model capability floor** in
    `website/docs/agent/providers.md` (weak/small models can fail the `emit_plan` contract even on
    trivial requests; route a capable model at the planning turn). A precise root-cause of
    `gpt-4o-mini`'s repair-loop divergence needs a live repro (`FLUX_PLANNER_TRACE=1` against that
    model); left as a documented floor rather than chasing universal weak-model pass.

## Notes
- Lowest-priority beta cluster (rec #7). Weak-model parity is explicitly a non-goal of the epic; the
  deliverable is a guardrail + guidance, not universal pass.
- Related: parse-resilience epic ([parse-resilience](../designs/parse-resilience.md)) already hardened
  arg-JSON handling for qwen/deepseek — this is the loop-behavior sibling.
- Epic: [beta-hardening](../designs/beta-hardening.md).
