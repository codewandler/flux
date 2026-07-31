//! Message extraction from codex rollouts.
//!
//! Shape, as observed in `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`: every line is
//! `{timestamp, type, payload}`. `session_meta` opens the file with the session id and `cwd`,
//! `turn_context` announces the active model, and the conversation itself is carried by
//! `response_item` payloads of type `message`, with a `role` and a `content` array of typed
//! `input_text` / `output_text` blocks.
//!
//! **Why `response_item` and not `event_msg`.** codex mirrors each turn into both: an `event_msg`
//! of type `user_message`/`agent_message` carrying a flat string, and a `response_item` carrying
//! the structured message the model actually saw. Reading both double-counts every turn. The
//! `response_item` is the better of the two — it has the normalized role and the structured content
//! — at the cost of also surfacing the instruction/environment preamble codex sends as a user turn.
//! Over-collecting framing is recoverable; silently halving a transcript is not.

use std::collections::BTreeMap;
use std::path::Path;

use flux_core::Result;
use serde_json::Value;

use super::message::{file_stem, flatten_content, json_epoch_ms_at, HarnessMessage, MessageRole};
use super::scan::{jsonl_files, open_jsonl, JsonlLine, ScanBudget, SkipReason};
use super::{HarnessKind, MessageSink, MessageStats};

/// Extract every message under a codex `sessions` root.
///
/// Only an unreadable `root` is an error; everything below it degrades by skipping and counting.
pub fn codex_messages(
    root: &Path,
    budget: ScanBudget,
    emit: &mut dyn FnMut(HarnessMessage),
) -> Result<MessageStats> {
    let scan = jsonl_files(root, budget)?;
    let mut sink = MessageSink::new(budget, emit);
    sink.skip_unreadable(scan.skipped());

    let mut ordinals = BTreeMap::<String, u32>::new();

    for file in scan.files() {
        sink.scanned();
        // Over the file cap is a budget decision, unopenable is a broken environment — see the same
        // split in `claude.rs`. Folding them together loses the distinction `MessageStats` draws.
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
        // A rollout is one session, but the id and workspace only arrive with `session_meta`, and
        // the model can change mid-file — so these are per-file state, folded forward as it reads.
        let mut session_id = file_stem(file);
        let mut workspace: Option<String> = None;
        let mut model: Option<String> = None;

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
            // The cheap pre-filter, and it is not a micro-optimization: a rollout tree is mostly
            // captured tool output (1.7 GB of it on the machine this was calibrated against), and
            // handing all of that to `serde_json` is the dominant cost of a scan. `role` is the
            // discriminator because it is the field this adapter actually needs — a `function_call`
            // or `function_call_output` payload does not carry one.
            let interesting = text.contains("\"session_meta\"")
                || text.contains("\"turn_context\"")
                || (text.contains("\"response_item\"") && text.contains("\"role\""));
            if !interesting {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                sink.skip_malformed();
                continue;
            };
            let payload = match value.get("payload") {
                Some(payload) => payload,
                None => continue,
            };
            match value.get("type").and_then(Value::as_str) {
                Some("session_meta") => {
                    if let Some(id) = payload.get("id").and_then(Value::as_str) {
                        session_id = id.to_string();
                    }
                    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                        workspace.get_or_insert_with(|| cwd.to_string());
                    }
                    continue;
                }
                Some("turn_context") => {
                    if let Some(m) = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                    {
                        model = Some(m.to_string());
                    }
                    continue;
                }
                Some("response_item") => {}
                _ => continue,
            }
            if payload.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(content) = payload.get("content") else {
                continue;
            };
            let Some(role) = payload
                .get("role")
                .and_then(Value::as_str)
                .and_then(MessageRole::normalize)
            else {
                sink.skip_malformed();
                continue;
            };
            let ordinal = ordinals.entry(session_id.clone()).or_default();
            let message = HarnessMessage {
                harness: HarnessKind::Codex,
                session_id: session_id.clone(),
                index: *ordinal,
                role,
                text: flatten_content(content, budget.max_message_bytes),
                model: model.clone(),
                workspace: workspace.clone(),
                ts_ms: json_epoch_ms_at(&value, &["timestamp"]),
                path: file.clone(),
            };
            *ordinal += 1;
            if !sink.offer(message) {
                return Ok(sink.into_stats());
            }
        }
    }
    Ok(sink.into_stats())
}
