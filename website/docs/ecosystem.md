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
| **flux-connectors** | Vendor APIs, compiled into typed operations. | You want to call Zendesk, Slack or Stripe without writing an integration. |
| **flux-exchange** | A platform that holds credentials, terminates channels, and runs operations for many callers. | You want that shared by a team, reachable by agents, and auditable — instead of living in one person's environment variables. |

## The boundary test

One question each. If you are wondering where something belongs, this is the answer:

- **Does it change what happens when an effect executes?** → flux
- **Is it true of the vendor regardless of who runs it?** → flux-connectors
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

## flux-connectors — vendor descriptions, compiled

Integrating a vendor usually means re-encoding what the vendor already published: a base URL, an
auth scheme, endpoints, parameters, response shapes. flux-connectors makes that information
**compiler input**.

You describe a provider once — often little more than a pointer at its OpenAPI document plus a few
corrections — and the build emits typed Flux operations, a capability manifest, and a catalogue
entry. A connector describes **both directions**: the operations you call, and the events the vendor
sends back.

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
- **Secrets are references the host resolves.** A credential never appears in a provider definition,
  in generated Flux, or in a lockfile. The generated call carries an auth *reference*; whoever runs
  it resolves the value and registers it with the redactor.

A connector describes an external capability reached over a declared protocol. Authentication is
optional — a public search API is as valid a connector as a paid SaaS product. What makes something a
connector is that its surface can be *described*.

flux-connectors produces descriptions. It runs nothing.

## flux-exchange — the platform

Everything above works on your laptop with credentials in your environment. That stops working the
moment you want a team to share an integration, an agent to use it unattended, or an auditor to ask
what happened.

flux-exchange is the deployed answer: **a service that holds the credentials, terminates the
channels, runs the operations, and records what happened.** You sign in, connect a provider (by
pasting a token or by OAuth), and the operations become available — to you in a console, and to your
agents over an API.

Its primary caller is an **agent**, not a human. Humans sign in to wire things up and to see what
happened; agents are what actually call operations all day. That inverts the usual assumption and
shapes the whole design.

**What it manages:**

- **Connections** — a connector plus its credentials and settings, per tenant.
- **Channels** — inbound surfaces it holds on your behalf: a webhook endpoint, a Slack socket, a
  schedule.
- **Workflows** — stored programs: triggers, conditions, and flows of operations. An agent may be a
  step in one; nothing requires it.
- **Custom operations** — compositions of existing operations, authored visually, which compile down
  to the same Flux as everything else and are therefore indistinguishable from vendor operations to
  whoever calls them.
- **Runs** — what fired, what it called, what came back.

**The security property that makes it usable by agents:**

> The credential never crosses the boundary; the authority does. Outbound, a caller names an
> operation and gets a result. Inbound, a vendor's signed payload is verified by the service and the
> caller receives a typed, declared event. In neither direction does the caller come to hold a value
> it did not already have.

An agent's token grants access to *operations*, never to credentials — and grants are written over
declared metadata (`risk`, `effects`, `idempotency`) rather than over lists of names. "This role may
only call non-writing operations" is a rule the catalogue can check, not a list somebody maintains.

### Running it locally

flux-exchange runs as a single process with no sign-in, one tenant, and the full runtime set —
including the runtimes that cannot be safely shared. That is the development mode, and it is also a
perfectly good single-operator deployment.

```bash
git clone https://github.com/codewandler/flux-exchange && cd flux-exchange
cargo run
```

It is a **separate repository**, not a member of this workspace — `cargo run -p flux-exchange` from
a flux checkout will not resolve.

> **What that command does today: prints which runtimes each deployment shape would serve, and
> exits.** It binds no port, holds no credential, and answers no request. Sign-in, the credential
> store, `invoke`, `subscribe`, channels, stored workflows and execution records are all described
> above and none of them are built. The repository's own README carries the itemized inventory; treat
> this section as the charter it is working towards, not as a description of shipped software.

### Runtimes

Not everything is HTTP. A connector declares how it executes, and the operator grants it:

| Runtime | For |
|---|---|
| `http` | REST and GraphQL APIs — most connectors |
| `socket` | TCP, UDP, ICMP — reachability checks, protocol probes |
| `process` | local binaries, with sandboxing |
| `container` | the same, inside Docker or Kubernetes |
| `plugin` | the flux plugin protocol |

The runtime is declared by the connector and never chosen by the caller — a caller who can pick the
runtime is a caller who can pick an effect.

One rule follows from this and is enforced rather than documented: **a locally-executing runtime
cannot be safely multi-tenant in one process.** Process, container and raw-socket runtimes consume
the host's own identity and network position. A shared deployment refuses them; a single-tenant one
serves them. Because the runtime is in the manifest, the service can make that call mechanically.

## How they compose

The three fit together at exactly two seams.

**Locally — flux loads connectors directly.** A connector module and its manifest sit in
`~/.flux/`, and flux runs the operations itself with credentials from your environment. No service
involved.

**Hosted — flux talks to flux-exchange.** flux points at an exchange, authenticates, and gets two
verbs:

- **`invoke`** — name an operation, get a result. The exchange resolves the credential and builds the
  request from the operation's own compiled Flux.
- **`subscribe`** — receive verified inbound events. The exchange terminates the webhook or holds the
  socket, checks the signature with the credential it owns, and emits typed events a `trigger`
  routes to a journey.

> **Neither verb is built.** `invoke` exists inside flux-connectors' own host and is not yet reachable
> from flux; `subscribe` does not exist anywhere. What *does* work today is the **local** connector
> channel below — flux reads a published connector manifest, binds the listener itself, and verifies
> the signature with a credential the operator supplies. The remote form is the proposal; this is the
> shipped one.

```flux
channel support
  kind "connector"
  connector "slack"
  binding "events-api"
  addr "0.0.0.0:8790"
  path "/slack/events"

trigger on_mention
  on "support.app_mention"
  run answer
```

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
| **flux-exchange** | `github.com/codewandler/flux-exchange` — the platform |

Crates publish to crates.io under the `codewandler-` prefix (`codewandler-flux-sdk`,
`codewandler-connector-pack`, …); the bare `flux-*` names are taken, so package names are
decoupled from crate names and your `use flux_sdk::…` imports are unaffected.

## Related

- [Concepts](./concepts.md) — the vocabulary all three share
- [Infrastructure](https://codewandler.github.io/flux/docs/infrastructure) — how the engine's pieces fit at runtime
- [Plugins](https://codewandler.github.io/flux/docs/plugins/using-plugins) — the other extension path, and when it is still the right one
<!-- END generated:ecosystem -->
