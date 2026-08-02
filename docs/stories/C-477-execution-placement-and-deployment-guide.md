---
id: C-477
title: "One execution-placement map — Docker, Kubernetes and microVMs without conflating four different jobs"
pillar: Core
status: done
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [website, docs]
note: "the public topology page explains local vs remote effects, but does not separate infrastructure management, effect placement, worker placement and isolation provisioning — so 'supports Docker/Kubernetes' has four incompatible answers"
---

# One execution-placement map

## Goal

Give operators one complete, release-accurate answer to “does Flux support Docker, Kubernetes or a
microVM?” without presenting infrastructure management, guarded-effect placement, agent-worker
placement and isolation provisioning as though they were the same feature.

## Acceptance

- [x] Failing-first website contract: the public docs must carry one matrix with the four placement
      jobs as distinct columns and explicit shipped/proposed/BYO states for native, Docker,
      Kubernetes and microVM targets.
- [x] A dedicated Docker plugin page documents only the operations and limitations in
      `plugins/docker`, including the guarded Unix-socket capability and the fact that Docker daemon
      access is host-equivalent authority.
- [x] The Kubernetes plugin page says explicitly that managing cluster resources is not the same as
      placing Flux effects or an agent worker in a pod.
- [x] A deployment guide shows the same shipped `flux system serve` contract inside an operator-
      supplied container, pod or microVM: one canonical workspace, TLS, bearer token, persistence,
      health/readiness expectations and the local `--remote` client. It must call these **BYO
      deployment profiles**, not native Docker/Kubernetes/microVM provisioning.
- [x] The guide states the remote-mode compatibility boundary: port-aware core operations run on the
      selected system; native integrations/plugins are currently hidden and refused rather than
      falling back locally.
- [x] Stale execution-substrate and fleet story/design state is reconciled against the tree, without
      closing the still-open container-effect, Docker-worker or Kubernetes-worker stories.
- [x] Concrete follow-up stories exist for explicit operation placement metadata, remote-capable
      plugin execution and a first-class remote-system deployment artifact/profile.
- [x] `CHANGELOG.md` and `WHATS-NEW.md` describe the documentation improvement in their respective
      engineering and customer registers; the tracked website changelog mirror is regenerated.
- [x] Focused website contracts and the docs-appropriate gate are green.

## Progress

- 2026-08-02: filed from the post-v0.51 remote-execution audit. The selected local/remote system
  mode ships; this story closes the product-language and deployment-documentation gap around it.
- 2026-08-02: the public site now has the four-job placement matrix, Docker guide, explicit
  Kubernetes boundary, and one BYO deployment contract for containers, pods, VMs and microVMs. The
  focused website contract failed before these pages and passes after them. C-478/C-479/C-480 own
  the implementation follow-ups. The website build, workspace gate, architecture gate and both CI
  sandbox postures are green.

## Notes

- [C-397](C-397-container-process-backend.md) owns container placement for individual guarded
  process effects.
- [A-124](A-124-docker-runtime.md) and [A-125](A-125-kubernetes-runtime.md) own container/pod
  placement for whole fleet workers.
- The Docker and Kubernetes plugins manage existing infrastructure. They do not satisfy any of the
  three placement stories above merely by existing.
- A microVM can host the shipped remote-system daemon today, but Flux does not provision, pool,
  snapshot or attest microVMs.
