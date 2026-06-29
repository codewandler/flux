//! `flux-capabilities` — the L5 capability tools the agent can call.
//!
//! Two coherent siblings folded into one crate (C-01 phase 3):
//! - [`browser`] — guarded web access ([`WebFetchTool`], SSRF-safe egress);
//! - [`datasource`] — a knowledge layer (D-07): a pluggable [`DatasourceBackend`] over
//!   `flux-datasource` records + the retrieval ops `search`/`get`/`list`/`relation`/`batch_get`.
//!
//! Caller identity (`flux-auth`) is deliberately *not* here — it is a distinct concern (surfaces
//! resolve identity into `(Caller, Trust)`), not a tool capability.

pub mod browser;
pub mod datasource;

pub use browser::WebFetchTool;
pub use datasource::{
    freshness, ingest_markdown, ingest_openapi, register_datasource_ops, reindex,
    DatasourceBackend, Embedder, MemoryBackend, SqliteBackend,
};
