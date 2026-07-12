---
title: Datasources (RAG)
description: "Give an embedded agent retrieval ops — search/get/list over a knowledge backend — via register_pack and a direct flux-capabilities dependency."
---

# Datasources (RAG)

You can give an embedded agent **retrieval ops** — `search`, `get`, `list`, `relation`,
`batch_get`, `sources` — over your own knowledge backend. There is no first-class
`with_datasource(...)` builder method **yet** (see [Why there's no first-class API](#why-theres-no-first-class-api-yet)); today you attach a
datasource through the existing `register_pack` seam with a direct dependency on
`codewandler-flux-capabilities`.

## The recipe

Add the capabilities crate alongside the SDK:

```toml
[dependencies]
codewandler-flux-sdk = "0.19"
codewandler-flux-capabilities = "0.19"
```

Build a backend, index documents into it, and register its ops onto a client. The registered ops
dispatch through the **same** authorization → approval → guarded-IO envelope as every built-in:

```rust
use std::sync::Arc;

use flux_capabilities::{
    ingest_markdown, register_datasource_ops, DatasourceBackend, MemoryBackend,
};
use flux_sdk::FlowClient;

# fn provider() -> Arc<dyn flux_provider::Provider> { unimplemented!() }
# async fn ex() -> flux_core::Result<()> {
// 1. Build a backend and index documents. `MemoryBackend` is the default keyword backend;
//    a Postgres/embeddings backend implements the same `DatasourceBackend` trait.
let backend: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
let docs = vec![(
    "intro.md".to_string(),
    "# Flux\nFlux is a deterministic agent platform.".to_string(),
)];
ingest_markdown(&*backend, "local", &docs)?;

// 2. Register the retrieval ops via `register_pack` — the same seam any op-pack uses.
let mut client = FlowClient::builder().auto_approve(true).build(provider(), ".")?;
client.register_pack(move |registry| register_datasource_ops(registry, backend.clone()));

// 3. `search`/`get`/`list`/`relation`/`batch_get`/`sources` are now in the catalog.
assert!(client.op_names().iter().any(|n| n == "search"));
# Ok(()) }
```

The same `register_pack(...)` call works on `ClientBuilder` for the conversational
[`Client`](./sessions.md). A runnable version of this recipe lives at
[`examples/datasource_recipe.rs`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/examples/datasource_recipe.rs)
(`cargo run -p codewandler-flux-sdk --example datasource_recipe`).

## Why there's no first-class API yet

This is deliberate. A first-class `with_datasource(...)` surface is held back until the **async,
paged live-backend seam** lands — tracked as story **D-62**. Freezing a datasource API on top of
today's synchronous backend trait would bake in the wrong contract (no streaming pages, no async
live sources), so the SDK deliberately leaves datasources as the `register_pack` recipe above until
that seam exists. The rationale is recorded in the "Out of scope" section of the
[sdk-surface design](https://github.com/codewandler/flux/blob/main/docs/designs/sdk-surface.md).
When D-62 lands, this recipe is replaced by a first-class builder method.
