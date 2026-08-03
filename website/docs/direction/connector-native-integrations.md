---
title: Connector-native integrations
description: "Why every official integration is moving to flux-connectors, what Flux and Exchange retain, and which parity gates prevent a premature cutover."
---

# Connector-native integrations

Flux is moving from two official integration models to one:

> Every official external integration is a connector. Protocol richness selects its declared
> runtime; it does not select a different repository or trust model.

Generated HTTP connectors already prove the declaration and catalogue model. The destination also
includes Docker, Kubernetes, SQL, Prometheus, Loki, secret stores, collaboration systems, and every
other vendor-specific adapter that currently ships in Flux's signed plugin pack.

## Ownership after the migration

| Layer | Owns |
|---|---|
| **flux-connectors** | Vendor operations, schemas, effects, configuration, events, runtime selection, and any immutable vendor-specific runtime artifact. |
| **Flux** | Generic guarded runtime mechanisms, local-first execution, authorization, approval, and guarded IO. Stdio/process, HTTP, socket, container, plugin, and remote execution are mechanisms—not vendor catalogues. |
| **Flux Exchange** | Tenant authority, connections, grants, hosted connector execution, subscriptions, and audit records. The credential stays behind its boundary. |

The connector declares its runtime; a caller never chooses one to widen its authority. The same
connector address should run locally in Flux or under tenant authority in Exchange without either
host rebuilding vendor behavior.

## What ships today

- The signed native plugin pack remains supported and is still the current path for the rich
  integrations listed in [Using plugins](../plugins/using-plugins.md).
- Generated HTTP connectors run locally, and Flux has a guarded generated WebSocket channel
  substrate. Rich outbound connector runtimes are not complete.
- Exchange invokes admitted HTTP connector operations and terminates generated connector socket
  channels. General rich-runtime dispatch remains planned.

This direction changes ownership and the migration target. It does **not** claim that Docker,
Kubernetes, SQL, observability, or secret-store connectors already replace their plugins.

## Cutover gates

No vendor-specific plugin disappears merely because a connector with the same name exists. Each
migration must prove:

1. operation, schema, effect, refusal, streaming, and configuration parity;
2. the same conformance contract in local Flux and hosted Exchange placement;
3. authorization → approval → guarded IO with no vendor-specific bypass;
4. immutable, attested runtime artifacts where declarations alone are insufficient; and
5. an explicit compatibility and removal decision for the old plugin crate.

Until those gates pass, the plugin is the supported compatibility implementation. Third-party
plugins and the generic stdio protocol may remain even after the first-party vendor fleet has moved.

## Program map

- [Ecosystem](../ecosystem.md) — the complete Flux / connectors / Exchange boundary and current
  capability snapshot.
- [Topologies](../topologies.md) — local, remote-effect, worker, and hosted placement without
  conflating Docker/Kubernetes management with execution placement.
- [Flux roadmap](https://github.com/codewandler/flux/blob/main/docs/roadmap.md#connector-native-integrations--one-catalogue-across-every-runtime-epic--in-progress-c-500)
  — the runtime, conformance, migration, and retirement stories.
- [flux-connectors roadmap](https://github.com/codewandler/flux-connectors/blob/main/docs/roadmap.md)
  — runtime declaration, artifact, projection, and per-adapter migration work.
- [Flux Exchange roadmap](https://github.com/codewandler/flux-exchange/blob/main/docs/roadmap.md)
  — hosted rich-runtime dispatch, tenant isolation, subscriptions, and audit work.
