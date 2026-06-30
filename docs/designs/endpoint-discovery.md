# Design: endpoint discovery & brokerage (references-only plugin IO)

**Status:** proposed (epic) · **Pillar:** Core (platform) · **Layer:** L0 schema + L4 (`flux-plugin`)
broker, invoked from L6 surfaces · **Owner:** Timo · **Stories:**
[D-25](../stories/D-25-endpoint-reference-model.md) ·
[D-26](../stories/D-26-endpoint-discovery-broker.md) ·
[D-27](../stories/D-27-reference-based-io.md) ·
[D-28](../stories/D-28-kubernetes-endpoint-provider.md) ·
[D-29](../stories/D-29-migrate-plugins-to-references.md) ·
[D-30](../stories/D-30-endpoint-lifecycle-cli.md) · depends on
[D-20](../stories/D-20-scoped-private-net-egress.md)

## Why

flux's plugins each talk to a single, statically-configured service. The fluxplane pack they were
modelled on (`~/projects/fluxplane/fluxplane-endpoint`) had a richer essentials feature flux
**deliberately deferred** in [D-10](process-plugin-protocol.md) and
[the parity epic](fluxplane-plugins-parity.md) (both list *"a `.dex`-style endpoint registry"* as a
non-goal): **cross-plugin endpoint discovery**. The kubernetes plugin discovers "endpoints" — each
kubeconfig context is one cluster endpoint, and once connected it discovers *other* services in the
cluster: crossplane/RDS manifests → endpoints for the `sql` plugin; a monitoring namespace →
endpoints for grafana / loki / prometheus / alertmanager. A consuming plugin asks the host *"which
endpoints exist?"*; the host **fans out** to provider plugins to answer; the result is a **weak
reference** (a URL + a *credential reference*, never the secret). When the consumer connects, the
host **resolves** the reference and **injects** the credential host-side. **This epic reverses that
deferral** — it is now an essentials feature.

Reversing it forces a sharper statement of a principle flux already half-implements, and which is
the organizing idea of this whole epic:

## The reference invariant (the spine)

> **A plugin operation deals only in references.** While executing an operation a plugin never
> reads, names, or receives an environment variable; never receives a raw secret value; never
> receives or assembles a URL that embeds credentials. Every host-bound or sensitive thing is an
> **opaque, host-managed reference** — an `endpoint_ref` (where + how to connect, carrying its
> `credential_ref` *inside*) or a bare `credential_ref` (`flux_secret::Ref`). The **host alone**
> resolves a reference to a concrete connection / secret and performs the privileged IO.

This *generalizes a pattern flux already has*. `conn.dial` returns an opaque `conn_id`,
`process.spawn` a `proc_id`, `blob.put` a `blob_ref` (`crates/flux-plugin/src/lib.rs:843–993`) — the
plugin holds only the handle, never the underlying resource. The epic extends that discipline
**upstream**, to the endpoint/credential *binding*, removing the two places a plugin still touches
host config today:

1. **Manifest env-coupling** — `EndpointSpec.env: ["GITLAB_URL"]` and `AuthMethod.env:
   ["GITLAB_TOKEN"]` (`crates/flux-plugin/src/lib.rs:200,148`) hardcode env-var *names* into the
   plugin's own manifest.
2. **Resolved-URL handback** — the `endpoint` host capability returns the env-resolved URL *string*
   to the plugin (`:772–775`, `:506–522`), which the plugin then assembles into request URLs itself.

Each clause of the invariant is a **testable assertion**, not prose:

- *No env-var name on the op surface.* No native plugin's manifest or op handler names an environment
  variable. (Env keys live only in host-side binding config — see Resolution.)
- *No raw secret to the plugin.* The resolution path returns an injection, never a value; the existing
  `resolve_purpose`/`resolve_auth` contract (`:486–616`) already holds this and is extended, not
  weakened.
- *No URL-with-credentials anywhere a plugin or the model can see.* A `Resolved` endpoint (which
  carries injected credentials) is host-only and **never serialized to a model-visible surface**;
  inline credentials in any URL are split out into a `credential_ref` before the URL is surfaced.

## Reference model

Three forms, mirroring `fluxplane-endpoint`, mapped onto flux types:

- **Weak reference — `EndpointRef`** (model-safe, the currency a plugin/agent holds):
  ```
  EndpointRef {
    id: String,                  // canonical "@endpoint/<id>"
    url: String,                 // scheme://host[:port][/base] — NO embedded credentials
    product: String,             // "postgres" | "prometheus" | "loki" | ...
    protocol: Option<String>,    // "http" | "postgres" | "ami" | ...
    source: SourceRef,           // who produced it (env/config | plugin/instance | discovery)
    credential_ref: Option<flux_secret::Ref>,   // WHERE the secret lives, not its value
    labels: Map<String,String>,
  }
  ```
  `credential_ref` reuses `flux_secret::Ref` verbatim — its `Env` / `Plugin` / **`Kubernetes`**
  schemes (`crates/flux-secret/src/lib.rs:29`) are exactly the weak-credential vocabulary we need
  (`kubernetes/ns/name/key` is already spellable today).
