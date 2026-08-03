---
id: C-504
title: "Prove each legacy adapter through Exchange before deletion"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "a reusable frozen-fixture harness compares each legacy plugin with its Exchange connector replacement before that plugin is deleted"
---

# Prove each legacy adapter through Exchange before deletion

## Goal

Make adapter retirement evidence mechanical by freezing each legacy plugin's observable contract and
requiring its connector replacement to pass that contract through Exchange before deletion.

## Acceptance

- [ ] The conformance inventory from flux-connectors C-505 maps every legacy adapter to frozen
      fixtures and its Exchange connector replacement without omitted or duplicate adapters.
- [ ] Observable results, declared errors, effects, approval consequences and refusal behavior agree;
      transport diagnostics may differ only where the replacement contract says they must.
- [ ] The reusable harness starts before the first migration wave and accumulates HTTP, process/plugin,
      socket and container cases as their Exchange runtimes and lifecycle surfaces become available.
- [ ] Each adapter's evidence is an independent release prerequisite, so C-505 can delete it in the
      same release train without waiting for a global big-bang cutover.
- [ ] No fixture invokes an official connector locally or treats a local fallback as expected behavior.

## Progress

- (not started)

## Notes

- This is legacy-versus-replacement conformance, not placement parity. Exchange is the replacement's
  only official execution path.
