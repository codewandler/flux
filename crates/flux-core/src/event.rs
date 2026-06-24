//! High-level session events.
//!
//! Events are the single stream that drives the TUI, the SDK, and the audit log. The set will
//! grow as the runtime lands (tool authorization, approval, plugin, skill events); this is the
//! starter set needed for the M0 agent turn.

use serde::{Deserialize, Serialize};

use crate::content::ContentBlock;
use crate::stream::{StopReason, Usage};

/// A discriminated event in the lifetime of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TurnStarted {
        turn: u32,
    },
    AssistantTextDelta {
        text: String,
    },
    AssistantMessage {
        content: Vec<ContentBlock>,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
    },
    Usage(Usage),
    TurnEnded {
        turn: u32,
        stop_reason: Option<StopReason>,
    },
    Error {
        message: String,
    },
}
