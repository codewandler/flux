//! Session-shape validity as types (A-99).
//!
//! The persisted conversation must always project to a **valid provider history**. Three shapes
//! break every provider that enforces the Messages contract, and each has broken flux at least once,
//! always on a newly added turn-termination path:
//!
//! 1. an **empty assistant message** (no blocks, or nothing but blank text),
//! 2. a **split `tool_use`/`tool_result` pair**,
//! 3. a **broken role alternation** (`user` after `user`).
//!
//! Until now the rules lived as discipline: every writer funnelled through one `finish_turn`, and
//! compaction snapped its own boundary with a local helper. This module makes the rules *types*, so
//! a writer cannot express an invalid shape rather than being trusted not to.
//!
//! [`AssistantMessage`] and [`ValidHistory`] are the only two ways to name a shape-checked value.
//! Neither can be constructed except through a checking constructor, and neither exposes a mutable
//! interior — so once you hold one, the invariant holds by construction.
//!
//! **Scope:** this governs the *persisted* log only. The transient per-call histories that model
//! stages build (`flux-flow`'s `staged.rs`) legitimately hold in-flight tool pairs mid-assembly and
//! are constructed and consumed in one place; they are deliberately not covered.

use std::collections::BTreeSet;
use std::fmt;

use flux_core::{ContentBlock, Message, Role};

/// Which session-shape invariant a candidate value broke, and where.
///
/// The index is always the position in the candidate history, so a caller can log something more
/// useful than "invalid" — the whole point of rejecting at the write seam instead of at a provider
/// 400 several seconds later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeError {
    /// An assistant message with no content blocks, or whose only content is blank text.
    EmptyAssistant { index: usize },
    /// Two messages in a row with the same role — the classic `user`-after-`user`.
    ConsecutiveRole { index: usize, role: Role },
    /// A history that does not open on a `user` message.
    MustStartWithUser,
    /// A `system` message inside the conversation log. System content belongs in the prompt, never
    /// in the persisted alternation.
    SystemInHistory { index: usize },
    /// A `tool_result` whose answering `tool_use` is not in the immediately preceding message.
    OrphanedToolResult { index: usize, tool_use_id: String },
    /// A `tool_use` whose `tool_result` is not in the immediately following message.
    OrphanedToolUse { index: usize, tool_use_id: String },
    /// `open_turn` was called while the log already owes an assistant answer (A-100). The
    /// alternation-level [`ConsecutiveRole`](Self::ConsecutiveRole) seen as a *transition* rather
    /// than as a property of a finished sequence — this is the one the write seam reports, because
    /// it is raised **before** anything is appended.
    TurnAlreadyOpen,
    /// `close_turn` was called with no turn open — the log is empty, or its last message is
    /// already an assistant answer (A-100). Appending here would produce `assistant` after
    /// `assistant`, or a history that does not open on a `user` message.
    NoTurnOpen,
    /// `open_turn` was handed a message that is not a `user` message (A-100).
    NotAUserMessage { role: Role },
}

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAssistant { index } => {
                write!(f, "empty assistant message at index {index}")
            }
            Self::ConsecutiveRole { index, role } => write!(
                f,
                "two {role:?} messages in a row at index {index} — broken alternation"
            ),
            Self::MustStartWithUser => write!(f, "history must open on a user message"),
            Self::SystemInHistory { index } => write!(
                f,
                "system message at index {index} — system content belongs in the prompt"
            ),
            Self::OrphanedToolResult { index, tool_use_id } => write!(
                f,
                "tool_result {tool_use_id} at index {index} answers no tool_use in the preceding message"
            ),
            Self::OrphanedToolUse { index, tool_use_id } => write!(
                f,
                "tool_use {tool_use_id} at index {index} is never answered by a tool_result"
            ),
            Self::TurnAlreadyOpen => write!(
                f,
                "a turn is already open — the log's last message is a user message still awaiting \
                 its assistant answer"
            ),
            Self::NoTurnOpen => write!(
                f,
                "no turn is open — an assistant answer needs a preceding user message"
            ),
            Self::NotAUserMessage { role } => {
                write!(f, "a turn opens on a user message, not on {role:?}")
            }
        }
    }
}

impl std::error::Error for ShapeError {}

/// An assistant message that is guaranteed non-empty.
///
/// There is no way to build one that violates the rule, which is what lets `close_turn` (A-100)
/// accept it without re-checking. A message whose only blocks are `tool_use` is legitimately
/// "textless" and **is** accepted — emptiness here means *no content at all*, not *no prose*.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantMessage(Message);

impl AssistantMessage {
    /// Check `content` and wrap it. Rejects an empty block list, and a block list whose every entry
    /// is text that trims to nothing.
    pub fn new(content: Vec<ContentBlock>) -> Result<Self, ShapeError> {
        if is_empty_assistant_content(&content) {
            return Err(ShapeError::EmptyAssistant { index: 0 });
        }
        Ok(Self(Message::assistant(content)))
    }

