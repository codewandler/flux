# Design: Connector-backed storage — one registry, two safe facades

> **Superseded in part (2026-08-04, C-514).** This design predates flux-roadmap Decisions
> 0001–0005 and 0006 and is superseded by them where they overlap: official vendor integrations
> execute only in Exchange (0001), and vendor-facing surfaces follow 0006's **declared-surface
> pattern** — Flux owns the contract and fixed tool surface, connectors declare the vendor mapping
> as a projection over their own operations, Exchange binds it per tenant and executes every
> mutation as an admitted, granted operation. Any revival of this design must be re-derived under
> that pattern (see Decision 0006's "Boards are their own first-class surface" section, which
> states it once for the family) rather than implemented as written — in particular its native
> store adapters, its own registry/binding control plane, and its charter revision for
> flux-connectors are the parts the decisions overtake. Its data-plane safety rules (no secret
> output, host-derived tenant paths, bounded blobs) remain good constraints for whatever replaces
> it.

**Status:** proposed · **Pillar:** Core · **Epic:**
[D-221](../stories/D-221-connector-backed-storage-facade-epic.md)

> This is a joined design for `flux`, `../flux-connectors`, and a future hosted connectors
> platform. Flux owns execution, tenancy, authorization, guarded IO, and secret handling.
> flux-connectors owns portable declarations and generated adapter descriptions. The hosted
> platform owns account-scoped configuration and the platform-managed default binding.

## Why

The hosted connectors platform needs ordinary customer storage and credential storage to be
replaceable without giving every backend a bespoke call path. An account may bring several stores,
while an unconfigured account should still be able to use a platform-managed object store. The
shared problem is selection and tenancy; the data-plane safety contracts are intentionally
different.

## Decision

A platform account may configure many named stores. Each store has a backend type such as
`aws.s3`, `local`, `hashicorp.vault`, or `onepassword.connect`, but callers select a store by its
stable **name**, not by type: two S3 stores are a valid configuration. Omitting the name selects the
account's default. In the hosted product, an account with no object-store default receives the
hidden platform-managed S3 binding.

One registry serves two deliberately different facades:

```text
                    account-scoped StoreRegistry
                    names · types · defaults · bindings
                              │
                    capability check + routing
                      ┌───────┴────────┐
                      │                │
              ObjectStoreFacade  CredentialStoreFacade
              bytes via blobs     secrets via host-only records
                      │                │
             object_storage      secret_storage
             local · AWS S3      local · Vault · 1Password
```

The common registry is configuration and routing, not an untyped storage API. Ordinary objects and
credentials never cross through a common `get(anything) -> bytes` operation. That separation is the
load-bearing security decision in this design.

## What exists today

Flux already has a credential store, but it is narrower and less consistently bound than the name
suggests:

- `flux_credentials::CredentialStore` stores an `OAuthToken` under an arbitrary string key. The
  default `FileCredentialStore` writes `~/.flux/credentials.toml` with mode 0600.
- `VaultCredentialStore` persists the same OAuth-shaped value in HashiCorp Vault KV-v2. It supports
  a static Vault token and renewable Kubernetes authentication.
- `SystemHostCaps::with_credential_store` injects that port for plugin credential resolution, but
  `flux auth set`, provider login, status, and doctor still call the file helpers directly. There is
  no process-wide or account-wide default-store choice.
- Flux ships separate Vault and 1Password Connect plugins for management operations. Those plugins
  do not make 1Password a `CredentialStore`, and their secret-bearing reads are not the structural
  never-return-secret contract designed here.

Flux does **not** have the proposed general object store:

- `flux_sdk::Storage` owns event and Flow-Lang state. It is not user object storage and must not be
  widened into it.
- plugin `blob.*` is a call-scoped, in-memory transfer area. It gives this design the right opaque
  handle shape, but not durability or cross-request identity.
- `flux_lang::ValueStore`, `DurableStore`, event storage, vector stores, and datasource backends are
  each domain-specific ports. None is a generic customer-path object store.

