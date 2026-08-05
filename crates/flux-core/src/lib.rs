//! `flux-core` — the pure contract layer for flux.
//!
//! This crate defines the fundamental, IO-free types shared across the whole system:
//! the unified content/message model, the streaming chunk protocol, high-level events,
//! and the common error type. Nothing here performs IO; provider clients, the runtime,
//! and the surfaces all build on these types.

mod audio;
mod content;
mod context;
mod dispatch;
mod error;
mod event;
pub mod humanize;
mod message;
pub mod pricing;
pub mod readiness;
mod redaction;
mod stream;
mod timing;
mod urlencode;

pub use audio::{AudioEncoding, AudioFormat};
pub use content::{
    ContentBlock, ImageSource, Role, ToolResultContent, ARGS_PARSE_ERROR_KEY, ARGS_RAW_PREFIX_KEY,
};
pub use context::{escape_knowledge_base_body, render_knowledge_blocks, ContextBlock};
pub use dispatch::DispatchId;
pub use error::{Error, GuardedIoError, GuardedIoFailure, Result};
pub use event::Event;
pub use humanize::{fmt_count, fmt_elapsed};
pub use message::Message;
pub use pricing::{
    canonical_model_parts, canonical_model_spec, is_metered_cloud_spec, is_subscription,
    resolve_role_model, CostSource, Money, PricingTable, RateOverride, Rates,
};
pub use redaction::{redact_json_total, JsonRedaction};
pub use stream::{CacheEfficiency, Chunk, StopReason, Usage};
pub use timing::OperationTiming;
pub use urlencode::percent_encode_component;
