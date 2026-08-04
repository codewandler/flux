---
id: C-509
title: "Complete the first-run local Exchange and integration CLI tutorial"
pillar: Core
status: ready
priority: 0
epic: connector-native-integrations
design: docs/designs/managed-exchange-lifecycle.md
note: "Queued behind X-134 implementation, X-126 and C-510: C-509 owns native plan/grant UX and typed ceremony/mint inputs, never secret-bearing FXLM"
---

# Complete the first-run local Exchange and integration CLI tutorial

## Goal

Make the complete first-run tutorial real from one Flux installation: a person starts the local
Exchange, creates labelled company GitLab and Jira connections from their complete connector-declared
settings, grants their authority, verifies the effective tools and uses them from Flux without Flux
ever receiving vendor credentials. The only Exchange runtime credential that crosses into Flux is
one Service Account token frame delivered directly to C-509's dedicated receiving writer and stored
behind an opaque runtime reference. A verified Exchange helper, not Flux, owns every secret-bearing
local-management transaction from `BEGIN` through `SECRET`/`COMMIT` and its terminal result.

## Acceptance

- [ ] C-509 consumes C-510's channel-selected, verified and process-owned local Exchange endpoint,
      `flux.exchange-local-status.v1` status and two typed in-process launch capabilities bound to
      the already-selected `VerifiedInstallGuard`: vendor ceremony and Service Account mint. C-510
      exclusively owns local lifecycle
      selection, install, import, cache and quarantine; supervision, control, readiness and
      liveness; lifecycle idempotence; `start|status|stop` semantics; and every lifecycle diagnostic
      and exit code. Neither capability exposes a path, executable handle, arbitrary argv, alternate
      binary, secret, arbitrary management operation, endpoint, tenant, address, cwd, environment,
      extra argv, raw FD/HANDLE or lifecycle/status field. C-509 neither searches `PATH` nor rediscovers/reopens
      the cache and adds no duplicate lifecycle machinery, outcome or reclassification.
- [ ] C-509 owns plan projection, user selection, grant proposal/confirmation and the
      owner-authenticated `exchange.local-management.v1` value-free FXLM client for plan and grant
      management. For a credential-bearing connection it owns only one canonical non-secret
      initiating frame and exactly one value-free terminal helper result. Human/operator management
      remains structurally separate from the Service Account runtime client, whose only authority is
      X-129's effective-catalogue discovery and invoke HTTP v1 surface. FXLM never carries lifecycle
      operations and never shares C-510 control, readiness/liveness or FXSA streams. Its native peer
      authentication and closed state machines, framing, bounds, receipts and value-free errors come
      from X-134 through X-126's future post-X-134 release-v2 inventory.
- [ ] C-509 launches vendor-input only through C-510's typed guard-bound ceremony operation and only
      with X-134's closed provider helper mode. Every connect and credential acquire/rotate uses it,
      including settings-only zero-secret create. For vendor-secret onboarding,
      Flux writes exactly one at-most-65,548-byte canonical initiating FXLM frame plus EOF to a
      one-way request pipe and reads exactly one at-most-65,548-byte value-free receipt or error frame
      plus EOF from a distinct bounded terminal-result pipe. The request is only connect `BEGIN`
      `0x0001` or credential acquire/rotate `BEGIN` `0x0030`; the result is only connect `RECEIPT`
      `0x0006`, credential `RECEIPT` `0x0032` or `ERROR` `0x7fff`. The verified Exchange helper keeps
      its owner-authenticated `PLAN_QUERY` validation peer separate from the secret-bearing `BEGIN`
      ceremony peer, which owns and parses `NEED_SECRETS`, prompts directly through TTY/browser,
      sends `SECRET` and `COMMIT`, and retains the transaction id, secret ordinals and provider bytes
      until terminal state. For a zero-secret ceremony it receives `NEED_SECRETS` with `secrets:[]`,
      opens no prompt, sends no `SECRET`, sends `COMMIT` directly and returns the normal result; Flux
      cannot learn whether prompting occurred. Create and held-label create replay validate with
      `selection:null`; credential acquire/rotate validate with `selection:BEGIN.label`. The helper's
      connection count, state and traffic are X-134-owned and invisible to Flux;
      neither peer nor any intermediate value or secret-bearing frame reaches Flux.
      Non-secret settings are part of the initiating connection proposal and are never prewritten as
      a separate transaction. There is no argv, environment, JSON, generic `--field`, stdin or Flux
      prompt route for a vendor secret. C-509 consumes C-510's absolute deadline enforcement: request
      frame plus EOF within five seconds of spawn and one 335-second result deadline from request EOF,
      never reset by traffic. Empty stdout/stderr and exit 0 mean the complete receipt or application
      refusal crossed; exit 1 means capability/transport/result-write failure prevented the contract.
