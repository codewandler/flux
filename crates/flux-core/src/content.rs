//! The unified content and message model.
//!
//! `ContentBlock` is the single representation of a piece of message content across every
//! provider. Its serde shape intentionally mirrors the Anthropic Messages wire format
//! (internally tagged on `type`, snake_case), which keeps the provider mapping nearly free
//! while staying a clean internal type for session persistence.

use serde::{Deserialize, Serialize};

/// The conversational role of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A single block of message content.
///
/// `Thinking` blocks carry a `signature` that MUST be preserved verbatim and resubmitted
/// when continuing a tool-use loop, or the provider will reject the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    Image {
        source: ImageSource,
    },
}

/// Content nested inside a `ToolResult` (text or image only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text { text: String },
    Image { source: ImageSource },
}

/// The source of an image block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

impl ContentBlock {
    /// Convenience constructor for a plain text block.
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    /// Convenience constructor for a text-only tool result.
    pub fn tool_result_text(
        tool_use_id: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error,
        }
    }

    /// Returns the text of a `Text` block, if this is one.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }
}