In flux-connectors, provider services and global addresses already exist, including the motivating
`com.amazonaws/s3:2006-03-01` shape. `CredentialRef` and `TenantLayout` already define stable,
tenant-scoped credential addresses. The connector Tool pack is also the intended runtime bridge.
What is absent is a declaration that a service fulfils a storage role and a host-only projection of
that declaration.

This design deliberately changes flux-connectors' current charter sentence that puts every
technology adapter in Flux plugins. The revised boundary is behavioral: a remotely callable,
declarable vendor API belongs in the connector catalogue even when the vendor sells infrastructure;
a stateful protocol adapter or local technology remains a Flux adapter. HashiCorp Vault's HTTP API,
1Password Connect, and AWS S3 are catalogue candidates. A local filesystem is not.

## The account-scoped registry

`StoreRegistry` is the facade's control plane. It resolves configuration; it does not move bytes.
The hosted implementation reads platform-owned integration records. The standalone implementation
reads trusted Flux configuration. Tests use an in-memory implementation.

Conceptually, the port is:

```rust
#[async_trait]
pub trait StoreRegistry: Send + Sync {
    async fn list(
        &self,
        account: &AccountId,
        filter: StoreFilter,
    ) -> Result<Vec<StoreSummary>>;

    async fn resolve(
        &self,
        account: &AccountId,
        selector: StoreSelector,
        required: StoreCapability,
    ) -> Result<StoreBinding>;
}

pub struct StoreSelector {
    pub name: Option<StoreName>, // None means the capability-specific default
}

pub enum StoreCapability {
    Object,
    Credential,
}
```

`StoreSummary` may expose the profile name, backend type, capabilities, health state, and whether it
is the account default. It never exposes an endpoint credential, bucket credential, bootstrap
token, or raw connector configuration. A type filter such as `type = "aws.s3"` is valid for
discovery. It is not valid for object access because it may match more than one store.

`StoreBinding` is host-only and contains the prepared backend plus safe identity metadata. A binding
is immutable for one request/turn, like `TurnIdentity`; changing an account default while a request
is live cannot redirect that request halfway through an operation.

### Two defaults, because capabilities differ

An account has independent `default_object_store` and `default_credential_store` selections. One
unqualified `default_store` would either route secrets into ordinary storage or make a useful S3
default impossible.

- Hosted object storage: the account-selected default wins. If absent, the platform registry
  returns the hidden `platform` binding backed by a platform-owned S3 bucket and credentials.
- Standalone object storage: a configured default wins; otherwise object access reports that no
  default is configured. Flux does not silently choose the workspace.
- Standalone credentials: the existing 0600 file store remains the compatibility default.
- Hosted credentials: the account or operator must bind a credential-capable profile. There is no
  pod-local-file fallback and the platform S3 object binding is not credential-capable.

An explicit store name always wins over a default. An unknown name, a name owned by another
account, or a profile without the requested capability is an error before backend IO.

### Configuration trust

Standalone store profiles and defaults are trusted control-plane configuration. They may come from
managed configuration or `~/.flux/config.toml`; repository-controlled `.flux/config.toml` is
refused if it attempts to declare or select a store. A repository must not redirect credential
resolution or object writes to an attacker-controlled service merely by being opened.

The hosted registry is supplied by the platform, keyed by authenticated account identity. The SDK
accepts an injected `Arc<dyn StoreRegistry>` so an embedder does not have to serialize its registry
through Flux TOML.

A standalone shape may look like this:

```toml
[storage]
default_object_store = "project-archive"
default_credential_store = "team-vault"

[storage.stores.project-archive]
type = "aws.s3"
endpoint_ref = "@endpoint/aws-s3"
bucket = "project-archive"
region = "eu-central-1"
auth_ref = "env/AWS_PROFILE"

[storage.stores.team-vault]
type = "hashicorp.vault"
endpoint_ref = "@endpoint/vault"
mount = "secret"
prefix = "flux"
auth_ref = "kubernetes/flux/vault/store"

[storage.stores.personal]
type = "onepassword.connect"
endpoint_ref = "@endpoint/onepassword-connect"
vault = "Automation"
auth_ref = "env/OP_CONNECT_TOKEN"
```

