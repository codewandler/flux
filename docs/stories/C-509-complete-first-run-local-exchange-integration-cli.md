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

- [ ] C-509 consumes C-510's channel-selected, verified and process-owned local Exchange endpoint and
      `flux.exchange-local-status.v1` status. C-510 exclusively owns local lifecycle selection,
      install, import, cache and quarantine; supervision, control, readiness and liveness; lifecycle
      idempotence; `start|status|stop` semantics; and every lifecycle diagnostic and exit code. C-509
      adds no duplicate lifecycle machinery, outcome or reclassification.
- [ ] Human/operator management authentication is separate from the Service Account Flux uses at
      runtime. Human/operator plan, connection, grant and Service Account minting routes use a
      separately authenticated management surface and cannot be added to the Service-Account-only
      client, whose runtime authority remains limited to effective-catalogue discovery and invoke.
      A host-owned secret resolver contains the Exchange-owned direct TTY, browser or inherited
      file-descriptor handoff that places a one-time Service Account token in an OS credential store
      (or another explicitly reviewed owner-only store). In the final path its bytes may exist only
      inside that resolver and the sensitive Authorization transport. They never enter argv, the
      environment, ordinary diagnostics or JSON, configuration, logs, events, session state or
      model-visible state. An unavailable secure store, resolver or handoff refuses setup instead of
      degrading to plaintext; this Milestone 1 path supersedes C-503's transitional environment-token
      bootstrap.
- [ ] `flux integration connect <connector> --name <name>` consumes Exchange X-125's single
      machine-readable labelled-connection plan backed by Connectors C-87/C-508 declarations. Flux
      accepts exactly `exchange.connection-plan.v1` before showing a prompt or writing any state and
      refuses every absent, unknown or incompatible plan version. Interactive prompts cover every
      declared non-secret setting; vendor secrets use an Exchange-owned direct TTY, browser or
      inherited file-descriptor handoff, so Flux never parses or receives them and never accepts them
      through argv, environment variables or JSON. Flux keeps neither their values nor a
      connector-specific form schema.
- [ ] A failing-first CLI projection corpus covers published connector v0.19.1's GitLab custom HTTPS
      `origin`, Jira Cloud `site` and account settings, and Zendesk `subdomain` as well as credential
      fields. The current derived convenience aliases are `--origin`, `--site` and `--subdomain`
      respectively, and exist only because the plan publishes them for those field identities;
      every non-secret field remains scriptable through a generic
      `--field <identity>=<value>` fallback.
      Flux maintains no vendor alias list. It does not invent `--endpoint` or `--domain` compatibility
      aliases unless Exchange publishes and proves them. Unknown aliases/identities, omitted required
      or unprojected fields visibly refuse before submission rather than producing an incomplete
      connection.
- [ ] `flux integration grant` first previews and applies a low-risk metadata-selector read grant;
      the tutorial proves a write remains refused under it, then previews and applies a high-risk
      metadata-selector grant and separately asks for the concrete write approval before that write
      executes. `flux integration list` reports labelled connection and effective-operation state;
      `flux integration doctor` distinguishes human-bootstrap, Service Account auth, incomplete
      settings, missing grant and Exchange integration-refusal outcomes without printing
      credential-shaped data. It consumes C-510's typed endpoint/status; a local-process or
      Exchange-unavailable lifecycle failure preserves and points to C-510's status and diagnostic
      rather than duplicating or reclassifying it. No grant is an operation-name allowlist.
- [ ] Each C-509-owned integration command has a non-interactive JSON mode with no hidden prompt,
      stable integration-only success/refusal categories and deterministic exit status. Repeating an
      identical connection or grant request is idempotent; a conflicting connection definition
      refuses and names the connector plus label, never a setting or secret value. Lifecycle JSON,
      idempotence, diagnostics and exit status remain exclusively C-510 acceptance.
