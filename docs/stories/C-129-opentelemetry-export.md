---
id: C-129
title: OpenTelemetry export — turns, ops, and model calls as OTLP spans + metrics
pillar: Core
status: backlog
priority:
epic:
design:
note: "a feature-gated projection over the event store emitting OTLP traces (turn → plan → per-op spans with latency/retry/cost attributes) and metrics (tokens, spend, op error rates); serves the PG-backend server audience — OTel is just another projection, per the event-store-unification canon"
---

# OpenTelemetry export — turns, ops, and model calls as OTLP spans + metrics

## Goal
Give server deployments real observability without flux inventing its own dashboards: a
feature-gated projection over the unified event log that emits OTLP traces (turn → plan → per-op
spans carrying latency, retry, and cost attributes) and metrics (tokens, spend, op error rates)
to any OTel collector (Grafana/Tempo etc.).

## Acceptance
- [ ] Feature-gated (`otel` or similar); the default build gains no new dependencies.
- [ ] A recorded run exports a trace whose span tree mirrors the run structure (turn → plan →
  per-op), with cost/latency/retry attributes — asserted against an in-process OTLP collector stub
  in a failing-first test.
- [ ] Metrics: tokens, spend, and op error rates emitted with session/agent attributes.
- [ ] The exporter is a pure consumer of the event store — no new writes, no behavior change to
  execution (behavior-lock test: exporter on/off produces identical run events).
- [ ] Redaction rules apply: no secret-bearing payloads in span attributes.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Home: `flux-server` or a small dedicated module; aligns with the projections canon
  (conversation/run-trace/metrics are projections — OTel is one more).
