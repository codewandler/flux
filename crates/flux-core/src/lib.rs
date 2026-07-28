//! `flux-core` — the pure contract layer for flux.
//!
//! This crate defines the fundamental, IO-free types shared across the whole system:
//! the unified content/message model, the streaming chunk protocol, high-level events,
//! and the common error type. Nothing here performs IO; provider clients, the runtime,
//! and the surfaces all build on these types.

mod audio;
mod content;
mod context;
mod error;
mod event;
pub mod humanize;
mod message;
pub mod pricing;
mod stream;
mod timing;

pub use audio::{AudioEncoding, AudioFormat};
pub use content::{
    ContentBlock, ImageSource, Role, ToolResultContent, ARGS_PARSE_ERROR_KEY, ARGS_RAW_PREFIX_KEY,
};
pub use context::{render_knowledge_blocks, ContextBlock};
pub use error::{Error, Result};
pub use event::Event;
pub use humanize::{fmt_count, fmt_elapsed};
pub use message::Message;
pub use pricing::{
    canonical_model_parts, canonical_model_spec, is_metered_cloud_spec, is_subscription,
    resolve_role_model, CostSource, Money, PricingTable, RateOverride, Rates,
};
pub use stream::{CacheEfficiency, Chunk, StopReason, Usage};
pub use timing::OperationTiming;
