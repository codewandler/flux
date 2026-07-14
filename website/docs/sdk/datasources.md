---
title: Datasources
description: "Attach indexed knowledge or an async live system of record to an embedded flux agent."
---

# Datasources

The SDK supports two complementary datasource shapes:

| | Indexed knowledge | Live system of record |
|---|---|---|
| Data lives | In a flux-owned record index | In the backend API or database |
| Contract | `DatasourceBackend` | `LiveDatasource` |
| Operations | `sources`, `search`, `get`, `list`, `relation`, `batch_get` | `<domain>.list`, `<domain>.get` |
| Paging | Numeric offsets over an indexed snapshot | Backend-owned opaque cursors |
| SDK wiring | `try_register_pack` | `ClientBuilder::try_with_live_datasource` |

Both project ordinary operations into the catalog. Every call still crosses authorization →
approval → guarded execution; registering a datasource does not create an IO or policy bypass.

## Live systems of record

Implement [`LiveDatasource`](https://docs.rs/codewandler-flux-sdk/latest/flux_sdk/datasource/trait.LiveDatasource.html)
for a domain such as support, CRM, or inventory, then attach it to the conversational client:

```rust
use std::sync::Arc;

use flux_sdk::datasource::LiveDatasource;
use flux_sdk::Client;

# fn provider() -> Box<dyn flux_sdk::Provider> { unimplemented!() }
# fn support_backend() -> Arc<dyn LiveDatasource> { unimplemented!() }
# fn ex() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder()
    .model("my-model")
    .try_with_live_datasource("support", support_backend())?
    .build(provider(), ".")?;

assert!(client
    .engine()
    .executor
    .registry()
    .get("support.list")
    .is_some());
# Ok(()) }
```

`flux_sdk::datasource` re-exports the complete consumer contract: `LiveDatasource`, `LiveAccess`,
`LiveSchema`, `LiveEntity`, typed filter declarations, `Page`/`PageRequest`, and weak `Row`/
`Reference` values. A live backend therefore needs the SDK, but not a direct dependency on flux's
internal capability crate.

Registration generates exactly two operations:

- **`<domain>.list { entity, page?, limit?, filters? }`** validates the entity and filters, applies
  the entity's default/maximum page size, invokes the async backend, and returns compact rows plus
  `next: <cursor>` when another page exists.
- **`<domain>.get { entity, id }`** validates the entity, re-enters the backend by stable id, and
  returns the full row or `not found`.

The backend's `schema()` is validated when it is registered. On each list call, flux rejects
unknown filters, missing required filters, wrong scalar types, and invalid enum values before the
backend runs. Filter types are deliberately small: string, integer, boolean, and declared enum.
The `page` cursor is opaque: flux passes it through unchanged, and the backend that minted it owns
its validation. Cursors must be continuation data, never credentials or connection state.

### Authority and safe values

Every generated call requires exact `datasource.read` authority for
`<domain>/<entity>`. A backend declares any additional external access with `LiveAccess`:

- `LiveAccess::Network { subject }` adds exact `network.fetch` authority.
- `LiveAccess::Connection { subject }` adds exact `connection.dial` authority.
- An in-process backend returns an empty list and needs no external-resource grant.

The filter values, cursor, and row id never become permission subjects. Planning and dispatch use
the same typed authority contract, and authorization denial happens before backend entry. Actual
network, process, or filesystem work must still use flux's guarded host surfaces.

Rows are projection data, not capabilities. A `Row` contains only a stable id, title, summary, and
an optional `Reference`. That reference is either another `(entity, id)` or a non-secret navigation
URL—never a credential, session, database handle, presigned secret URL, or live connection. A later
`get` call resolves the id again through host-owned authentication.

### Evidence-gated catalog surfacing

`try_with_live_datasource` installs the generated tools, a per-domain evidence group, and the
configured-domain ambient signal as one unit. Consequently, `support.list` and `support.get` are
advertised only when the `support` backend is actually configured. Lower-level hosts using
`try_register_live_datasource` receive the same group/signal description and must carry it into
their engine assembly. `FLUX_SURFACE_ALL` remains the explicit catalog-debug override; it does not
widen authorization.

The no-key reference implementation exercises two entities, typed filters, cursor paging, get,
not-found, and real executor dispatch:

- [`examples/live_datasource.rs`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/examples/live_datasource.rs)
- [`examples/support/live_datasource.rs`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/examples/support/live_datasource.rs)

```bash
cargo run -p codewandler-flux-sdk --example live_datasource
```

## Indexed knowledge

Use the indexed backend when records should be ingested into flux and searched by keyword or
semantic similarity. This remains the right contract for workspace docs, program-declared
knowledge, and plugin-contributed records.

Add the capabilities crate alongside the SDK:

```toml
[dependencies]
codewandler-flux-sdk = "0.25"
codewandler-flux-capabilities = "0.25"
```

Build a backend, index documents, and attach the six retrieval operations through the fallible pack
seam:

```rust
use std::sync::Arc;

use flux_capabilities::{
    ingest_markdown, try_register_datasource_ops, DatasourceBackend, MemoryBackend,
};
use flux_sdk::FlowClient;

# fn provider() -> Arc<dyn flux_sdk::Provider> { unimplemented!() }
# async fn ex() -> Result<(), Box<dyn std::error::Error>> {
let backend: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
let docs = vec![(
    "intro.md".to_string(),
    "# Flux\nFlux is a deterministic agent platform.".to_string(),
)];
ingest_markdown(&*backend, "local", &docs)?;

let mut client = FlowClient::builder()
    .auto_approve(true)
    .build(provider(), ".")?;
client.try_register_pack(move |registry| {
    try_register_datasource_ops(registry, backend.clone())
})?;

assert!(client.op_names().iter().any(|name| name == "search"));
# Ok(()) }
```

The same installer works with `ClientBuilder::try_register_pack` for a conversational
[`Client`](./sessions.md). The runnable indexed recipe remains at
[`examples/datasource_recipe.rs`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/examples/datasource_recipe.rs):

```bash
cargo run -p codewandler-flux-sdk --example datasource_recipe
```

See [the agent datasource concept](../agent/datasources.md) for how indexed and live datasources fit
into the operation catalog.
