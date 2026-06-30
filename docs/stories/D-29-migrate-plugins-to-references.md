---
id: D-29
title: Migrate native plugins to references & consume discovered endpoints
pillar: Agent
status: done
design: docs/designs/endpoint-discovery.md
---

# Migrate native plugins to references & consume discovered endpoints

## Goal
Clean-cutover **every** native plugin off env-coupled `EndpointSpec.env` / `AuthMethod.env` onto
**reference-based IO** — each op selects/receives an `endpoint_ref` and passes it to the host IO
capabilities (no env-var names, no raw URLs anywhere on the op surface). The discovery consumers
(`sql`, `grafana`, `loki`, `prometheus`, `alertmanager`) additionally accept **discovered** endpoint
refs with multi-instance selection via the registry, and the `flux app run` + agent paths load the
broker so a running agent can discover→use endpoints.

## Why
The reference invariant must hold for the whole pack, not just the discovery consumers: today all
~16 native plugins hardcode env keys in their manifests and assemble URLs from resolved strings. This
story is the migration that makes the invariant true in practice, and connects discovered endpoints to
the consumers that use them (mirroring how [D-09](D-09-agentic-channel-target.md) wired `flux app run`
to load plugin + datasource tools). See the [epic design](../designs/endpoint-discovery.md) —
*Cutover*.

## Acceptance
- [x] **Primary IO is ref-based** — every plugin that built URLs from the host's URL-handback now passes
      an `endpoint_ref` + `path` to the host-kit `*_ref`/`conn_dial_ref` helpers; the op no longer holds a
      URL string for its primary IO. Wave 1: the shared `SystemHostCaps` named-vs-discovered split + the
      host-kit helpers. Wave 2: alertmanager/grafana/huggingface/slack/confluence/jira migrated.
- [x] **Consume a discovered endpoint** — `sql` resolves a discovered `@endpoint/<id>` (host:port/db/
      user from the secret-free weak ref, dial via `conn_dial_ref`, password via the gated `credential`
      capability). Test `sql_queries_discovered_endpoint`.
- [x] **Multi-instance selection** — two discovered refs select two distinct targets (no global state).
      Test `multi_instance_selection`.
- [x] **App/agent wiring** — `flux run` and `flux app run` inject the broker as the `ReferenceResolver`
      and register the `endpoint.*` ops (D-26/D-28); verified, no extra wiring needed.
- [~] **Full URL-handback removal** — **deferred to a follow-up.** `host.endpoint` is retained for three
      narrow residuals that need new capabilities: confluence/jira attachment byte-IO (no `http_bytes_ref`
      yet), jira's *constructed* Atlassian gateway URL (`api.atlassian.com/ex/jira/{cloud_id}` — not a
      static named endpoint) + its `cloud_id`/`email` **config** reads, and sql's static env endpoint
      path. The references-only invariant holds for the primary IO surface; full removal of the
      capability is tracked (see Notes).
- [x] Gate green: full `plugins/` workspace gate (fmt + clippy `-D warnings` + tests, all packages) +
      the root gate for the wave-1 seam.

## Progress
- **Done** (with one tracked residual). Wave 1 (`45e6f8f`): the `SystemHostCaps` named-vs-discovered
  resolution split, the host-kit ref helpers (`http_ref`/`get_json_ref`/`send_json_ref`/`conn_dial_ref`/
  `credential`), and `sql` consuming a discovered Postgres endpoint. Wave 2 (`cd5c25a`): migrated the 6
  remaining URL-handback callers (alertmanager, grafana, huggingface, slack, confluence, jira) to
  ref-based JSON IO via parallel package-scoped work, then the full plugins-workspace gate green.
- **Residual / follow-up:** removing `host.endpoint` entirely needs an `http_bytes_ref` host capability
  (byte-IO), a non-secret `config`-value read (jira `cloud_id`/`email`), and dynamic-endpoint resolution
  (the Atlassian gateway). Filed as a follow-up under the epic.

## Notes
- The per-plugin migration ran one sub-agent over the 6 callers, package-scoped; the orchestrator ran the
  shared wave-1 seam + the final cross-workspace gate (parallel-subagent convention).
- Touch points: the 6 `plugins/<name>/src/main.rs` op handlers; `host-kit`'s `Host` client (wave 1);
  `crates/flux-plugin` `SystemHostCaps` (wave 1). `flux app run` wiring already present (D-26/D-28).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
