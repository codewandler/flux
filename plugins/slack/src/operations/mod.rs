//! Slack operation families and their shared input, text, and datasource helpers.

use super::*;

mod files;
mod messages;
mod workspace;

pub(super) use files::*;
pub(super) use messages::*;
pub(super) use workspace::*;

pub(super) fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

pub(super) fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Percent-encode a query-parameter value (alnum + `-_.~` pass, everything else `%XX`).
pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Normalize a Slack timestamp: drop a leading `p`, trim a `?`/`#` suffix, and insert the dot for the
/// `archives` permalink form (`1718031600123456` → `1718031600.123456`).
pub(super) fn normalize_ts(ts: &str) -> String {
    let mut t = ts.trim();
    if let Some(rest) = t.strip_prefix('p') {
        if !rest.is_empty() {
            t = rest;
        }
    }
    if let Some(idx) = t.find(['?', '#']) {
        t = &t[..idx];
    }
    if !t.contains('.') && t.len() > 10 && t.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}.{}", &t[..10], &t[10..])
    } else {
        t.to_string()
    }
}

/// Parse a message reference: a permalink URL (`…/archives/<channel>/p<ts>`) or `channel:ts`.
pub(super) fn parse_ref(reference: &str) -> Option<(String, String)> {
    let r = reference.trim();
    if r.is_empty() {
        return None;
    }
    if r.contains("://") {
        if let Some(idx) = r.find("/archives/") {
            let rest = &r[idx + "/archives/".len()..];
            let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
            if parts.len() >= 2 {
                return Some((parts[0].to_string(), normalize_ts(parts[1])));
            }
        }
        return None;
    }
    if let Some((ch, ts)) = r.split_once(':') {
        let ch = ch.trim();
        let ts = normalize_ts(ts);
        if !ch.is_empty() && !ts.is_empty() {
            return Some((ch.to_string(), ts));
        }
    }
    None
}

/// Resolve `(channel, ts)` from either a `ref` input or explicit `channel`+`ts`.
pub(super) fn resolve_ref(input: &Value) -> Result<(String, String), String> {
    resolve_ref_parts(
        opt_str(input, "ref"),
        opt_str(input, "channel"),
        opt_str(input, "ts"),
    )
}

/// Resolve a message reference from already-deserialized typed input fields.
pub(super) fn resolve_ref_parts(
    reference: Option<&str>,
    channel: Option<&str>,
    ts: Option<&str>,
) -> Result<(String, String), String> {
    if let Some(r) = reference.filter(|value| !value.is_empty()) {
        if let Some(pair) = parse_ref(r) {
            return Ok(pair);
        }
    }
    let channel = channel
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let ts = ts
        .filter(|value| !value.is_empty())
        .map(normalize_ts)
        .filter(|s| !s.is_empty());
    match (channel, ts) {
        (Some(c), Some(t)) => Ok((c, t)),
        _ => Err("provide `ref` (permalink or channel:ts) or both `channel` and `ts`".into()),
    }
}

/// Resolved message-content payload: a text fallback plus optional Block Kit blocks,
/// plus the Slack message-options that control unfurling/parsing.
#[derive(Default)]
pub(super) struct MessageContent {
    text: String,
    blocks: Vec<Value>,
    unfurl_links: Option<bool>,
    unfurl_media: Option<bool>,
    parse: String,
}

/// Build a message-content payload from `text`, `markdown`, or `blocks` (mutually exclusive
/// carriers), mirroring fluxplane's `messageContent`. A `blocks` payload still requires a
/// `text` fallback string.
pub(super) fn message_content(input: &Value) -> Result<MessageContent, String> {
    let text = opt_str(input, "text")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let markdown = opt_str(input, "markdown")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let blocks = input
        .get("blocks")
        .and_then(|v| v.as_array())
        .cloned()
        .filter(|a| !a.is_empty());
    let has_blocks = blocks.is_some();
    match (text.is_some(), markdown.is_some(), has_blocks) {
        (true, true, _) => {
            return Err("exactly one of text, markdown, or blocks is required".into());
        }
        (_, true, true) => {
            return Err("blocks cannot be combined with markdown".into());
        }
        (false, false, false) => {
            return Err("exactly one of text, markdown, or blocks is required".into());
        }
        (false, false, true) => {
            return Err("text fallback is required when blocks are provided".into());
        }
        _ => {}
    }

    let mut content = MessageContent::default();
    if let Some(md) = markdown {
        content.text = md.to_string();
        content.blocks = vec![markdown_section_block(md)];
    } else if let Some(t) = text {
        content.text = t.to_string();
        if has_blocks {
            content.blocks = blocks.unwrap_or_default();
        }
    } else {
        // unreachable because of the match above
        return Err("exactly one of text, markdown, or blocks is required".into());
    }

    content.unfurl_links = input.get("unfurl_links").and_then(|v| v.as_bool());
    content.unfurl_media = input.get("unfurl_media").and_then(|v| v.as_bool());
    content.parse = opt_str(input, "parse").unwrap_or("").to_string();
    Ok(content)
}

