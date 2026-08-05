# Design: the flux ecosystem — three repositories, one vocabulary

**Status:** amended by C-508 and flux-roadmap Decision 0001, 2026-08-03 · **Scope:** Flux's local
contract within the cross-repository program · **Produces:**
[`docs/ecosystem.md`](../ecosystem.md) (the end-user description), amendments to
`flux-connectors/docs/vision.md`, and the charter for `codewandler/flux-exchange`

> Cross-repository architecture, ordering and milestone scope are owned by the sibling
> `flux-roadmap` repository. This document is Flux's local reasoning and implementation contract;
> `docs/ecosystem.md` is the public description derived from it. Where either conflicts with
> flux-roadmap Decision 0001, amend the local contract before implementation.
>
> Measurements were taken on **2026-08-01** against the working trees at
> `~/projects/flux`, `~/projects/flux-connectors`, and the downstream product tree that consumes
> them. Re-grep by symbol; line numbers move.

## Why this exists

Three repositories grew from one idea and their charters no longer describe them. flux-connectors'
vision says connectors are *"paid SaaS services"* and that technology adapters *"stay core to flux as
plugins"* — a sentence that has now blocked four filed stories (C-46, C-123, C-133, C-157), each one
an instance of the same missing category. Meanwhile a downstream product had independently built a
*fourth* implementation of the same host, and had written a design around publishing crates that
were, in fact, already published.

The cost of not having this document is not confusion; it is duplicated work and stalled backlog.

## The four domains, and the test that separates them

Each domain gets one interrogative. The test is mechanical on purpose — a boundary that requires
taste is a boundary that erodes.

| Domain | Test | Owns |
|---|---|---|
| **flux** (engine) | *Does it change what happens when an effect executes?* | The envelope, the substrate, Flux-Lang, the agent, the SDK. Knows **kinds**, never vendors. |
| **flux-connectors** | *Is it true of the integration regardless of who runs it?* | Connector facts, compiled: operations, events, credentials-required, config-required, runtime binding, and vendor-specific runtime artifacts. No tenancy or credential values. |
| **flux-exchange** | *Does it require holding a credential or knowing a tenant?* | Principals, connections, credentials, channels, installed apps, datasource/trigger bindings, event deliveries, model profiles, leases, stored programs, execution records. |
| **a downstream product** | *Is it true only of one company's customers?* | Its accounts, its channels, its console, its identity provider. |

Three consequences worth stating because they are the ones people get wrong:

- **A vendor definition belongs in the public flux-connectors repository whenever the API it
  describes is public** — that is what makes it a vendor fact rather than a private one. An
  *identity adapter* for one company never does, because it is true only of that company.
- **A connector replaces an official plugin; it does not create another Flux extension path.** A
  connector-owned artifact may temporarily speak the framed stdio protocol behind Exchange, but it
  is never installed, executed or released by Flux.
- **No flux-family repository names a downstream company.** The rule is the fourth row applied to
  the documentation itself: a product's name is true only of that product, so it belongs in that
  product's repository. This document's own first draft violated it, which is evidence the rule
  needs to be written down rather than assumed. A pre-existing audit of the flux tree found ten
  files already carrying such a name, including three crate sources — that cleanup is its own
  story, not this document's.

## What the charters get wrong, specifically

Three claims in `flux-connectors/docs/vision.md` and its README must be replaced, not softened.

**1. "Connectors are paid SaaS services."** False, and expensively so. A Wikipedia search connector
has no credential at all. Ollama is a local process. A generic `http` or `mcp` connector is neither a
SaaS product nor a technology adapter — C-46 named it *"a third category the boundary does not
name"* two months before this document. The replacement framing:

> A connector describes **an external capability reached over a declared protocol.** Authentication
> is optional. What makes something a connector is that its surface can be *described*; what makes
> something not one is that it cannot.

**2. "Technology adapters stay core to flux as plugins."** Superseded by the runtime axis below.
Docker, Kubernetes and SQL do not need to be plugins-rather-than-connectors; they need a runtime
other than HTTP, which is a different statement.

