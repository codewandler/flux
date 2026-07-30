---
id: A-125
title: KubernetesRuntime — an agent as a pod, over the existing kubernetes plugin
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet, plugins]
note: "the k8s plugin and its endpoint provider (D-28) already exist — this rides them rather than adding a second cluster client"
---

# KubernetesRuntime — an agent as a pod, over the existing kubernetes plugin

## Goal
`k8s://prod/deploy/flux-worker` makes a cluster workload a fleet member: startable, stoppable,
observable, and — with A-126 — discoverable without anyone editing a roster file. This is the
runtime where flux stops being a process on someone's laptop.

## Acceptance
- [ ] `KubernetesRuntime` implements `AgentRuntime` and **passes A-121's contract suite unmodified**.
- [ ] Cluster access goes through the **existing** kubernetes plugin and its endpoint provider
      (D-28) — no second cluster client, no new kubeconfig handling.
- [ ] Failing-first test: `status` maps pod phase **and** readiness to `AgentStatus` correctly —
      a `Running` pod whose agent card does not answer is `Starting`, a `CrashLoopBackOff` is
      `Exited`, an unreachable API server is `Unreachable` and never silently `Ready`.
- [ ] Failing-first test: `stop` scales/deletes the workload and is idempotent.
- [ ] Cluster credentials are referenced, never read into the address: `credential_ref` semantics
      as the endpoint broker already defines them (a location, never a value).
- [ ] Tests are offline against a stubbed API; any live-cluster test is ignored by default.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
- Depends on A-120, A-121. Independent of A-124.
- Multi-cluster needs no cluster object: a cluster is a `labels` value on the discovered endpoint,
  and `fleet.list` filters on it (A-126).
