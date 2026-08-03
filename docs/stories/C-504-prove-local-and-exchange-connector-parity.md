---
id: C-504
title: "Prove connector parity across local and Exchange placements"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "the same Flux program and connector contract run against local and hosted backends; observable results/errors/streams agree and unsupported placement is explicit"
---

# Prove connector parity across local and Exchange placements

## Goal

Make locality a deployment choice rather than a second integration implementation by running one
connector conformance contract against Flux's local host and Exchange's remote binding.

## Acceptance

- [ ] The conformance corpus from flux-connectors C-505 drives both placements without backend-only
      expected schemas or operation names.
- [ ] Results, declared errors, effects, cancellation, stream termination and lease cleanup agree;
      transport diagnostics may differ only where the contract says they must.
- [ ] Representative HTTP, plugin/process, socket and container connectors are covered, including one
      inbound channel and one long-lived operation.
- [ ] A runtime that a shared Exchange deployment cannot isolate is a tested explicit refusal while
      the same connector remains available locally.
- [ ] The suite is a release prerequisite for removing an official Flux adapter.

## Progress

- (not started)

## Notes

- D-225's SIP two-locality conformance is prior art; the connector suite generalizes that shape.
