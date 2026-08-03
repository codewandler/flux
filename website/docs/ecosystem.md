---
sidebar_position: 4
title: Ecosystem
description: "flux, flux-connectors and flux-exchange — what each one is, when you reach for it, and how they compose."
---

<!-- BEGIN generated:ecosystem -->
# Ecosystem

flux is three projects that share one vocabulary. You can use the first on its own forever; the other
two exist because two specific problems turned out not to belong in an engine.

> **Source of truth.** This file is `docs/ecosystem.md`. `website/docs/ecosystem.md` mirrors it inside
> a generated block; `crates/flux-lang/tests/website_in_sync.rs` fails on drift. Edit it here. The
> reasoning is [`docs/designs/ecosystem.md`](https://github.com/codewandler/flux/blob/main/docs/designs/ecosystem.md).

| | What it is | You need it when |
|---|---|---|
| **flux** | The engine, the language, the agent. | Always. It is the thing that runs. |
| **flux-connectors** | Every official integration as compiled operations, manifests, a catalogue, and any vendor-specific runtime artifact. | You want reusable integration truth instead of hand-writing every vendor or protocol adapter. |
| **flux-exchange** | A shared host for tenant credentials, metadata grants, connector invocation, and declared event channels. | You want integration authority or runtime placement managed outside one person's environment. Rich outbound runtimes remain planned work. |

> **Dated capability snapshot (checked 2026-08-03).** The connector source reports v0.17.0;
> Exchange reports v0.16.0. These repositories move independently from flux, so use their linked
> READMEs for the live inventory rather than reading this date as a compatibility promise.

## The boundary test

One question each. If you are wondering where something belongs, this is the answer:

- **Does it change what happens when an effect executes?** → flux
- **Is it true of the integration regardless of who runs it?** → flux-connectors
- **Does it require holding a credential or knowing a tenant?** → flux-exchange

## flux — the engine

A Rust agent platform built on one thesis: **the LLM is not the runtime.** The model interprets
intent and proposes actions; an authored Flux-Lang loop and a deterministic runtime own everything
after that. Every effect crosses one chain — authorization → approval → guarded IO — with no bypass.

It ships as a CLI and TUI you use daily, an embeddable Rust SDK, and an HTTP server. It knows about
*kinds* of things — operations, channels, datasources, secrets — and deliberately knows nothing about
any particular vendor.

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
catalogue entries, and a host Tool pack. A connector can describe **both directions**: the operations
you call, and the events the vendor sends back. The current catalogue is curated rather than
automatically ingested from OpenAPI.

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
- **Secrets are host-owned.** A credential never appears in a provider definition or generated
  Flux. The Tool pack resolves the value, applies the declared authentication scheme, and registers
  it with the host redactor before asking a caller-supplied `http.request` to dispatch.

A connector describes an external capability reached over a declared protocol. Authentication is
optional — a public search API is as valid a connector as a paid SaaS product. What makes something a
connector is that its surface can be *described*.

Generated HTTP is the complete runtime today, not the permanent boundary. Docker, Kubernetes, SQL,
Prometheus, secret stores, and other rich protocols are migration targets too. Their connectors may
carry attested vendor-specific binaries or images and select guarded socket, process, container,
remote, or plugin runtimes. Flux supplies those generic mechanisms; the connector repository owns
the integration-specific code and declaration.

The **published** connector crates open no socket. The repository has a non-published loopback API
host that proves the pack against real guarded HTTP, but deployment remains the consuming host's
job. See the [live connector inventory](https://github.com/codewandler/flux-connectors#readme).

## flux-exchange — the platform

Everything above works on your laptop with credentials in your environment. That stops working the
moment you want a team to share an integration, an agent to use it unattended, or an auditor to ask
what happened.

flux-exchange is the deployed answer: **a service that holds tenant credentials and settings, applies
metadata grants, invokes admitted connector operations, and terminates generated connector socket
channels.** A human signs in, connects a provider, previews and saves a grant, then can invoke from
the admin console. Service Accounts can authenticate for unattended calls; rich outbound runtimes
are not part of that shipped path yet.

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

One rule follows from this and is enforced rather than documented: **a locally-executing runtime
cannot be safely multi-tenant in one process.** Process, container and raw-socket runtimes consume
the host's own identity and network position. A shared deployment refuses them; a single-tenant one
serves them. Because the runtime is in the manifest, the service can make that call mechanically.

## How they compose

The three fit together at exactly two seams.

**Locally — flux remains complete without Exchange.** The binary currently runs built-ins and
installed native plugins through its own safety envelope. The migration replaces those
vendor-specific plugins with connector bundles behind the same generic guarded runtimes. Flux can
also read a published connector manifest for the `connector` inbound channel, but today that adapter
serves only explicitly unsigned webhook bindings and does not auto-install the external outbound Tool pack. See
[Connector channels](https://codewandler.github.io/flux/docs/channels/connector) for the exact limits.

**Hosted — Exchange implements HTTP invoke and one subscribe slice of the remote binding.** The family vocabulary has
two verbs:

- **`invoke`** — name an operation, get a result. The exchange resolves the credential and builds the
  request from the operation's own compiled Flux.
- **`subscribe`** — receive verified inbound events. The exchange terminates the webhook or holds the
  socket, checks the signature with the credential it owns, and emits typed events a `trigger`
  routes to a journey.

> **Current seam:** Exchange `invoke` is built for signed-in humans and Service Accounts whose tenant
> has a connection and an admitting grant. Generated socket channels can publish their closed declared event sets to
> authenticated `/api/subscribe`. General subscribe does not exist beyond that socket slice. Flux
> itself has no Exchange client binding, the general
> rich-runtime stream/lease protocol is not built, and a Flux agent cannot mount that route as a
> connector placement today. The shipped local connector channel is separate and deliberately narrower.

**flux never *requires* the exchange.** The local path is complete and stays complete. Trading
binary-distribution pain for service lock-in would be a bad trade made twice.

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
- [Plugins](https://codewandler.github.io/flux/docs/plugins/using-plugins) — the current compatibility path and the generic stdio runtime
<!-- END generated:ecosystem -->
