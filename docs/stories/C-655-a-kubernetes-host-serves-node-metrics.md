---
id: C-655
title: "A Kubernetes host serves node metrics"
pillar: "Core"
status: backlog
epic: host-metrics-seam
areas: [flux-system]
design: host-metrics-seam
note: "Decision 0018 rule 6: node/pod metrics mapped into the same closed vocabulary; follows the container/Kubernetes backend (C-397, C-480)"
---

# A Kubernetes host serves node metrics

## Goal

A Kubernetes host serves node metrics through the same seam. When a binding's backend is a cluster
substrate, the metrics seam answers node and pod readings — capacity, allocatable, usage where the
cluster's metrics pipeline exposes them — mapped into the closed vocabulary. Anything the cluster
does not expose is explicitly unavailable, with the denying reason typed.

## Acceptance

- [ ] The Kubernetes backend serves node cpu/memory capacity and usage through `GuardedMetrics`
      using its own transport, with no Flux-side `kubectl` shelling.
- [ ] The vocabulary is not widened per vendor: cluster readings map into existing kinds plus at
      most one bounded capacity group added under review.
- [ ] An absent metrics pipeline or an RBAC denial answers explicitly-unavailable with a typed
      reason; a fixture test proves it.
- [ ] Depends on the container/Kubernetes backend lineage (C-397, C-480) and on C-653/C-654; not
      dispatchable before them.
