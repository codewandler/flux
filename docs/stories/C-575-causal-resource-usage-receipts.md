---
id: C-575
title: "Record immutable causal resource-usage receipts"
pillar: Core
status: ready
epic: resource-accounting
design: docs/designs/resource-accounting.md
areas: [flux-events, flux-runtime, flux-flow, flux-system]
note: "one span tree for tokens/calls, wall/CPU, network time/bytes, process/tool/artifact resources and validation"
---

# Measure work once at the owning boundary

## Goal

Record small immutable causal spans for the resources Flux can actually observe so budgets, bills,
Fleet control and usage views share one measurement source.

## Acceptance

- [ ] Failing first, one fixture model call plus guarded network/process/tool work has token events
      and wall time in separate surfaces but no common causal receipt or honest CPU/network/byte
      coverage. The fixed run emits one root/span tree with stable ids and parent links.
- [ ] The versioned receipt carries request/result, span/parent, agent/session/backend/loop phase,
      timestamps/precision, measurements, monetary facts, source/freshness/coverage and optional
      correction identity. It is append-only and idempotent across event retry.
- [ ] Measurement catalogue covers model calls and every Usage tier; runtime wall time, loop/tool/
      report/retry counts; process CPU/RSS/output where owned; guarded network DNS/connect/TLS/TTFB/
      transfer plus bytes; measured file/artifact bytes; capacity occupancy/queue; and targeted/
      review/gate command resources.
- [ ] Unsupported/not-reported/not-attributable is typed per dimension. An in-process or foreign
      backend never emits zero CPU/RSS/network merely because it lacks an honest meter.
- [ ] `CallUsage` remains canonical and legacy `TurnEnded.usage` only fills uncovered history.
      Provider-reported and estimated money retain basis/coverage; independent child receipts are not
      double-counted through an already-inclusive parent total.
- [ ] Receipts contain no prompt/answer/reasoning, tool arguments/results, command output, file
      content, secret-bearing URL or network payload. Labels and payload sizes are bounded/redacted
      before persistence.
- [ ] Hermetic tests cover nested sub-agents, one Fleet writer/reviewer/rework path, an owned child
      process, guarded HTTP fixture, unsupported foreign metrics, cancellation and correction.

## Progress

- (not started)

## Notes

- This instrumentation is the measurement foundation C-571 and C-573 consume.
