---
id: C-502
title: "Supersede local official connector execution"
pillar: Core
status: done
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "closed without implementation: Decision 0001 assigns every official integration runtime to Exchange and forbids a local Flux fallback"
---

# Supersede local official connector execution

## Goal

Close the proposed local official-connector host without implementing it, because the accepted
cross-repository topology assigns every official external integration runtime to Exchange.

## Acceptance

- [x] No local connector host, runtime-plan dispatcher, artifact installer, or official integration
      fallback is added to Flux.
- [x] C-500 and C-503 assign Flux only the embedded Exchange client, Service Account authentication,
      effective-catalogue projection, approval, and invocation request.
- [x] Connector-declared runtimes and artifacts remain connector-owned and execute only behind
      Exchange; the caller cannot choose runtime, credential, tenant, or endpoint authority.
- [x] Flux remains useful without Exchange for the language, agent loop and core tools, without
      pretending official external integrations remain available.

## Progress

- 2026-08-03: Superseded without implementation by C-508 and flux-roadmap Decision 0001.

## Notes

- Historical proposal only. Do not restore this as an implementation story; amend the
  cross-repository decision first if the execution topology ever changes.
