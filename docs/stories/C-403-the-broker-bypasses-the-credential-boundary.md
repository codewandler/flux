---
id: C-403
title: "The endpoint broker calls `call_with_host` without the credential boundary, and the boundary's scope statement does not cover it"
pillar: Core
status: in-progress
priority: 9
epic: connector-platform
areas: [flux-capabilities, flux-plugin]
note: "found by C-312's review. C-312 puts the boundary on the projected-tool path and on `flux plugin call`, and its module header excuses only a host-dispatched `internal: true` op — but `kubernetes.endpoint.discover` is declared with `read_op_typed`, not `internal_op`, so the carve-out does not actually cover the site that skips the check. Scope-statement gap, not a demonstrated leak"
---

# The broker is a second plugin-response ingest surface

## Goal

Close the gap between what [C-312](C-312-connector-credential-boundary.md)'s credential boundary
*claims* to cover and what it *does* cover — either by extending the boundary to the broker, or by
making the boundary's scope statement true.

C-312 checks a platform-sourced response at ingest, the moment `call_with_host` returns. Two other
call sites reach `call_with_host` and never hit that check:

- `crates/flux-capabilities/src/endpoint/broker.rs:206` — `endpoint.discover`
- `crates/flux-capabilities/src/endpoint/broker.rs:311` — `secret.read`

The boundary's module header (`crates/flux-plugin/src/host/credential_boundary.rs`) scopes itself to
"the projected-tool path and `flux plugin call`", and excuses one class: a **host-dispatched
`internal: true` op**, on the stated grounds that it "is never advertised to the model, and its
result goes to host code, not to a log or a transcript".

**That excuse does not cover the discover site.** `kubernetes.endpoint.discover` is declared with
`read_op_typed` (`plugins/kubernetes/src/main.rs:304`), not `internal_op` — so it is an ordinary
advertised operation reached through a path with no boundary on it.

This is a **scope-statement gap, not a demonstrated leak**: the results go to the endpoint registry
and the credential reader rather than to a transcript. But a safety boundary whose written scope is
wider than its enforcement is exactly the thing that decays into a real hole.

## Acceptance

- [x] **Failing-first**: a test driving a hostile platform deployment through
      `broker.rs`'s `endpoint.discover` path, asserting credential material is refused — failing at
      the merge base because no boundary runs there.
      → `crates/flux-capabilities/tests/credential_boundary_broker.rs::a_platform_sourced_discovery_carrying_a_vendor_credential_is_refused`
- [x] Either the boundary runs on the broker's `endpoint.discover` path, **or** the module header's
      scope statement is corrected to name this site and say precisely why it is exempt. Whichever is
      chosen, the reason is recorded at the definition, not in a commit message.
      → **Both.** The boundary runs (`crates/flux-capabilities/src/endpoint/broker.rs`, in
      `HostProviderInvoker::discover`) *and* the scope statement now names all four
      `call_with_host` call sites in a table (`crates/flux-plugin/src/host/credential_boundary.rs`).
- [x] **`secret.read` is decided separately and explicitly.** It is the one op whose *purpose* is to
      return credential material to host code, so refusing a credential-shaped response there would
      be wrong. Say so at the call site, so the next reader does not "fix" it.
      → `HostCredentialReader::read` in `broker.rs`, and mirrored in the boundary's scope statement.
- [x] The `internal: true` carve-out is re-checked against the ops that actually exist — if no
      shipped op relies on it, the carve-out should say that rather than describe a hypothetical.
      → It relies on nothing: the only `internal: true` op in the tree is `plugin.validate`,
      auto-injected by `host-kit`. The carve-out now says so.
- [x] Full gate green in both workspaces.

## Notes

- Depends on C-312 landing first — this story only makes sense against the boundary it extends.
- `flux-capabilities` is L5 and `flux-plugin` is L4, so the broker may call into the boundary; the
  layering rule permits the direction this needs.
- The related fail-open on an unknown op (`crates/flux-cli/src/plugin_cmd.rs:1266,1283`) is being
  fixed inside C-312's rework round, not here.

## Progress

- Filed 2026-08-01 from C-312's independent review, which verified the boundary's placement on the
  paths it does cover and found these two sites outside it.
- Implemented 2026-08-01 on `impl/C-403`. **The choice was to extend the boundary AND correct the
  scope statement**, because the two failure modes are different: the discover path needed the
  check, and the scope statement needed to stop being wider than what it described.
  - `HostProviderInvoker::discover` now reads the op's `platform` declaration out of the provider
    manifest and applies `credential_boundary::refuse_response` to the raw response and
    `scrub_error` to the `err` frame — before `serde_json::from_value` builds any
    `EndpointCandidate`. A refused provider is discarded whole; `EndpointBroker::discover` already
    logs-and-skips a provider error, so one refusal never fails the query.
  - `resolve_op_name` was re-expressed on a new `resolve_op` returning the whole `OperationSpec`,
    so reading the declaration cannot introduce an unreachable "no declaration found" fail-open
    branch.
  - `HostProviderInvoker::with_redactor` installs the session redactor (wired in
    `flux-cli`'s `assemble_integrations`). Without it the check works on shape alone; the
    registered-value pass — the only one that can see the deployment's own session bearer coming
    back — needs the session's store. Same trade-off `flux plugin call` records for its fresh
    redactor.
  - `secret.read` is **left unchecked on purpose**, argued at the call site: a credential-shaped
    response is its success case, so the check would fire on success and pass on failure. What
    bounds it is `resolve_credential_for`'s deny-by-default grant + first-use approval and the
    value's disposition (host code and the redactor, never a tool result or the registry).
- **What did NOT change, verified:** `kubernetes.endpoint.discover` is `read_op_typed`, i.e.
  `PlatformSourcing::None`, so the check is a no-op for every discovery provider in the pack today.
  The `a_provider_that_is_not_platform_sourced_is_not_refused` test is that control.
- **The `internal: true` census** (`grep -rn 'internal_op\|internal: true' plugins/ crates/`): the
  only `internal: true` op in the tree is `plugin.validate`, which `host-kit`'s builder auto-injects
  into every manifest and which answers `{operation, valid, problems}`. No shipped plugin declares
  one of its own; `host-kit`'s `aws-bedrock.auth` example is a design sketch, not a plugin here.
- The C-312 fixture (`crates/flux-plugin/src/bin/platform_plugin.rs`) was extended rather than
  duplicated: it now declares `discovers: ["zendesk"]` and a platform-sourced `endpoint.discover`,
  with `leak-discover` / `leak-discover-unmarked` / `leak-discover-error` modes and a
  `local-discover` control that declares no `platform` sourcing.
