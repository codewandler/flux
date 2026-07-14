# Design: an async, paged live-backend datasource seam

**Status:** accepted 2026-07-15 · **Pillar:** Agent · **Layer:** L0 (`flux-datasource` data types)
+ L5 (`flux-capabilities` `datasource` module) · **Story:**
[D-62](../stories/D-62-async-live-datasource-seam.md) ·
**Related:** [datasource-discoverability.md](datasource-discoverability.md) (names D-62 as the
live-backend seam, out of that epic's scope) · [datasource-rag.md](datasource-rag.md) (the sync,
index-shaped `DatasourceBackend` this sits *beside*, not on top of)

## Why

`DatasourceBackend` (`crates/flux-capabilities/src/datasource/mod.rs:62`) is **synchronous** and
**index-shaped**: `upsert`/`clear`/`len` plus keyword `search`/`get`/`list`/`relation`/`batch_get`
over local `Record`s the host already holds. Its `list` even pages by numeric **offset**
(`ListInput.offset`, `crates/flux-datasource/src/lib.rs:223`) — correct for a stable local snapshot,
wrong for a live feed. This is genuinely the right shape for a *local index* and the wrong shape for
a *remote system-of-record*:

| | `DatasourceBackend` (sync index) | the missing seam (live backend) |
|---|---|---|
| Data lives | in flux's own store (upserted, indexed) | in a remote API/DB; flux holds none of it |
| Access | keyword rank + `(source,entity,id)` lookup | paged `list(entity, filters)` + `get(id)` |
| Paging | numeric offset over a fixed snapshot | continuation cursor over a changing feed |
| IO | synchronous, in-process | async, network — auth, latency, failure |

The reviewed downstream consumer hit exactly this gap and built the missing layer **app-side**: typed
pages, per-entity bindings, two generated ops per domain (`<domain>.list` / `<domain>.get`), filter-key
validation, limit clamping, and compact id/title/summary row rendering — and documents the gap in its
own module header. Only its per-entity *fetch/get closures* are app-specific; the
paging/validation/projection machinery is generic and every consumer of a live backend will otherwise
rebuild it. That machinery belongs in flux.

The organizing idea: **a second trait, not a retrofit.** `DatasourceBackend` keeps serving the local
index. A new `LiveDatasource` serves remote systems-of-record, and flux owns the generic two-op
projection over it so a backend author writes only the fetch/get closures.

## The model

### A second trait — `LiveDatasource`

A domain (e.g. a ticketing system, a CRM) implements one trait; flux does the rest.

```rust
// crates/flux-capabilities/src/datasource/live.rs (L5)
#[async_trait]
pub trait LiveDatasource: Send + Sync {
    /// Declares the domain's entities and, per entity, its filter keys, page defaults, and
    /// which fields render. Drives op-schema generation, filter validation, and limit clamping.
    fn schema(&self) -> LiveSchema;

    /// One page of an entity, honoring already-validated `filters` and a page cursor.
    async fn list(
        &self,
        ctx: &ToolContext,
        entity: &str,
        page: PageRequest,
        filters: &Filters,
    ) -> Result<Page<Row>>;

    /// One row of an entity by id — resolves through the backend's own host-side auth,
    /// NOT by dereferencing a handle the model holds.
    async fn get(&self, ctx: &ToolContext, entity: &str, id: &str) -> Result<Option<Row>>;

    /// The concrete guarded resource(s) used by this backend, separate from the datasource
    /// resource added by the projection itself.
    fn access(&self) -> Vec<LiveAccess>;
}
```

The supporting **data types are pure and live in the L0 `flux-datasource` crate** (beside `Record`,
`ListInput`, `SourceSummary`, …), so — exactly as the sync record contract is shared — a future
plugin-contributed live datasource can speak the same shapes without a layering violation:

```rust
// crates/flux-datasource/src/live.rs (L0, pure data)
pub struct Row {                 // the projection output — NOT a capability
    pub id: String,              // a name the backend can re-resolve
    pub title: String,
    pub summary: String,         // one-line, human-facing
    pub reference: Option<Reference>, // opaque locator (see Q3) — never a handle/secret
}
pub struct Page<T> { pub rows: Vec<T>, pub next: Option<String> } // next = opaque cursor
pub struct PageRequest { pub cursor: Option<String>, pub limit: usize }
pub struct Filters(/* validated (key -> scalar) map */);
pub struct LiveSchema { pub entities: Vec<LiveEntity> }
pub struct LiveEntity {
    pub entity: String,
    pub filters: Vec<FilterKey>,     // declared, typed — the validation contract
    pub default_page: usize,
    pub max_page: usize,             // the clamp ceiling
    pub description: Option<String>,
}
pub struct FilterKey { pub name: String, pub ty: FilterType, pub required: bool, pub description: Option<String> }
pub enum FilterType { String, Int, Bool, Enum(Vec<String>) }
```

