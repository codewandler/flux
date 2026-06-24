//! Conversation messages: an ordered list of content blocks with a role.

use serde::{Deserialize, Serialize};

use crate::content::{ContentBlock, Role};

/// A single conversation message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }

    pub fn user(content: Vec<ContentBlock>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self::new(Role::Assistant, content)
    }

    pub fn system(content: Vec<ContentBlock>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![ContentBlock::text(text)])
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::assistant(vec![ContentBlock::text(text)])
    }

    pub fn system_text(text: impl Into<String>) -> Self {
        Self::system(vec![ContentBlock::text(text)])
    }

    /// Concatenate the text of all `Text` blocks in this message.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let Some(t) = block.as_text() {
                out.push_str(t);
            }
        }
        out
    }
}