- [ ] C-509 owns a hidden receiving credential-writer mode of the currently running shipped Flux
      executable, the dedicated owner-only Service Account store, opaque runtime reference and
      resolver. Exactly one `exchange.service-account-handoff.v1` FXSA frame crosses from the
      verified Exchange mint helper/server into that writer; the CLI parent and supervisor never
      read the token pipe. The writer reports `credential_stored` only after an atomic owner-only
      store commit. The runtime client holds only the reference; the resolver reads the token only
      while constructing a sensitive Authorization header, bounds its lifetime and never registers
      it in the shared redactor. Unsafe/unavailable storage, framing, resolver or handoff refuses
      without environment, configuration or plaintext fallback, superseding C-503's transitional
      environment-token bootstrap.
      C-509 passes that writer only to C-510's separate typed Service Account mint operation. Unix
      mint maps only the FXSA writer to FD 5; Windows mint inherits only the exact writer HANDLE in a
      closed HANDLE list. Mint never shares ceremony request FD 6/result FD 7 or their Windows handles,
      and neither typed operation accepts a caller-selected program, mode, endpoint, tenant, address,
      cwd, environment, extra argv or raw handle.
- [ ] `flux integration connect <connector> --name <name>` consumes X-134's provider-owned
      machine-readable labelled-connection plan backed by Connectors C-87/C-508 declarations. Flux
      sends FXLM `PLAN_QUERY` opcode `0x0007` to the supervised native owner endpoint and accepts only
      the matching `PLAN_RESPONSE` opcode `0x0008` containing the exact canonical
      `exchange.connection-plan.v2` response before showing a prompt or writing state. The request
      payload is exactly `{"connector":Connector,"selection":Label|null}`, including required JSON
      `null` for create and held-label create replay; credential acquire/rotate use the exact
      `BEGIN.label`. In the response every secret field has `set:null`, aggregate plan state ignores
      secret presence, and `credential_revision` is `null` exactly when `selection:null` and is an
      opaque nonzero 256-bit value encoded as exactly 64 lowercase hexadecimal characters for every
      selected label regardless of credential presence; the complete all-zero value refuses.
      Credential acquire/rotate copy that exact revision into `BEGIN`. Flux validates/projects it as
      a value-free CAS and never interprets it; static plan/target revisions remain distinct.
      Native OS-owner
      authentication is not HTTP identity; Flux must not use its Service Account HTTP client or a
      browser capability for this read. It refuses every absent, unknown, obsolete v1 or incompatible
      plan identity and strictly
      projects every plan-published non-secret target and no other target. Vendor fields remain in
      the verified Exchange helper/server and never enter Flux's parser. Flux keeps neither vendor
      values nor a connector-specific form schema.
