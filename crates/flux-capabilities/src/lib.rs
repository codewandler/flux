//! `flux-capabilities` — the L5 capability tools the agent can call.
//!
//! - [`datasource`] — a knowledge layer (D-07): a pluggable [`DatasourceBackend`] over
//!   `flux-datasource` records + the retrieval ops
//!   `search`/`get`/`list`/`relation`/`batch_get`/`sources` (D-114).
//!
//! Web access (`http.request`, `web_fetch`, `browser.*`) moved to the native `flux-web` crate
//! (web-capabilities epic, D-120); the former `browser` module retired with it.
//!
//! Caller identity (`flux-auth`) is deliberately *not* here — it is a distinct concern (surfaces
//! resolve identity into `(Caller, Trust)`), not a tool capability.

pub mod datasource;
pub mod endpoint;

pub use datasource::{
    chunk_text, datasource_tools, freshness, ingest_markdown, ingest_openapi, ingest_text,
    records_to_context_blocks, register_datasource_ops, reindex, ChunkOptions, DatasourceBackend,
    DatasourceHostCaps, Embedder, EmbeddingUsage, MemoryBackend, MemoryVectorStore, SemanticIndex,
    SqliteBackend, VectorStore,
};
pub use endpoint::{
    endpoint_tools, register_endpoint_ops, CredentialReader, CrossPluginApprover, CrossPluginAudit,
    CrossPluginGrants, EndpointBroker, EndpointBrokerHostCaps, EndpointRegistry,
    HostCredentialReader, HostProviderInvoker, PluginRegistry, ProviderEntry, ProviderInvoker,
    StaticResolver, ENDPOINT_GROUP,
};

#[cfg(feature = "embeddings")]
pub use datasource::OpenAiEmbedder;

#[cfg(feature = "local-embeddings")]
pub use datasource::FastEmbedEmbedder;

#[cfg(feature = "sqlite-vec")]
pub use datasource::SqliteVecStore;