**3. "Flux can execute the same connector locally."** Superseded. Flux remains a complete language,
agent loop and core-tool product without Exchange, but official external integrations require
Exchange. When Exchange is absent, those tools are unavailable; Flux does not fall back to a plugin,
connector bundle or vendor adapter.

## The runtime axis — what replaces the plugin/connector dichotomy

The stated pain that motivated flux-connectors was distribution: writing a plugin means Rust, the
plugin protocol, cross-compilation, GitHub release artifacts, a signed pack index, crates.io, and an
install step for every user. That tax is real and it is not intrinsic to the *protocol* — it is
intrinsic to shipping a binary to every user.

**Reframe: runtime is a connector declaration that Exchange executes.**

| Runtime | Exchange executes by | Ownership |
|---|---|---|
| `http` | evaluating the connector's compiled Flux through guarded HTTP | declaration in flux-connectors; credential, grant and execution in Exchange |
| `socket` | a guarded connector-declared socket plan | declaration/artifact in flux-connectors; execution and lifecycle in Exchange |
| `process` | a guarded, argv-only runtime artifact | immutable artifact in flux-connectors; installation and execution in Exchange |
| `container` | a digest-pinned isolated runtime artifact | immutable artifact in flux-connectors; installation and execution in Exchange |
| `plugin` | a temporary framed-stdio artifact behind Exchange | connector/Exchange pipeline only; never a Flux release artifact |

The invariant that must survive the generalization, because it is the one that makes the whole thing
reviewable:

> **The runtime is declared by the connector, never chosen by the caller.** A caller who can pick the
> runtime is a caller who can pick an effect. The manifest names; the operator grants.

### The physical ownership consequence

Every official integration moves to flux-connectors, including adapters whose implementation remains
hand-written Rust. Exchange is the only official execution placement and owns credential resolution,
grants, runtime execution and lifecycle. Flux retains its safety envelope and guarded mechanisms for
core capabilities, but no connector runtime host, stdio plugin protocol, vendor adapter or local
fallback. Connector-specific binaries and images are immutable artifacts built by the
connector/Exchange pipeline and attested with their declarations.

Measured on 2026-08-03 with
`find plugins -mindepth 2 -maxdepth 2 -name Cargo.toml -printf '%h\n' | sort`, the current Flux tree
contains eighteen integration adapters after excluding `host-kit` and `pack-index`: collaboration
(`confluence`, `gitlab`, `jira`, `slack`), infrastructure (`docker`, `kubernetes`), observability
(`alertmanager`, `grafana`, `loki`, `opsgenie`, `prometheus`), data/secrets (`onepassword`, `sql`,
`vault`), and remaining adapters (`aws`, `homer`, `huggingface`, `websearch`). Connector stories
C-499…C-503 own those connector migration waves; Flux C-505 owns retirement only after parity.

### The multi-tenancy rule that falls out of it

HTTP is easy to multi-tenant because the effect leaves the machine. Process spawning, container
exec and raw sockets do not — they consume the host's own identity, network position, filesystem and
descriptors.

> **A locally-executing runtime cannot be safely multi-tenant in one process.** It requires either
> single-tenant deployment (which is exactly local-dev mode: one operator, no login) or per-tenant
> isolation at the OS or pod level.

Because the manifest *declares* its runtime, an Exchange deployment can **refuse** a `process` connector
mechanically rather than relying on an operator noticing. That is a fail-closed rule and it costs one
check.

## `flux-system` as the shared substrate

Flux core tools and Exchange runtimes can need the same execution primitives under different policy.
The instinct is to create a new crate for it.
That instinct is wrong, because the crate already exists and a new one would collide with
`flux-runtime` — which is the **opposite** concern.

- `flux-runtime` (L2) — `Executor::dispatch`. Permission → approval → execute. **Judgment.**
- `flux-system` (L2) — the only place real IO happens. **Mechanism.**

They are peers at the same layer, not stacked, and fusing them would force every consumer of the
substrate to also take flux's approval model — including consumers with no human at a terminal to
prompt. Reimplementing guarded IO to escape that is precisely the failure the substrate prevents.

**Decision: publish `codewandler-flux-system` as the shared substrate and grow its `port` module.**
No new crate, no new name. Two follow-ons:

