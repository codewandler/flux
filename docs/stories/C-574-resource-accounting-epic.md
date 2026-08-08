---
id: C-574
title: "Every result carries a realistic attributable resource bill (epic)"
pillar: Core
status: backlog
epic: resource-accounting
design: docs/designs/resource-accounting.md
areas: [flux-events, flux-runtime, flux-orchestrate, flux-cli]
note: "measure physical usage and money provenance once; attribute causally to request/story; roll up without double-counting"
---

# Put a truthful price tag on produced work

## Goal

Make the measured resources and realistic monetary coverage used to produce each result inspectable
and attributable, then aggregate the same immutable receipts by story, epic, worker, wave and Fleet.

## Acceptance

- [ ] C-575 records one bounded causal receipt vocabulary for model, runtime, process, network,
      filesystem/artifact, capacity and validation resources with explicit source/coverage.
- [ ] C-576 binds receipts to root request/result, worker/wave and exact BoardRef/assignment revision,
      and defines exclusive, inclusive and explicitly allocated totals without double-counting
      children or shared overhead.
- [ ] C-577 links a compact immutable bill as Board evidence and exposes bounded JSON/human queries
      and rollups by request/story/epic/worker/wave/repository/Fleet.
- [ ] Provider-reported, table-estimated, subscription-equivalent, operator-rated and unpriced money
      remain separate. Token/CPU/network/byte usage remains visible when no monetary rate exists;
      unknown is never zero.
- [ ] C-571 budgets, C-573 adaptive policy and C-518 Usage Observatory consume these receipts rather
      than adding competing accounting paths.
- [ ] Accounting never loads or persists prompt, answer, tool argument, command output, file content
      or network payload bodies and never gives workers authority to rewrite attribution.

## Progress

- 2026-08-05 — filed from the operator request for realistic per-story resource cost after reconciling
  the narrower existing C-518/C-520 model-token/cost history contracts.

## Notes

- This epic is independent of whether a hard budget is configured. Accounting remains useful when
  execution is intentionally unbounded.