Those names are illustrative wire spellings for the later configuration story, not permission to
put literal values in config. `auth_ref` is always a reference. A profile's capability comes from
its adapter; an operator cannot turn S3 into a credential store with `credential = true`.

## Object storage facade

The object facade is a small, byte-oriented contract independent of S3 terminology:

```rust
#[async_trait]
pub trait ObjectStoreBackend: Send + Sync {
    async fn head(&self, key: &ObjectKey) -> Result<Option<ObjectMetadata>>;
    async fn get(&self, key: &ObjectKey, sink: &mut dyn BlobSink)
        -> Result<Option<ObjectMetadata>>;
    async fn put(
        &self,
        key: &ObjectKey,
        source: &mut dyn BlobSource,
        options: PutOptions,
    ) -> Result<ObjectMetadata>;
    async fn delete(&self, key: &ObjectKey, version: Option<&VersionTag>) -> Result<DeleteOutcome>;
    async fn list(&self, prefix: &ObjectKey, page: PageRequest) -> Result<ObjectPage>;
}
```

`ObjectStoreFacade` takes a request context, optional store name, and caller-relative path. It
resolves the binding, derives the physical key, applies authorization/approval, and then invokes the
backend through guarded IO. Backends never receive caller identity as an untrusted path string.

### Customer paths are host-derived

The caller supplies a validated relative `ObjectPath`, for example `calls/2026-07/receipt.json`.
It cannot be empty, absolute, contain `.`/`..`, contain NUL, or exceed segment/key limits. The host
derives the authenticated account and tenant from the request-owned binding and renders:

```text
<profile-prefix>/tenants/<tenant>/objects/<relative-path>
```

The tenant is never accepted from an operation parameter or request header that has not already
been resolved into immutable identity by the hosting surface. A privileged cross-tenant API is not
part of v1; platform administration must bind a different request context explicitly.

For the platform binding, bucket, region, endpoint, prefix, and AWS identity are host-owned and
absent from the account's visible profile. The customer sees the binding name and safe status, not
the bucket credentials or an S3 URL.

### GET returns a blob handle, not arbitrary inline bytes

`storage.get` streams the object into a bounded host blob and returns:

```json
{
  "blob_ref": "blob:sha256:…",
  "name": "receipt.json",
  "size": 4812,
  "content_type": "application/json",
  "etag": "…",
  "version": "…"
}
```

The handle is scoped to the authorized request/session and is not a signed public URL. Separate
guarded blob operations can inspect bounded text, transfer the blob to another operation, or export
it through an explicit surface. Large objects therefore do not become model context, JSON base64,
or unbounded memory allocations. `put` consumes a blob handle for the same reason.

The durable object remains in its configured backend; the blob is a bounded transfer handle. This
extends the existing `blob.*` idea but does not change its current call-scoped semantics in place.
A new host blob port owns limits, streaming, lifetime, and cleanup.

### Version and list semantics

`ObjectMetadata` carries an opaque backend version tag. Conditional writes use `CreateOnly` or
`IfVersion(VersionTag)`; unconditional overwrite must be explicit. A backend that cannot implement
a requested condition refuses it. This maps to S3 conditional/ETag behavior and gives a local
backend a testable lost-update contract without pretending every backend has S3 version IDs.

`list` is bounded and paged. The backend cursor is opaque, bound to the store and tenant, and byte-
capped before it reaches a model-visible result. Object keys may be sensitive metadata, so list is
a separately authorized read, not a consequence of having permission to get one known path.

## Credential storage facade

Credentials use the registry but never the object facade. `CredentialStoreFacade` accepts a stable
credential address plus an optional explicitly bound store name and resolves only a
`StoreCapability::Credential` backend.

The record generalizes today's OAuth-only value:

```rust
pub struct CredentialRecord {
    pub secret: SecretValue,
    pub refresh: Option<SecretValue>,
    pub expires_at_ms: Option<i64>,
    pub account_id: Option<String>,
    pub metadata: BTreeMap<String, String>,
}
```

