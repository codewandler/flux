---
id: C-727
title: "Instrument the runtime seams that produce resource receipts"
pillar: "Core"
status: backlog
priority: 2
epic: resource-accounting
areas: [flux-runtime]
note: "C-575 landed the receipt ledger, the 36-dimension catalogue and its conformance suite, but nothing writes to it: the model-call seam, the guarded transport, the guarded process runner and the tool dispatcher all record nothing. Until these produce receipts the catalogue is a promise rather than a bill"
---

# Instrument the runtime seams that produce resource receipts

## Goal

C-575 landed the resource-receipt ledger, its 36-dimension catalogue and a conformance suite — and
**nothing writes to it.** The model-call seam, the guarded transport, the guarded process runner and
the tool dispatcher each record nothing, so every query against the ledger returns an empty answer
that is indistinguishable from a run that cost nothing.

Until those seams emit receipts the catalogue is a promise rather than a bill. Every downstream
consumer — budget ceilings, per-wave cost attribution, the `COST-wave-*` reports — is reading from an
empty table and cannot tell that it is empty.

## Acceptance

- [ ] Each of the four seams emits a receipt: the model call, the guarded transport, the guarded
      process runner, and the tool dispatcher. Name the exact call site for each in the Progress
      section, so the census is checkable rather than asserted.
- [ ] A receipt carries enough to attribute cost to the thing that incurred it — at minimum the
      dimension from C-575's catalogue, the quantity, and the run/turn it belongs to.
- [ ] **Failing-first**: a test that executes one turn through each seam and asserts the ledger is
      non-empty afterwards. It must fail before the instrumentation, which is the whole point — an
      empty ledger currently passes every existing test.
- [ ] Emission cannot fail the operation it measures. A receipt that cannot be written is reported,
      never propagated as an error into the model call or the process spawn.
- [ ] The conformance suite C-575 shipped is run against real emitted receipts rather than fixtures,
      so the catalogue and the emitters cannot drift apart silently.
- [ ] A dimension in the catalogue that no seam emits is reported by a test, so the next unwritten
      dimension is a failure rather than another empty column.
- [ ] Full gate green: `scripts/release-full-gate.sh`.