- The workspace-confined **file surface becomes a port** (C-269 deferred this on the correct
  grounds that its consumers all held a concrete `System`, so a trait had no seam — a second
  consumer is exactly the condition that changes).
- New shared execution primitives may land as `Backend` variants or port implementations, never as a
  Flux-side official connector dispatcher or second IO path.

## The three lifetimes

Routinely conflated, and the conflation produces real bugs — a webhook endpoint that dies when an
agent disconnects, or a lease that outlives the grant that opened it.

| | Scope | Direction | Dies when |
|---|---|---|---|
| **Session** | a conversation | — | it is closed or expires; resumable |
| **Channel** | a deployment | pushes | the operator removes it |
| **Lease** | a caller's grant | pulls | the holder releases it, or TTL |

A **room** is a channel with attribution — flux already models this correctly, and the reason it
carries an occupant id on every event is that attribution is the precondition for deciding whether
to answer.

**On the word "lease".** The owner's original framing was "session (open|close)", which is the right
mechanism under a colliding name: flux's `session` already means an event-sourced conversation, and
the two have opposite lifetimes and opposite owners. `lease` is used so that a sentence about one can
never be misread as a sentence about the other.

## The embedded Exchange binding: one-shot first, lifecycle later

The core Flux binary embeds one native Exchange client. It is not an operator-selected placement,
helper executable, plugin or installed pack. The first useful slice is deliberately smaller than the
event and runtime lifecycle:

- **Milestone 1: effective catalogue plus `invoke`.** Exchange returns only the connected and granted
  operations available to one Service Account, with a stable generation identity. Flux refreshes
  that projection only between turns, then sends an operation id and arguments. The host resolves the credential,
  evaluates the operation's own compiled Flux to build the request, and dispatches. This exists in
  the non-published `flux-connectors/crates/connectors-api` proof host and, for tenant/grant-scoped
  HTTP operations, in flux-exchange v0.13.0.
- **Milestone 3: `subscribe`, streams, cancellation and leases.** The host terminates the vendor
  webhook, holds the socket, or runs the poll; it
  verifies the signature **with the credential it owns**; it maps the payload through the binding the
  manifest declares; and it emits a normalized, typed event to a subscriber. Exchange v0.16.0 has
  the generated WebSocket-channel slice for declared socket event sets; webhooks, polls, arbitrary
  rich-runtime streams, replay, and lease liveness remain program work.

The invariant, which is the whole security argument:

> **The credential never crosses the seam in either direction; the authority does.** Outbound, flux
> sends an operation id and arguments and receives a result. Inbound, the vendor sends a signed
> payload to the host and flux receives a verified, connector-declared event. In neither direction
> does flux come to hold a value it did not already have.

### The confused-deputy argument, second half

`flux-connectors/docs/designs/connectors-api.md` makes this argument for `invoke` and has never had
to make it for `subscribe`. It owes it, and this is the shape:

The outbound answer is that *the caller cannot name the authority* — not a host (the URL comes from
compiled Flux), not a credential (the address is derived from session tenant + manifest authority),
not a tenant (read from the session and from nothing a caller controls).

The inbound answer is its mirror: **a subscriber cannot name a binding it has not been granted.** The
event stream is scoped by the same tenant derivation as the credential address. A subscription is not
a request for events from a source the caller names; it is a projection of the connections that
tenant already has.

### Transport

**HTTP** is the complete Milestone 1 Flux client surface: effective catalogue and stateless
`invoke`. Credential and connection management remain operator surfaces rather than Service Account
client responsibilities. On supported Linux, plan reads, connection changes, grants and Service
Account minting require the separately OS-owner-authenticated native management client; those routes
cannot be added to the Service-Account-only runtime client or aimed at a remote host. Every Flux
platform, including Linux, may use an independently provisioned Linux Exchange only through the
runtime HTTP client; secure remote provisioning requires a future provider contract. In the final
Linux-local bootstrap, Service Account token bytes may
exist only inside a host-owned secret resolver and sensitive Authorization transport, never argv,
the environment, ordinary diagnostics or JSON, configuration, logs, events, session state or
model-visible state.

