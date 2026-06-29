//! `flux-capabilities` — the L5 capability tools the agent can call.
//!
//! Two coherent siblings folded into one crate (C-01 phase 3), each a [`flux_runtime::Tool`]:
//! - [`browser`] — guarded web access ([`WebFetchTool`], SSRF-safe egress);
//! - [`datasource`] — an in-memory keyword [`Index`] with a [`SearchTool`] for RAG-style lookup.
//!
//! Caller identity (`flux-auth`) is deliberately *not* here — it is a distinct concern (surfaces
//! resolve identity into `(Caller, Trust)`), not a tool capability.

pub mod browser;
pub mod datasource;

pub use browser::WebFetchTool;
pub use datasource::{Document, Index, SearchHit, SearchTool};
