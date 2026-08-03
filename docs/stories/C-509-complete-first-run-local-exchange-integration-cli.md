---
id: C-509
title: "Complete the first-run local Exchange and integration CLI tutorial"
pillar: Core
status: ready
priority: 0
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Milestone 1 product exit: one clean Flux install starts local Exchange, connects named custom-origin GitLab and Jira, grants, diagnoses and invokes without holding vendor credentials"
---

# Complete the first-run local Exchange and integration CLI tutorial

## Goal

Make the complete first-run tutorial real from one Flux installation: a person starts the local
Exchange, creates labelled company GitLab and Jira connections from their complete connector-declared
settings, grants their authority, verifies the effective tools and uses them from Flux without Flux
ever receiving vendor credentials.

## Acceptance

- [ ] `flux exchange local start`, `status` and `stop` own one loopback-only local Exchange lifecycle
      with deterministic state and useful idempotency: repeated start/status/stop calls either report
      the same state or a specific machine-readable refusal, never silently create a second service.
- [ ] Human/operator bootstrap is a separate ceremony from the Service Account Flux uses at runtime.
      A one-time Service Account token moves directly from Exchange into an OS credential store (or
      another explicitly reviewed owner-only store), never through argv, environment variables,
      stdout, logs, model-visible output or project configuration; an unavailable secure store
      refuses setup instead of degrading to plaintext.
- [ ] `flux integration connect <connector> --name <name>` consumes Exchange X-125's single
      machine-readable labelled-connection plan backed by Connectors C-508 declarations. Interactive
      prompts and scriptable flags/JSON cover every declared non-secret setting, while vendor secrets
      are accepted only by an Exchange-owned secure surface and Flux keeps neither their values nor a
      connector-specific form schema.
- [ ] A failing-first CLI projection corpus covers GitLab's custom HTTPS `endpoint`, Jira Cloud's
      `site` and account settings, and Zendesk's declared `domain` as well as credential fields. A
      convenience option such as `--endpoint`, `--site` or `--domain` must map to the declaration;
      unknown, omitted required or unprojected fields visibly refuse rather than producing an
      incomplete connection.
- [ ] `flux integration grant` previews and then applies Exchange metadata-selector grants;
      `flux integration list` reports labelled connection and effective-operation state; and
      `flux integration doctor` distinguishes local-process, human-bootstrap, Service Account auth,
      incomplete settings, missing grant, Exchange refusal and Exchange-unavailable outcomes without
      printing credential-shaped data.
- [ ] Every command has a non-interactive JSON mode with no hidden prompt, stable success/refusal
      categories and deterministic exit status. Repeating an identical lifecycle, connection or
      grant request is idempotent; a conflicting connection definition refuses and names the
      connector plus label, never a setting or secret value.
- [ ] A failing-first clean-machine end-to-end test and the user documentation execute this exact
      sequence against a non-published workspace that locally binds Flux, flux-connectors and
      flux-exchange: start Exchange; connect `gitlab/company` with a custom endpoint; connect
      `jira/company` with its Cloud site; preview/apply read grants; list and diagnose effective
      tools; complete one read and one separately approved write from Flux; then stop Exchange.
      The proof asserts that no vendor credential enters Flux output, logs, events, session state or
      persisted configuration and that stopping Exchange removes only official external tools.

## Progress

- (not started)

## Notes

- Cross-repository source: `../flux-roadmap/decisions/0002-declaration-driven-connection-onboarding.md`.
- Depends on Flux C-503, Connectors C-508 and Exchange X-125. C-508 extends the existing connector
  settings foundation in C-87; X-125 closes the complete-settings projection gap left by X-80.
- The independent CLI command/output skeleton may proceed alongside C-503/C-508/X-125. The exact
  clean-machine tutorial is complete only when all four contracts converge in the local three-repo
  acceptance proof.
- The connection name is Exchange's existing tenant-scoped label. It is not a tenant, authority,
  endpoint, credential address or caller-selected runtime placement.