`SecretValue` does not implement `Display`, `Debug`, or general-purpose serialization. Backend
codecs receive explicit access to its bytes. Every secret and refresh value is registered with the
request's shared `Redactor` immediately after load and before auth assembly or any fallible request
construction. Metadata is non-secret by contract, size-bounded, and restricted to backend-neutral
keys; a backend-specific response cannot copy arbitrary vendor fields into it.

The port supports `load`, `save`, and `delete`. `load` returns `Result<Option<_>>`, distinguishing a
real absence from transport, authentication, authorization, and decode failures. Writes and deletes
never fall back to another store.

### Stable addressing, movable storage

flux-connectors' `CredentialRef` and `TenantLayout` remain the connector credential address:

```text
tenants/<tenant>/<authority>/<service>/<credential>
```

The store profile is deliberately not embedded in this handle. A platform can migrate an account
from 1Password to Vault without changing every connector handle. Routing is binding metadata:
the platform or SDK binds an exact account/connector scope to a store name, otherwise the account's
credential default is used.

Flux's existing provider/plugin keys remain valid. Compatibility adapters continue to support
`claude`, `codex`, and `plugin:<name>:<purpose>` while the public trait moves to `CredentialRecord`.
The file and Vault decoders accept both the legacy OAuthToken document and the new versioned record
envelope. Existing data therefore needs no eager migration; a later successful save may rewrite one
record in the new form atomically.

### Bootstrap is outside the selected store

A remote store needs its own credential. Its bootstrap resolver admits only trusted, explicitly
declared sources:

- environment or a protected file reference;
- an operator-bound workload identity, including Vault Kubernetes auth;
- a host-supplied credential callback that never enters a model-facing symbol.

A bootstrap reference cannot use the store registry or another store profile in v1. That makes
cycles structurally impossible and keeps startup comprehensible. The bootstrap value is registered
with the redactor before any connection attempt.

A selected remote store returning an error fails closed. It never falls back to the local file.
A genuine `NotFound` may allow the connector's next declared credential source (for example an env
reference) only when that source order is part of the connector auth declaration. Store failure and
credential absence are never conflated.

Configuration is validated without network so pure, credential-free Flux programs still run. A
serving or unattended surface that selects a remote credential default performs an eager health/
authentication check at startup; an interactive CLI may connect lazily and reports the failure on
the first credential operation. `flux doctor` and the future store-status command perform explicit
health checks without printing secret values.

## flux-connectors roles and projections

flux-connectors declares **what a service can do**. It does not implement the registry, choose a
tenant, hold a token, or open a socket.

### `object_storage`

A service claiming the `object_storage` role maps members for `head`, `get`, `put`, `delete`, and
`list`. The role records which conditional-write and version capabilities the service implements.
The compiler refuses a role missing a required member, a member from another service, an unbounded
list response, or an operation whose risk/idempotency metadata contradicts the role.

AWS is the first remote provider: `com.amazonaws/s3:2006-03-01`. The connector catalogue still
needs SigV4 request signing and honest XML response handling before the generated S3 adapter can be
used. The hosted platform adapter may be supplied natively first, using already-bound AWS values,
but it implements the same conformance suite and does not create a second public API.

### `secret_storage`

A service claiming `secret_storage` maps host-only `load`, `save`, `delete`, and `health` members
plus any operator management members. The compiler also declares the vendor codec that maps a
`CredentialRecord` to the remote representation. Host-only members are not operations a model can
discover or call.

The first providers are:

