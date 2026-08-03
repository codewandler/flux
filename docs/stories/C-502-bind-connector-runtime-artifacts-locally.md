---
id: C-502
title: "Bind connector runtime artifacts through Flux's guarded system"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "one local connector host dispatches http/socket/process/container/plugin plans through flux-system; no vendor-specific match and no bypass around Executor::dispatch"
---

# Bind connector runtime artifacts locally

## Goal

Load a connector bundle and execute its declared runtime locally through Flux's existing safety
envelope and guarded IO, so rich connectors remain fully usable without Exchange.

## Acceptance

- [ ] The host consumes the closed zero-IO plan published by flux-connectors C-504 and exhaustively
      binds `http`, `socket`, `process`, `container`, `plugin` and `remote` to generic mechanisms.
- [ ] Every model-facing operation still enters `Executor::dispatch`; process and network effects use
      the single guarded `flux-system` paths and declared effects/capabilities.
- [ ] Runtime, artifact, endpoint authority and credential reference come from the connector/operator
      binding and cannot be selected by an operation caller.
- [ ] Signed runtime artifacts are verified before execution; an absent or incompatible artifact is a
      named refusal, never an ambient `PATH` fallback.
- [ ] Failing-first tests run representative HTTP, plugin, socket and container plans and prove a
      vendor-specific dispatcher is unnecessary.

## Progress

- (not started)

## Notes

- Depends on C-394/C-397/C-435 and flux-connectors C-497/C-498/C-504.
- C-399's remote guarded-IO backend is already delivered.
