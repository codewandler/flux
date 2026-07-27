---
id: C-89
title: Let process access carry network and write effects in the authority contract
pillar: Core
status: done
design: docs/designs/integration-plugins.md
note: the typed-authority validator rejected every process-mediated plugin op (kubernetes/aws), so `flux run` aborted at registration with "declares a network effect without network, browser, or provider access"
---

# Let process access carry network and write effects in the authority contract

## Goal
A tool whose reach is an allow-listed subprocess must be able to state that honestly. The typed
authority validator landed in the harness-hardening epic accepted network and write effects only
from network-family or filesystem-family access, which made every CLI-driven integration an invalid
declaration — and since registration is all-or-nothing, one such op took down the whole session.

## Acceptance
- [x] `authority_requirements_from_declaration` accepts `AccessKind::Process` as a carrier for
      `Effect::Network`, and for `Effect::Write` (pinning `operation.mutate` on the operation).
      Failing-first test: `flux-runtime::tests::process_access_carries_network_and_write_effects`.
- [x] Every shape the plugin host projects from process-only capabilities is a valid contract,
      including the effect-less `[Process, Network]` fallback in `plugin_tool_spec`. Failing-first
      test: `flux-plugin::host::tests::process_only_capabilities_project_valid_authority_contracts`.
- [x] The `process.exec` requirement on the named program is still produced, so the envelope names
      a concrete resource and nothing is waved through.
- [x] Workspace gate green.

## Progress
- **Done (2026-07-27).** Both carriers added in `crates/flux-runtime/src/lib.rs`; error text now
  names process among the accepted access kinds. Regression test added on the plugin-host side so
  the manifest→ToolSpec projection is checked against the validator that will judge it at load.

## Notes
- Reported from the field: after `task install`, `flux run` failed with
  `invalid authority contract for kubernetes.secret.read from plugin:kubernetes`. The kubernetes
  plugin declares `process: ["kubectl"]` and five ops with `Effect::Network`
  (`secret.read`, `portforward.start`, `deployment.scale`, `deployment.restart`, `pod.exec`);
  `aws` has the same shape.
- The check itself came from `13ecbf4 feat(runtime): enforce typed authority requirements`. The
  plugins were not wrong — the floor moved under them and no test covered manifest projection
  against the validator.
- The built-in `git_push` tool (`crates/flux-tools/src/lib.rs`) shows the convention that hid the
  gap: it declares *both* `Process` and `Network` access, so it never exercised the process-only
  path.
- Follow-on: [C-90](C-90-process-capability-argument-constraints.md) — the process gate matches
  `argv[0]` only, so `kubectl` grants everything the kubeconfig allows regardless of the op's
  declared risk.