/// True if any of a channel's searchable string fields contain `query` (case-insensitive).
pub(super) fn channel_matches_query(channel: &serde_json::Map<String, Value>, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = [
        channel.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        channel.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        channel
            .get("topic")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        channel
            .get("purpose")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    ];
    haystack
        .iter()
        .any(|s| s.to_ascii_lowercase().contains(query))
}

/// True if any of a user's searchable string fields contain `query` (case-insensitive).
pub(super) fn user_matches_query(user: &serde_json::Map<String, Value>, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = [
        user.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        user.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        user.get("profile")
            .and_then(|v| v.get("real_name"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        user.get("profile")
            .and_then(|v| v.get("display_name"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        user.get("profile")
            .and_then(|v| v.get("email"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    ];
    haystack
        .iter()
        .any(|s| s.to_ascii_lowercase().contains(query))
}

/// True if any of a bookmark's searchable string fields contain `query` (case-insensitive).
pub(super) fn bookmark_matches_query(bookmark: &Value, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = [
        bookmark.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        bookmark.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        bookmark.get("link").and_then(|v| v.as_str()).unwrap_or(""),
        bookmark.get("type").and_then(|v| v.as_str()).unwrap_or(""),
    ];
    haystack
        .iter()
        .any(|s| s.to_ascii_lowercase().contains(query))
}

/// True if any of a file record's searchable string fields contain `query` (case-insensitive).
pub(super) fn file_matches_query(file: &Value, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = [
        file.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        file.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        file.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        file.get("mimetype").and_then(|v| v.as_str()).unwrap_or(""),
        file.get("filetype").and_then(|v| v.as_str()).unwrap_or(""),
        file.get("pretty_type")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        file.get("user").and_then(|v| v.as_str()).unwrap_or(""),
    ];
    haystack
        .iter()
        .any(|s| s.to_ascii_lowercase().contains(query))
}

/// Aggregate ticket references collected from search matches into the fluxplane
/// `{key, mentions, permalinks}` shape, sorted by key then permalink.
pub(super) fn collect_search_ticket_mentions(mentions: &[Value]) -> Vec<Value> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in mentions {
        let key = m.get("key").and_then(|v| v.as_str()).unwrap_or("");
        let permalink = m.get("permalink").and_then(|v| v.as_str()).unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let entry = seen.entry(key.to_string()).or_default();
        if !permalink.is_empty() {
            entry.insert(permalink.to_string());
        }
    }
    seen.into_iter()
        .map(|(key, permalinks)| {
            let links: Vec<&String> = permalinks.iter().collect();
            json!({ "key": key, "mentions": links.len(), "permalinks": links })
        })
        .collect()
}

/// A Slack Block Kit `section` block backed by a single `mrkdwn` text object.
pub(super) fn markdown_section_block(markdown: &str) -> Value {
    json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": markdown }
    })
}

/// Text rendering mode for message reads, matching fluxplane's `textFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextFormat {
    Markdown,
    Mrkdwn,
    Both,
}

/// Parse the `text_format` enum (`markdown`/`mrkdwn`/`both`, default `markdown`).
pub(super) fn parse_text_format(raw: &str) -> TextFormat {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mrkdwn" => TextFormat::Mrkdwn,
        "both" => TextFormat::Both,
        _ => TextFormat::Markdown,
    }
}

/// Apply the requested `text_format` to a raw Slack message object in-place:
/// `markdown` returns readable Markdown, `mrkdwn` keeps raw mrkdwn, `both`
/// returns both forms as `text` and `text_mrkdwn`.
pub(super) fn render_message_text(
    message: &mut serde_json::Map<String, Value>,
    format: TextFormat,
) {
    let raw = message
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match format {
        TextFormat::Mrkdwn => {
            message.insert("text_mrkdwn".into(), Value::Null);
        }
        TextFormat::Both => {
            message.insert("text".into(), json!(mrkdwn_to_markdown(&raw)));
            message.insert("text_mrkdwn".into(), json!(raw));
        }
        TextFormat::Markdown => {
            message.insert("text".into(), json!(mrkdwn_to_markdown(&raw)));
            message.insert("text_mrkdwn".into(), Value::Null);
        }
    }
}

/// Decode a checked Slack response into its typed stable envelope while retaining open fields.
pub(super) fn decode_response<T: serde::de::DeserializeOwned>(
    operation: &str,
    value: Value,
) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("decode `{operation}` response envelope: {error}"))
}