The types live under `flux_datasource::live` so the intentionally small names (`Row`, `Page`) do not
pollute the existing record/index namespace. `Filters` uses deterministic key ordering. `Reference`
is a tagged entity locator or human-navigation URL, never an opaque capability handle.

`ToolContext` is passed into `list`/`get` so native implementations receive flux's guarded runtime
context rather than inventing a parallel execution context. `LiveAccess` is an L5, closed enum
(`Network { subject }` / `Connection { subject }`); the generated tools translate it into exact
`AuthorityRequirement`s. An in-memory backend declares no external access. This keeps model-facing
schema, datasource identity, and backend authority separate: the projection always requires
`datasource.read` for `<domain>/<entity>`, then adds the exact network/connection requirements the
backend declares.

### The generic two-op projection

Registering a `LiveDatasource` under a domain name yields exactly two ops, built entirely from
`schema()` — no per-domain code beyond the trait impl. This mirrors `register_datasource_ops`
(`crates/flux-capabilities/src/datasource/ops.rs:33`):

```rust
pub fn try_register_live_datasource(
    registry: &mut ToolRegistry,
    groups: &mut Vec<ToolGroup>,
    domain: &str,
    backend: Arc<dyn LiveDatasource>,
) -> Result<LiveDatasourceSurface>;
```

- **`<domain>.list {entity, page?, limit?, filters?}`** — validate `entity` against `schema()`;
  validate every `filters` key is declared for that entity and coerce its value to the declared
  `FilterType` (reject unknown keys / enum mismatches); clamp `limit` to the entity's `max_page`
  (apply `default_page` when omitted); call `list`; render rows compactly, one per line, and append a
  `next: <cursor>` line when `Page.next` is `Some` so the model knows how to page.
- **`<domain>.get {entity, id}`** — validate `entity`; call `get`; render the row in full or
  `not found`.

Row rendering reuses the established compact shape (`ops.rs:57` `render_record` →
`[entity id] title` + body): `<domain>.list` prints `[entity id] title — summary`, `<domain>.get`
prints the same with the reference locator. The op input schema advertises each entity's declared
filter keys as documented properties, so the model sees precisely which filters an entity supports
instead of guessing at a free-form bag.

Everything the app currently reimplements per domain — filter-key validation, limit clamping, row
projection, op generation — is now in this function. The backend supplies only `schema` + `list` +
`get`.

## Shape & touch points

- **Where it lives.** New module `crates/flux-capabilities/src/datasource/live.rs` for the trait +
  projection; the pure `Row`/`Page`/`PageRequest`/`Filters`/`LiveSchema`/`FilterKey` types in L0
  `crates/flux-datasource/src/lib.rs`. `flux-capabilities` is already **L5**
  (`crates/flux-codegate/src/lib.rs:42`) and `flux-datasource` is L0 — no new crate, no layer change
  (the "prefer one crate + modules" rule).
