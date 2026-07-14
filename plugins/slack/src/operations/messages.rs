//! Authentication, messaging, search, mention, and unread operations.

use super::*;

// ---------------------------------------------------------------------------
// auth / identity
// ---------------------------------------------------------------------------

pub(crate) fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let mut tokens = Vec::new();
    let mut ok_count = 0;
    for (role, purpose) in [("user", "user_token"), ("bot", "bot_token")] {
        let entry = match sl_send(host, "POST", "/auth.test", Some(purpose), &json!({})) {
            Ok(v) if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) => {
                ok_count += 1;
                json!({
                    "role": role,
                    "ok": true,
                    "url": v.get("url").cloned().unwrap_or(Value::Null),
                    "team": v.get("team").cloned().unwrap_or(Value::Null),
                    "team_id": v.get("team_id").cloned().unwrap_or(Value::Null),
                    "user": v.get("user").cloned().unwrap_or(Value::Null),
                    "user_id": v.get("user_id").cloned().unwrap_or(Value::Null),
                    "bot_id": v.get("bot_id").cloned().unwrap_or(Value::Null)
                })
            }
            Ok(v) => json!({
                "role": role,
                "ok": false,
                "error": v.get("error").and_then(|e| e.as_str()).unwrap_or("auth failed")
            }),
            Err(e) => json!({ "role": role, "ok": false, "error": e }),
        };
        tokens.push(entry);
    }
    let status = match ok_count {
        2 => "ok",
        1 => "degraded",
        _ => "failed",
    };
    Ok(json!({ "status": status, "count": tokens.len(), "tokens": tokens }))
}

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

pub(crate) fn message_send(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let content = message_content(&input)?;
    let mut body = json!({ "channel": channel, "text": content.text });
    if !content.blocks.is_empty() {
        body["blocks"] = json!(content.blocks);
    }
    if let Some(ts) = opt_str(&input, "thread_ts") {
        body["thread_ts"] = json!(ts);
    }
    if input
        .get("reply_broadcast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        body["reply_broadcast"] = json!(true);
    }
    if let Some(v) = content.unfurl_links {
        body["unfurl_links"] = json!(v);
    }
    if let Some(v) = content.unfurl_media {
        body["unfurl_media"] = json!(v);
    }
    if !content.parse.is_empty() {
        body["parse"] = json!(content.parse);
    }
    check_ok(sl_send(
        host,
        "POST",
        "/chat.postMessage",
        Some("bot_token"),
        &body,
    )?)
}

pub(crate) fn message_list(
    input: MessageListInput,
    host: &mut Host,
) -> Result<MessageListOutput, String> {
    let MessageListInput {
        channel,
        limit,
        cursor,
        oldest,
        latest,
        text_format,
    } = input;
    if channel.is_empty() {
        return Err("`channel` (string) required".into());
    }
    let limit = limit.unwrap_or(50);
    let mut path = format!(
        "/conversations.history?channel={}&limit={limit}&inclusive=true",
        urlencode(&channel),
    );
    for (key, value) in [("cursor", cursor), ("oldest", oldest), ("latest", latest)] {
        if let Some(val) = value.filter(|value| !value.is_empty()) {
            path.push_str(&format!("&{key}={}", urlencode(&val)));
        }
    }
    let format = parse_text_format(text_format.as_deref().unwrap_or(""));
    let value = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    let mut output: MessageListOutput = decode_response("slack.message.list", value)?;
    for message in &mut output.messages {
        render_message_text(&mut message.0, format);
    }
    Ok(output)
}

pub(crate) fn message_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let content = message_content(&input)?;
    let mut body = json!({ "channel": channel, "ts": ts, "text": content.text });
    if !content.blocks.is_empty() {
        body["blocks"] = json!(content.blocks);
    }
    if let Some(v) = content.unfurl_links {
        body["unfurl_links"] = json!(v);
    }
    if let Some(v) = content.unfurl_media {
        body["unfurl_media"] = json!(v);
    }
    if !content.parse.is_empty() {
        body["parse"] = json!(content.parse);
    }
    check_ok(sl_send(
        host,
        "POST",
        "/chat.update",
        Some("bot_token"),
        &body,
    )?)
}

