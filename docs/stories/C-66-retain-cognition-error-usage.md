---
id: C-66
title: Retain cognition usage on provider errors
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: usage accumulated before a declared stream failure is discarded and never billed to the run
---

# Retain cognition usage on provider errors

## Goal

Record model usage emitted before a cognition stream fails or is cancelled, without changing the
original provider error or double-counting successful calls.

## Acceptance

- [x] Cognition's stream collector returns accumulated `Usage` independently from the call's
      `Result`, matching the already-proven `flux-flow` result-plus-usage pattern without creating an
      L3 sibling dependency.
- [x] Failing-first `Usage → declared provider error` coverage preserves and records usage exactly
      once while returning the original error unchanged.
- [x] Failing-first `Usage → cancellation/drop` coverage records only observed usage and retains the
      correct cancellation terminal state.
- [x] Successful and zero-usage calls retain current results and do not double-count observations or
      execution totals.
- [x] SDK `ExecutionResult`, event projections, cost summaries, and sub-agent aggregation include the
      retained usage consistently.

## Progress

- 2026-07-14 — Separated cognition call results from accumulated usage and added drop-time
  accounting. Provider-error, cancellation, zero/success, engine turn/cost projection, SDK, and
  sub-agent tests prove usage is retained exactly once without changing terminal outcomes.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Residual of [D-150](D-150-flow-execution-usage.md), which covered successful cognition usage.
