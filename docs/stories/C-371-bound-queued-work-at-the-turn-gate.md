---
id: C-371
title: Bound queued work at the turn gate
pillar: Core
status: backlog
epic: serving-surface-and-turn-outcome-residuals
design: docs/designs/serving-surface-and-turn-outcome-residuals.md
note: "FlowEngine::turn_gate is one mutex per engine, so one principal's slow turn head-of-line-blocks every other principal; per-principal in-flight limits do not isolate the resource that is actually scarce. Queue depth is unbounded on every ingress"
---

# Bound queued work at the turn gate

## Goal

Make the per-principal concurrency limit mean something by bounding the thing every request
ultimately contends for.

## Acceptance

- [ ] Waiting work at `FlowEngine::turn_gate` (`crates/flux-flow/src/engine.rs:182`, acquired at
      `:710`/`:728`) is bounded — by depth, by wait deadline, or by making the gate per-realm rather
      than per-engine.
- [ ] Exceeding the bound produces a typed limit response with an `X-Flux-Limit` value, not an
      unbounded wait.
- [ ] Failing-first: with one principal holding a slow turn, a second principal's request either
      proceeds or is rejected promptly — it does not queue indefinitely.
- [ ] The interaction with the existing `max_inflight_per_key` × bucket-cardinality ceiling
      (4 × 4096) is documented, so the effective bound is stated rather than inferred.

## Progress

- 2026-08-01 — filed from validation of SRV-02. This is the "queue" limb of the original claim and
  is genuinely unaddressed.

## Notes

- Changing the gate's granularity touches turn serialization semantics; treat it as a design
  decision, not a parameter tweak.
