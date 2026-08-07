---
id: C-677
title: "A microvm binding resolves to a served guest substrate"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system]
design: first-class-hosts
note: "Decision 0018 rule 3 within C-480's boundary: flux never provisions a VM; a microvm host is the remote protocol served from the C-480 guest profile"
---

# A microvm binding resolves to a served guest substrate

## Goal

The closed host backend vocabulary has no `microvm` word — `container` and `kubernetes` exist
unwired, a VM guest not even that. Decision 0018 rule 3 composes rather than invents: a microVM
host is the delivered remote protocol (`flux system serve`) running inside a guest built from
C-480's VM profile (hardened service unit, cloud-init install contract, durable workspace disk,
TLS/token file modes). This story adds `microvm` as a declarable, probeable, selectable binding
kind that names such a served endpoint and admits only what the protocol handshake verifies.
C-480's boundary holds: Firecracker, Kata, Cloud Hypervisor and every lifecycle verb stay behind
a future generic isolation-provisioner contract — the binding consumes an endpoint that already
exists, it never creates one.

## Acceptance

- [ ] `microvm` joins the closed `HostBackend` vocabulary: declarable in `[[host]]`, rendered by
      `flux host ls`/`show`, the unknown-key hard error unchanged; absent a served endpoint the
      static availability answer says unwired and selection fails closed naming the gap.
- [ ] A microvm binding with an endpoint resolves to the remote-protocol client; admission
      requires the identity handshake; `flux host probe` reports the negotiated protocol version
      and the guest's `SubstrateIdentity` with `remotely_reported` provenance.
- [ ] No provisioning surface exists: nothing starts, stops or creates a VM anywhere in the
      change; the reference docs point at C-480's guest profile as how the endpoint comes to be.
- [ ] The backend and placement censuses stay clean, and the non-native kind keeps
      `browser.*`/`web.crawl` hidden under a microvm selection.

## Notes

- Depends on C-480 for an end-to-end probe against a real guest; the vocabulary, fail-closed
  unwired answer and endpoint resolution are implementable before it with the same fixture
  discipline the remote backend already uses.
- Sequencing: touches `flux-cli/src/host_cmd.rs` and `flux-capabilities/src/host/mod.rs` — wait
  for C-654 (metrics surface) to integrate before dispatch.
