---
id: C-648
title: "A host config table, HostRef and session registry"
pillar: "Core"
status: in-progress
epic: first-class-hosts
areas: [flux-config, flux-capabilities]
design: first-class-hosts
note: "Decision 0018 rules 1-2: declare named hosts the way endpoints are declared; credential references, never values"
---

# A host config table, HostRef and session registry

## Goal

Hosts become declarable, named entities. Today the remote substrate is an anonymous
`--remote <url>` flag and the Exchange binding is a transitional environment-variable pair;
neither can be listed, inspected or granted as a named thing. A `[[host]]` config table mirrors
`[[endpoint.static]]`, a `HostRef` joins the reference vocabulary in `flux-secret`, and a session
registry mirrors `EndpointRegistry` (`crates/flux-capabilities/src/endpoint/`) so bindings resolve
by name. Credential material enters only as a reference — an env-var name or a credential-store
location — never a value.

## Acceptance

- [x] `[[host]]` entries parse with `id`, `backend` (`local` | `sandboxed` | `container` |
      `kubernetes` | `remote`), optional `url`, `credential_ref` and `labels`; an unknown backend
      kind is a hard config error, proven by a failing-first test.
- [x] `HostRef` resolves credentials through the existing reference schemes; no configuration path
      accepts an inline secret value, proven by a refusal test.
- [x] A `HostRegistry` registers config-declared hosts at session start and answers list/get by id,
      following the `EndpointRegistry` persistence pattern.
- [x] Registry entries expose backend kind and address for display without ever holding a resolved
      credential value.
