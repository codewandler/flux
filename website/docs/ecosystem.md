---
sidebar_position: 4
title: Ecosystem
description: "flux, flux-connectors and flux-exchange — what each one is, when you reach for it, and how they compose."
---

<!-- BEGIN generated:ecosystem -->
# Ecosystem

flux is three projects that share one vocabulary. The engine, language, agent loop and core tools
remain useful on their own; official external integrations cross the other two projects because
their declarations and execution authority do not belong in the engine.

> **Source of truth.** This file is `docs/ecosystem.md`. `website/docs/ecosystem.md` mirrors it inside
> a generated block; `crates/flux-lang/tests/website_in_sync.rs` fails on drift. Edit it here. The
> reasoning is [`docs/designs/ecosystem.md`](https://github.com/codewandler/flux/blob/main/docs/designs/ecosystem.md).

| | What it is | You need it when |
|---|---|---|
| **flux** | The engine, the language, the agent, and one embedded Exchange client. | Always. It owns the model loop and core capabilities, not official integration execution. |
| **flux-connectors** | Every official integration as compiled operations, manifests, a catalogue, and any vendor-specific runtime artifact. | You want reusable integration truth instead of hand-writing every vendor or protocol adapter. |
| **flux-exchange** | The only official integration executor: tenant credentials, grants, connector invocation, runtime lifecycle, and audit. | You want to use an official external integration. Rich outbound runtimes remain planned work. |

> **Dated capability snapshot (checked 2026-08-03).** The connector source reports v0.17.0;
> Exchange reports v0.16.0. These repositories move independently from flux, so use their linked
> READMEs for the live inventory rather than reading this date as a compatibility promise.

## The boundary test

One question each. If you are wondering where something belongs, this is the answer:

- **Does it change what happens when an effect executes?** → flux
- **Is it true of the integration regardless of who runs it?** → flux-connectors
- **Does it execute an official integration or require its credential or tenant grant?** → flux-exchange

## flux — the engine

A Rust agent platform built on one thesis: **the LLM is not the runtime.** The model interprets
intent and proposes actions; an authored Flux-Lang loop and a deterministic runtime own everything
after that. Every effect crosses one chain — authorization → approval → guarded IO — with no bypass.

It ships as a CLI and TUI you use daily, an embeddable Rust SDK, and an HTTP server. It knows about
*kinds* of things — operations, channels, datasources, secrets — and deliberately knows nothing about
any particular vendor. Its future official-integration surface is one native Exchange client, not a
connector runtime host.

```bash
flux run "add a test for the parser"
```

That is a complete, self-contained product. Everything below is optional.

See [Concepts](./concepts.md) for the vocabulary and [the agent loop](https://codewandler.github.io/flux/docs/agent/agent-loop) for how
a turn actually works.

## flux-connectors — integrations, compiled

Integrating a vendor usually means re-encoding what the vendor already published: a base URL, an
auth scheme, endpoints, parameters, response shapes. flux-connectors makes that information
**compiler input**.

You describe a provider once and the build emits typed Flux operations, a capability manifest,
catalogue entries, and runtime declarations consumed by Exchange. A connector can describe **both
directions**: the operations you call, and the events the vendor sends back. The current catalogue is
curated rather than automatically ingested from OpenAPI.

```flux
op zendesk-ticket-comment-add(ticket_id: Number, body: String, public: Bool) -> Any
  description "Add a comment to a ticket; internal note unless public is explicitly true"
  risk "medium"
  idempotency "conditional"
  effects ["network"]
```

Two properties are worth understanding before you build on it:

- **A connector is compiled, never interpreted.** The TOML is input; the artifact that runs is Flux,
  a real typed language with an analyzer and first-class `retry`, `throttle` and approval gates.
- **Secrets are Exchange-owned.** A credential never appears in a provider definition, generated
  Flux, the effective Service Account catalogue, or the Flux client. Exchange resolves it and applies
  the declared authentication scheme behind its authenticated boundary.

A connector describes an external capability reached over a declared protocol. Authentication is
optional — a public search API is as valid a connector as a paid SaaS product. What makes something a
connector is that its surface can be *described*.

Generated HTTP is the complete outbound runtime today, not the permanent boundary. Docker,
Kubernetes, SQL, Prometheus, secret stores, and other rich protocols are migration targets too.
Their connectors may carry attested vendor-specific binaries or images and select guarded socket,
process, container, or temporary framed-stdio runtimes. flux-connectors owns the integration-specific
code and declaration; Exchange installs and executes it. Flux does neither.

The **published** connector crates open no socket. The repository has a non-published loopback API
host that proves generated output against real guarded HTTP, but it is test infrastructure rather
than a Flux deployment path. See the
[live connector inventory](https://github.com/codewandler/flux-connectors#readme).

## flux-exchange — the platform

flux-exchange is the only official integration executor: **a service that holds tenant credentials
and settings, applies metadata grants, invokes admitted connector operations, and terminates declared
connector channels.** It may run locally for one operator or in a suitably isolated hosted
deployment; locality does not create a second Flux execution path. A human signs in, connects a
provider, previews and saves a grant, then can invoke from the admin console. Service Accounts can
authenticate for unattended calls; rich outbound runtimes are not part of that shipped path yet.

Its designed primary caller is **non-human**, not a human. Humans sign in to wire things up and to see
what happened; Service Accounts can call admitted operations all day, and future Managed Agents will
use the same bounded authority. That inverts the usual assumption and shapes the design.

**What v0.16.0 manages today:**

- **Connections** — a connector plus its credentials and settings, per tenant.
- **Identity** — complete OIDC sign-in for humans, tenant-scoped sessions, and canonical Service
  Account lifecycle plus bearer authentication.
- **Grants** — connector/risk selectors with a preview endpoint; operation-id lists are refused.
- **Invocation** — admitted HTTP operations whose destination authority comes from the connector.
- **Generated socket channels** — persistent supervised WebSockets with closed declared event sets,
  delivered through authenticated `/api/subscribe`.
- **Workflow drafts and runs** — immutable published versions execute through Flux with grant gates
  and value-free node activity.
- **Service Accounts** — create, list, revoke, and present a one-time `fxsa_…` bearer token; the
  durable store keeps only its verifier. `/api/agents` is a bounded compatibility alias for create.

**Still direction:** rich outbound runtime dispatch, webhooks and polls, general hosted channels
beyond the generated socket slice, general execution records beyond value-free workflow node
activity, streamed results, leases, isolated per-tenant workers, and installed Apps. The
[Exchange inventory](https://github.com/codewandler/flux-exchange#what-exists-today) is authoritative.

**The security property that makes it usable by agents:**

> The credential never crosses the boundary; the authority does. Outbound, a caller names an
> operation and gets a result. Inbound, a vendor's signed payload is verified by the service and the
> caller receives a typed, declared event. In neither direction does the caller come to hold a value
> it did not already have.

Grants are written over declared metadata (`risk`, `effects`, `idempotency`) rather than operation-id
lists. A Service Account token grants operations, never credentials, and lifecycle management
remains human-only.

### Running it locally

flux-exchange runs as one HTTP process. `cargo run` binds `127.0.0.1:8080`; configure its OIDC
variables for real sign-in and explicit store paths for durable credentials, settings, grants, and
Service Account records. A reachable bind without a real identity provider is refused.

```bash
git clone https://github.com/codewandler/flux-exchange && cd flux-exchange
cargo run
```

It is a **separate repository**, not a member of this workspace — `cargo run -p flux-exchange` from
a flux checkout will not resolve.

> **Verified against flux-exchange v0.16.0 on 2026-08-03.** The process serves health, catalogue,
> session/sign-in, tenant connection/settings/grant management, legacy agent minting, gated `invoke`,
> workflow drafts/runs, generated connector WebSocket channels, and authenticated `subscribe`. It
> still does **not** dispatch rich outbound runtimes or provide the general stream/lease protocol.

### Runtime model

The host vocabulary can describe several runtimes, but this is a destination rather than the v0.16.0
outbound execution inventory. Current Exchange invocation is the shareable HTTP path; generated
WebSocket channels are the first hosted rich-protocol slice.

| Runtime | For |
|---|---|
| `http` | REST and GraphQL APIs — most connectors |
| `socket` | TCP, UDP, ICMP — modelled, not currently invoked by the public server |
| `process` | local binaries — modelled and refused in a shared deployment |
| `container` | container execution — modelled, not a shipped executor |
| `plugin` | the flux plugin protocol — modelled, not a shipped executor |

The runtime is declared by the connector and never chosen by the caller — a caller who can pick the
runtime is a caller who can pick an effect.

One rule follows from this and is enforced rather than documented: **an Exchange runtime that uses
local host identity cannot be safely multi-tenant in one process.** Process, container and raw-socket
runtimes consume the host's own identity and network position. A shared deployment refuses them; a
single-tenant Exchange may serve them. Because the runtime is in the manifest, the service can make
that call mechanically.

## How they compose

The three fit together at exactly two seams.

**Core Flux — useful without Exchange.** The language, agent loop, SDK and built-in tools remain a
complete useful product. Today the binary also runs installed native plugins as a temporary
compatibility path. That does not define the destination: Flux has no local connector runtime and no
plugin fallback after migration. Its separate `connector` inbound-channel adapter remains a narrow
current feature rather than an official outbound integration executor. See
[Connector channels](https://codewandler.github.io/flux/docs/channels/connector) for its exact limits.

**Official integrations — Exchange only.** The family vocabulary has two verbs:

- **`invoke`** — name an operation, get a result. The exchange resolves the credential and builds the
  request from the operation's own compiled Flux.
- **`subscribe`** — receive verified inbound events. The exchange terminates the webhook or holds the
  socket, checks the signature with the credential it owns, and emits typed events a `trigger`
  routes to a journey.

> **Current seam:** Flux's embedded client authenticates as one Service Account, projects its
> effective catalogue at turn boundaries, and calls Exchange's one-shot HTTP `invoke` route.
> Configure it only through `FLUX_EXCHANGE_URL` and
> `FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN`; tenant, connection, credential, grant and runtime remain
> Exchange-owned. Generated socket channels can publish their closed declared event sets to
> authenticated `/api/subscribe`, but Flux does not yet consume subscribe, streaming, cancellation,
> terminal lifecycle or leases. Today's native plugin pack remains a temporary compatibility route
> for adapters that have not completed migration.

If Exchange is absent or unavailable, official external operations are withdrawn; Flux does not
change placement or fall back. Core Flux remains useful without Exchange.

## Embedding the exchange in your own product

flux-exchange publishes its host as a **crate**, not only as a binary. Routes, tenancy, the grant
model and the runtime registry are all behind traits, so a product can compose the same machinery
into its own service with its own identity provider, its own secret store, and its own additional
runtimes — without forking anything and without that product's concerns reaching the shared code.

The arrangement is what keeps the shared code shared: the public crate has no downstream dependency
to leak through, because it has only traits.

## Where things live

| | |
|---|---|
| **flux** | [github.com/codewandler/flux](https://github.com/codewandler/flux) — the engine, the language, the agent |
| **flux-connectors** | [github.com/codewandler/flux-connectors](https://github.com/codewandler/flux-connectors) — the compiler and the catalogue |
| **flux-exchange** | [github.com/codewandler/flux-exchange](https://github.com/codewandler/flux-exchange) — the shared authority host |

Crates publish to crates.io under the `codewandler-` prefix (`codewandler-flux-sdk`,
`codewandler-connector-pack`, …); the bare `flux-*` names are taken, so package names are
decoupled from crate names and your `use flux_sdk::…` imports are unaffected.

## Related

- [Concepts](./concepts.md) — the vocabulary all three share
- [Infrastructure](https://codewandler.github.io/flux/docs/infrastructure) — how the engine's pieces fit at runtime
- [Plugins](https://codewandler.github.io/flux/docs/plugins/using-plugins) — the temporary compatibility path scheduled for removal
<!-- END generated:ecosystem -->
