---
id: C-676
title: "A kubernetes binding resolves to a served pod substrate"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system]
design: first-class-hosts
note: "Decision 0018 rule 3: compose the delivered remote protocol with the C-480 artifacts; today HostBackend::Kubernetes is 'unwired (selection fails closed)'"
---

# A kubernetes binding resolves to a served pod substrate

## Goal

`kubernetes` is a word in the closed backend vocabulary and nothing more: a `[[host]]` entry can
declare it, `flux host ls` renders it, and selection fails closed as "unwired". Decision 0018
rule 3 already names the composition — a Kubernetes host is the delivered remote protocol
(`flux system serve`) running in a pod built from the delivered C-480 OCI artifacts over the
C-397 container backend. This story wires the resolution: a kubernetes binding names its cluster
contract, resolves to the pod's serve endpoint through the kubernetes plugin and endpoint broker,
and admits only what the protocol handshake verifies. No new substrate code — the pod answers the
same port every other remote host answers.

## Acceptance

- [ ] A kubernetes binding declares cluster, namespace and endpoint selection value-free in
      config (credential as reference), and resolves to the remote protocol served by a
      C-480-image pod; admission requires the identity handshake.
- [ ] `flux host probe` on a kubernetes binding performs the side-effect-free identity check
      against the pod endpoint and reports the negotiated protocol version.
- [ ] Absent cluster access, endpoint or image contract, resolution fails closed naming the
      missing piece; nothing falls back to local execution.
- [ ] `SubstrateIdentity` reports kind `kubernetes` with `remotely_reported` provenance, keeping
      `browser.*`/`web.crawl` hidden under the selection.
