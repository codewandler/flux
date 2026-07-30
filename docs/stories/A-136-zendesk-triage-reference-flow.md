---
id: A-136
title: The runnable Zendesk triage reference workflow
pillar: Agent
status: done
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [examples, flux-lang]
note: "flow RETAINED and provider-free-tested, but NOT runnable until the flux-connectors interop replaces the removed plugin"
---

# The runnable Zendesk triage reference workflow

## Goal

Ship `examples/zendesk.triage.flux` as the first end-to-end demonstration that Flux-Lang can own a
deterministic integration workflow while using a model only inside bounded analysis steps.

## Acceptance

- [x] Named `setup`, `triage(query)`, `brief(ticket_id)`, and `eod(query)` flows compile and run via
      L-92's `flux run … --entry` surface.
- [x] The module demonstrates bounded retry, parallel gathering, timeout, fallback, and context
      budgeting; provider failure returns gathered Zendesk evidence.
- [x] Static plan inspection proves no Zendesk write operation is reachable and model output never
      supplies an operation name or mutation input.
- [x] Failing-first parser/analyzer fixtures and an offline mock-provider scenario cover all four
      entrypoints without live Zendesk credentials or network.

## Progress

- 2026-07-30 — shipped the four-entrypoint module plus a static read-only operation-set guard and an
  offline test that lowers and executes the exact file with static Zendesk/model operations. The
  provider-failure leg proves gathered ticket evidence survives cognition failure.
