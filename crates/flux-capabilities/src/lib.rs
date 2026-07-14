//! `flux-capabilities` — the L5 capability tools the agent can call.
//!
//! - [`datasource`] — two deliberately separate read shapes: indexed knowledge through a pluggable
//!   [`DatasourceBackend`] and its `search`/`get`/`list`/`relation`/`batch_get`/`sources` operations;
//!   and async systems of record through [`LiveDatasource`], projected as generated
//!   `<domain>.list` / `<domain>.get` operations with schema validation, evidence surfacing, and
//!   exact datasource plus declared network/connection authority.
//!
//! Web access (`http.request`, `web.fetch`, `browser.*`) moved to the native `flux-web` crate
//! (web-capabilities epic, D-120); the former `browser` module retired with it.
//!
//! Caller identity (`flux-auth`) is deliberately *not* here — it is a distinct concern (surfaces
//! resolve identity into `(Caller, Trust)`), not a tool capability.

pub mod datasource;
pub mod endpoint;

pub use datasource::{
    chunk_text, datasource_tools, freshness, ingest_markdown, ingest_openapi, ingest_text,
    live_datasource_tools, records_to_context_blocks, register_datasource_ops, reindex,
    try_register_datasource_ops, try_register_live_datasource, validate_live_contract,
    ChunkOptions, DatasourceBackend, DatasourceHostCaps, Embedder, EmbeddingUsage, LiveAccess,
    LiveDatasource, LiveDatasourceSurface, MemoryBackend, MemoryVectorStore, SemanticIndex,
    SqliteBackend, VectorStore,
};
pub use endpoint::{
    endpoint_tools, register_endpoint_ops, try_register_endpoint_ops, CredentialReader,
    CrossPluginApprover, CrossPluginAudit, CrossPluginGrants, EndpointBroker,
    EndpointBrokerHostCaps, EndpointRegistry, HostCredentialReader, HostProviderInvoker,
    PluginRegistry, ProviderEntry, ProviderInvoker, StaticResolver, ENDPOINT_GROUP,
};

#[cfg(feature = "embeddings")]
pub use datasource::OpenAiEmbedder;

#[cfg(feature = "local-embeddings")]
pub use datasource::FastEmbedEmbedder;

#[cfg(feature = "sqlite-vec")]
pub use datasource::SqliteVecStore;
