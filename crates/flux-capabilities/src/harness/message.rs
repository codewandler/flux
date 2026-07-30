//! The message-shaped view of a harness's local state, and the budget that makes producing it safe.
//!
//! Every one of the external parsers `flux usage` already runs descends to the object holding a
//! message body and takes only the token counts out of it. This module is the other projection of
//! that same descent: what was actually said, addressed well enough to go and read the rest.
//!
//! Two things here are load-bearing and neither is the parse.
//!
//! **Messages stream; they are never collected.** A [`HarnessMessage`] carries full text where a
//! usage record carries eight integers, so the same scan produces one to three orders of magnitude
//! more output — against directories holding years of history. Adapters therefore push into a
//! [`MessageSink`] and forget, and nothing in this module builds a `Vec` proportional to the input.
//!
//! **The budget is enforced against bodies, not just files.** [`ScanBudget`]'s inherited file and
//! count caps are necessary and not sufficient: one file within every file cap can still hold a
//! single body larger than memory. [`MessageSink`] is the one place the per-body, total-bytes and
//! message-count ceilings are applied, and every ceiling it hits is *counted* into
//! [`MessageStats`] rather than swallowed.
//!
//! This module extracts. It does not sanitize — redaction and escaping belong to the ingest seam
//! (C-215/C-216), so there is exactly one place they can be forgotten.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::scan::ScanBudget;
use super::HarnessKind;

/// One message read out of a harness's local state.
///
/// `index` is the message's ordinal *within its session*, assigned in scan order. It is the address
/// C-215 builds record ids on, so it must name the same message on a re-scan — see
/// [`MessageSink`]'s note on ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessMessage {
    /// Which harness this came out of.
    pub harness: HarnessKind,
    /// The harness's own session identifier.
    pub session_id: String,
    /// The message's ordinal within its session, dense and ascending in scan order.
    pub index: u32,
    /// The role, normalized across the harnesses' differing vocabularies.
    pub role: MessageRole,
    /// The body, flattened out of whatever content shape the harness stored — see
    /// [`flatten_content`].
    pub text: String,
    /// The model that produced the message, as the harness spells it. `None` when the harness does
    /// not record one for this message (a user turn, typically).
    pub model: Option<String>,
    /// The directory the session was running in, when the harness records one.
    pub workspace: Option<String>,
    /// Epoch milliseconds, when the harness records a usable timestamp.
    pub ts_ms: Option<i64>,
    /// The file or database the message was read from.
    pub path: PathBuf,
}

/// The role a message was written in, normalized.
///
/// The harnesses disagree on vocabulary — codex says `assistant`, its event stream says
/// `agent_message`, claude-code files a tool result under `user` — so passing the raw string
/// through would push that disagreement onto every consumer. [`MessageRole::normalize`] is the one
/// place it is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageRole {
    /// The human.
    User,
    /// The model.
    Assistant,
    /// Harness- or operator-authored framing: system prompts, instructions, environment context.
    System,
    /// The output of a tool call, fed back into the conversation.
    Tool,
}

impl MessageRole {
    /// The stable machine identifier, as it appears in record metadata.
    pub fn id(self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        }
    }

    /// Map a harness's own spelling onto the normalized set. `None` for anything unrecognized — an
    /// unknown role is a record to skip and count, not one to guess at.
    pub fn normalize(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "user" | "human" | "user_message" => Some(MessageRole::User),
            "assistant" | "agent" | "model" | "agent_message" => Some(MessageRole::Assistant),
            "system" | "developer" => Some(MessageRole::System),
            "tool" | "function" | "tool_result" | "function_call_output" => Some(MessageRole::Tool),
            _ => None,
        }
    }
}

/// What one message-shaped scan did, including everything it declined to do.
///
/// Skips are the whole point of this being a value: a scan that quietly dropped half a transcript
/// and a scan that read it are otherwise indistinguishable to the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageStats {
    /// Inputs examined: `.jsonl` files listed, or database rows walked.
    pub scanned: usize,
    /// Messages handed to the sink.
    pub emitted: usize,
    /// Total bytes of `text` handed to the sink.
    pub body_bytes: u64,
    /// Inputs that could not be read at all — an unreadable file, an unlistable subdirectory.
    pub skipped_unreadable: usize,
    /// Records that were read but did not parse, or did not carry a role this layer recognizes.
    pub skipped_malformed: usize,
    /// Records passed over for being individually too big: a line over
    /// [`ScanBudget::max_line_bytes`], or a body over [`ScanBudget::max_message_bytes`].
    pub skipped_oversize: usize,
    /// Records passed over because a whole-scan ceiling had already been reached.
    pub skipped_over_budget: usize,
    /// Whether a whole-scan ceiling stopped the scan early. When this is set, the remaining input
    /// was **not** read, so `skipped_over_budget` is a lower bound on what was left behind.
    pub budget_exhausted: bool,
}