- **Async in L5 is a non-issue.** `flux-capabilities` already depends on `tokio` and `async-trait`
  as *runtime* deps (its `Cargo.toml` notes the endpoint broker holds plugin hosts behind
  `tokio::sync::Mutex`; codegate's own header, `lib.rs:12`, records "flux-capabilities … uses
  tokio"). The `Tool::execute` seam is already `async` and the sync datasource ops are already
  `#[async_trait]` (`ops.rs`) — they simply `await` `list`/`get` instead of calling sync methods.
- **Composition with the sync backend.** Strictly additive, no bridge. `DatasourceBackend` and
  `LiveDatasource` are independent traits for independent needs (local index vs remote SoR); a domain
  registers one, the other, or both under different names. No shared supertrait, no retrofit — the
  point of the story.
- **Registration** is host-owned. `try_register_live_datasource(...)` installs the two tools and
  returns the domain's group + ambient signal as one `LiveDatasourceSurface`. The SDK conversational
  builder gains a convenience seam that carries all three together. Lower-level hosts may compose
  the same registry/group/signal values directly; the CLI wires a live domain only when it has a
  concrete configured backend and does not invent a generic backend configuration in v1.
- **Evidence-gated surfacing.** The two ops are grouped under a per-domain `ToolGroup`
  (`crates/flux-evidence/src/lib.rs:185`) named for the domain, `surface_when` a `<domain>` signal —
  and the signal is emitted **because the domain is configured/registered**, exactly like the
  `endpoint` group surfaces when the endpoints store is non-empty (`crates/flux-tools/src/groups.rs`,
  D-115). So a live domain's ops appear in the catalog only when that domain is actually wired
  (honest catalog, the browser/endpoint pattern); `FLUX_SURFACE_ALL` still forces. This is a
  deliberate contrast with the sync retrieval ops, which are ungrouped-*core* because the local index
  is always present — a live domain is a configured integration, so it gates.
- **The safety envelope — reads that still traverse it.** Both ops are honest `Effect::Read`,
  `Risk::Low`, `Idempotency::Idempotent` (`ToolSpec::read_only`, `crates/flux-spec/src/lib.rs:153`),
  so `RiskApprover` auto-allows them (`crates/flux-runtime/src/approval.rs` — "permits reads
  freely"). But they are dispatched through the same runtime path as every tool (approval →
  guarded execute) — never a side channel. Crucially, when the backend does network egress the ops
  also declare the matching `Effect::Network` / `AccessKind::Network` (or connection access) so
  catalog metadata stays honest. More importantly, their `authority_requirements` override returns
  exact `datasource.read` plus backend `network.fetch` / `connection.dial` requirements; planning
  and dispatch consume that same typed contract. The egress itself goes through flux's guarded
  seams (the net guard for a native backend; gated plugin host caps for a plugin-backed future).
  `permission_subjects` surfaces the stable `<domain>/<entity>` resource rather than filter values.
  Reads remain low-risk for approval purposes but are never authorization-free.

## Open questions (resolved)

### Q1 — Filter typing: string-only vs typed. **Recommendation: declared, typed filter keys per entity.**

Not a free-form string bag, and not a full query DSL. Each entity declares its filter keys in
`schema()` as `FilterKey { name, ty: String|Int|Bool|Enum, required, description }`. The generic
projection rejects unknown keys, coerces each value to its declared type, and checks enum membership
— *this is exactly the app-side "filter-key validation" being lifted into flux*, and it lets the op
schema document each entity's real filters (the model stops guessing). The backend receives an
already-validated `Filters` and its closures stay trivial. Keep the scalar set small (string / int /
bool / enum); **no** nested objects and **no** operators (`>=`, `IN`, ranges) in v1 — those are a
system-of-record query language, out of scope; a backend that needs richer selection declares more
enum-shaped keys. String-only is rejected as under-validated (defeats the story's purpose); a typed
query DSL is rejected as over-scoped.

### Q2 — Paging: page-token (cursor) vs offset/limit. **Recommendation: opaque cursor tokens.**

`<domain>.list` takes an optional `page` (a cursor string) + optional `limit`; `Page<Row>` returns
`rows` + `next: Option<String>`. The cursor is **opaque to flux and to the model** — the backend
mints and interprets it (an API `next_page_token`, a keyset, or, for an offset-only backend, an
encoded offset). The projection passes `page` straight through and surfaces `next` back as a
"call again with `page=<token>`" hint. This is right because live systems-of-record expose
continuation tokens, not stable numeric offsets, and offset paging over a changing dataset drifts
(skips/dupes on insert) and is often capped for deep pages. Cursors **subsume** offset paging (an
offset-only backend base64-encodes its offset) without forcing every backend to fake stable offsets —
and the contrast with the sync `ListInput.offset` (right for a fixed local snapshot) is precisely why
this is a second trait. **Limit clamping** stays in the projection: clamp the requested `limit` to the
entity's declared `max_page`, apply `default_page` when omitted (the app-side clamp, generalized). The
cursor is subject to the same no-secrets rule as rows (Q3): it is a continuation pointer, not a live
handle — a backend must not encode credentials or connection state into a token that lands in
transcripts/`events.db`.

### Q3 — References: how rows carry references without smuggling secrets/handles. **Recommendation: a row is projection output, not a capability.**

A `Row` carries `id + title + summary` and at most an **opaque `Reference`** — a plain-data locator:
an `(entity, id)` pair, or a `URL`/permalink for human navigation — **never** a live handle, session,
token, presigned-with-credentials URL, or connection. `<domain>.get {entity, id}` resolves a row by
**re-entering the backend**, which re-establishes its own connection/auth host-side from its own
config; the model passes back only the `id` (a name), never a handle it was given. This is flux's
established weak-reference model applied unchanged:

- the `endpoint` subsystem returns weak refs — "URLs + a credential location, **never a secret**"
  (`crates/flux-tools/src/groups.rs`, the `endpoint` group description), and "resolution is host-side
  only: there is no capability that hands the resolved [value]"
  (`crates/flux-plugin/src/lib.rs:380`);
- plugin IO is references-only — "opaque `endpoint_ref`/`credential_ref`" (`docs/roadmap.md:843`),
  credentials materialized host-side and "never returned via any discovery/endpoint path"
  (`plugins/AUTHORING.md:105`);
- and it is exactly how `datasource.get {source,entity,id}` already works (`ops.rs:120`): the id is a
  name, the backend owns the fetch.

Two enforcement layers: **(1) by shape** — `Row` has no field capable of holding a handle/secret, so
a well-behaved backend *cannot* smuggle one (the way `EndpointRef` has no password field); **(2) by
the redactor** — rendered row text still passes flux's redaction seam (C-13/C-22), so a backend bug
that stuffs a secret into `summary` is caught the same way a plugin stuffing a secret into a `Record`
body is today. The shape is the primary defense; redaction is the backstop.

## Alternatives considered

- **Retrofit `DatasourceBackend` (make it async + add paged live methods).** Rejected — the story's
  core call. The two shapes serve different needs (local index vs remote SoR); one async, paged,
  filterable trait would either bloat the sync index trait with methods every in-memory/SQLite
  backend must stub, or force the local index to pretend it is a remote feed. Two clean traits beat
  one leaky one.
- **String-only filters** (Q1) — under-validated; loses the op-schema documentation that makes the
  filters discoverable.
- **Offset/limit paging** (Q2) — drifts over a live dataset and doesn't map onto token-based APIs;
  cursors subsume it.
- **A `Reference` that carries a live handle** (Q3) — breaks the references-only invariant and the
  weak-ref precedent; `get(id)` re-resolution keeps auth host-side.
- **Ungrouped-core surfacing** (like the sync retrieval ops) — rejected; a live domain is a
  configured integration, not an always-present local index, so it gates on its own signal (honest
  catalog, browser/endpoint pattern).
- **A new crate for the trait** — rejected; a module in `flux-capabilities` (already L5, already
  async) is the "one crate + modules" fit.

## Non-goals (v1)

Write/mutation ops on a live backend (this seam is read projection only); a query DSL with operators
or nested filters; cross-domain fan-out or joins; caching live rows into the sync index (a domain that
wants that ingests through `DatasourceBackend` separately); a plugin-protocol live-datasource
capability (the L0 types leave the door open, but v1 ships the native trait + projection only).

## Story map

Accepted implementation stories — each remains independently testable and committable:

1. **D-168 — L0 live-datasource types** — add `Row`, `Page<T>`, `PageRequest`, `Filters`, `Reference`,
   `LiveSchema`/`LiveEntity`, `FilterKey`/`FilterType` to `flux-datasource` (pure data, serde),
   with round-trip tests.
2. **D-169 — the `LiveDatasource` trait** — `async list`/`get` + `schema()` and closed
   `LiveAccess` declarations in `flux-capabilities/src/datasource/live.rs`, using the existing
   async/runtime dependencies.
3. **D-170 — the generic two-op projection** — `try_register_live_datasource(registry, groups,
   domain, backend)`
   emitting `<domain>.list` + `<domain>.get` with schema-generated input schemas and compact row +
   `next:` rendering.
4. **D-171 — validation, clamping & cursor plumbing** — filter-key/type validation (unknown-key + enum
   rejection), `limit` clamp to `max_page` with `default_page`, opaque cursor pass-through — with
   the failing-first tests that pin each rule.
5. **D-172 — surfacing + envelope wiring** — the per-domain `ToolGroup` + ambient signal, honest
   effects/access, exact typed authority requirements, stable permission subjects, and an SDK
   convenience seam; tests prove catalog gating and plan/dispatch requirement identity.
6. **D-173 — reference backend + adoption proof** — a hermetic in-memory `LiveDatasource` with
   cursor paging and declared filters, end-to-end SDK coverage, and public docs explaining when to
   use the live seam vs the sync index and why rows remain weak references.
