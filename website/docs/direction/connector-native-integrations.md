---
title: Connector-native integrations
description: "Why Exchange is the only future official integration executor, what ships today, and which gates prevent a premature plugin cutover."
---

# Connector-native integrations

Flux is moving from two official integration models to one:

> Every official external integration is a connector, and Exchange is the only official integration
> executor. Flux embeds the client; it does not execute the connector runtime or fall back locally.

Generated HTTP connectors already prove the declaration and catalogue model. The destination also
includes Docker, Kubernetes, SQL, Prometheus, Loki, secret stores, collaboration systems, and every
other vendor-specific adapter that currently ships in Flux's signed plugin pack.

## Ownership after the migration

| Layer | Owns |
|---|---|
| **flux-connectors** | Vendor operations, schemas, effects, configuration, events, runtime selection, and any immutable vendor-specific runtime artifact. |
| **Flux** | One embedded Exchange client, tool projection, authorization and approval. It owns no official connector runtime, installer, pack or fallback. |
| **Flux Exchange** | Tenant authority, connections, grants, connector runtime execution, subscriptions, leases, and audit records. The credential stays behind its boundary. |

The connector declares its runtime; a caller never chooses one to widen its authority. Exchange may
run locally for a single operator or in an isolated hosted deployment, but Flux never becomes a
second execution placement.

## What ships today

- The signed native plugin pack remains supported and is still the current path for the rich
  integrations listed in [Using plugins](../plugins/using-plugins.md). It is temporary compatibility
  behavior, not the future architecture.
- Exchange invokes admitted HTTP connector operations and terminates generated connector socket
  channels. General rich-runtime dispatch remains planned.
- Flux now embeds the Exchange client. When `FLUX_EXCHANGE_URL` and
  `FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN` are configured, it projects that account's effective
  catalogue between turns and invokes admitted one-shot HTTP operations. That environment bearer is
  transitional C-503 compatibility, not final onboarding; C-509 replaces it with an Exchange-owned
  direct handoff into secure storage. Subscribe, streaming,
  cancellation frames, terminal lifecycle and leases remain planned.

This direction changes ownership and the migration target. It does **not** claim that Docker,
Kubernetes, SQL, observability, or secret-store connectors already replace their plugins. Core Flux
remains useful without Exchange; Exchange-backed official external integrations are unavailable
when Exchange is unavailable.

## Cutover gates

No vendor-specific plugin disappears merely because a connector with the same name exists. Each
migration must prove:

1. operation, schema, effect, refusal, streaming, and configuration parity against frozen legacy
   fixtures;
2. the replacement running through Exchange, with no local connector or plugin fallback;
3. authorization and approval in Flux plus guarded execution in Exchange, with no vendor-specific
   bypass;
4. immutable, attested runtime artifacts where declarations alone are insufficient; and
5. deletion of the old plugin crate in the same release train as its proven replacement.

Until those gates pass, the plugin is the supported compatibility implementation. After the final
adapter moves, C-506 removes the plugin host and protocol, installer, signed pack, index, and every
Flux release artifact or upload path. A temporary framed-stdio artifact may exist only behind
Exchange and is built by the connector/Exchange pipeline.

## Program map

- [Ecosystem](../ecosystem.md) — the complete Flux / connectors / Exchange boundary and current
  capability snapshot.
- [Topologies](../topologies.md) — core Flux, remote effects, workers, and Exchange execution without
  conflating Docker/Kubernetes management with official integration placement.
- [Cross-repository roadmap](https://github.com/codewandler/flux-roadmap) — the source of truth for
  architecture, dependency order, milestones, and the active tranche.
- [Flux roadmap](https://github.com/codewandler/flux/blob/main/docs/roadmap.md#connector-native-integrations--one-catalogue-across-every-runtime-epic--in-progress-c-500)
  — C-500…C-506: documentation, embedded HTTP client, later lifecycle, per-adapter proof/deletion,
  and unconditional plugin-infrastructure removal.
- [flux-connectors roadmap](https://github.com/codewandler/flux-connectors/blob/main/docs/roadmap.md)
  — runtime declaration, artifact, projection, and per-adapter migration work.
- [Flux Exchange roadmap](https://github.com/codewandler/flux-exchange/blob/main/docs/roadmap.md)
  — effective Service Account catalogue, rich-runtime dispatch, tenant isolation, lifecycle, and
  audit work.