**One websocket per connected caller** is later lifecycle work for the three things that do not fit request/response, which
are all the same shape — a long-lived authenticated bidirectional frame stream:

1. inbound events (`subscribe`),
2. streamed operation output (`logs -f`, process stdout, a socket read loop),
3. lease liveness — the host must learn the holder died so it can release what it is holding.

Flux needs no new trigger concept to consume this. Exchange's authenticated `/api/subscribe` exists
for its generated socket slice; projecting those events, streamed operation output, cancellation and
lease liveness into Flux requires a separate lifecycle story and is not acceptance for C-503.

## Principals and grants

flux-exchange's primary caller is **non-human**, not a human. Humans sign in to manage and to observe.
That inverts the usual assumption and it has to be in the model from the start rather than bolted on.

**Three principal kinds, one grant model:**

- **User** — a human. Manages connections, credentials, groups; may run operations interactively.
- **Service Account** — a non-human API principal holding its own minted token, belonging to roles.
  Flux Exchange's current `/api/agents` route is the legacy spelling to migrate.
- **Service** — another backend acting on behalf of `(account, actor)`. Products that already front
  a connector service tend to have invented this header set independently, so adopting the model is
  usually a rename rather than a change.

A grant is `(principal | role) × connection × operation-selector`, and the selector is a **predicate
over declared metadata** — `risk <= low`, `effects ⊆ {network}`, `idempotency = idempotent` — plus
explicit allow/deny by operation id for exceptions. Writing it over metadata rather than over names
is what stops it drifting: the catalogue already publishes `risk`, `effects` and `idempotency` for
every operation, so "this role may only call non-writing operations" is checkable rather than
maintained.

The property that makes Service Accounts safe:

> **A Service Account token grants access to an operation, never to a credential.** The credential is
> resolved by the host from the connection the grant names. A stolen token yields a bounded
> operation set against one tenant's connections — never a vendor token.

A Flux **Agent** keeps its core meaning: model + loop + bounded operations/datasources. When
flux-exchange hosts one inside an installed App it is a **Managed Agent**, receiving reviewed App
authority without becoming the Service Account that calls the public API.

## Apps are installed Programs, not a second workflow model

flux-exchange needs to persist "workflows": triggers, conditions, schedules, and flows of operations
that may or may not involve an agent. `flux-app` already is this — a `.flux` program declaring
`agent`, `channel`, `datasource`, `trigger` and `journey`, where an agent is one node kind and
nothing requires one.

**Decision: an App is a stored, versioned, per-tenant Program plus its installed bindings.** A visual
editor emits the IR; the IR lowers to Flux. “Workflow” remains ordinary descriptive prose, not a
second domain type or execution model. The simplified schema an editor wants is a *projection*,
never a second model.

What that buys, and why any other choice is worse: determinism, replay, fork/diff, approval gates,
typing, and risk derivation all come free, and a composed operation becomes **indistinguishable from
a vendor one** — same catalogue entry, same gating, same address. An agent cannot tell whether an
operation came from an OpenAPI document or from someone dragging boxes, which is precisely what makes
the editor useful to agents rather than only to humans.

This also means flux-exchange never ships an interpreter, which keeps flux-connectors' north star
(*"a connector is compiled, never interpreted"*) intact across the whole family.

**Unblocked upstream; not yet downstream — and the distinction is the whole story.** The prerequisite
was `http.request` returning a flat string, which refused any composite operation reading a field out
of a previous step's response. It now returns the record `{status, headers, body}` as canonical
`content`, keeping the flat rendering as the model-facing `view` (`flux-web/src/http.rs`).

That landed in flux at **v0.43.0**. The downstream migration has now happened: flux-connectors
v0.16.0 pins the Flux engine crates, including `flux-web`, on the **0.52** line. The earlier statement
that its 0.41 pin still blocked composite response selection became false; retaining the dated
history here explains the seam without presenting a closed dependency migration as current work.

## How a downstream product reuses this without forking it

The requirement: a product must be able to adopt flux-exchange without a rewrite, and without its own
concerns leaking into a public repository. Both halves matter — a reuse story that requires a fork is
not reuse, and one that requires the shared repository to learn a customer's name is not shared.

