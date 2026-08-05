---
id: C-573
title: "Live Fleet metrics drive bounded model, effort and concurrency policy"
pillar: Core
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-runtime, flux-orchestrate, flux-cli, flux-tui]
depends_on: [C-571, C-583]
note: "closed-loop optimization inside authorized caps: freshness-labelled metrics, allowlisted actuators, safe-boundary changes, hysteresis and durable decisions"
---

# Adapt the Fleet without moving its safety boundary

## Goal

Use live total/per-wave/per-task/per-worker metrics to tune model choice, reasoning effort,
concurrency and delegated budget inside an operator-authorized policy, improving cost/deadline
performance without silently changing a worker contract or trading away verified quality.

## Acceptance

- [ ] A bounded live projection reports, at Fleet/wave/assignment/worker/loop-phase scope, wall and
      available CPU time, model calls, input/output tokens, priced/reported cost, reservations,
      queue/throughput, failures, review/rework and gate outcomes. Every value carries source,
      freshness and estimated/reported/unsupported state; unknown cost or CPU is never zero.
- [ ] A revisioned policy declares objectives and immutable constraints: Fleet hard limits/deadline,
      allowed model/provider ladder, minimum model/effort by task kind/risk, concurrency range,
      per-task floors, quality stop-lines, cooldown and hysteresis. The controller cannot raise hard
      caps, add a provider/model, grant tools/roots or weaken review/gates.
- [ ] Actuators cover future task placement/admission, allowed base model and effort, concurrency and
      reservation size. An active worker changes only at a C-570 safe report/yield boundary and only
      when its admitted loop/backend explicitly supports that adjustment; otherwise the change
      applies to a new session/admission.
- [ ] Every decision records policy/input snapshot digests, reason, old/new value, affected scope,
      expected trade-off and acknowledgement. Restart/replay produces the same decision, and a human
      can pin/disable an actuator without editing Fleet state by hand.
- [ ] Stale/noisy metrics do not cause flapping: minimum sample, hysteresis, cooldown, bounded change
      rate and reservation safety are failing-first tested. Concurrent decisions cannot oversubscribe
      C-571's remaining aggregate budget.
- [ ] Quality feedback is host-verified handoff/tests, fresh-review findings, rework rate and gates,
      never worker confidence or prose. A cost-saving change that crosses a configured quality
      stop-line rolls back or parks rather than continuing to optimize spend.
- [ ] Hermetic simulations prove representative policies: approaching a cost cap lowers effort/model
      or concurrency only where allowed; deadline risk increases concurrency within capacity; a
      high-risk/rework-heavy task retains its floor; unsupported CPU/cost data produces no invented
      optimization.
- [ ] CLI JSON and later TUI show current policy, last decisions, pins and why an actuator did or did
      not change. Metrics collection and control remain bounded and never block worker execution.

## Progress

- 2026-08-05 — extracted from the operator proposal after C-571 established the hierarchical metrics
  and budget ledger boundary.

## Notes

- The first controller should be deterministic and replayable. A model may advise on policy later,
  but it cannot be the component that grants itself more money, authority or concurrency.
- Model/effort is part of the admitted execution contract. “Adaptive” means selecting within a
  pre-authorized ladder at a safe boundary, not mutating an opaque live harness.
- C-583 owns the typed manual capacity actuator. This story may drive that same service from policy;
  it does not add a second concurrency mutation path.