- **Discovery candidate** — an `EndpointRef` plus discovery metadata (`score`, `reasons[]`, probe
  hints). What a provider returns to the broker before it is committed to the registry.
- **Resolved** (host-only, **never serialized to the model**): the runtime-ready view — absolute URL
  + injected credential material (headers / connection auth). Lives only inside the host while it
  performs one IO call; mirrors `fluxplane-endpoint`'s `Resolved` and `flux_secret::Material` (whose
  value never serializes).

## Reference resolution

The host owns a **`ReferenceResolver`** — a resolver chain that binds an `EndpointRef`/`credential_ref`
to a concrete value. Three sources, deny-by-default, tried in declared order:

1. **Env / config resolver (static).** The replacement for today's per-plugin env coupling. The
   *operator* binds a named endpoint/credential to env keys or config values **in host config**, not
   in the plugin manifest. This is a **clean cutover**: today's `EndpointSpec.env` / `AuthMethod.env`
   move out of the plugin into host binding config; no parallel old+new path (per the no-fallbacks
   rule). A plugin's manifest now declares only the *references it needs* (by product / purpose /
   protocol).
2. **Stored credential resolver.** `flux-credentials` / `flux-secret` materialization for refs backed
   by the credential store.
3. **Discovery resolver.** Provider plugins (D-26) — endpoints discovered at runtime, held in the
   registry with owner + TTL.

The agent/operator sees only the weak `EndpointRef`; resolution to `Resolved` happens host-side,
behind `Executor::dispatch`, at the moment of an IO call.

## Discovery protocol (additive to `flux.plugin.v1`)

- **Provider declaration.** A plugin manifest gains `discovers: [products]` — the set of products it
  can discover endpoints for (k8s declares `[kubernetes, prometheus, loki, grafana, alertmanager,
  postgres, mysql]`). A provider implements a standard op contract `endpoint.discover(product, query,
  limit) -> [candidate]`.
- **Consumer capability.** A new host capability `endpoint.discover` (deny-by-default, manifest-gated
  like every other host cap): a consumer plugin asks the host *"what endpoints exist for product
  X?"*. The host **fan-out broker** matches the product against registered providers, calls each
  provider's `endpoint.discover`, aggregates, ranks by score, and returns **weak refs only** (no
  secrets, no `Resolved`).
- **Registry.** Discovered endpoints are committed to an `EndpointRegistry` keyed by id, each
  `EndpointRecord` carrying `owner` (the discovering plugin), `discovered_at`, `ttl`, and optional
  health — so a provider can `replace_owned` its set on refresh without disturbing others' entries.
  This is fluxplane's `Registry` / `RuntimeRecord` ported.

The fan-out is host-mediated end to end: providers and consumers never address each other; the broker
is the only intermediary (provider→consumer coupling stays loose, new plugins drop in without
touching existing ones).

## Ref-based IO & connect

The cutover that *enforces* the invariant. Today `http.do` takes a plugin-assembled `url` +
`auth_purpose`; `conn.dial` takes a `tcp:host:port` the plugin built from a resolved URL. After this
epic the host IO capabilities accept an **`endpoint_ref` + a relative path / sub-target**:

