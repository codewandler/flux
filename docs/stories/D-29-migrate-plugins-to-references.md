---
id: D-29
title: Migrate native plugins to references & consume discovered endpoints
pillar: Agent
status: backlog
priority:
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
- [ ] **No env/URL on the op surface** — a representative migrated plugin (e.g. `gitlab`) names no
      environment variable in its manifest and handles no URL/token in its op handlers; proven by a test
      scanning the manifest + op surface. Failing-first test `plugins::no_env_or_url_on_op_surface`.
- [ ] **Consume a discovered endpoint** — `sql` resolves a discovered `endpoint_ref` and runs a query
      against it (host-injected credential). Test `sql::queries_discovered_endpoint` (MockHost).
- [ ] **Multi-instance selection** — two discovered `postgres` endpoints are individually selectable
      via their refs. Test `endpoint::multi_instance_selection`.
- [ ] **App/agent wiring** — the `flux app run` and agent paths load the discovery broker + registry so
      an agent turn can discover endpoints and use them through the envelope.
- [ ] **Clean cutover** — no plugin retains a parallel env-resolved path; host binding config is the
      only static source (per the no-fallbacks rule).
- [ ] Gate green: full `plugins/` workspace + host gate, clippy `-D warnings`, fmt; `scripts/smoke-plugins.sh`
      updated for the ref-based path.

## Progress
- (not started — needs [D-27](D-27-reference-based-io.md) and
  [D-28](D-28-kubernetes-endpoint-provider.md).)

## Notes
- **Parallelize the per-plugin migration** — one sub-agent per plugin, run in parallel (the
  parallel-subagent convention used on D-14); the orchestrator keeps the shared host/schema changes and
  the final cross-workspace gate.
- Touch points: every `plugins/<name>/src/main.rs` manifest + op handlers; `host-kit`'s `Host` client;
  the `flux app run` registry wiring (cf. D-09).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