- **HashiCorp Vault KV-v2.** Path is the configured mount/prefix plus the rendered credential
  address. The codec uses Vault's nested `data.data` document and opaque version metadata. Static
  token and Kubernetes auth remain supported. The API semantics are pinned to the official
  [KV-v2 API](https://developer.hashicorp.com/vault/api-docs/secret/kv/kv-v2) and
  [Kubernetes auth API](https://developer.hashicorp.com/vault/api-docs/auth/kubernetes).
- **1Password Connect Server.** One credential is one `API_CREDENTIAL` item in the configured
  vault. Its exact credential address is stored in a dedicated field and tag; the title is display
  metadata, not identity. Lookup validates exactly one matching address and fails on duplicates.
  Secret/refresh fields are concealed, and item version is the opaque version tag. The supported
  surface is the official [Connect Server API](https://www.1password.dev/connect/api-reference),
  which provides vault/item CRUD, activity, and health.
- **Local credentials.** The existing 0600 file backend implements the role contract inside Flux;
  it is not emitted as a fake HTTP connector.

1Password account or service-account provisioning is out of v1. Connect manages accessible vaults
and items, not the vendor account control plane. A 1Password service-account token may later become
a bootstrap source for a separate SDK adapter, but the initial provider is Connect Server.

### Two generated projections

One connector service may produce two outputs:

1. A **store adapter descriptor** containing inert, validated request/response mappings for the host
   store factory. It is not a Tool and cannot return a secret through `ToolResult`.
2. An **operator pack** containing management Tools. These carry ordinary effects, risk,
   idempotency, permission subjects, and network intents, and execute through Flux's dispatcher.

The connector pack may link Flux runtime contracts, as it already does for the Tool pack. It still
does not become a runtime or perform IO on its own. The Flux host binds the descriptor to guarded,
DNS-pinned HTTP after resolving the account, tenant, endpoint, bootstrap identity, and private-net
grant.

No implementation calls a Tool's `execute` method as a shortcut. Automatic store traffic uses the
host-only adapter port; operator operations use `Executor::dispatch`.

## Operator management surface

Management APIs are useful, but secret managers are the worst possible place to expose generic
vendor responses to a model. Connector management operations therefore declare
`audience = "operator"` and install into a separate admin registry. They are absent from every
model-facing tool catalogue, including explicit tool subsets and evidence groups.

The local CLI and hosted admin API may dispatch an admin operation only with request-owned operator
identity and explicit policy. Caller kind is not sufficient by itself: authorization, approval,
accurate permission subjects, audit, redaction, and guarded network IO still apply.

The v1 management scope is:

- Vault health, auth-mount/policy metadata, KV metadata/version lifecycle, and reference-based
  secret writes/deletes;
- 1Password Connect health/activity, vault metadata, and item create/update/delete;
- store list/status/default selection through the facade rather than vendor-specific operations.

Secret reads return a credential handle and safe metadata, never a value. Secret-bearing writes
accept a host-owned ephemeral secret-input handle or an existing `CredentialRef`; raw secrets do
not appear in operation parameters, approval sheets, event payloads, progress, or error strings.
A token minted by a management operation is diverted directly into the selected credential store
before any output is constructed, following flux-connectors' credential-diversion design.

The existing Vault and 1Password plugins remain compatibility surfaces until connector-backed
management parity, migration guidance, and live smoke tests land. Installing both old and new packs
with the same operation names fails with the existing source-labelled collision diagnostic. After
parity, the plugins are deprecated and then removed in a release; they are not silently shadowed.

## Safety and failure rules

These are acceptance properties, not implementation advice:

- **Identity is immutable.** Account and tenant are request-owned and fixed before registry
  resolution. Backend configuration cannot replace them.
- **No caller-authored tenant paths.** Traversal, absolute paths, ambiguous normalization, and
  cross-tenant addressing are refused before IO.
- **Capability is derived.** Profile configuration cannot claim that an object adapter is safe for
  credentials.
- **No secret output.** A credential value never becomes a Tool input/output, blob, session symbol,
  log field, progress line, error, approval parameter, catalogue field, or generated artifact.
- **No fallback on failure.** A named/default binding is one backend for the operation. Transport,
  auth, decode, and authorization failures stop there.
- **Every effect stays guarded.** Connector declarations perform no IO; host adapters use Flux's
  guarded network/filesystem primitives; operator Tools use the dispatcher.
- **Private endpoints remain scoped.** A Vault or Connect service on private/loopback space needs an
  operator grant for that exact profile/host. Selecting it as a default grants no egress.
- **Bounds are host-owned.** Blob size, response bytes, list page size, cursor length, object path,
  credential record size, metadata count, and timeouts have hard host limits independent of vendor
  claims.
- **Configuration contains references only.** Store definitions, connectors, manifests, catalogues,
  locks, and audit rows never carry bootstrap or customer credentials.

## Delivery slices

This design is intentionally split across owners so no story has to invent a cross-repository
contract while implementing it.

### Flux

1. Land `StoreRegistry`, names/types/capabilities, immutable `StoreBinding`, and account/default
   resolution with in-memory contract tests.
2. Land the object facade, tenant/path derivation, backend contract, conditional operations, and a
   local adapter.
3. Land bounded blob-transfer handles and object Tool/SDK surfaces.
4. Generalize `CredentialStore` to `CredentialRecord` with load/save/delete, redaction, legacy
   file/Vault decoding, and one assembled registry/default seam used by CLI, SDK, providers, plugin
   host, doctor, and the connector pack.
5. Land trusted standalone configuration, hosted registry injection, bootstrap resolution, eager
   serving health checks, and status/default-selection surfaces.
6. Land the connector store-adapter host, operator-only registry, secret-input handles, and the
   old-plugin migration path.

### flux-connectors

1. Add `object_storage` and `secret_storage` role IR, loader refusals, catalogue/manifests, and the
   two generated projections.
2. Add the HashiCorp Vault KV-v2 provider and its store/admin mappings.
3. Add the 1Password Connect provider and its store/admin mappings.
4. Complete AWS S3 provider prerequisites (SigV4, XML, service configuration), then claim the
   object-storage role.
5. Extend `connector-pack` with store factories and the separate operator pack, pinned to released
   Flux contracts.

### Hosted platform

1. Persist account-scoped named profiles and two defaults; keep backend configuration and bootstrap
   values out of tenant-visible records.
2. Supply the hidden platform S3 object binding and derive the authenticated tenant for every
   object request.
3. Bind BYOS profiles to connector adapter descriptors, private-network grants, audit, and health
   reporting.

## Verification matrix

Each implementation slice owns failing-first tests, and the assembled journey must cover:

- two named S3 stores in one account; type filtering returns both, access by type is refused, access
  by name selects exactly one;
- an account-selected object default, the platform fallback when absent, and no standalone
  workspace fallback;
- a malicious `../`, absolute, encoded-separator, or foreign-tenant path never reaches a backend;
- two accounts asking for the same relative path receive different physical keys;
- object GET streams to a bounded blob handle and no object bytes appear in `ToolResult`;
- oversized GET, PUT, list page, metadata, cursor, and path inputs fail at host bounds;
- wrong-capability selection (platform S3 as credential store) fails before IO;
- remote store `NotFound` is distinct from 401/403, transport, decode, and timeout failures, and no
  failure reads or writes the local store;
- bootstrap accepts env/file/workload identity, refuses store references, and never surfaces its
  sentinel token;
- a credential sentinel appears in none of result content, view, errors, progress, approvals,
  events, blobs, generated artifacts, or model-visible catalogues;
- legacy Claude/Codex/plugin OAuth records load unchanged through file and Vault adapters, and a
  new general record round-trips;
- Vault loopback tests cover KV-v2 load/save/delete, static auth, Kubernetes renewal/re-auth, version
  conditions, private-host grants, and error redaction;
- 1Password Connect loopback tests cover item lookup, create/update/delete, duplicate-address
  refusal, version conditions, and concealed-field diversion;
- operator operations are absent from the agent registry and present only in an explicitly
  authorized admin registry;
- the native platform S3 adapter and later connector-generated S3 adapter pass one object-backend
  conformance suite.

## Explicitly out of scope

- Treating the platform-managed S3 bucket as a credential vault. A future S3 credential adapter
  needs its own KMS/envelope-encryption and key-policy design.
- Fan-out reads across stores or automatic reconciliation. One operation resolves one binding.
- A routing-rule language. V1 has exact named selection, two defaults, and exact host bindings.
- Caller-selected cross-tenant access.
- Signed download URLs as the normal GET result.
- Vendor account/user/service-account provisioning.
- Replacing Flux event/session/Flow-Lang storage with the object facade.
- Reusing call-scoped plugin `blob.*` as durable customer storage.