**The mechanism is ports plus published crates, not a fork.**

- flux-exchange publishes its host as a **crate**, not only a binary: routes, tenancy, the grant
  model and the runtime registry, all behind traits. A product composes that crate into its own
  binary with its own identity adapter, its own secret store, and any additional runtimes.
- **Identity is a port.** flux-exchange ships an OIDC adapter for self-serve sign-in and a dev mode;
  a product with its own IdP binds token introspection instead. Same trait.
- **The secret store, the transport and the runtime registry are already ports** in
  `connector-pack` / `connector-secrets`.

This makes the no-leak rule structural rather than disciplinary: the public crate has no downstream
dependency to leak through, because it has only traits. It is also why the reuse story is documented
here in the abstract and the adoption plan lives in the adopting repository — the split is the rule
working, not an omission.

One fact belongs here rather than downstream, because it is a property of this family and more than
one consumer has recorded it wrongly:

> **The connector crates are published.** The source workspace reports
> `codewandler-connector-{catalog,spec,secrets,pack}` on **0.16.0** as of 2026-08-03. Re-check the
> registry and the consuming engine line before designing around a version; this family moves faster
> than a copied dependency example.

Two patterns generalize from the first adoption and are worth stating for the next one:

- **An HTTP consumer cannot tell a folded host from a remote one**, so folding the exchange into a
  product's own process as an interim and splitting it out later is a configuration change rather
  than a migration — provided the consumer speaks to it over HTTP from the first day.
- **An existing hand-written integration becomes a runtime behind the host** rather than something to
  port operation-by-operation before the host is useful. Its binary becomes an implementation detail
  of one deployment instead of a published, versioned artifact — which is the distribution tax this
  whole family exists to remove.

## Migration program

The accepted direction is filed as one cross-repository program rather than left as an architectural
wish:

- Flux C-500 is the epic; C-501 aligns its public contract, C-502 closes the rejected local runtime
  host without implementation, C-503 embeds the Milestone 1 effective-catalogue/one-shot HTTP client,
  C-504 provides per-adapter legacy-versus-Exchange conformance, C-505 retires adapters incrementally,
  and C-506 unconditionally deletes the remaining plugin support and release infrastructure.
- flux-connectors C-495 is the declaration/artifact epic with C-497…C-505 covering runtime bindings,
  artifacts, migration waves, pack projection, and the cutover gate.
- Exchange X-111 is the hosted-runtime epic with X-113…X-120 covering protocol completion, dispatch,
  tenant isolation, streams, leases, artifact trust, and conformance journeys.

The plan consumes rather than duplicates delivered Flux approval/catalogue seams and Exchange's
Service Account work. Live branch and story status comes from each repository, while cross-repository
ordering comes from `flux-roadmap`; neither is copied into this design.

## Open questions

- **Whether flux-exchange's console reuses flux-connectors' explorer components.** They import no
  framework by tested invariant, so they are mountable — but the two data models are cousins, not the
  same type. Deferred to the flux-exchange charter.
- **How `subscribe` follows shipped multi-tenant sign-in.** OIDC identity and tenant derivation now
  exist in Exchange; inbound grant shape, transport, replay, and delivery evidence remain undecided
  implementation work rather than a sign-in ordering question.
- **Contract conformance.** `flux-connectors/docs/designs/connector-contracts.md` is hard-blocked on
  a global operation-naming story (C-23, `backlog`, never started) because `fills_slot` *infers*
  conformance from trailing name segments. That inference is measurably broken in both directions —
  a bare `list` now matches 42 of 53 providers, and `put` matches nothing at all. **Recommendation:
  drop inference and declare conformance explicitly** (a service states
  `secret_store.get = "onepassword-item-get"`), which dissolves C-23 as a prerequisite for roughly
  the cost of one IR field. Not decided here; it belongs to flux-connectors' board.

## Related

- [`docs/ecosystem.md`](../ecosystem.md) — the description this design produces
- [`docs/concepts.md`](../concepts.md) — the shared vocabulary
- [`docs/vision.md`](../vision.md) — flux's own charter, aligned with this document