impl MessageStats {
    /// Everything that was passed over, for whatever reason.
    pub fn skipped(&self) -> usize {
        self.skipped_unreadable
            + self.skipped_malformed
            + self.skipped_oversize
            + self.skipped_over_budget
    }
}

/// Where extracted messages go, and the one place the body budget is enforced.
///
/// Adapters offer messages here rather than returning them, so a scan's peak memory is one message
/// rather than one transcript tree. `offer` returns `false` once a whole-scan ceiling is reached;
/// an adapter that keeps going past that is only inflating `skipped_over_budget`.
///
/// **On ordering.** The sink assigns nothing — adapters do, because only they know the harness's
/// own order. What the sink requires of them is that the order be a function of the input alone
/// (sorted file list, then line order; a totally-ordered SQL `order by`), so that `index` addresses
/// the same message on a re-scan and an *append* extends the numbering instead of shifting it.
pub struct MessageSink<'a> {
    budget: ScanBudget,
    stats: MessageStats,
    emit: &'a mut dyn FnMut(HarnessMessage),
}

impl<'a> MessageSink<'a> {
    /// Wrap a consumer in the budget it will be fed under.
    pub fn new(budget: ScanBudget, emit: &'a mut dyn FnMut(HarnessMessage)) -> Self {
        Self {
            budget,
            stats: MessageStats::default(),
            emit,
        }
    }

    /// Count one examined input — a file, or a database row.
    pub(crate) fn scanned(&mut self) {
        self.stats.scanned += 1;
    }

    /// Count `n` inputs that could not be read at all.
    pub(crate) fn skip_unreadable(&mut self, n: usize) {
        self.stats.skipped_unreadable += n;
    }

    /// Count one record that was read but could not be understood.
    pub(crate) fn skip_malformed(&mut self) {
        self.stats.skipped_malformed += 1;
    }

    /// Count one record passed over for being individually over budget.
    pub(crate) fn skip_oversize(&mut self) {
        self.stats.skipped_oversize += 1;
    }

    /// Offer one message. Returns whether the scan should continue.
    ///
    /// A body over [`ScanBudget::max_message_bytes`] is dropped and counted but does **not** stop
    /// the scan — one pathological message must not cost the rest of the history. A whole-scan
    /// ceiling does stop it, and says so in [`MessageStats::budget_exhausted`].
    pub(crate) fn offer(&mut self, message: HarnessMessage) -> bool {
        if self.stats.budget_exhausted {
            self.stats.skipped_over_budget += 1;
            return false;
        }
        let bytes = message.text.len();
        if bytes > self.budget.max_message_bytes {
            self.stats.skipped_oversize += 1;
            return true;
        }
        if self.stats.emitted >= self.budget.max_messages
            || self.stats.body_bytes + bytes as u64 > self.budget.max_message_total_bytes
        {
            self.stats.budget_exhausted = true;
            self.stats.skipped_over_budget += 1;
            return false;
        }
        self.stats.emitted += 1;
        self.stats.body_bytes += bytes as u64;
        (self.emit)(message);
        true
    }

    /// The finished tally.
    pub fn into_stats(self) -> MessageStats {
        self.stats
    }
}

/// Flatten a harness's `content` field into one body.
///
/// **The decision this function encodes** (C-214 asked for it to be written down rather than
/// discovered): a message's text is every block that carried something a reader would want to
/// search, joined by newlines, and a block that carried none still leaves a marker.
///
/// - a bare string is itself;
/// - a text-bearing block (`text`, `input_text`, `output_text`, `thinking`, `reasoning`) yields its
///   text;
/// - a tool call yields `[tool_use: <name>]`, **not** `""` — a tool-call-only message is a real
///   message and must be addressable, and the tool name is the part of it worth finding;
/// - a tool result yields its own content, flattened, because that text was in the conversation;
/// - anything else yields `[<type>]`.
///
/// Appending stops once the result passes `cap`, which the caller sets to
/// [`ScanBudget::max_message_bytes`]: the over-cap result is then skipped and counted by
/// [`MessageSink::offer`], so an enormous body costs the cap rather than its own size.
pub(crate) fn flatten_content(value: &Value, cap: usize) -> String {
    let mut out = String::new();
    append_content(value, cap, &mut out);
    out
}

