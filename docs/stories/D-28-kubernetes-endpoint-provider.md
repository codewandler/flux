---
id: D-28
title: Kubernetes endpoint provider (the reference provider)
pillar: Agent
status: backlog
priority:
design: docs/designs/endpoint-discovery.md
---

# Kubernetes endpoint provider (the reference provider)

## Goal
Elevate the kubernetes plugin's existing `endpoint.discover` / `cluster.list` / `secret.read` ops into
a real discovery **provider** (per [D-26](D-26-endpoint-discovery-broker.md)) declaring
`discovers: [kubernetes, prometheus, loki, grafana, alertmanager, postgres, mysql]`: kubeconfig
contexts → cluster endpoints; in-cluster Services/Ingresses → product endpoints; crossplane/RDS Secrets
→ **credential weak-references** (`kubernetes/ns/name/key`, a location, never a value). The reference
implementation other providers are modelled on.

## Why
The kubernetes plugin already discovers this data (`plugins/kubernetes/src/main.rs:65,82`, from D-14)
but only returns it to the agent — nothing brokers it to other plugins. fluxplane's kubernetes plugin
is the canonical endpoint provider (`~/projects/fluxplane/fluxplane-plugins/kubernetes`): each
kubeconfig context is a cluster endpoint, and in-cluster scanning surfaces monitoring + database
endpoints for the observability and `sql` plugins. See the
[epic design](../designs/endpoint-discovery.md) — *Why* and *Reuse*.

## Acceptance
- [ ] **Provider declaration** — the manifest declares `discovers: [kubernetes, prometheus, loki,
      grafana, alertmanager, postgres, mysql]` and implements the `endpoint.discover` op contract. Test
      `kubernetes::declares_discovery_products`.
- [ ] **Clusters from kubeconfig** — `product == "kubernetes"` yields one cluster `EndpointRef` per
      kubeconfig context. Test `kubernetes::contexts_become_cluster_endpoints` (MockHost).
- [ ] **Services/Ingresses → product endpoints** — in-cluster Services/Ingresses matched by name/label
      become product `EndpointRef`s (prometheus/loki/grafana/alertmanager). Test
      `kubernetes::services_become_product_endpoints`.
- [ ] **RDS/crossplane Secrets → credential refs** — a discovered database surfaces a `postgres`/`mysql`
      `EndpointRef` whose `credential_ref` is a `Kubernetes`-scheme reference (location only — no secret
      value in the candidate). Test `kubernetes::rds_secret_becomes_credential_ref`.
- [ ] `plugins/` gate green (one MockHost test per op, per the D-14 pattern); a smoke entry where a
      live cluster is available.

## Progress
- (not started — needs [D-26](D-26-endpoint-discovery-broker.md) and
  [D-27](D-27-reference-based-io.md).)

## Notes
- Op shapes copied (not code) from `~/projects/fluxplane/fluxplane-plugins/kubernetes/operations.go`.
- The existing ops are the starting point — wire them to the D-26 broker + D-27 resolution, don't
  rewrite from scratch.
- Unblocks [D-29](D-29-migrate-plugins-to-references.md) (the consumers).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
