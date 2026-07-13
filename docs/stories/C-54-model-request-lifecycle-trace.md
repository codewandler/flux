---
id: C-54
title: Trace model request shape and latency milestones end to end
pillar: Core
status: done
note: "2026-07-13 hardening: smaller plugin context did not explain the remaining 20s+ turn latency; need wire-level request/stream evidence before tuning."
---

# Trace model request shape and latency milestones end to end

## Goal

Make every native model consultation explainable without ad-hoc instrumentation: what request shape
was serialized, how long body/auth/connect/retry/first-response/stream phases took, which content
kind arrived first, and what usage the provider reported.

## Acceptance

- [x] `FLUX_MODEL_TRACE=1` emits a one-line request summary and one-line completion record with a
      correlation id and monotonic milestones (response headers, first chunk, first thinking/tool/
      text, usage, done, total).
- [x] `FLUX_MODEL_TRACE=full` additionally prints the exact credential-free JSON body passed to the
      transport, with a conspicuous opt-in data-sensitivity warning in docs/help.
- [x] Trace mode does not alter request bytes, prompt-cache ordering, retries, stream decoding, or
      error propagation; disabled mode adds no per-chunk work.
- [x] Planner, completion, compaction, cognition, and sub-agent requests all traverse the same trace
      seam; request records identify reasoning effort/thinking and cache-segment sizes.
- [x] A live same-task low/high effort probe records usage, reasoning tokens, TTFT, total latency,
      planner repairs, and answer correctness.
- [x] The measured dominant latency source is reduced or recorded as a provider/model constraint
      with a concrete routing/default recommendation.