    /// The common case: one text block, rejected when blank.
    pub fn text(text: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(vec![ContentBlock::Text { text: text.into() }])
    }

    /// Borrow the checked message.
    pub fn as_message(&self) -> &Message {
        &self.0
    }

    /// Consume into the underlying message.
    pub fn into_message(self) -> Message {
        self.0
    }
}

/// A message sequence that satisfies every session-shape invariant.
///
/// Built only through [`ValidHistory::new`] (or `TryFrom`), and never handed out mutably, so the
/// invariant cannot be broken after the check.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ValidHistory(Vec<Message>);

impl ValidHistory {
    /// Check a candidate history. An empty history is valid (a fresh session).
    pub fn new(messages: Vec<Message>) -> Result<Self, ShapeError> {
        validate(&messages)?;
        Ok(Self(messages))
    }

    /// Borrow the checked messages.
    pub fn as_slice(&self) -> &[Message] {
        &self.0
    }

    /// Consume into the underlying messages.
    pub fn into_inner(self) -> Vec<Message> {
        self.0
    }

    /// Is this history empty?
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many messages.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The compaction split point: the largest `split <= len - keep` such that a single synthetic
    /// `user` summary message followed by `messages[split..]` is a valid history.
    ///
    /// This is the rule compaction used to carry inline as a local `has_tool_result` walk-back. It
    /// lives here because there is nothing caller-specific about it, and because that inline version
    /// only guarded *one* of the two ways a suffix can be unsplittable:
    ///
    /// - the suffix must not open on a message carrying a `tool_result` (its `tool_use` would be
    ///   summarized away), **and**
    /// - the suffix must not open on a `user` message, because it is about to be preceded by the
    ///   synthetic `user` summary — which is exactly `user`-after-`user`.
    ///
    /// Returns `None` when no split works, i.e. nothing can be summarized without breaking shape.
    pub fn snap(messages: &[Message], keep: usize) -> Option<usize> {
        let keep = keep.min(messages.len());
        let mut split = messages.len().checked_sub(keep)?;
        while split > 0 {
            let head = &messages[split];
            let opens_on_tool_result = head
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
            if opens_on_tool_result || head.role == Role::User {
                split -= 1;
                continue;
            }
            return Some(split);
        }
        None
    }
}

impl TryFrom<Vec<Message>> for ValidHistory {
    type Error = ShapeError;

    fn try_from(messages: Vec<Message>) -> Result<Self, Self::Error> {
        Self::new(messages)
    }
}

/// True when an assistant content list carries nothing at all: no blocks, or only blank text.
fn is_empty_assistant_content(content: &[ContentBlock]) -> bool {
    if content.is_empty() {
        return true;
    }
    content.iter().all(|b| match b {
        ContentBlock::Text { text } => text.trim().is_empty(),
        _ => false,
    })
}

/// The `tool_use` ids a message issues.
fn tool_use_ids(m: &Message) -> BTreeSet<&str> {
    m.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect()
}

/// The `tool_use` ids a message answers.
fn tool_result_ids(m: &Message) -> BTreeSet<&str> {
    m.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect()
}