fn append_content(value: &Value, cap: usize, out: &mut String) {
    if out.len() > cap {
        return;
    }
    match value {
        Value::String(s) => push_part(s, out),
        Value::Array(items) => {
            for item in items {
                append_content(item, cap, out);
                if out.len() > cap {
                    return;
                }
            }
        }
        Value::Object(_) => append_block(value, cap, out),
        _ => {}
    }
}

fn append_block(block: &Value, cap: usize, out: &mut String) {
    let kind = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "tool_use" | "tool_call" | "tool" | "function_call" => {
            let name = block
                .get("name")
                .or_else(|| block.get("tool"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            push_part(&format!("[tool_use: {name}]"), out);
        }
        "tool_result" | "function_call_output" => {
            let before = out.len();
            for key in ["content", "output", "text"] {
                if let Some(inner) = block.get(key) {
                    append_content(inner, cap, out);
                }
                if out.len() > before {
                    return;
                }
            }
            push_part("[tool_result]", out);
        }
        _ => {
            // `thinking` blocks carry their text under their own name; everything else that carries
            // text at all carries it under `text`. A key that is *present but empty* falls through
            // to the marker rather than contributing nothing: claude-code writes a redacted
            // thinking block as `{"type":"thinking","thinking":"","signature":…}`, and on real
            // history that is a fifth of all assistant records — silently empty is the exact shape
            // this function exists to avoid.
            for key in ["text", "thinking", "reasoning", "summary"] {
                if let Some(text) = block.get(key).and_then(Value::as_str) {
                    if !text.is_empty() {
                        push_part(text, out);
                        return;
                    }
                }
            }
            if !kind.is_empty() {
                push_part(&format!("[{kind}]"), out);
            }
        }
    }
}

fn push_part(part: &str, out: &mut String) {
    if part.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(part);
}

/// The session identifier a file falls back to when the records inside it name none.
pub(crate) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// Read a harness timestamp — an RFC3339 string or an epoch integer — as epoch milliseconds.
pub(crate) fn json_epoch_ms(value: &Value) -> Option<i64> {
    if let Some(s) = value.as_str() {
        return parse_rfc3339_ms(s);
    }
    value.as_i64().map(normalize_epoch_ms)
}

/// Follow a path of object keys and read the value at the end as a timestamp.
pub(crate) fn json_epoch_ms_at(value: &Value, path: &[&str]) -> Option<i64> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    json_epoch_ms(cursor)
}

/// Harnesses write epochs in seconds and in milliseconds; anything below the year 2286 in
/// milliseconds must have been seconds. Same rule `flux usage` has always applied.
pub(crate) fn normalize_epoch_ms(n: i64) -> i64 {
    if n.abs() < 10_000_000_000 {
        n.saturating_mul(1000)
    } else {
        n
    }
}

