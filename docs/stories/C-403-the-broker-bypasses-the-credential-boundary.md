---
id: C-403
title: "The endpoint broker calls `call_with_host` without the credential boundary, and the boundary's scope statement does not cover it"
pillar: Core
status: ready
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

- [ ] **Failing-first**: a test driving a hostile platform deployment through
      `broker.rs`'s `endpoint.discover` path, asserting credential material is refused — failing at
      the merge base because no boundary runs there.
- [ ] Either the boundary runs on the broker's `endpoint.discover` path, **or** the module header's
      scope statement is corrected to name this site and say precisely why it is exempt. Whichever is
      chosen, the reason is recorded at the definition, not in a commit message.
- [ ] **`secret.read` is decided separately and explicitly.** It is the one op whose *purpose* is to
      return credential material to host code, so refusing a credential-shaped response there would
      be wrong. Say so at the call site, so the next reader does not "fix" it.
- [ ] The `internal: true` carve-out is re-checked against the ops that actually exist — if no
      shipped op relies on it, the carve-out should say that rather than describe a hypothetical.
- [ ] Full gate green in both workspaces.

## Notes

- Depends on C-312 landing first — this story only makes sense against the boundary it extends.
- `flux-capabilities` is L5 and `flux-plugin` is L4, so the broker may call into the boundary; the
  layering rule permits the direction this needs.
- The related fail-open on an unknown op (`crates/flux-cli/src/plugin_cmd.rs:1266,1283`) is being
  fixed inside C-312's rework round, not here.

## Progress

- Filed 2026-08-01 from C-312's independent review, which verified the boundary's placement on the
  paths it does cover and found these two sites outside it.
