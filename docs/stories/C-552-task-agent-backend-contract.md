---
id: C-552
title: "One TaskAgentBackend separates agent execution from fleet membership"
pillar: Core
status: backlog
epic: task-agent-backends
design: docs/designs/task-agent-backends.md
areas: [flux-orchestrate, flux-runtime, flux-cli]
note: "follow-up after local V1 — lifecycle/steering/receipts are generic; fleet remains backend-agnostic"
---

# One TaskAgentBackend separates agent execution from fleet membership

## Goal

Define one typed execution backend that a fleet admission can bind without coupling scheduling to a
specific provider or CLI harness.

## Acceptance

- [ ] The port covers capability discovery, start, accepted/delivered/completed steering, status,
      cancellation, resume and terminal result receipts with stable worker/session ids.
- [ ] Fleet admission stores a backend binding separately from identity, BoardRef, mode, capabilities,
      fences and lease; changing backend never silently changes membership authority.
- [ ] Durable receipts and event ids, not stdout prose, prove lifecycle transitions across restart.
- [ ] Resource limits, argv-only launch inputs, redaction and permission subjects are explicit.
- [ ] Native Flux sub-agents implement the contract without regressing V1; mock conformance tests run
      unmodified against every backend.
- [ ] Remote transport and individual CLI harness adapters remain outside this story.
