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

- [ ] `flux exchange local start`, `status` and `stop` consume C-510's channel-selected, verified and
      process-owned local Exchange lifecycle. This story adds no second downloader, verifier,
      executable discovery rule or lifecycle state machine.
- [ ] Human/operator bootstrap is a separate ceremony from the Service Account Flux uses at runtime.
      A one-time Service Account token moves over an Exchange-owned direct TTY, browser or inherited
      file-descriptor handoff into an OS credential store (or another explicitly reviewed owner-only
      store). Flux never parses or buffers the token, and neither bootstrap nor later runtime accepts
      it through argv, an environment variable, JSON, stdin shared with Flux, stdout, logs,
      model-visible output or project configuration. An unavailable secure store or handoff refuses
      setup instead of degrading to plaintext; this Milestone 1 path explicitly supersedes any
      environment-token bootstrap that C-503 used while proving the lower-level client.
- [ ] `flux integration connect <connector> --name <name>` consumes Exchange X-125's single
      machine-readable labelled-connection plan backed by Connectors C-87/C-508 declarations. Flux
      accepts exactly `exchange.connection-plan.v1` before showing a prompt or writing any state and
      refuses every absent, unknown or incompatible plan version. Interactive prompts cover every
      declared non-secret setting; vendor secrets use an Exchange-owned direct TTY, browser or
      inherited file-descriptor handoff, so Flux never parses or receives them and never accepts them
      through argv, environment variables or JSON. Flux keeps neither their values nor a
      connector-specific form schema.
- [ ] A failing-first CLI projection corpus covers GitLab's custom HTTPS `endpoint`, Jira Cloud's
      `site` and account settings, and Zendesk's declared `domain` as well as credential fields. A
      scriptable convenience option such as `--endpoint`, `--site` or `--domain` exists only when it
      maps from a field identity or alias published by the plan; every non-secret field remains
      scriptable through a generic `--field <identity>=<value>` fallback. Flux maintains no vendor
      alias list. Unknown aliases/identities, omitted required or unprojected fields visibly refuse
      before submission rather than producing an incomplete connection.
- [ ] `flux integration grant` first previews and applies a low-risk metadata-selector read grant;
      the tutorial proves a write remains refused under it, then previews and applies a high-risk
      metadata-selector grant and separately asks for the concrete write approval before that write
      executes. `flux integration list` reports labelled connection and effective-operation state;
      `flux integration doctor` distinguishes local-process, human-bootstrap, Service Account auth,
      incomplete settings, missing grant, Exchange refusal and Exchange-unavailable outcomes without
      printing credential-shaped data. No grant is an operation-name allowlist.
- [ ] Every command has a non-interactive JSON mode with no hidden prompt, stable success/refusal
      categories and deterministic exit status. Repeating an identical lifecycle, connection or
      grant request is idempotent; a conflicting connection definition refuses and names the
      connector plus label, never a setting or secret value.
- [ ] A failing-first clean-machine end-to-end test and the user documentation execute this exact
      sequence against both the released clean-machine path and a non-published workspace that locally
      binds Flux, flux-connectors and flux-exchange: install/start the compatible Exchange from
      C-510/X-126; connect `gitlab/company` with a custom endpoint; connect `jira/company` with its
      Cloud site; preview/apply the low-risk read grant; list and diagnose effective tools; complete
      one read; prove a write is refused; preview/apply the high-risk metadata grant; separately
      approve and complete that write from Flux; then stop Exchange. The proof asserts that no vendor
      credential or Service Account token enters Flux output, logs, events, session state or persisted
      configuration and that stopping Exchange removes only official external tools.
- [ ] The local Flux client and Exchange runtime are tested as an HTTP process boundary. Their Rust
      engine dependency lines may differ and are never unified with path/git dependencies or a
      combined Cargo workspace; compatibility comes only from the compatible Exchange release
      selected through C-510's signed channel and the provider protocol versions Flux supports.

## Progress

- 2026-08-04: Started the independently deliverable CLI command/output skeleton from canonical
  Flux `be76b1105926a1f01d81d95c63c79bbbca204400`. Provider-owned connection-plan, release,
  lifecycle, secure-handoff and end-to-end seams remain gated on Exchange X-125 through X-129 and
  Flux C-510.
- 2026-08-04: Landed the dependency-independent partial wave: the closed `exchange local` and
  `integration connect|grant|list|doctor` grammar, generic metadata-selector assignments,
  value-redacted argument diagnostics, and one stable human/JSON outcome projection. Connection
  fields remain withheld until the provider plan can classify them as non-secret. Until the
  provider contracts exist, every command exits deterministically with a value-free `unsupported`
  refusal instead of prompting, accepting a credential/token flag or pretending setup completed.
- 2026-08-04: Added a read-only `ExchangeClient::observe_catalogue` seam over the already-merged
  authenticated effective-catalogue API. It returns only a canonical SHA-256 generation, bounded
  operation identity, Exchange-grammar connection label and admitted state, with closed body-free
  authentication/unavailable/refusal/malformed errors. It deliberately cannot infer incomplete
  settings or consume the transitional
  environment token; CLI orchestration waits for the reviewed secure Service Account store/handoff.
- 2026-08-04: Failing-first evidence covers the command parser and real binary JSON boundary.
  Targeted `flux-cli`, `codewandler-flux-web`, exhaustive command-classifier, formatting and strict
  clippy checks are green. The assembled partial wave also passed the full repository gate before
  publication, without claiming acceptance that remains gated on X-125's connection plan, X-127's
  owner-only state, X-128's readiness contract, X-129's production wire identities/fixtures,
  X-126's signed release artifact/channel and C-510's compatible install/supervision plus secure
  handoff.
- 2026-08-04: Returned the story to `ready` with every acceptance item open. Decision 0003 removes
  externally gated work from the current wave rather than marking a dependency-independent command
  skeleton as complete; resume after the named Exchange and Flux lifecycle contracts ship.

## Notes

- Cross-repository source: `../flux-roadmap/decisions/0002-declaration-driven-connection-onboarding.md`.
- The separately released runtime and compatibility boundary come from
  `../flux-roadmap/decisions/0004-flux-manages-a-verified-local-exchange.md`.
- Depends on Flux C-503 and C-510, Connectors C-87/C-508, and Exchange X-125/X-126. C-508 extends the
  existing connector settings foundation in C-87; X-125 closes the complete-settings projection gap
  left by X-80; X-126 and C-510 supply the separately released, verified local executable.
- The independent CLI command/output skeleton may proceed alongside C-503/C-508/X-125/X-126/C-510.
  The exact clean-machine tutorial is complete only when all six contracts converge in the released
  clean-machine proof and the local three-repository acceptance workspace.
- The connection name is Exchange's existing tenant-scoped label. It is not a tenant, authority,
  endpoint, credential address or caller-selected runtime placement.
