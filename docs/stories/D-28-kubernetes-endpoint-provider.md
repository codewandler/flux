---
id: D-28
title: Kubernetes endpoint provider (the reference provider)
pillar: Agent
status: done
design: docs/designs/endpoint-discovery.md
note: the reference provider (`kubernetes.endpoint.discover` → cluster/Service/Ingress/RDS `EndpointCandidate`s with a `credential_ref`, latest-namespace) + the broker op-name reconciliation + the agent-facing `endpoint.discover/select/info/list` ops, `endpoint` group, `kubernetes` signal, and a mapping skill — the "connect to my latest namespace backend RDS" demo path is now wired
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
- [x] **Provider declaration** — the manifest `.discovers(...)` the 7 products and exposes
      `kubernetes.endpoint.discover`; the broker resolves namespaced provider ops via a new
      `resolve_op_name(manifest, suffix)` (matches `== suffix` or `ends_with(".suffix")`) so
      `kubernetes.endpoint.discover`/`kubernetes.secret.read` are addressed correctly (closing the
      bare-name integration gap from D-26/D-27). Tests `declares_discovery_products`,
      `resolve_op_name_matches_namespaced_and_bare`.
- [x] **Clusters from kubeconfig** — `product == "kubernetes"` yields one cluster candidate per context
      (current context scored 1.0). Test `contexts_become_cluster_endpoints`.
- [x] **Services/Ingresses → product endpoints** — Services **and** Ingresses matched by name or
      `app.kubernetes.io/name` become scored `EndpointCandidate`s. Test `services_become_product_endpoints`.
- [x] **RDS/crossplane Secrets → credential refs** — a discovered database surfaces a `postgres`/`mysql`
      candidate whose `url` is built from the secret's host/port keys and whose `credential_ref` is a
      `kubernetes/<ns>/<secret>/<password-key>` reference — **no password value** in the candidate. Test
      `rds_secret_becomes_credential_ref`. Plus `endpoint_discover_selects_latest_namespace` (newest
      namespace by `creationTimestamp`).
- [x] **Agent-facing ops** — `flux_capabilities::endpoint::ops` registers `endpoint.discover`/`select`/
      `info`/`list` (read-only, `group:"endpoint"`), surfaced when a `kubernetes` signal is present
      (`detect_signals` probes `KUBECONFIG`/`~/.kube/config`); the agent sees weak refs + a
      `[credential: host-injected]` flag, never a value. Wired into `flux run` (and `flux app run`).
- [x] `plugins/` gate green (MockHost test per op); root gate green. A live-cluster smoke is deferred to
      the epic's end-to-end verification.

## Progress
- **Done.** Reshaped `kubernetes.endpoint.discover` to emit `EndpointCandidate`s (contexts / Services+
  Ingresses / crossplane-RDS Secrets→`credential_ref`), added `discovers` + latest-namespace; fixed the
  broker to resolve the provider's real (namespaced) op name for both discovery and the cross-plugin
  `secret.read`; added the agent-facing `endpoint.*` ops + the `endpoint` group + the `kubernetes`
  signal, and wired them into the surfaces.

## Notes
- The bundled mapping skill (`namespace⇒k8s`, `RDS⇒postgres`, "latest", weak-ref invariant) lives in the
  **op descriptions** (which the catalog surfaces); a convenience copy was dropped in `.flux/skills/` but
  that path is gitignored, so the op-description hints are the shipping vehicle.
- Op shapes copied (not code) from `~/projects/fluxplane/fluxplane-plugins/kubernetes/operations.go`.
- Unblocks [D-29](D-29-migrate-plugins-to-references.md) (the consumers).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
