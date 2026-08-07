//! `flux-capabilities` — the L5 capability tools the agent can call.
//!
//! - [`datasource`] — two deliberately separate read shapes: indexed knowledge through a pluggable
//!   [`DatasourceBackend`] and its `search`/`get`/`list`/`relation`/`batch_get`/`sources` operations;
//!   and async systems of record through [`LiveDatasource`], projected as generated
//!   `<domain>.list` / `<domain>.get` operations with schema validation, evidence surfacing, and
//!   exact datasource plus declared network/connection authority. [`WorkBoard`] is the
//!   **write-capable** sibling of that second shape — the same registration convention, plus four
//!   mutating operations gated on concrete `<domain>/item/<id>` subjects and a closed item state
//!   machine.
//!
//! - [`harness`] — where every local coding harness (`flux | codex | claude-code | opencode`) keeps
//!   its state, and how to walk it read-only under a bounded scan budget. Acquisition only: it
//!   carries no opinion about what the records mean, so `flux usage`'s token-shaped projection and
//!   the message-shaped history datasource sit on the same discovery.
//!
//! Web access (`http.request`, `web.fetch`, `browser.*`) moved to the native `flux-web` crate
//! (web-capabilities epic, D-120); the former `browser` module retired with it.
//!
//! Caller identity (`flux-auth`) is deliberately *not* here — it is a distinct concern (surfaces
//! resolve identity into `(Caller, Trust)`), not a tool capability.

pub mod datasource;
pub mod endpoint;
pub mod harness;
pub mod host;
pub mod usage_observatory;

pub use datasource::{
    chunk_text, datasource_tools, datasource_tools_with_history, freshness, ingest_harness_history,
    ingest_markdown, ingest_openapi, ingest_text, live_datasource_tools, records_to_context_blocks,
    register_datasource_ops, reindex, try_register_datasource_ops,
    try_register_datasource_ops_with_history, try_register_live_datasource,
    try_register_work_board, validate_board_contract, validate_live_contract, work_board_tools,
    BoardBinding, BoardLedger, BoardRegistry, ChunkOptions, DatasourceBackend, DatasourceHostCaps,
    Embedder, EmbeddingUsage, HarnessHistory, HarnessIngestReport, HarnessSelector, LiveAccess,
    LiveDatasource, LiveDatasourceSurface, MarkdownBoard, MemoryBackend, MemoryBoard,
    MemoryVectorStore, SemanticIndex, SessionBoard, SessionBoardItem, SessionBoardSnapshot,
    SqliteBackend, VectorStore, WorkBoard, WorkBoardSurface, HARNESS_MESSAGE_ENTITY,
    HARNESS_SESSION_ENTITY, HARNESS_SESSION_REL, HARNESS_SOURCE, MAX_LIVE_SESSION_ENVELOPES,
};
pub use endpoint::{
    endpoint_tools, register_endpoint_ops, try_register_endpoint_ops, CredentialReader,
    CrossPluginApprover, CrossPluginAudit, CrossPluginGrants, EndpointBroker,
    EndpointBrokerHostCaps, EndpointRegistry, HostCredentialReader, HostProviderInvoker,
    PluginRegistry, ProviderEntry, ProviderInvoker, StaticResolver, ENDPOINT_GROUP,
};
pub use host::{
    host_tools, metric_answer_json, register_host_ops, render_metric_answer, static_availability,
    try_register_host_ops, HostMetrics, HostProbeFailure, HostProbeReport, HostProber,
    HostRegistry, HOST_GROUP,
};

#[cfg(feature = "embeddings")]
pub use datasource::OpenAiEmbedder;

#[cfg(feature = "local-embeddings")]
pub use datasource::FastEmbedEmbedder;

#[cfg(feature = "sqlite-vec")]
pub use datasource::SqliteVecStore;
