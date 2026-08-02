---
id: A-125
title: KubernetesRuntime — an agent as a pod, over the existing kubernetes plugin
pillar: Agent
status: ready
priority: 11
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-runtime, flux-orchestrate, plugins]
note: "the k8s plugin and its endpoint provider (D-28) already exist — this rides them rather than adding a second cluster client"
---

# KubernetesRuntime — an agent as a pod, over the existing kubernetes plugin

## Goal
A `KubernetesRuntime` configured with a cluster workload makes it a fleet member: startable,
stoppable, observable, and — with A-126 — discoverable without anyone editing a roster file. It
returns C-243's opaque worker id and A2A endpoint; this story does not revive the superseded
runtime-selecting `k8s://` URI.

## Acceptance
- [ ] `KubernetesRuntime` implements the shipped `flux_runtime::AgentRuntime` contract and shares a
      backend contract suite with `ProcessRuntime` rather than inventing Kubernetes-only lifecycle
      rules.
- [ ] Cluster access goes through the **existing** kubernetes plugin and its endpoint provider
      (D-28) — no second cluster client, no new kubeconfig handling.
- [ ] Failing-first test: `status` maps pod phase **and** readiness to `WorkerStatus` honestly — a
      `Running` pod whose agent card does not answer is `Starting`, a terminal/crashing pod is
      `Dead`, and an API lookup failure remains an error rather than being reported as `Live`.
- [ ] Failing-first test: `stop` scales/deletes the workload and is idempotent.
- [ ] Cluster credentials are referenced, never read into the address: `credential_ref` semantics
      as the endpoint broker already defines them (a location, never a value).
- [ ] Tests are offline against a stubbed API; any live-cluster test is ignored by default.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
- The prerequisite port/process implementation shipped in C-243. Independent of A-124.
- Multi-cluster needs no cluster object: a cluster is a `labels` value on the discovered endpoint,
  and `fleet.list` filters on it (A-126).
