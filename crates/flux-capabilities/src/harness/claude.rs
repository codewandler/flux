//! Message extraction from claude-code's project transcripts.
//!
//! Shape, as observed in `~/.claude/projects/<slug>/<session>.jsonl`: one JSON object per line, a
//! top-level `type` naming the record (`user`, `assistant`, `system`, plus a dozen bookkeeping
//! kinds), and the body under `message` with `role`, `model` and `content`. `content` is either a
//! bare string or an array of typed blocks — `text`, `thinking`, `tool_use`, `tool_result` — which
//! is exactly the case a naive `as_str()` turns into an empty body without failing.
//!
//! `flux usage` walks these same records and reads `message.usage` out of them; this reads
//! `message.content`.

use std::collections::BTreeMap;
use std::path::Path;

use flux_core::Result;
use serde_json::Value;

use super::message::{file_stem, flatten_content, json_epoch_ms_at, HarnessMessage, MessageRole};
use super::scan::{jsonl_files, open_jsonl, JsonlLine, ScanBudget, SkipReason};
use super::{HarnessKind, MessageSink, MessageStats};

/// Extract every message under a claude-code `projects` root.
///
/// Only an unreadable `root` is an error; everything below it degrades by skipping and counting,
/// exactly as the token-shaped scan does.
pub fn claude_messages(
    root: &Path,
    budget: ScanBudget,
    emit: &mut dyn FnMut(HarnessMessage),
) -> Result<MessageStats> {
    let scan = jsonl_files(root, budget)?;
    let mut sink = MessageSink::new(budget, emit);
    sink.skip_unreadable(scan.skipped());

    // Session ordinals live across files, not within one: claude-code writes one file per session,
    // but a resumed session can be continued in another, and `index` must not restart there.
    let mut ordinals = BTreeMap::<String, u32>::new();

    for file in scan.files() {
        sink.scanned();
        // The two skip reasons are not interchangeable: a file over the file cap is a budget
        // decision (`skipped_oversize`), one that would not open is a broken environment
        // (`skipped_unreadable`), and a caller reads them differently.
        let lines = match open_jsonl(file, budget) {
            Ok(lines) => lines,
            Err(SkipReason::TooLarge) => {
                sink.skip_oversize();
                continue;
            }
            Err(SkipReason::Unreadable) => {
                sink.skip_unreadable(1);
                continue;
            }
        };
        let fallback_session = file_stem(file);
        for line in lines {
            let text = match line {
                JsonlLine::Text(text) => text,
                JsonlLine::Unreadable => {
                    sink.skip_unreadable(1);
                    continue;
                }
                JsonlLine::TooLarge => {
                    sink.skip_oversize();
                    continue;
                }
            };
            // The cheap pre-filter `flux usage` already applies: most lines in a transcript are
            // bookkeeping, and parsing them all is the dominant cost of a scan.
            if !text.contains("\"message\"") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                sink.skip_malformed();
                continue;
            };
            let Some(message) = claude_message(&value, &fallback_session, file, budget) else {
                continue;
            };
            let ordinal = ordinals.entry(message.session_id.clone()).or_default();
            let message = HarnessMessage {
                index: *ordinal,
                ..message
            };
            *ordinal += 1;
            if !sink.offer(message) {
                return Ok(sink.into_stats());
            }
        }
    }
    Ok(sink.into_stats())
}

/// Project one transcript line, or `None` when it is not a conversation record at all.
fn claude_message(
    value: &Value,
    fallback_session: &str,
    path: &Path,
    budget: ScanBudget,
) -> Option<HarnessMessage> {
    let kind = value.get("type").and_then(Value::as_str)?;
    if !matches!(kind, "user" | "assistant" | "system") {
        return None;
    }
    let body = value.get("message")?;
    let content = body.get("content")?;
    let role = body
        .get("role")
        .and_then(Value::as_str)
        .and_then(MessageRole::normalize)
        .or_else(|| MessageRole::normalize(kind))?;
    // A tool result is filed under `user` because that is the wire position it occupies. Reporting
    // it as the human's words would be wrong, so the normalized role says what it actually is.
    let role = if role == MessageRole::User && all_tool_results(content) {
        MessageRole::Tool
    } else {
        role
    };
    let session_id = value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or(fallback_session)
        .to_string();
    Some(HarnessMessage {
        harness: HarnessKind::Claude,
        session_id,
        index: 0,
        role,
        text: flatten_content(content, budget.max_message_bytes),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        workspace: value.get("cwd").and_then(Value::as_str).map(str::to_string),
        ts_ms: json_epoch_ms_at(value, &["timestamp"])
            .or_else(|| json_epoch_ms_at(value, &["message", "timestamp"])),
        path: path.to_path_buf(),
    })
}

fn all_tool_results(content: &Value) -> bool {
    let Some(blocks) = content.as_array() else {
        return false;
    };
    !blocks.is_empty()
        && blocks
            .iter()
            .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
}