- The host resolves the ref → absolute URL + `credential_ref` → injects the credential host-side
  (reusing `resolve_auth`'s Bearer/Basic/Header/Query injection, `:586–616`), then performs the call.
- Cross-plugin credentials resolve transparently: a `credential_ref` with the **`Kubernetes`** scheme
  is materialized by calling the kubernetes plugin's gated `secret.read` op — the consuming plugin
  (e.g. `sql`) never sees the value; the host reads it from the provider and injects it.
- A model-safe **display URL** (host name, no credentials) may still be surfaced to the agent for
  reasoning — refs are the *execution* currency; the display URL is a derived, redacted projection.

## Security model & invariants

flux is security-first; cross-plugin credential brokerage widens the trust surface, so it is gated,
not implicit:

- **The reference invariant** (above), each clause test-backed.
- **Discovery is secret-free.** `endpoint.discover` returns weak refs (URLs + credential *references*)
  only. No `Resolved`, no `Material`, no env value ever crosses the wire to a plugin or into model
  context. A test asserts no discovery result serializes a secret-shaped value.
- **Cross-plugin credential resolution is deny-by-default**, layered:
  1. an **operator config grant** per *(consumer-plugin × product / endpoint)* — no grant, no
     resolution, exactly like today's `process`/`conn`/`secrets` allow-lists;
  2. a **first-use approval** gate on the actual resolution (session-scoped thereafter), through the
     existing approval envelope;
  3. an **audit event** (`flux-events`) recording which consumer resolved which endpoint under which
     grant.
- **Discovered internal hosts go through [D-20](scoped-private-net-egress.md).** Most discovered
  endpoints are private (in-cluster) addresses; reaching them requires the scoped, declared, audited
  private-net allowance D-20 introduces — never the global `allow_private_net` switch. D-20 is a hard
  prerequisite for [D-27](../stories/D-27-reference-based-io.md).
- **No bypass path.** Resolution and IO stay behind `Executor::dispatch`; this makes the envelope
  finer-grained, it does not add a path around it (the AGENTS.md invariant).

## Crate placement & layering (decision)

- **Reference schema types** (`EndpointRef`, `EndpointRecord`, `SourceRef`, discovery candidate/result)
  → a small **L0** schema module, mirroring how `flux-datasource` is the shared L0 schema for records.
  Preferred home: a new `endpoint` module in `flux-secret` (it already owns `Ref`, the credential
  weak-ref) **or** a sibling L0 crate if `flux-secret` should stay credential-only — settle in D-25.
  `credential_ref` is `flux_secret::Ref` regardless.
- **Registry + resolver chain + fan-out broker** → a module in **`flux-plugin` (L4)**, beside
  `SystemHostCaps`; invoked by the L6 surfaces (`flux-app`, `flux-cli`) which already hold the set of
  loaded plugins (`load_plugin_tools`). Resolution dispatches through `Executor::dispatch`.
- **`flux-runtime` (L2) gains no L5 dep**; the datasource precedent (L5 `DatasourceHostCaps` services
  `ds:*` while the trait lives in L4) is the model — the broker trait is L4, any L5/L6 wiring stays
  outward.

## Sequencing

```
D-25 (reference model + registry + static resolver)   ← the spine; reframes env as host config
   │
   ├── D-26 (provider role + endpoint.discover + fan-out broker)
   │        │
   │   D-20 (scoped private-net egress — hard prerequisite) ──┐
   │        │                                                 │
   └── D-27 (ref-based IO + host-injected connect + secret-safety)  ←┘
            │
            ├── D-28 (kubernetes endpoint provider — the reference provider)
            │        │
            │   D-29 (migrate all native plugins to refs + consume discovered endpoints)
            │
            └── D-30 (refresh runner + `flux endpoint` CLI + audit)
```

D-25→D-26→D-27 are the sequential core. D-28 is the first real provider; D-29 the consumer migration
(clean cutover of every native plugin, one sub-agent per plugin in parallel during impl). D-30 adds
lifecycle once a provider exists.

## Cutover

No dual model. Today's `EndpointSpec.env` / `AuthMethod.env` are **removed** from the plugin op
surface and re-expressed as host-side binding config; every native plugin migrates to ref-based IO in
D-29. The `endpoint` host capability that hands back a URL string is replaced by ref-based IO. The
protocol extension (`discovers`, `endpoint.discover`) is additive to `flux.plugin.v1` — no version
mode flag (the D-10 single-frame discipline holds).

## Non-goals

- A plugin **marketplace** or distribution mechanism (that is [D-21](../stories/D-21-plugin-distribution.md)).
- fluxplane's aggregator / provider-call surface, context providers, evidence observers, dual protocol
  modes — dropped in D-10, stay dropped.
- Forbidding the agent from seeing **hostnames / display URLs** — those are not secrets; the invariant
  is about env vars, raw secrets, and credential-bearing URLs, not host identity.
- Generic service-mesh / DNS-SD discovery beyond what provider plugins implement.

## Reuse, don't reimplement

- `flux_secret::Ref` (incl. `Kubernetes` scheme) as `credential_ref`; `Material` (unserializable
  value) + `Redactor` for the host-only resolved form.
- `SystemHostCaps`' `resolve_purpose` / `resolve_auth` injection + the opaque-handle pattern
  (`conn_id`/`proc_id`/`blob_ref`) as the precedent for `endpoint_ref`.
- The kubernetes plugin's existing `endpoint.discover` / `cluster.list` / `secret.read` ops
  (`plugins/kubernetes/src/main.rs:65,82`) — elevated into a provider in D-28, not written from
  scratch.
- D-20's scoped private-net allow-set for reaching discovered internal hosts.
- The `flux-events` audit substrate (D-02) for resolution audit.
- Prior art (shapes only): `fluxplane-endpoint/{endpoint,registry,discovery_registry,runner}.go`,
  `fluxplane-secret/secret.go`.