- [ ] A failing-first CLI projection corpus consumes X-134's canonical plan-v2 cases only through
      X-126's future post-X-134 `tests/fixtures/exchange-release-v2/` inventory/digest and covers the
      published connector declarations for GitLab custom HTTPS `origin`, Jira Cloud `site` and
      account settings, and Zendesk `subdomain`. The obsolete six-field
      `tests/fixtures/exchange-release-v1/` corpus and X-125 plan-v1 fixture are refused, never used
      as partial or fallback evidence. Credential targets are proved present in the plan but
      excluded from every Flux value parser and routed only by invoking the closed Exchange
      vendor-input helper. The current derived convenience aliases are `--origin`, `--site` and
      `--subdomain` respectively, and exist only because the plan publishes them for those field
      identities; every non-secret field remains scriptable through a generic
      `--field <identity>=<value>` fallback.
      Flux maintains no vendor alias list. It does not invent `--endpoint` or `--domain` compatibility
      aliases unless Exchange publishes and proves them. Unknown aliases/identities, omitted required
      or unprojected fields visibly refuse before submission rather than producing an incomplete
      connection. The corpus fails first unless plan discovery uses native `0x0007`/`0x0008`, rejects
      HTTP Service Account/browser substitution, emits `selection:null` for create and held-label
      create replay, emits `selection:BEGIN.label` for acquire/rotate, carries the exact opaque
      nonzero 256-bit credential revision encoded as exactly 64 lowercase hexadecimal characters for
      acquire/rotate, refuses its all-zero encoding, emits one canonical non-secret initiating frame,
      and exposes only one terminal value-free helper result. Compile-time/API tests leave no Flux
      construction or decoding route for `NEED_SECRETS`, transaction ids, secret ordinals, provider
      bytes, `SECRET` or `COMMIT`; transport tests reject a non-terminal opcode, second frame or
      oversized frame at the closed terminal boundary without decoding a secret-bearing payload.
      The same failing-first corpus consumes X-134's closed target-selection/partition fixtures for
      connect/acquire/rotate: create contains `connection.name`, every required routable target and
      exactly the optional targets selected by their plan target; acquire/rotate contain the complete
      credential partition and no name, setting or authority target. Each occurs once in plan order
      and in exactly one connection-name/settings/authority/credential partition. Omission,
      invention, reorder, duplication and cross-partition movement all refuse.
- [ ] `flux integration grant` first previews and applies a low-risk metadata-selector read grant;
      the tutorial proves a write remains refused under it, then previews and applies a high-risk
      metadata-selector grant and separately asks for the concrete write approval before that write
      executes. Preview consumes X-134's complete connector-scoped candidate, exact revision/ETag
      and proposal digest; compare-and-swap apply preserves unrelated connectors, inbound authority
      and provider-owned fields. Same-digest replay is idempotent, while stale revision, digest
      mismatch or unexpressible stored authority refuses before write. `flux integration list`
      reports labelled connection and effective-operation state;
      `flux integration doctor` distinguishes human-bootstrap, Service Account auth, incomplete
      settings, missing grant and Exchange integration-refusal outcomes without printing
      credential-shaped data. It consumes C-510's typed endpoint/status; a local-process or
      Exchange-unavailable lifecycle failure preserves and points to C-510's status and diagnostic
      rather than duplicating or reclassifying it. No grant is an operation-name allowlist.
- [ ] C-509 consumes X-134's exhaustive value-free FXLM error/receipt mapping from that same X-126
      v2 inventory. A pre-decision refusal follows only its closed `never|refresh|operator` retry
      instruction. When a receipt id crossed, direct native value-free connect, credential, grant and
      Service Account receipt queries use X-134's respective closed state machines. When no receipt id
      crossed, Flux does not manufacture one or substitute a query; the only recovery for a
      helper-mediated proposal is the byte-identical helper replay.
      A credential-bearing connection never exposes its transaction id or secret ordinals to Flux;
      after a missing terminal helper result Flux may only relaunch with the byte-identical non-secret
      initiating frame under X-134's final replay rule. Retry is never an edit path and never repeats
      vendor input after a committed receipt. Approval denial sends no Exchange
      invocation. Uncertain send state is never automatically replayed for a non-idempotent or
      conditional write; a high-risk/effectful invocation requires Flux approval for its exact
      permission subject even after the Exchange grant admits it.
- [ ] Each C-509-owned integration command has a non-interactive JSON mode with no hidden prompt,
      stable integration-only success/refusal categories and deterministic exit status. Repeating an
      identical connection or grant request is idempotent; a conflicting connection definition
      refuses and names the connector plus label, never a setting or secret value. Lifecycle JSON,
      idempotence, diagnostics and exit status remain exclusively C-510 acceptance. JSON mode never
      accepts or emits vendor input or a Service Account token; an operation requiring direct
      vendor input reports the provider's value-free handoff requirement.