/// The whole rule set, in one place. Checked in index order so the reported error is the *first*
/// problem, which is the one a caller can act on.
fn validate(messages: &[Message]) -> Result<(), ShapeError> {
    if messages.is_empty() {
        return Ok(());
    }
    if messages[0].role != Role::User {
        return Err(ShapeError::MustStartWithUser);
    }
    for (index, m) in messages.iter().enumerate() {
        if m.role == Role::System {
            return Err(ShapeError::SystemInHistory { index });
        }
        if m.role == Role::Assistant && is_empty_assistant_content(&m.content) {
            return Err(ShapeError::EmptyAssistant { index });
        }
        if index > 0 && messages[index - 1].role == m.role {
            return Err(ShapeError::ConsecutiveRole {
                index,
                role: m.role,
            });
        }
        // A tool_result must be answered by a tool_use in the message immediately before it.
        let answered_by_prev = if index > 0 {
            tool_use_ids(&messages[index - 1])
        } else {
            BTreeSet::new()
        };
        for id in tool_result_ids(m) {
            if !answered_by_prev.contains(id) {
                return Err(ShapeError::OrphanedToolResult {
                    index,
                    tool_use_id: id.to_string(),
                });
            }
        }
        // A tool_use must be answered by a tool_result in the message immediately after it.
        let answered_by_next = messages
            .get(index + 1)
            .map(tool_result_ids)
            .unwrap_or_default();
        for id in tool_use_ids(m) {
            if !answered_by_next.contains(id) {
                return Err(ShapeError::OrphanedToolUse {
                    index,
                    tool_use_id: id.to_string(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::ToolResultContent;

    fn user(text: &str) -> Message {
        Message::user_text(text)
    }

    fn assistant(text: &str) -> Message {
        Message::assistant_text(text)
    }

    fn tool_call(id: &str) -> Message {
        Message::assistant(vec![ContentBlock::ToolUse {
            id: id.into(),
            name: "read".into(),
            input: serde_json::json!({}),
        }])
    }

    fn tool_answer(id: &str) -> Message {
        Message::user(vec![ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: vec![ToolResultContent::Text { text: "ok".into() }],
            is_error: false,
        }])
    }

    // ---- AssistantMessage ----

    #[test]
    fn assistant_message_rejects_no_blocks() {
        assert_eq!(
            AssistantMessage::new(vec![]),
            Err(ShapeError::EmptyAssistant { index: 0 })
        );
    }

    #[test]
    fn assistant_message_rejects_blank_text() {
        assert_eq!(
            AssistantMessage::text("   \n\t "),
            Err(ShapeError::EmptyAssistant { index: 0 })
        );
        assert_eq!(
            AssistantMessage::text(""),
            Err(ShapeError::EmptyAssistant { index: 0 })
        );
    }

    #[test]
    fn assistant_message_accepts_real_text() {
        let m = AssistantMessage::text("done").unwrap();
        assert_eq!(m.as_message().role, Role::Assistant);
    }

    /// A tool-call-only assistant message carries no prose but is not *empty* — the provider
    /// contract cares about content blocks, not text.
    #[test]
    fn assistant_message_accepts_toolcall_only() {
        let m = AssistantMessage::new(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read".into(),
            input: serde_json::json!({}),
        }])
        .unwrap();
        assert_eq!(m.as_message().content.len(), 1);
    }

    // ---- ValidHistory: the three invariants ----

    #[test]
    fn empty_history_is_valid() {
        assert!(ValidHistory::new(vec![]).is_ok());
    }

    #[test]
    fn plain_alternation_is_valid() {
        let h = vec![
            user("hi"),
            assistant("hello"),
            user("more"),
            assistant("ok"),
        ];
        assert!(ValidHistory::new(h).is_ok());
    }

    #[test]
    fn user_after_user_is_rejected() {
        let h = vec![user("hi"), user("again")];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::ConsecutiveRole {
                index: 1,
                role: Role::User
            })
        );
    }

    #[test]
    fn assistant_after_assistant_is_rejected() {
        let h = vec![user("hi"), assistant("a"), assistant("b")];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::ConsecutiveRole {
                index: 2,
                role: Role::Assistant
            })
        );
    }

    #[test]
    fn empty_assistant_anywhere_is_rejected() {
        let h = vec![user("hi"), Message::assistant(vec![]), user("x")];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::EmptyAssistant { index: 1 })
        );
    }

    #[test]
    fn history_must_start_with_user() {
        assert_eq!(
            ValidHistory::new(vec![assistant("hi")]),
            Err(ShapeError::MustStartWithUser)
        );
    }

    #[test]
    fn system_in_history_is_rejected() {
        let h = vec![user("hi"), Message::system(vec![ContentBlock::text("sys")])];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::SystemInHistory { index: 1 })
        );
    }

    #[test]
    fn paired_tool_use_and_result_are_valid() {
        let h = vec![
            user("hi"),
            tool_call("t1"),
            tool_answer("t1"),
            assistant("done"),
        ];
        assert!(ValidHistory::new(h).is_ok());
    }

    #[test]
    fn orphaned_tool_use_is_rejected() {
        // The tool_use is never answered — exactly what compaction summarizing the answer away
        // would leave behind.
        let h = vec![user("hi"), tool_call("t1")];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::OrphanedToolUse {
                index: 1,
                tool_use_id: "t1".into()
            })
        );
    }

    #[test]
    fn orphaned_tool_result_is_rejected() {
        // The answering tool_use was dropped — the other half of the same bug.
        let h = vec![user("hi"), assistant("no call"), tool_answer("t1")];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::OrphanedToolResult {
                index: 2,
                tool_use_id: "t1".into()
            })
        );
    }

    /// Mismatched ids break the pairing from both sides at once. The reported error is the FIRST in
    /// index order — `t1`'s unanswered `tool_use` at index 1, not `t2`'s orphaned result at 2 —
    /// which is the documented contract: report the problem a caller reaches first.
    #[test]
    fn mismatched_tool_ids_are_rejected() {
        let h = vec![user("hi"), tool_call("t1"), tool_answer("t2")];
        assert_eq!(
            ValidHistory::new(h),
            Err(ShapeError::OrphanedToolUse {
                index: 1,
                tool_use_id: "t1".into()
            })
        );
    }

    #[test]
    fn error_names_the_invariant_and_the_index() {
        let e = ValidHistory::new(vec![user("a"), user("b")]).unwrap_err();
        let rendered = e.to_string();
        assert!(rendered.contains("index 1"), "{rendered}");
        assert!(rendered.contains("alternation"), "{rendered}");
    }

    // ---- snap ----

    /// The bug the inline compaction walk-back missed. With a strict `user/assistant` alternation
    /// and `keep = 2`, the naive split lands on a `user` message — and compaction prepends a
    /// synthetic `user` summary in front of it, producing `user`-after-`user`. `snap` walks back one
    /// further, onto the assistant message, which is the nearest split that survives the prepend.
    #[test]
    fn snap_walks_back_off_a_user_boundary() {
        let msgs = vec![user("u1"), assistant("a1"), user("u2"), assistant("a2")];
        // The naive `len - keep` is 2, which is `u2` — a user message.
        assert_eq!(ValidHistory::snap(&msgs, 2), Some(1));

        // And the resulting history really is valid once the summary is prepended.
        let mut rebuilt = vec![user("[summary of earlier conversation]\n…")];
        rebuilt.extend_from_slice(&msgs[1..]);
        assert!(ValidHistory::new(rebuilt).is_ok());
    }

    /// The case the inline version *did* guard: never split so a `tool_result` loses its `tool_use`.
    #[test]
    fn snap_walks_back_off_a_tool_result_boundary() {
        let msgs = vec![
            user("u1"),
            assistant("a1"),
            tool_call("t1"),
            tool_answer("t1"),
            assistant("a2"),
        ];
        // `len - keep` = 3 → the tool_answer; walking back lands on the tool_call at 2.
        assert_eq!(ValidHistory::snap(&msgs, 2), Some(2));
        let mut rebuilt = vec![user("[summary]")];
        rebuilt.extend_from_slice(&msgs[2..]);
        assert!(ValidHistory::new(rebuilt).is_ok());
    }

    #[test]
    fn snap_returns_none_when_nothing_can_be_summarized() {
        // Everything walks back to 0 — there is no prefix to summarize.
        let msgs = vec![user("u1"), user("u2")];
        assert_eq!(ValidHistory::snap(&msgs, 1), None);
    }

    #[test]
    fn snap_is_bounded_by_the_history_length() {
        let msgs = vec![user("u1")];
        assert_eq!(ValidHistory::snap(&msgs, 99), None);
    }

    // ---- property: try_from agrees with a reference predicate ----

    /// A deliberately naive, independently-written checker. If `ValidHistory` and this ever
    /// disagree on a generated sequence, one of them is wrong — which is the point.
    fn reference_is_valid(ms: &[Message]) -> bool {
        if ms.is_empty() {
            return true;
        }
        if ms[0].role != Role::User {
            return false;
        }
        for (i, m) in ms.iter().enumerate() {
            if m.role == Role::System {
                return false;
            }
            if m.role == Role::Assistant && is_empty_assistant_content(&m.content) {
                return false;
            }
            if i > 0 && ms[i - 1].role == m.role {
                return false;
            }
            for id in tool_result_ids(m) {
                if i == 0 || !tool_use_ids(&ms[i - 1]).contains(id) {
                    return false;
                }
            }
            for id in tool_use_ids(m) {
                match ms.get(i + 1) {
                    Some(next) if tool_result_ids(next).contains(id) => {}
                    _ => return false,
                }
            }
        }
        true
    }

    #[test]
    fn try_from_agrees_with_the_reference_predicate() {
        // A small deterministic corpus over the interesting shapes: plain turns, tool pairs,
        // empties, and every adjacent pairing of them.
        let alphabet: Vec<Message> = vec![
            user("u"),
            assistant("a"),
            Message::assistant(vec![]),
            tool_call("t1"),
            tool_answer("t1"),
            Message::system(vec![ContentBlock::text("s")]),
        ];
        let n = alphabet.len();
        let mut checked = 0;
        // All sequences of length 1..=3 over the alphabet.
        for len in 1..=3usize {
            let mut idx = vec![0usize; len];
            loop {
                let seq: Vec<Message> = idx.iter().map(|&i| alphabet[i].clone()).collect();
                assert_eq!(
                    ValidHistory::new(seq.clone()).is_ok(),
                    reference_is_valid(&seq),
                    "disagreement on {idx:?}"
                );
                checked += 1;
                // odometer increment
                let mut p = len;
                loop {
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                    idx[p] += 1;
                    if idx[p] < n {
                        break;
                    }
                    idx[p] = 0;
                    if p == 0 {
                        break;
                    }
                }
                if idx.iter().all(|&i| i == 0) {
                    break;
                }
            }
        }
        assert!(checked >= n + n * n, "corpus actually ran: {checked}");
    }
}
