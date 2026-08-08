---
id: C-575
title: "Record immutable causal resource-usage receipts"
pillar: Core
status: ready
epic: resource-accounting
design: docs/designs/resource-accounting.md
areas: [flux-events, flux-runtime, flux-flow, flux-system]
note: "one span tree for tokens/calls, wall/CPU, network time/bytes, process/tool/artifact resources and validation"
priority: 11
---

# Measure work once at the owning boundary

## Goal

Record small immutable causal spans for the resources Flux can actually observe so budgets, bills,
Fleet control and usage views share one measurement source.

## Acceptance

- [x] Failing first, one fixture model call plus guarded network/process/tool work has token events
      and wall time in separate surfaces but no common causal receipt or honest CPU/network/byte
      coverage. The fixed run emits one root/span tree with stable ids and parent links.
- [x] The versioned receipt carries request/result, span/parent, agent/session/backend/loop phase,
      timestamps/precision, measurements, monetary facts, source/freshness/coverage and optional
      correction identity. It is append-only and idempotent across event retry.
- [x] Measurement catalogue covers model calls and every Usage tier; runtime wall time, loop/tool/
      report/retry counts; process CPU/RSS/output where owned; guarded network DNS/connect/TLS/TTFB/
      transfer plus bytes; measured file/artifact bytes; capacity occupancy/queue; and targeted/
      review/gate command resources.
- [x] Unsupported/not-reported/not-attributable is typed per dimension. An in-process or foreign
      backend never emits zero CPU/RSS/network merely because it lacks an honest meter.
- [ ] `CallUsage` remains canonical and legacy `TurnEnded.usage` only fills uncovered history.
      Provider-reported and estimated money retain basis/coverage; independent child receipts are not
      double-counted through an already-inclusive parent total.
- [x] Receipts contain no prompt/answer/reasoning, tool arguments/results, command output, file
      content, secret-bearing URL or network payload. Labels and payload sizes are bounded/redacted
      before persistence.
- [x] Hermetic tests cover nested sub-agents, one Fleet writer/reviewer/rework path, an owned child
      process, guarded HTTP fixture, unsupported foreign metrics, cancellation and correction.

## Progress

- The ledger is in: `flux_events::receipt` (the receipt vocabulary, the 36-dimension catalogue, the
  backend/family absence table, `span_tree`) plus three `EventStore` methods —
  `record_resource_span`, `resource_receipts`, `resource_history` — over a `resource:<root-id>`
  ad-hoc stream riding `EventKind::Custom`, exactly as A-107 memory does. `retention.rs` gained the
  matching `Retained` row, without which the C-231 gate fails.
- `crates/flux-events/tests/resource_receipts.rs` is the specification and was taken verbatim from
  `f0e05f01` (a rescued failing-first test from wave-745). It was proved failing at the merge base
  — 29 compile errors, the module and every method absent — and no assertion in it was weakened.
- **What is NOT done, and is the obvious next step:** nothing *produces* receipts yet. The story's
  `areas` name `flux-runtime`, `flux-flow` and `flux-system`, and none of them is instrumented —
  the model-call seam, the guarded transport, the guarded process runner and the tool dispatcher
  still record nothing. This commit is the ledger and its conformance suite; the call sites that
  fill it are follow-on work, and until they exist the catalogue is a promise rather than a bill.
- Acceptance 5 is deliberately left unticked. Its money half holds (`ChargeBasis`/`PriceCoverage`
  survive persistence, an unpriced dimension carries no charge at all, and `CallUsage` is untouched
  and still canonical). Its non-double-counting half does not: no rollup exists here to be correct.
  `span_tree` guarantees every receipt appears exactly once, and `Dimension`'s doc records which
  token tiers are subsets of others, which is what C-576's rollup needs — but C-576 owns that rule,
  per the design's own delivery order.

## Notes

- This instrumentation is the measurement foundation C-571 and C-573 consume.