/// Best-effort Slack mrkdwn → Markdown renderer. Links, mentions, channels, and
/// subteam/special broadcasts are translated; bold/italic/strike and HTML
/// entity decoding are applied outside code spans.
pub(super) fn mrkdwn_to_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Code spans/fences are preserved verbatim.
        if text[i..].starts_with("```") {
            if let Some(end) = text[i + 3..].find("```") {
                out.push_str(&text[i..i + 3 + end + 3]);
                i += 3 + end + 3;
                continue;
            }
        }
        if bytes[i] == b'`' {
            if let Some(end) = text[i + 1..].find('`') {
                out.push_str(&text[i..i + 1 + end + 1]);
                i += 1 + end + 1;
                continue;
            }
        }
        // mrkdwr links / mentions / channels / subteams on one scan.
        if bytes[i] == b'<' {
            if let Some(j) = text[i + 1..].find('>') {
                let inner = &text[i + 1..i + 1 + j];
                if let Some((left, right)) = inner.split_once('|') {
                    if left.starts_with("https://") || left.starts_with("http://") {
                        out.push_str(&format!("[{right}]({left})"));
                    } else if left.starts_with('@')
                        || left.starts_with('#')
                        || left.starts_with('!')
                    {
                        out.push_str(&format!("@{right}"));
                    } else {
                        out.push_str(&format!("<{inner}>"));
                    }
                } else if inner.starts_with("https://")
                    || inner.starts_with("http://")
                    || inner.starts_with('@')
                    || inner.starts_with('#')
                    || inner.starts_with('!')
                {
                    out.push_str(inner);
                } else {
                    out.push_str(&format!("<{inner}>"));
                }
                i += 1 + j + 1;
                continue;
            }
        }
        // Emphasis outside code spans.
        if bytes[i] == b'*' {
            if let Some(j) = text[i + 1..].find('*') {
                let inner = &text[i + 1..i + 1 + j];
                if !inner.contains('\n') && !inner.is_empty() {
                    out.push_str("**");
                    out.push_str(inner);
                    out.push_str("**");
                    i += 1 + j + 1;
                    continue;
                }
            }
        }
        if bytes[i] == b'~' {
            if let Some(j) = text[i + 1..].find('~') {
                let inner = &text[i + 1..i + 1 + j];
                if !inner.contains('\n') && !inner.is_empty() {
                    out.push_str("~~");
                    out.push_str(inner);
                    out.push_str("~~");
                    i += 1 + j + 1;
                    continue;
                }
            }
        }
        if bytes[i] == b'_' {
            if let Some(j) = text[i + 1..].find('_') {
                let inner = &text[i + 1..i + 1 + j];
                if !inner.contains('\n') && !inner.is_empty() {
                    out.push('*');
                    out.push_str(inner);
                    out.push('*');
                    i += 1 + j + 1;
                    continue;
                }
            }
        }
        // HTML entities.
        if text[i..].starts_with("&lt;") {
            out.push('<');
            i += 4;
            continue;
        }
        if text[i..].starts_with("&gt;") {
            out.push('>');
            i += 4;
            continue;
        }
        if text[i..].starts_with("&amp;") {
            out.push('&');
            i += 5;
            continue;
        }
        // Copy the current char verbatim — CHAR-wise, not byte-wise: pushing `bytes[i] as char`
        // mangled every multi-byte char into mojibake and left `i` mid-sequence, so the next
        // `text[i..]` slice panicked on a non-boundary (D-127). Every other branch advances by
        // whole tokens found at char boundaries, so `i` stays a boundary from here on.
        let ch = text[i..].chars().next().expect("i is on a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ---------------------------------------------------------------------------
// datasource contribution
// ---------------------------------------------------------------------------

/// Contribute `slack.channel` records from a `conversations.list` reply; returns the number indexed.
pub(super) fn contribute_channels(host: &mut Host, v: &Value) -> usize {
    let Some(arr) = v.get("channels").and_then(|c| c.as_array()) else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|c| {
            let id = c.get("id").and_then(|x| x.as_str())?;
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or(id);
            let body = c
                .get("topic")
                .and_then(|t| t.get("value"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(name);
            Some(Record::new(
                Source::new("slack"),
                "slack.channel",
                id,
                name,
                body,
            ))
        })
        .collect();
    if records.is_empty() {
        return 0;
    }
    host.contribute(&records).unwrap_or(0)
}

/// Contribute `slack.user` records from a `users.list` reply; returns the number indexed.
pub(super) fn contribute_users(host: &mut Host, v: &Value) -> usize {
    let Some(arr) = v.get("members").and_then(|m| m.as_array()) else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|u| {
            let id = u.get("id").and_then(|x| x.as_str())?;
            let name = u.get("name").and_then(|x| x.as_str()).unwrap_or(id);
            let body = u
                .get("profile")
                .and_then(|p| p.get("real_name"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(name);
            Some(Record::new(
                Source::new("slack"),
                "slack.user",
                id,
                name,
                body,
            ))
        })
        .collect();
    if records.is_empty() {
        return 0;
    }
    host.contribute(&records).unwrap_or(0)
}
