---
id: C-571
title: "Fleet budgets reserve and settle across fleet, wave, task, agent and loop"
pillar: Core
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-runtime, flux-flow, flux-orchestrate, flux-cli, flux-tui]
depends_on: [C-542, C-570, C-575]
note: "narrow-only hierarchical targets/limits with durable reservation, idempotent settlement, warnings and resumable exhaustion"
---

# Bound the whole Fleet, not only each worker

## Goal

Allocate one honest budget tree from Fleet through wave, assignment, agent, turn and loop segment so
five individually bounded workers cannot oversubscribe an unbounded aggregate.

## Acceptance

- [ ] Failing first, concurrent workers each satisfy today's per-agent limits while their aggregate
      exceeds a configured Fleet token/model-call limit. The fixed path reserves before start and
      refuses or queues the overcommitted assignment deterministically.
- [ ] Budget targets and hard limits compose across `fleet -> wave -> assignment/task -> agent/
      session -> turn -> loop phase/model segment/tool dispatch`. A child receives the intersection
      of profile limits, delegated reservation and every ancestor's remaining ceiling; resume and
      config reload cannot widen it silently.
- [ ] Durable reservation ids and idempotent settlement survive restart, cancellation, retry and
      duplicate terminal receipts. Unused reservation returns to its parent; usage is charged once
      and rollups do not double-count child spend.
- [ ] V1 dimensions cover wall time, model calls, input/output/total tokens, tool dispatches, loop
      iterations, live agents/concurrent tool calls, review/rework attempts and report/output/
      evidence bytes. Each dimension states its safe enforcement boundary.
- [ ] Approaching a target emits one bounded C-570 warning. A hard exhaustion yields a typed result
      naming scope/dimension/spent/reserved/limit and checkpoints resumably where safe. Raising a
      limit is an explicit revisioned authority action.
- [ ] CPU/RSS/process-output/filesystem/network dimensions are advertised as enforced only when the
      selected backend owns a real meter/limit; unsupported and observation-only states are explicit.
- [ ] C-130 priced/reported currency spend plugs into the same ledger, including conservative
      treatment of unknown prices, without blocking the non-monetary first slice.
- [ ] JSON status and later TUI projections read the ledger and remain bounded for five active
      workers and long history; no surface independently recalculates totals.
- [ ] The projection labels source/freshness and reported/estimated/unsupported state and groups
      usage by Fleet, wave, assignment, worker and loop phase so C-573 can consume it without reading
      raw transcripts or re-aggregating rolled-up totals.

## Progress

- (not started)

## Notes

- C-542 owns the shared local time/token target-versus-limit vocabulary and live projection.
- C-575 is the one physical measurement receipt source; this story adds reservations, hard-limit
  enforcement and hierarchical settlement rather than another usage collector.
- Existing `ResourceLimits` tree census and Fleet's two-rework ceiling become dimensions, not
  competing budget systems.
