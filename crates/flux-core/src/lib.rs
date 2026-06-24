//! `flux-core` — the pure contract layer for flux.
//!
//! This crate defines the fundamental, IO-free types shared across the whole system:
//! the unified content/message model, the streaming chunk protocol, high-level events,
//! and the common error type. Nothing here performs IO; provider clients, the runtime,
//! and the surfaces all build on these types.

mod content;
mod error;
mod event;
mod message;
mod stream;

pub use content::{ContentBlock, ImageSource, Role, ToolResultContent};
pub use error::{Error, Result};
pub use event::Event;
pub use message::Message;
pub use stream::{Chunk, StopReason, Usage};
