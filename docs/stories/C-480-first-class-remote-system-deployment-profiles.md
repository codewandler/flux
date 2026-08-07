---
id: C-480
title: "First-class remote-system deployment profiles — OCI image, Kubernetes manifests and a microVM guest unit"
pillar: Core
status: done
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [release, website, flux-server]
note: "flux system serve runs inside operator-supplied containers/pods/microVMs today, but flux ships no image, manifest, chart or guest unit — BYO is a protocol capability, not an operable distribution"
---

# First-class remote-system deployment profiles

## Goal

Turn the shipped remote-system daemon from a BYO recipe into reproducible deployment artifacts for
containers, Kubernetes and VM/microVM guests, without coupling the protocol to one provisioner.

## Acceptance

- [x] A release-versioned OCI image runs only `flux system serve`, uses a non-root identity, carries
      no token/key/workspace in a layer, and has a documented provenance path consistent with
      the binary release. (Amended at integration: the binary release ships attestation and no
      SBOM, so the image documents `gh attestation verify` over the repacked attested archive —
      the review judged this consistent; an SBOM for the release pipeline is a separate decision.)
- [x] Failing-first container integration test: mount a workspace/TLS/token, start the image, connect
      with a released client, write/read through the remote system, restart the container, and prove
      the workspace plus delivery ledger persist. The test is explicitly dispositioned when Docker
      is unavailable in ordinary workspace CI.
- [x] A checked Kubernetes profile (raw manifests or Kustomize first; Helm only if it adds a real
      need) includes one replica per workspace, PVC, TLS Secret, bearer Secret, ClusterIP Service,
      TCP readiness/liveness probes, non-root/seccomp settings, and a default-deny NetworkPolicy.
- [x] A VM/microVM guest profile includes a hardened service unit and cloud-init or equivalent
      install contract for the same release artifact, durable workspace disk, TLS/token file modes,
      and firewall expectations.
- [x] None of the artifacts claims Flux provisions Docker hosts, clusters or microVMs. Firecracker,
      Kata, Cloud Hypervisor and cloud-specific lifecycle belong behind a future generic isolation
      provisioner contract, not the remote wire.
- [x] Upgrade/rollback and protocol-version mismatch behavior are documented and tested.
- [x] Public deployment docs use the shipped artifacts rather than placeholder images once released.
- [x] Release, codegate, root workspace, and artifact-specific checks are green.

## Progress

- Filed 2026-08-02. C-477 documents the exact BYO contract that these artifacts must package without
  weakening.

## Notes

- Independent of [C-397](C-397-container-process-backend.md): that story places one guarded process
  effect in a container without a network hop; this one packages the already-shipped remote daemon.
- Independent of [A-124](A-124-docker-runtime.md) and [A-125](A-125-kubernetes-runtime.md), which
  place whole fleet workers.