pub(crate) fn message_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let body = json!({ "channel": channel, "ts": ts });
    check_ok(sl_send(
        host,
        "POST",
        "/chat.delete",
        Some("bot_token"),
        &body,
    )?)
}

pub(crate) fn thread(input: ThreadInput, host: &mut Host) -> Result<ThreadOutput, String> {
    let (channel, ts) = resolve_ref_parts(
        input.r#ref.as_deref(),
        input.channel.as_deref(),
        input.ts.as_deref(),
    )?;
    let limit = input.limit.unwrap_or(100);
    // max_bytes gates per-image downloads in fluxplane; this handler still
    // surfaces the raw message envelope, but records the cap for callers.
    let _max_bytes = input.max_bytes.unwrap_or(10_485_760);
    let path = format!(
        "/conversations.replies?channel={}&ts={}&limit={limit}&inclusive=true",
        urlencode(&channel),
        urlencode(&ts),
    );
    let format = parse_text_format(input.text_format.as_deref().unwrap_or(""));
    let value = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    let mut output: ThreadOutput = decode_response("slack.thread", value)?;
    for message in &mut output.messages {
        render_message_text(&mut message.0, format);
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// search / mentions / unreads (user token)
// ---------------------------------------------------------------------------

pub(crate) fn search(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = req_str(&input, "query")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
    let path = format!("/search.messages?query={}&count={limit}", urlencode(query));
    let mut v = check_ok(sl_get(host, &path, Some("user_token"))?)?;
    let want_tickets = input
        .get("tickets")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let keys = ticket_keys(&input);
    if want_tickets {
        let mut mentions = Vec::new();
        if let Some(matches) = v
            .get_mut("messages")
            .and_then(|m| m.get_mut("matches"))
            .and_then(|m| m.as_array_mut())
        {
            for m in matches.iter_mut() {
                let text = m.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let tix = extract_tickets(text, &keys);
                if !tix.is_empty() {
                    m["tickets"] = json!(tix);
                }
                let permalink = m.get("permalink").and_then(|p| p.as_str()).unwrap_or("");
                for ticket in &tix {
                    mentions.push(json!({
                        "key": ticket,
                        "permalink": permalink,
                    }));
                }
            }
        }
        v["tickets"] = json!(collect_search_ticket_mentions(&mentions));
    }
    Ok(v)
}

pub(crate) fn mentions(input: Value, host: &mut Host) -> Result<Value, String> {
    let search_bot = input.get("bot").and_then(|v| v.as_bool()).unwrap_or(false);
    let target = match opt_str(&input, "user") {
        Some(u) => u.to_string(),
        None => {
            // Fall back to the requested token identity (user by default, bot if `bot: true`).
            let purpose = if search_bot {
                "bot_token"
            } else {
                "user_token"
            };
            let me = check_ok(sl_send(
                host,
                "POST",
                "/auth.test",
                Some(purpose),
                &json!({}),
            )?)?;
            me.get("user_id")
                .and_then(|v| v.as_str())
                .ok_or("no `user` given and could not resolve the token identity")?
                .to_string()
        }
    };
    let raw_since = opt_str(&input, "since").unwrap_or("").to_string();
    // `since` for mentions: empty means today's (UTC) midnight, else `now - duration`.
    // Returns the unix lower bound (for client-side filtering) and the `after:` search
    // term (`since - 1 day` as `YYYY-MM-DD`) — mirrors fluxplane's `mentionSince`.
    let (since_unix, after_query) = mention_since(&raw_since)?;
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .map(|n| n.min(50))
        .unwrap_or(20);
    let unhandled = input
        .get("unhandled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let want_tickets = input
        .get("tickets")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ticket_keys = ticket_keys(&input);
    let max_thread = input
        .get("max_thread")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .map(|n| n.min(50))
        .unwrap_or(50);
    let mut query = format!("<@{target}>");
    if !after_query.is_empty() {
        query.push_str(&format!(" after:{after_query}"));
    }
    let path = format!("/search.messages?query={}&count={limit}", urlencode(&query));
    let v = check_ok(sl_get(host, &path, Some("user_token"))?)?;
    let messages = v.get("messages").cloned().unwrap_or(Value::Null);
    let total = messages
        .get("total")
        .and_then(|t| t.as_i64())
        .unwrap_or_default();
    let matches = messages
        .get("matches")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    // Resolve the token identities once so each match can be classified by whether
    // *we* replied to or reacted on the mention.
    let own = own_user_ids(host);
    let mut mentions = Vec::with_capacity(matches.len());
    for m in &matches {
        let ts = m
            .get("ts")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        // Drop matches older than the `since` boundary (the `after:` search term is
        // day-granular, so a precise unix filter trims the same-day remainder).
        if since_unix > 0 && slack_ts_unix(&ts) < since_unix {
            continue;
        }
        let channel = search_match_channel(m);
        let permalink = m.get("permalink").and_then(|p| p.as_str()).unwrap_or("");
        let thread_ts = extract_thread_ts(permalink);
        let user = m
            .get("user")
            .and_then(|u| u.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| m.get("username").and_then(|u| u.as_str()))
            .unwrap_or_default();
        let (status, files) = classify_mention(host, &channel, &ts, &thread_ts, &own, max_thread)?;
        if unhandled && status != "pending" {
            continue;
        }
        let text = m.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
        let mut item = json!({
            "channel": channel,
            "ts": ts,
            "thread_ts": thread_ts,
            "user": user,
            "text": text,
            "permalink": permalink,
            "status": status,
            "files": files,
        });
        if want_tickets {
            item["tickets"] = json!(extract_tickets(text, &ticket_keys));
        }
        mentions.push(item);
    }
    // Aggregate ticket references across the surfaced mentions: `{key, mentions, permalinks}`,
    // sorted by key then permalink, mirroring fluxplane's `collectTicketMentionsFromMentions`.
    let tickets = collect_ticket_mentions(&mentions);
    Ok(json!({
        "target": target,
        "since": raw_since,
        "count": mentions.len(),
        "total": total,
        "unhandled": unhandled,
        "mentions": mentions,
        "tickets": tickets,
    }))
}

/// Parse the mentions `since` window into `(unix_lower_bound, after_search_term)`.
/// Empty `raw` means today's UTC midnight; otherwise `now - duration`. The search term is
/// `since - 1 day` formatted `YYYY-MM-DD` (Slack's `after:` is exclusive & day-granular).
/// Mirrors fluxplane's `mentionSince` (UTC here, as no timezone dep is available).
pub(crate) fn mention_since(raw: &str) -> Result<(i64, String), String> {
    let raw = raw.trim();
    let now = unix_now();
    let since = if raw.is_empty() {
        now - now.rem_euclid(86_400) // floor to UTC midnight
    } else {
        now - parse_slack_duration(raw)?
    };
    let after = civil_date(since - 86_400);
    Ok((since, after))
}

/// Parse the unreads `since` window into `(unix_lower_bound, echoed_label)`.
/// Empty `raw` defaults to `14d`. Mirrors fluxplane's `unreadSince`.
pub(crate) fn unread_since(raw: &str) -> Result<(i64, String), String> {
    let raw = raw.trim();
    let label = if raw.is_empty() { "14d" } else { raw };
    let since = unix_now() - parse_slack_duration(label)?;
    Ok((since, label.to_string()))
}

/// Parse a Slack-style duration (`1h`, `30m`, `45s`, or `Nd` days) into seconds.
/// `Nd` is days×24h, matching fluxplane's `parseSlackDuration`.
pub(crate) fn parse_slack_duration(raw: &str) -> Result<i64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(0);
    }
    let bad = || format!("invalid since duration {raw:?}");
    let (num, mult) = if let Some(days) = raw.strip_suffix('d') {
        (days, 86_400)
    } else if let Some(hours) = raw.strip_suffix('h') {
        (hours, 3_600)
    } else if let Some(mins) = raw.strip_suffix('m') {
        (mins, 60)
    } else if let Some(secs) = raw.strip_suffix('s') {
        (secs, 1)
    } else {
        return Err(bad());
    };
    let value: f64 = num.trim().parse().map_err(|_| bad())?;
    if !value.is_finite() || value < 0.0 {
        return Err(bad());
    }
    Ok((value * mult as f64) as i64)
}

/// Seconds since the Unix epoch (UTC).
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// The integer-second unix value of a Slack `ts` (`1718031600.123456` → `1718031600`),
/// normalizing the permalink forms first. Mirrors fluxplane's `slackTimestampUnix`.
pub(crate) fn slack_ts_unix(ts: &str) -> i64 {
    let t = normalize_ts(ts);
    let secs = t.split_once('.').map(|(s, _)| s).unwrap_or(&t);
    secs.trim().parse().unwrap_or(0)
}

/// Format a unix second as a `YYYY-MM-DD` (UTC) civil date — pure arithmetic (no TZ dep),
/// using Howard Hinnant's days-from-civil inverse.
pub(crate) fn civil_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

/// The cleaned, uppercased `ticket_keys` input (empty when absent).
pub(crate) fn ticket_keys(input: &Value) -> Vec<String> {
    input
        .get("ticket_keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Extract issue keys (`PROJ-123`) from `text`, deduped + sorted + uppercased. With no `keys`,
/// matches the case-sensitive `\b[A-Z][A-Z0-9]+-\d+\b`; with keys, only those project prefixes,
/// matched case-insensitively (`(?i)`). Mirrors fluxplane's `extractTickets` without a regex dep.
pub(crate) fn extract_tickets(text: &str, keys: &[String]) -> Vec<String> {
    let keyed = !keys.is_empty();
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // A candidate starts on a word boundary at a letter. The default (no-keys) rule is
        // case-sensitive (uppercase only); the keyed rule is case-insensitive, so allow either.
        let boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        let starts = if keyed {
            bytes[i].is_ascii_alphabetic()
        } else {
            bytes[i].is_ascii_uppercase()
        };
        if boundary && starts {
            // Prefix: a leading letter then letters/digits (uppercase-only for the default rule).
            let prefix_start = i;
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit()
                    || if keyed {
                        bytes[j].is_ascii_alphabetic()
                    } else {
                        bytes[j].is_ascii_uppercase()
                    })
            {
                j += 1;
            }
            // Require at least two prefix chars and a `-<digits>` suffix on a word boundary.
            if j > prefix_start + 1 && j < bytes.len() && bytes[j] == b'-' {
                let digits_start = j + 1;
                let mut k = digits_start;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                let trailing_boundary = k == bytes.len() || !is_word_byte(bytes[k]);
                if k > digits_start && trailing_boundary {
                    let prefix = &text[prefix_start..j];
                    if !keyed || keys.iter().any(|p| p.eq_ignore_ascii_case(prefix)) {
                        let key = text[prefix_start..k].to_ascii_uppercase();
                        if !out.contains(&key) {
                            out.push(key);
                        }
                    }
                    i = k;
                    continue;
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out.sort();
    out
}

/// True for ASCII word bytes (`[A-Za-z0-9_]`) — the boundary class used by ticket extraction.
pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Aggregate per-mention `tickets` into `[{key, mentions, permalinks}]`, sorted by key then
/// permalink. `mentions` is the count of distinct permalinks. Mirrors fluxplane's
/// `collectTicketMentionsFromMentions` + `ticketMentionRecords`.
pub(crate) fn collect_ticket_mentions(mentions: &[Value]) -> Vec<Value> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in mentions {
        let permalink = m.get("permalink").and_then(|p| p.as_str()).unwrap_or("");
        if let Some(tickets) = m.get("tickets").and_then(|t| t.as_array()) {
            for ticket in tickets.iter().filter_map(|t| t.as_str()) {
                let entry = seen.entry(ticket.to_string()).or_default();
                if !permalink.is_empty() {
                    entry.insert(permalink.to_string());
                }
            }
        }
    }
    seen.into_iter()
        .map(|(key, permalinks)| {
            let links: Vec<&String> = permalinks.iter().collect();
            json!({ "key": key, "mentions": links.len(), "permalinks": links })
        })
        .collect()
}

/// The Slack user IDs behind our two tokens — both the user token and the bot token identities,
/// used by [`classify_mention`] to decide whether *we* have already handled a mention.
pub(crate) fn own_user_ids(host: &mut Host) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for purpose in ["user_token", "bot_token"] {
        if let Ok(v) = sl_send(host, "POST", "/auth.test", Some(purpose), &json!({})) {
            if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
                if let Some(uid) = v.get("user_id").and_then(|u| u.as_str()) {
                    let uid = uid.trim();
                    if !uid.is_empty() {
                        ids.insert(uid.to_string());
                    }
                }
            }
        }
    }
    ids
}

/// The `thread_ts` query parameter of a Slack permalink, normalized — empty if the permalink carries
/// no thread (i.e. the message is a channel root, not a reply).
pub(crate) fn extract_thread_ts(permalink: &str) -> String {
    let q = match permalink.split_once('?') {
        Some((_, q)) => q,
        None => return String::new(),
    };
    for pair in q.split('&') {
        if let Some(val) = pair.strip_prefix("thread_ts=") {
            let decoded = val.replace("%2E", ".").replace("%2e", ".");
            return normalize_ts(&decoded);
        }
    }
    String::new()
}

/// The channel id of a `search.messages` match (Slack nests it under `channel.id`).
pub(crate) fn search_match_channel(m: &Value) -> String {
    m.get("channel")
        .and_then(|c| c.get("id"))
        .and_then(|id| id.as_str())
        .or_else(|| m.get("channel").and_then(|c| c.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// Classify a mention's handling status by walking its thread: `replied` if we authored the matched
/// reply or any later reply, `acked` if we reacted on the matched reply, else `pending`. Also returns
/// the files attached to the matched reply. Mirrors fluxplane's `classifyMention`.
pub(crate) fn classify_mention(
    host: &mut Host,
    channel: &str,
    ts: &str,
    thread_ts: &str,
    own: &std::collections::HashSet<String>,
    max_thread: i64,
) -> Result<(&'static str, Value), String> {
    let root_ts = if thread_ts.is_empty() { ts } else { thread_ts };
    if root_ts.is_empty() || channel.is_empty() {
        return Ok(("pending", json!([])));
    }
    let path = format!(
        "/conversations.replies?channel={}&ts={}&limit={max_thread}&inclusive=true",
        urlencode(channel),
        urlencode(root_ts),
    );
    let thread = check_ok(sl_get(host, &path, Some("user_token"))?)?;
    let replies = thread
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if replies.is_empty() {
        return Ok(("pending", json!([])));
    }
    let mut files = json!([]);
    for (index, reply) in replies.iter().enumerate() {
        let reply_user = reply
            .get("user")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .trim();
        if reply.get("ts").and_then(|t| t.as_str()) == Some(ts) {
            files = reply.get("files").cloned().unwrap_or_else(|| json!([]));
            if own.contains(reply_user) {
                return Ok(("replied", files));
            }
            if let Some(reactions) = reply.get("reactions").and_then(|r| r.as_array()) {
                for reaction in reactions {
                    if let Some(users) = reaction.get("users").and_then(|u| u.as_array()) {
                        if users
                            .iter()
                            .filter_map(|u| u.as_str())
                            .any(|u| own.contains(u.trim()))
                        {
                            return Ok(("acked", files));
                        }
                    }
                }
            }
        }
        if index > 0 && own.contains(reply_user) {
            return Ok(("replied", files));
        }
    }
    Ok(("pending", files))
}

pub(crate) fn unreads(input: Value, host: &mut Host) -> Result<Value, String> {
    let filter = opt_str(&input, "channel");
    // `since` for unreads: empty defaults to `14d`; a positive lower bound raises the
    // history `oldest` floor (never below the `last_read` cursor). Mirrors `unreadSince`.
    let (since_unix, since_label) = unread_since(opt_str(&input, "since").unwrap_or(""))?;
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .map(|n| n.min(100))
        .unwrap_or(50);
    let channel_cap = if filter.is_some() { 200 } else { 50 };

    let mut channels = Vec::new();
    let mut cursor = String::new();
    while channels.len() < channel_cap {
        let mut list_path = String::from(
            "/users.conversations?types=public_channel,private_channel,mpim,im&exclude_archived=true&limit=200",
        );
        if !cursor.is_empty() {
            list_path.push_str(&format!("&cursor={}", urlencode(&cursor)));
        }
        let listed = check_ok(sl_get(host, &list_path, Some("user_token"))?)?;
        let page = listed
            .get("channels")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        for ch in page {
            channels.push(ch);
            if channels.len() >= channel_cap {
                break;
            }
        }
        let next_cursor = listed
            .get("response_metadata")
            .and_then(|m| m.get("next_cursor"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if next_cursor.is_empty() || next_cursor == cursor {
            break;
        }
        cursor = next_cursor;
    }

    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for ch in channels.iter() {
        let Some(id) = ch.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(f) = filter {
            let name_match = ch
                .get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.eq_ignore_ascii_case(f))
                .unwrap_or(false);
            if !id.eq_ignore_ascii_case(f) && !name_match {
                continue;
            }
        }
        let latest = ch
            .get("latest")
            .and_then(|l| l.get("ts"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(last_read) = ch
            .get("last_read")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            skipped.push(json!({
                "id": id,
                "reason": "missing_last_read",
                "latest": latest,
            }));
            continue;
        };
        if latest.as_ref().is_some_and(|ts| ts <= &last_read) {
            continue;
        }
        // Raise the `oldest` floor to the `since` window when it is newer than the
        // `last_read` cursor (string compare is safe: both are fixed-form Slack ts).
        let oldest = if since_unix > 0 {
            let since_ts = format!("{since_unix}.000000");
            if since_ts > last_read {
                since_ts
            } else {
                last_read.clone()
            }
        } else {
            last_read.clone()
        };
        let hist_path = format!(
            "/conversations.history?channel={}&oldest={}&limit={limit}&inclusive=false",
            urlencode(id),
            urlencode(&oldest),
        );
        let hist = check_ok(sl_get(host, &hist_path, Some("user_token"))?)?;
        let raw = hist
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        if raw.is_empty() {
            continue;
        }
        // Slack returns history newest-first; reverse to chronological order.
        let msgs: Vec<Value> = raw.into_iter().rev().collect();
        let name = if ch.get("is_im").and_then(|v| v.as_bool()).unwrap_or(false) {
            ch.get("user").and_then(|v| v.as_str()).unwrap_or(id)
        } else {
            ch.get("name").and_then(|v| v.as_str()).unwrap_or(id)
        };
        out.push(json!({
            "id": id,
            "name": name,
            "is_private": ch.get("is_private").and_then(|v| v.as_bool()).unwrap_or(false),
            "is_dm": ch.get("is_im").and_then(|v| v.as_bool()).unwrap_or(false),
            "unread_count": msgs.len(),
            "last_read": last_read,
            "messages": msgs,
        }));
    }
    Ok(json!({
        "since": since_label,
        "count": out.len(),
        "channels": out,
        "skipped": skipped,
        "scanned": channels.len(),
    }))
}
