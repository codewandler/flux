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
pub mod endpoint;

pub use browser::WebFetchTool;
pub use datasource::{
    chunk_text, datasource_tools, freshness, ingest_markdown, ingest_openapi, ingest_text,
    records_to_context_blocks, register_datasource_ops, reindex, ChunkOptions, DatasourceBackend,
    DatasourceHostCaps, Embedder, MemoryBackend, MemoryVectorStore, SemanticIndex, SqliteBackend,
    VectorStore,
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
