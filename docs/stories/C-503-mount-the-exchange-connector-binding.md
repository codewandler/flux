---
id: C-503
title: "Mount Exchange as a remote connector binding"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Flux authenticates as a Service Account, sends operation ids/arguments, consumes subscribe/streams/leases, and never receives the Exchange-held credential"
---

# Mount Exchange as a remote connector binding

## Goal

Let an operator select Exchange as the placement for a connector while keeping the Flux program and
connector vocabulary identical to local execution.

## Acceptance

- [ ] A Flux connector client authenticates with Exchange's canonical Service Account API and derives
      no tenant, credential, endpoint or runtime from model-controlled input.
- [ ] One-shot operations use `invoke`; inbound events, streamed results and lease liveness share the
      authenticated connector WebSocket and remain cancellable and bounded.
- [ ] Refused authority, unreachable Exchange, runtime failure and stream loss stay distinct errors.
- [ ] A remote connector appears under the same operation/channel ids as its local placement; Flux
      source does not branch on locality.
- [ ] Failing-first integration tests prove an Exchange-held credential never enters Flux output,
      logs, events or persisted session state.

## Progress

- (not started)

## Notes

- Depends on Exchange X-107 and X-111/X-113…X-120.
- X-107 already delivers canonical Service Account authentication; this story consumes it and does
  not duplicate lifecycle or bearer verification.