- [ ] A failing-first clean-machine end-to-end test and the user documentation execute this exact
      sequence against both the released clean-machine path and a non-published workspace that locally
      binds Flux, flux-connectors and flux-exchange: install/start the compatible Exchange from
      C-510/X-126 after X-134; mint and store the runtime credential through exactly one FXSA frame;
      connect `gitlab/company` with a custom origin; connect `jira/company` with its Cloud site;
      preview/apply the low-risk read grant; list and diagnose effective tools; complete one read;
      prove a write is refused; preview/apply the high-risk metadata grant; separately approve and
      complete that write from Flux; then stop Exchange. The proof asserts that no vendor credential
      enters any Flux surface and the Service Account token enters only the dedicated receiving
      writer/store and sensitive Authorization transport—never output, logs, events, session state,
      ordinary configuration or model-visible state. Stopping Exchange removes only official
      external tools. Both journeys
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
- 2026-08-04: Decision 0007 reconciliation explicitly superseded that X-125/six-field evidence.
  C-509 adopted X-134's plan-v2, FXLM and FXSA provider contract while remaining gated on the
  connectors C-515 registry release, X-134 implementation, X-126's regenerated post-X-134 release-v2
  corpus and C-510's guard-bound helper capability. This documentation repair does not claim C-509
  implementation or provider conformance; every acceptance item remains open.
- 2026-08-04: Reconciled the native-helper boundary to roadmap commit
  `4511f44b4defcb6de92ab8fc1b56bd5b4356ca78`: the contract now fixes plan selection and opaque
  credential CAS projection, provider-owned target partitions, zero-secret ceremony behavior, typed
  ceremony/mint separation, deadlines and receipt-id recovery. C-509 remains `ready` with every
  acceptance item open; implementation remains gated on X-134 implementation, X-126's post-X-134
  release-v2 fixture inventory and C-510.

## Notes

- Cross-repository authority is flux-roadmap Decisions 0002, 0004 and 0007 at coordinator commit
  `4511f44b4defcb6de92ab8fc1b56bd5b4356ca78`; the canonical Flux baseline inspected for this
  amendment is `1f3283517f98818d09cd3cdb9e1c77ce8c485852`. Canonical Exchange merge
  `3b16bcb5b1c52984449118775125fe66da1686da` contains accepted X-134 contract head
  `9dc414c76f231bd179358fd526019a16872a7be1`.
- C-509's direct contract inputs are Flux C-503's delivered four-identity HTTP Service Account
  catalogue/invoke client, Flux C-510's local endpoint/status and guard-bound helper capability,
  Connectors C-87/C-508's declarations, and Exchange X-134's plan-v2/local-management-v1/
  service-account-handoff-v1 contracts. X-129 proves only the four unchanged HTTP v1 identities;
  X-134 supersedes X-125's unpublished plan-v1/submission evidence and owns the remaining provider
  fixtures. C-509 owns strict dynamic CLI projection, the value-free native plan/grant client, one
  non-secret helper request and terminal-result projection, the FXSA receiving writer/store, opaque
  runtime resolver, grant CAS and approval/retry behavior. It never owns the secret-bearing FXLM peer.
- The local-release chain is transitive through C-510: C-510 owns all Flux lifecycle behavior and
  consumes Exchange X-126/X-128/X-134 lifecycle identities; C-509 does not acquire lifecycle or
  release ownership from that chain. Its exact released clean-machine journey waits for X-134
  implementation, X-126 publication and C-510, while its non-published three-repository journey
  remains separate required acceptance evidence. Roadmap dependency order keeps this `ready` story
  queued.
- The connection name is Exchange's existing tenant-scoped label. It is not a tenant, authority,
  endpoint, credential address or caller-selected runtime placement.
- Canonical Exchange merge `3b16bcb5b1c52984449118775125fe66da1686da` and its accepted X-134
  contract head `9dc414c76f231bd179358fd526019a16872a7be1` are the exact-byte authority. This does
  not satisfy the connectors C-515 registry-release gate, claim X-134 implementation or provide
  X-126's post-X-134 release-v2 fixtures. C-509 must consume the implemented provider result verbatim
  and must not invent alternate selection, target partition, connection timing, name or byte
  contracts.