- [ ] A failing-first clean-machine end-to-end test and the user documentation execute this exact
      sequence against both the released clean-machine path and a non-published workspace that locally
      binds Flux, flux-connectors and flux-exchange: install/start the compatible Exchange from
      C-510/X-126; connect `gitlab/company` with a custom origin; connect `jira/company` with its
      Cloud site; preview/apply the low-risk read grant; list and diagnose effective tools; complete
      one read; prove a write is refused; preview/apply the high-risk metadata grant; separately
      approve and complete that write from Flux; then stop Exchange. The proof asserts that no vendor
      credential or Service Account token enters Flux output, logs, events, session state or persisted
      configuration and that stopping Exchange removes only official external tools. Both journeys
      test the local Flux client and Exchange runtime across the real HTTP process boundary. Their
      Rust engine dependency lines may differ and are never unified with path/git dependencies or a
      combined Cargo workspace; compatibility comes only from the Exchange release selected through
      C-510's signed channel and the provider protocol versions Flux supports.

## Progress

- 2026-08-04: Started the independently deliverable CLI command/output skeleton from canonical
  Flux `be76b1105926a1f01d81d95c63c79bbbca204400`. Provider-owned connection-plan, release,
  lifecycle, secure-handoff and end-to-end seams were dependency-gated at that point.
- 2026-08-04: Landed the dependency-independent partial wave: the closed `exchange local` and
  `integration connect|grant|list|doctor` grammar, generic metadata-selector assignments,
  value-redacted argument diagnostics, and one provisional human/JSON outcome projection. Connection
  fields remain withheld until the provider plan can classify them as non-secret. Until the
  provider contracts exist, every command exits deterministically with a value-free `unsupported`
  refusal instead of prompting, accepting a credential/token flag or pretending setup completed.
  That projection is temporary dependency gating, not stable or final lifecycle semantics.
- 2026-08-04: Added a read-only `ExchangeClient::observe_catalogue` seam over the already-merged
  authenticated effective-catalogue API. It returns only a canonical SHA-256 generation, bounded
  operation identity, Exchange-grammar connection label and admitted state, with closed body-free
  authentication/unavailable/refusal/malformed errors. It deliberately cannot infer incomplete
  settings or consume the transitional
  environment token; CLI orchestration waits for the reviewed secure Service Account store/handoff.
- 2026-08-04: Failing-first evidence covers the command parser and real binary JSON boundary.
  Targeted `flux-cli`, `codewandler-flux-web`, exhaustive command-classifier, formatting and strict
  clippy checks are green. The assembled partial wave also passed the full repository gate before
  publication, without claiming C-509 acceptance.
- 2026-08-04: Returned the story to `ready` with every acceptance item open. Decision 0003 removes
  externally gated work from the current wave rather than marking a dependency-independent command
  skeleton as complete.
- 2026-08-04: Post-provider audit recorded Exchange X-125, X-127, X-128 and X-129 delivered at
  `4e398a73dcb8de17466cbedea77122dd489bed4f`, X-126 active and Flux C-510 ready. C-509 can now consume
  X-125's strict plan/management contract, while the released clean-machine journey remains gated on
  X-126 and C-510. The existing generic `unsupported` response remains only that temporary dependency
  gate; C-510, not C-509, will supply the lifecycle contract it replaces.

## Notes

- Cross-repository source: `../flux-roadmap/decisions/0002-declaration-driven-connection-onboarding.md`.
- The separately released runtime and compatibility boundary come from
  `../flux-roadmap/decisions/0004-flux-manages-a-verified-local-exchange.md`.
- C-509's direct contract inputs are Flux C-503's delivered Service Account catalogue/invoke client,
  Flux C-510's local endpoint/status, Connectors C-87/C-508's declarations and Exchange X-125's
  human/operator plan and submission contract. X-125 is delivered; C-509 consumes its fixture and
  owns the strict dynamic CLI projection, secure vendor-secret handoff and integration behavior.
- The local-release chain is transitive through C-510: C-510 owns all Flux lifecycle behavior and
  consumes Exchange X-126/X-128; X-126 in turn is gated by the delivered X-125/X-127/X-128/X-129
  provider contracts. C-509 does not acquire any lifecycle or release ownership from that chain.
  Its exact released clean-machine journey waits for X-126 and C-510, while its non-published
  three-repository journey remains separate required acceptance evidence.
- The connection name is Exchange's existing tenant-scoped label. It is not a tenant, authority,
  endpoint, credential address or caller-selected runtime placement.
