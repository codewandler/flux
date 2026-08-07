---
id: C-649
title: "flux host CLI family and host.* operations"
pillar: "Core"
status: in-progress
epic: first-class-hosts
areas: [flux-cli, flux-capabilities]
design: first-class-hosts
note: "Decision 0018 rule 1: ls/add/show/rm/probe plus an ambient-gated host.* op group, mirroring the endpoint surface"
---

# flux host CLI family and host.* operations

## Goal

The host entity becomes operable: a `flux host` CLI family (`ls`, `show`, `add`, `rm`, `probe`)
and a `host.*` operation group — ambient-signal-gated like the `ENDPOINT_GROUP` pattern in
`crates/flux-capabilities/src/endpoint/ops.rs` — expose the registry to users and agents. `probe`
performs the backend's identity check (its `SubstrateIdentity`; the protocol handshake for a
remote backend) without side effects.

## Acceptance

- [x] `flux host ls`/`show` render id, backend kind, address and availability; JSON output is the
      automation API.
- [x] `add`/`rm` edit the `[[host]]` config table the way `flux endpoint add` edits endpoints, and
      refuse inline secret values.
- [x] `probe` reports the resolved `SubstrateIdentity` (kind, workspace, confinement,
      remotely_reported) and, for a remote backend, the negotiated protocol version; failures are
      typed, not stringly.
- [x] `host.*` operations register with `LocalControlPlane` placement behind the ambient gate; the
      placement census test pins the new operation list deliberately.