/// Parse the RFC3339 forms the harnesses actually write, to epoch milliseconds.
///
/// Hand-rolled rather than delegated: this crate has no date library, and the accepted grammar is
/// narrow — `YYYY-MM-DDTHH:MM:SS`, an optional fractional second, and `Z` or `±HH:MM`. A naive
/// timestamp is read as UTC, which is what every harness here means by one.
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let field = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    if b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let year = field(0, 4)?;
    let month = field(5, 7).filter(|m| (1..=12).contains(m))?;
    let day = field(8, 10).filter(|d| (1..=31).contains(d))?;
    let hour = field(11, 13).filter(|h| (0..24).contains(h))?;
    let minute = field(14, 16).filter(|m| (0..60).contains(m))?;
    let second = field(17, 19).filter(|s| (0..62).contains(s))?;

    let mut rest = &s[19..];
    let mut millis = 0i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits: &str = {
            let end = fraction
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(fraction.len());
            &fraction[..end]
        };
        if digits.is_empty() {
            return None;
        }
        let mut ms = String::from(digits);
        ms.truncate(3);
        while ms.len() < 3 {
            ms.push('0');
        }
        millis = ms.parse().ok()?;
        rest = &rest[1 + digits.len()..];
    }

    let offset_seconds = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            let body = &rest[1..];
            let (hh, mm) = match body.len() {
                5 if body.as_bytes()[2] == b':' => (&body[..2], &body[3..5]),
                4 => (&body[..2], &body[2..4]),
                2 => (&body[..2], "00"),
                _ => return None,
            };
            sign * (hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(seconds * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rfc3339_parses_the_forms_the_harnesses_write() {
        // The claude-code form, to the millisecond.
        assert_eq!(
            parse_rfc3339_ms("2026-01-02T03:04:05.123Z"),
            Some(1_767_323_045_123)
        );
        // No fraction, and a naive timestamp read as UTC.
        assert_eq!(
            parse_rfc3339_ms("2026-01-02T03:04:05Z"),
            Some(1_767_323_045_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-01-02T03:04:05"),
            Some(1_767_323_045_000)
        );
        // Offsets, both signs and both spellings.
        assert_eq!(
            parse_rfc3339_ms("2026-01-02T04:04:05+01:00"),
            Some(1_767_323_045_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-01-02T02:04:05-0100"),
            Some(1_767_323_045_000)
        );
        // The epoch itself, and a leap day, pin the civil-date arithmetic.
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(
            parse_rfc3339_ms("1969-12-31T23:59:59Z"),
            Some(-1000),
            "dates before the epoch stay monotonic"
        );

        for bad in [
            "",
            "not a date",
            "2026-01-02",
            "2026-13-02T00:00:00Z",
            "2026-01-02X03:04:05Z",
            "2026-01-02T03:04:05.Z",
            "2026-01-02T03:04:05~",
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn epochs_in_seconds_and_milliseconds_both_land_in_milliseconds() {
        assert_eq!(
            json_epoch_ms(&json!(1_767_323_045)),
            Some(1_767_323_045_000)
        );
        assert_eq!(
            json_epoch_ms(&json!(1_767_323_045_123i64)),
            Some(1_767_323_045_123)
        );
        assert_eq!(
            json_epoch_ms(&json!("2026-01-02T03:04:05Z")),
            Some(1_767_323_045_000)
        );
        assert_eq!(json_epoch_ms(&json!(null)), None);
    }

    #[test]
    fn roles_normalize_across_the_harnesses_vocabularies() {
        for (raw, expected) in [
            ("user", MessageRole::User),
            ("Human", MessageRole::User),
            ("agent_message", MessageRole::Assistant),
            ("assistant", MessageRole::Assistant),
            ("developer", MessageRole::System),
            ("tool_result", MessageRole::Tool),
        ] {
            assert_eq!(MessageRole::normalize(raw), Some(expected), "{raw}");
        }
        assert_eq!(MessageRole::normalize("wat"), None);
    }

    #[test]
    fn a_tool_call_only_message_is_marked_rather_than_silently_empty() {
        let content = json!([{"type": "tool_use", "name": "Bash", "input": {"command": "ls"}}]);
        assert_eq!(flatten_content(&content, 1024), "[tool_use: Bash]");
    }

    #[test]
    fn flattening_stops_once_it_passes_the_cap() {
        let blocks: Vec<Value> = (0..40)
            .map(|_| json!({"type": "text", "text": "x".repeat(64)}))
            .collect();
        let flat = flatten_content(&Value::Array(blocks), 100);
        assert!(
            flat.len() <= 100 + 65,
            "bounded by the cap plus one part: {}",
            flat.len()
        );
    }

    #[test]
    fn an_unknown_block_still_leaves_a_marker() {
        let content = json!([{"type": "image", "source": {}}, {"type": "text", "text": "hi"}]);
        assert_eq!(flatten_content(&content, 1024), "[image]\nhi");
    }

    #[test]
    fn a_redacted_thinking_block_is_marked_rather_than_yielding_an_empty_body() {
        // claude-code's shape for thinking it will not hand back. Left to fall through, this is a
        // message with no text at all, and on real history it is a fifth of the transcript.
        let content = json!([{"type": "thinking", "thinking": "", "signature": "abc"}]);
        assert_eq!(flatten_content(&content, 1024), "[thinking]");
    }
}
