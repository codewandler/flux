//! `slack` — a flux integration plugin for the Slack Web API: token info, messaging, threads, search,
//! reactions, channels, files (via host blobs), bookmarks, users, presence, and emoji. Authenticates
//! with tokens injected as bearer headers — purpose `bot_token` for most calls, `user_token` for the
//! search-scoped reads (search/mentions/unreads) and presence writes. The base URL is the
//! `slack.endpoint` (defaults to `https://slack.com/api`). List ops contribute datasource records
//! (`slack.channel` / `slack.user`) so the agent can search them; `slack.index.build` rebuilds both.
//!
//! Slack replies are JSON carrying an `"ok": bool`; a falsey `ok` is surfaced as an error built from the
//! response's `"error"` field. File ops never inline base64 — `slack.file.upload` reads its bytes from a
//! host `blob_ref`, and the download ops stage the fetched bytes back into the blob store, returning a ref.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("slack", "0.1.0")
        .capabilities(Caps {
            http: true,
            blob: true,
            secrets: vec!["SLACK_BOT_TOKEN".into(), "SLACK_USER_TOKEN".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "bot_token".into(),
            env: vec!["SLACK_BOT_TOKEN".into()],
            description: "Slack bot token (xoxb-…) for posting/reading via the bot.".into(),
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "user_token".into(),
            env: vec!["SLACK_USER_TOKEN".into()],
            description: "Slack user token (xoxp-…) for search/mentions/unreads and presence.".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "slack.endpoint".into(),
            env: vec!["SLACK_API_URL".into()],
            description: "Slack Web API base URL (default https://slack.com/api)".into(),
        })
        .datasource(ds("slack.channels", "slack.channel", "Slack channels."))
        .datasource(ds("slack.users", "slack.user", "Slack workspace users."))
        // -- auth / identity ------------------------------------------------
        .operation(
            read_op(
                "slack.test",
                "Test Slack user and bot token authentication.",
                json!({"type": "object", "properties": {}}),
            ),
            auth_test,
        )
        .operation(
            read_op(
                "slack.info",
                "Show Slack token identity and workspace information.",
                json!({"type": "object", "properties": {}}),
            ),
            auth_test,
        )
        // -- messages -------------------------------------------------------
        .operation(
            write_op(
                "slack.message.send",
                "Send a message to a channel (channel id or DM channel; optionally as a thread reply).",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "text": {"type": "string"},
                    "thread_ts": {"type": "string", "description": "reply in this thread"},
                    "reply_broadcast": {"type": "boolean"}
                }, "required": ["channel", "text"]}),
            ),
            message_send,
        )
        .operation(
            read_op(
                "slack.message.list",
                "Read recent messages from a channel (conversations.history); paginate with next_cursor.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "limit": {"type": "integer", "description": "max messages (default 50)"},
                    "cursor": {"type": "string"},
                    "oldest": {"type": "string"},
                    "latest": {"type": "string"}
                }, "required": ["channel"]}),
            ),
            message_list,
        )
        .operation(
            write_op(
                "slack.message.edit",
                "Edit a Slack message. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
                json!({"type": "object", "properties": {
                    "ref": {"type": "string"},
                    "channel": {"type": "string"},
                    "ts": {"type": "string"},
                    "text": {"type": "string"}
                }, "required": ["text"]}),
            ),
            message_edit,
        )
        .operation(
            write_op(
                "slack.message.delete",
                "Delete a Slack message. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
                json!({"type": "object", "properties": {
                    "ref": {"type": "string"},
                    "channel": {"type": "string"},
                    "ts": {"type": "string"}
                }}),
            ),
            message_delete,
        )
        .operation(
            read_op(
                "slack.thread",
                "View a Slack thread. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
                json!({"type": "object", "properties": {
                    "ref": {"type": "string"},
                    "channel": {"type": "string"},
                    "ts": {"type": "string", "description": "parent message timestamp"},
                    "limit": {"type": "integer"}
                }}),
            ),
            thread,
        )
        // -- search / mentions / unreads (user token) -----------------------
        .operation(
            read_op(
                "slack.search",
                "Search Slack messages (search.messages; requires a user token).",
                json!({"type": "object", "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                }, "required": ["query"]}),
            ),
            search,
        )
        .operation(
            read_op(
                "slack.mentions",
                "Search Slack mentions of a user and classify whether each was handled (search.messages \
                 + per-mention thread inspection; requires a user token).",
                json!({"type": "object", "properties": {
                    "user": {"type": "string", "description": "user id (U…/W…); defaults to the token identity"},
                    "since": {"type": "string", "description": "time window such as 1h, 7d, or 14d; empty means today"},
                    "limit": {"type": "integer"},
                    "unhandled": {"type": "boolean", "description": "only return pending (unhandled) mentions"},
                    "max_thread": {"type": "integer", "description": "max thread messages inspected for status classification (default 50)"},
                    "tickets": {"type": "boolean", "description": "extract ticket references from mention text"},
                    "ticket_keys": {"type": "array", "items": {"type": "string"}, "description": "optional ticket project keys to extract, e.g. DEV or TEL; empty extracts uppercase issue keys"}
                }}),
            ),
            mentions,
        )
        .operation(
            read_op(
                "slack.unreads",
                "List Slack conversations with recent (unread) messages (requires a user token).",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string", "description": "optional channel id filter"},
                    "since": {"type": "string", "description": "time window such as 1h, 7d, or 14d; defaults to 14d"},
                    "limit": {"type": "integer", "description": "max messages fetched per channel"}
                }}),
            ),
            unreads,
        )
        // -- reactions ------------------------------------------------------
        .operation(
            write_op(
                "slack.reaction.add",
                "Add a reaction to a Slack message. Provide `ref` OR `channel`+`ts`, plus `emoji`.",
                json!({"type": "object", "properties": {
                    "ref": {"type": "string"},
                    "channel": {"type": "string"},
                    "ts": {"type": "string"},
                    "emoji": {"type": "string", "description": "emoji name without colons"}
                }, "required": ["emoji"]}),
            ),
            reaction_add,
        )
        .operation(
            write_op(
                "slack.reaction.remove",
                "Remove a reaction from a Slack message. Provide `ref` OR `channel`+`ts`, plus `emoji`.",
                json!({"type": "object", "properties": {
                    "ref": {"type": "string"},
                    "channel": {"type": "string"},
                    "ts": {"type": "string"},
                    "emoji": {"type": "string", "description": "emoji name without colons"}
                }, "required": ["emoji"]}),
            ),
            reaction_remove,
        )
        // -- channels -------------------------------------------------------
        .operation(
            read_op(
                "slack.channel.list",
                "List public and private channels (plus group/direct conversations) in the workspace.",
                json!({"type": "object", "properties": {}}),
            ),
            channel_list,
        )
        .operation(
            write_op(
                "slack.channel.join",
                "Join a Slack public channel.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"}
                }, "required": ["channel"]}),
            ),
            channel_join,
        )
        .operation(
            write_op(
                "slack.channel.mark-read",
                "Mark a Slack channel read through a timestamp. Provide `ref` OR `channel`+`ts`.",
                json!({"type": "object", "properties": {
                    "ref": {"type": "string"},
                    "channel": {"type": "string"},
                    "ts": {"type": "string"}
                }}),
            ),
            channel_mark,
        )
        // -- files (blobs) --------------------------------------------------
        .operation(
            write_op(
                "slack.file.upload",
                "Upload a file to a Slack channel, DM, or thread. Bytes come from a host `blob_ref`.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "blob_ref": {"type": "string", "description": "host blob ref holding the file bytes"},
                    "filename": {"type": "string"},
                    "thread_ts": {"type": "string"},
                    "initial_comment": {"type": "string"},
                    "alt_text": {"type": "string"}
                }, "required": ["channel", "blob_ref"]}),
            ),
            file_upload,
        )
        .operation(
            write_op(
                "slack.file.download",
                "Download a Slack file to a host blob; returns the `blob_ref`.",
                json!({"type": "object", "properties": {
                    "file_id": {"type": "string"},
                    "filename": {"type": "string"}
                }, "required": ["file_id"]}),
            ),
            file_download,
        )
        .operation(
            write_op(
                "slack.download",
                "Download a Slack file to a host blob; returns the `blob_ref`.",
                json!({"type": "object", "properties": {
                    "file_id": {"type": "string"},
                    "filename": {"type": "string"}
                }, "required": ["file_id"]}),
            ),
            file_download,
        )
        .operation(
            read_op(
                "slack.file.info",
                "Show Slack file information.",
                json!({"type": "object", "properties": {
                    "file_id": {"type": "string"}
                }, "required": ["file_id"]}),
            ),
            file_info,
        )
        .operation(
            read_op(
                "slack.file.list",
                "List Slack files (optionally filtered by channel/user/type).",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "user": {"type": "string"},
                    "types": {"type": "string"},
                    "limit": {"type": "integer"}
                }}),
            ),
            file_list,
        )
        .operation(
            write_op(
                "slack.file.delete",
                "Delete a Slack file.",
                json!({"type": "object", "properties": {
                    "file_id": {"type": "string"}
                }, "required": ["file_id"]}),
            ),
            file_delete,
        )
        // -- bookmarks ------------------------------------------------------
        .operation(
            write_op(
                "slack.bookmark.add",
                "Add a Slack channel bookmark.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "title": {"type": "string"},
                    "link": {"type": "string"},
                    "emoji": {"type": "string"}
                }, "required": ["channel", "title", "link"]}),
            ),
            bookmark_add,
        )
        .operation(
            write_op(
                "slack.bookmark.edit",
                "Edit a Slack channel bookmark.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "bookmark_id": {"type": "string"},
                    "title": {"type": "string"},
                    "link": {"type": "string"},
                    "emoji": {"type": "string"}
                }, "required": ["channel", "bookmark_id"]}),
            ),
            bookmark_edit,
        )
        .operation(
            write_op(
                "slack.bookmark.delete",
                "Delete a Slack channel bookmark.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "bookmark_id": {"type": "string"}
                }, "required": ["channel", "bookmark_id"]}),
            ),
            bookmark_delete,
        )
        .operation(
            read_op(
                "slack.bookmark.list",
                "List Slack channel bookmarks.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"}
                }, "required": ["channel"]}),
            ),
            bookmark_list,
        )
        // -- users / presence / emoji ---------------------------------------
        .operation(
            read_op(
                "slack.user.list",
                "List users in the workspace.",
                json!({"type": "object", "properties": {}}),
            ),
            user_list,
        )
        .operation(
            read_op(
                "slack.presence.get",
                "Get Slack user presence.",
                json!({"type": "object", "properties": {
                    "user": {"type": "string", "description": "user id; empty asks for the token identity"}
                }}),
            ),
            presence_get,
        )
        .operation(
            write_op(
                "slack.presence.set",
                "Set Slack user presence (auto|away; requires a user token).",
                json!({"type": "object", "properties": {
                    "presence": {"type": "string", "enum": ["auto", "away"]}
                }, "required": ["presence"]}),
            ),
            presence_set,
        )
        .operation(
            read_op(
                "slack.emoji.list",
                "List Slack custom emoji.",
                json!({"type": "object", "properties": {}}),
            ),
            emoji_list,
        )
        // -- index ----------------------------------------------------------
        .operation(
            read_op(
                "slack.index.build",
                "Build the Slack channel and user reverse-lookup indexes.",
                json!({"type": "object", "properties": {}}),
            ),
            index_build,
        )
}

/// A contributing datasource: searchable, gettable, and feedable by `slack.index.build`.
fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

/// Resolve the Slack API base URL (config), defaulting to the public endpoint; trailing slash trimmed.
fn base_url(host: &mut Host) -> String {
    host.endpoint("slack.endpoint")
        .unwrap_or_else(|_| "https://slack.com/api".into())
        .trim_end_matches('/')
        .to_string()
}

/// Slack returns `{"ok": bool, …}`; treat a falsey `ok` as an error built from the `"error"` field.
fn check_ok(v: Value) -> Result<Value, String> {
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        Ok(v)
    } else {
        let err = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error");
        Err(format!("slack error: {err}"))
    }
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Percent-encode a query-parameter value (alnum + `-_.~` pass, everything else `%XX`).
fn urlencode(s: &str) -> String {
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
fn normalize_ts(ts: &str) -> String {
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
fn parse_ref(reference: &str) -> Option<(String, String)> {
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
fn resolve_ref(input: &Value) -> Result<(String, String), String> {
    if let Some(r) = opt_str(input, "ref") {
        if let Some(pair) = parse_ref(r) {
            return Ok(pair);
        }
    }
    let channel = opt_str(input, "channel").map(str::to_string);
    let ts = opt_str(input, "ts")
        .map(normalize_ts)
        .filter(|s| !s.is_empty());
    match (channel, ts) {
        (Some(c), Some(t)) => Ok((c, t)),
        _ => Err("provide `ref` (permalink or channel:ts) or both `channel` and `ts`".into()),
    }
}

// ---------------------------------------------------------------------------
// auth / identity
// ---------------------------------------------------------------------------

fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let url = format!("{}/auth.test", base_url(host));
    let mut tokens = Vec::new();
    let mut ok_count = 0;
    for (role, purpose) in [("user", "user_token"), ("bot", "bot_token")] {
        let entry = match host.send_json("POST", &url, Some(purpose), &json!({})) {
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

fn message_send(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let text = req_str(&input, "text")?;
    let mut body = json!({ "channel": channel, "text": text });
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
    let url = format!("{}/chat.postMessage", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

fn message_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
    let mut url = format!(
        "{}/conversations.history?channel={}&limit={limit}&inclusive=true",
        base_url(host),
        urlencode(channel),
    );
    for key in ["cursor", "oldest", "latest"] {
        if let Some(val) = opt_str(&input, key) {
            url.push_str(&format!("&{key}={}", urlencode(val)));
        }
    }
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

fn message_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let text = req_str(&input, "text")?;
    let body = json!({ "channel": channel, "ts": ts, "text": text });
    let url = format!("{}/chat.update", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

fn message_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let body = json!({ "channel": channel, "ts": ts });
    let url = format!("{}/chat.delete", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

fn thread(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    let url = format!(
        "{}/conversations.replies?channel={}&ts={}&limit={limit}&inclusive=true",
        base_url(host),
        urlencode(&channel),
        urlencode(&ts),
    );
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

// ---------------------------------------------------------------------------
// search / mentions / unreads (user token)
// ---------------------------------------------------------------------------

fn search(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = req_str(&input, "query")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
    let url = format!(
        "{}/search.messages?query={}&count={limit}",
        base_url(host),
        urlencode(query),
    );
    check_ok(host.get_json(&url, Some("user_token"))?)
}

fn mentions(input: Value, host: &mut Host) -> Result<Value, String> {
    let target = match opt_str(&input, "user") {
        Some(u) => u.to_string(),
        None => {
            // Fall back to the user-token identity.
            let url = format!("{}/auth.test", base_url(host));
            let me = check_ok(host.send_json("POST", &url, Some("user_token"), &json!({}))?)?;
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
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
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
        .unwrap_or(50);
    let mut query = format!("<@{target}>");
    if !after_query.is_empty() {
        query.push_str(&format!(" after:{after_query}"));
    }
    let url = format!(
        "{}/search.messages?query={}&count={limit}",
        base_url(host),
        urlencode(&query),
    );
    let v = check_ok(host.get_json(&url, Some("user_token"))?)?;
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
        let (status, files) = classify_mention(host, &channel, &ts, &thread_ts, &own, max_thread);
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
fn mention_since(raw: &str) -> Result<(i64, String), String> {
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
fn unread_since(raw: &str) -> Result<(i64, String), String> {
    let raw = raw.trim();
    let label = if raw.is_empty() { "14d" } else { raw };
    let since = unix_now() - parse_slack_duration(label)?;
    Ok((since, label.to_string()))
}

/// Parse a Slack-style duration (`1h`, `30m`, `45s`, or `Nd` days) into seconds.
/// `Nd` is days×24h, matching fluxplane's `parseSlackDuration`.
fn parse_slack_duration(raw: &str) -> Result<i64, String> {
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
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// The integer-second unix value of a Slack `ts` (`1718031600.123456` → `1718031600`),
/// normalizing the permalink forms first. Mirrors fluxplane's `slackTimestampUnix`.
fn slack_ts_unix(ts: &str) -> i64 {
    let t = normalize_ts(ts);
    let secs = t.split_once('.').map(|(s, _)| s).unwrap_or(&t);
    secs.trim().parse().unwrap_or(0)
}

/// Format a unix second as a `YYYY-MM-DD` (UTC) civil date — pure arithmetic (no TZ dep),
/// using Howard Hinnant's days-from-civil inverse.
fn civil_date(unix: i64) -> String {
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
fn ticket_keys(input: &Value) -> Vec<String> {
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
fn extract_tickets(text: &str, keys: &[String]) -> Vec<String> {
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
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Aggregate per-mention `tickets` into `[{key, mentions, permalinks}]`, sorted by key then
/// permalink. `mentions` is the count of distinct permalinks. Mirrors fluxplane's
/// `collectTicketMentionsFromMentions` + `ticketMentionRecords`.
fn collect_ticket_mentions(mentions: &[Value]) -> Vec<Value> {
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
fn own_user_ids(host: &mut Host) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    let url = format!("{}/auth.test", base_url(host));
    for purpose in ["user_token", "bot_token"] {
        if let Ok(v) = host.send_json("POST", &url, Some(purpose), &json!({})) {
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
fn extract_thread_ts(permalink: &str) -> String {
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
fn search_match_channel(m: &Value) -> String {
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
fn classify_mention(
    host: &mut Host,
    channel: &str,
    ts: &str,
    thread_ts: &str,
    own: &std::collections::HashSet<String>,
    max_thread: i64,
) -> (&'static str, Value) {
    let root_ts = if thread_ts.is_empty() { ts } else { thread_ts };
    if root_ts.is_empty() || channel.is_empty() {
        return ("pending", json!([]));
    }
    let url = format!(
        "{}/conversations.replies?channel={}&ts={}&limit={max_thread}&inclusive=true",
        base_url(host),
        urlencode(channel),
        urlencode(root_ts),
    );
    let thread = match host.get_json(&url, Some("user_token")) {
        Ok(v) if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) => v,
        _ => return ("pending", json!([])),
    };
    let replies = thread
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if replies.is_empty() {
        return ("pending", json!([]));
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
                return ("replied", files);
            }
            if let Some(reactions) = reply.get("reactions").and_then(|r| r.as_array()) {
                for reaction in reactions {
                    if let Some(users) = reaction.get("users").and_then(|u| u.as_array()) {
                        if users
                            .iter()
                            .filter_map(|u| u.as_str())
                            .any(|u| own.contains(u.trim()))
                        {
                            return ("acked", files);
                        }
                    }
                }
            }
        }
        if index > 0 && own.contains(reply_user) {
            return ("replied", files);
        }
    }
    ("pending", files)
}

fn unreads(input: Value, host: &mut Host) -> Result<Value, String> {
    let filter = opt_str(&input, "channel");
    // `since` for unreads: empty defaults to `14d`; a positive lower bound raises the
    // history `oldest` floor (never below the `last_read` cursor). Mirrors `unreadSince`.
    let (since_unix, since_label) = unread_since(opt_str(&input, "since").unwrap_or(""))?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    // Membership-scoped channel list: only conversations the user token is a member of (or open
    // DMs/MPIMs), so a `last_read` cursor is meaningful for each.
    let list_url = format!(
        "{}/users.conversations?types=public_channel,private_channel,mpim,im&exclude_archived=true&limit=200",
        base_url(host),
    );
    let listed = check_ok(host.get_json(&list_url, Some("user_token"))?)?;
    let channels = listed
        .get("channels")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
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
        // Genuine unreads only: read history strictly *after* the channel's `last_read` cursor
        // (falling back to the latest message ts, then 0). `inclusive=false` excludes the
        // already-read boundary message itself.
        let last_read = ch
            .get("last_read")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                ch.get("latest")
                    .and_then(|l| l.get("ts"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "0".to_string());
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
        let hist_url = format!(
            "{}/conversations.history?channel={}&oldest={}&limit={limit}&inclusive=false",
            base_url(host),
            urlencode(id),
            urlencode(&oldest),
        );
        let Ok(hist) = host.get_json(&hist_url, Some("user_token")) else {
            continue;
        };
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
    Ok(json!({ "since": since_label, "count": out.len(), "channels": out }))
}

// ---------------------------------------------------------------------------
// reactions
// ---------------------------------------------------------------------------

fn reaction_add(input: Value, host: &mut Host) -> Result<Value, String> {
    reaction(input, host, "reactions.add")
}

fn reaction_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    reaction(input, host, "reactions.remove")
}

fn reaction(input: Value, host: &mut Host, method: &str) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let emoji = req_str(&input, "emoji")?.trim_matches(':');
    let body = json!({ "channel": channel, "timestamp": ts, "name": emoji });
    let url = format!("{}/{method}", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

// ---------------------------------------------------------------------------
// channels
// ---------------------------------------------------------------------------

fn channel_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let url = format!(
        "{}/conversations.list?types=public_channel,private_channel,mpim,im&limit=200",
        base_url(host),
    );
    let v = check_ok(host.get_json(&url, Some("bot_token"))?)?;
    contribute_channels(host, &v);
    Ok(v)
}

fn channel_join(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let url = format!("{}/conversations.join", base_url(host));
    check_ok(host.send_json(
        "POST",
        &url,
        Some("bot_token"),
        &json!({ "channel": channel }),
    )?)
}

fn channel_mark(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let url = format!("{}/conversations.mark", base_url(host));
    check_ok(host.send_json(
        "POST",
        &url,
        Some("bot_token"),
        &json!({ "channel": channel, "ts": ts }),
    )?)
}

// ---------------------------------------------------------------------------
// files (host blobs)
// ---------------------------------------------------------------------------

fn file_upload(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?.to_string();
    let blob_ref = req_str(&input, "blob_ref")?.to_string();
    let bytes = host.blob_get(&blob_ref)?;
    let filename = opt_str(&input, "filename")
        .map(str::to_string)
        .unwrap_or_else(|| "upload.bin".into());

    // 1. Reserve an external upload URL.
    let reserve_url = format!(
        "{}/files.getUploadURLExternal?filename={}&length={}",
        base_url(host),
        urlencode(&filename),
        bytes.len(),
    );
    let reserved = check_ok(host.get_json(&reserve_url, Some("bot_token"))?)?;
    let upload_url = reserved
        .get("upload_url")
        .and_then(|v| v.as_str())
        .ok_or("files.getUploadURLExternal returned no upload_url")?
        .to_string();
    let file_id = reserved
        .get("file_id")
        .and_then(|v| v.as_str())
        .ok_or("files.getUploadURLExternal returned no file_id")?
        .to_string();

    // 2. Send the bytes to the pre-signed URL byte-exact (no auth; the URL carries its own token).
    //    `http_bytes` ships the raw body so binary files round-trip without UTF-8 corruption.
    let resp = host.http_bytes("PUT", &upload_url, None, &[], Some(&bytes), false)?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "slack file upload → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }

    // 3. Complete the upload, attaching the file to the channel/thread.
    let mut complete = json!({
        "files": [{ "id": file_id, "title": filename }],
        "channel_id": channel,
    });
    if let Some(ts) = opt_str(&input, "thread_ts") {
        complete["thread_ts"] = json!(ts);
    }
    if let Some(comment) = opt_str(&input, "initial_comment") {
        complete["initial_comment"] = json!(comment);
    }
    let complete_url = format!("{}/files.completeUploadExternal", base_url(host));
    let done = check_ok(host.send_json("POST", &complete_url, Some("bot_token"), &complete)?)?;
    Ok(json!({
        "ok": true,
        "channel": channel,
        "file_id": file_id,
        "filename": filename,
        "size": bytes.len(),
        "files": done.get("files").cloned().unwrap_or_else(|| json!([])),
    }))
}

fn file_download(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?.to_string();
    let info_url = format!("{}/files.info?file={}", base_url(host), urlencode(&file_id),);
    let info = check_ok(host.get_json(&info_url, Some("bot_token"))?)?;
    let file = info.get("file").cloned().unwrap_or(Value::Null);
    let download_url = file
        .get("url_private_download")
        .or_else(|| file.get("url_private"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("file has no private download URL")?
        .to_string();
    // Fetch byte-exact: `binary_response = true` returns the raw bytes so non-UTF-8 files
    // round-trip without corruption. The download URL still needs the bot token as bearer auth.
    let resp = host.http_bytes("GET", &download_url, Some("bot_token"), &[], None, true)?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "slack download → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }
    let bytes = resp.bytes;
    let filename = opt_str(&input, "filename")
        .map(str::to_string)
        .or_else(|| {
            file.get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| file_id.clone());
    let blob_ref = host.blob_put(&filename, &bytes)?;
    Ok(json!({
        "ok": true,
        "file_id": file_id,
        "filename": filename,
        "size": bytes.len(),
        "blob_ref": blob_ref,
        "file": file,
    }))
}

fn file_info(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?;
    let url = format!("{}/files.info?file={}", base_url(host), urlencode(file_id));
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

fn file_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    let mut url = format!("{}/files.list?count={limit}&page=1", base_url(host));
    for key in ["channel", "user", "types"] {
        if let Some(val) = opt_str(&input, key) {
            url.push_str(&format!("&{key}={}", urlencode(val)));
        }
    }
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

fn file_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?;
    let url = format!("{}/files.delete", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &json!({ "file": file_id }))?)
}

// ---------------------------------------------------------------------------
// bookmarks
// ---------------------------------------------------------------------------

fn bookmark_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let title = req_str(&input, "title")?;
    let link = req_str(&input, "link")?;
    let mut body = json!({ "channel_id": channel, "title": title, "type": "link", "link": link });
    if let Some(emoji) = opt_str(&input, "emoji") {
        body["emoji"] = json!(emoji.trim_matches(':'));
    }
    let url = format!("{}/bookmarks.add", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

fn bookmark_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let bookmark_id = req_str(&input, "bookmark_id")?;
    let mut body = json!({ "channel_id": channel, "bookmark_id": bookmark_id });
    if let Some(title) = opt_str(&input, "title") {
        body["title"] = json!(title);
    }
    if let Some(link) = opt_str(&input, "link") {
        body["link"] = json!(link);
    }
    if let Some(emoji) = opt_str(&input, "emoji") {
        body["emoji"] = json!(emoji.trim_matches(':'));
    }
    let url = format!("{}/bookmarks.edit", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

fn bookmark_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let bookmark_id = req_str(&input, "bookmark_id")?;
    let body = json!({ "channel_id": channel, "bookmark_id": bookmark_id });
    let url = format!("{}/bookmarks.remove", base_url(host));
    check_ok(host.send_json("POST", &url, Some("bot_token"), &body)?)
}

fn bookmark_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let url = format!(
        "{}/bookmarks.list?channel_id={}",
        base_url(host),
        urlencode(channel),
    );
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

// ---------------------------------------------------------------------------
// users / presence / emoji
// ---------------------------------------------------------------------------

fn user_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let url = format!("{}/users.list?limit=200", base_url(host));
    let v = check_ok(host.get_json(&url, Some("bot_token"))?)?;
    contribute_users(host, &v);
    Ok(v)
}

fn presence_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut url = format!("{}/users.getPresence", base_url(host));
    if let Some(user) = opt_str(&input, "user") {
        url.push_str(&format!("?user={}", urlencode(user)));
    }
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

fn presence_set(input: Value, host: &mut Host) -> Result<Value, String> {
    let presence = req_str(&input, "presence")?;
    let url = format!("{}/users.setPresence", base_url(host));
    check_ok(host.send_json(
        "POST",
        &url,
        Some("user_token"),
        &json!({ "presence": presence }),
    )?)
}

fn emoji_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let url = format!("{}/emoji.list", base_url(host));
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

fn index_build(_input: Value, host: &mut Host) -> Result<Value, String> {
    let mut total = 0usize;
    let ch_url = format!(
        "{}/conversations.list?types=public_channel,private_channel,mpim,im&limit=200",
        base_url(host),
    );
    let channels = check_ok(host.get_json(&ch_url, Some("bot_token"))?)?;
    total += contribute_channels(host, &channels);

    let user_url = format!("{}/users.list?limit=200", base_url(host));
    let users = check_ok(host.get_json(&user_url, Some("bot_token"))?)?;
    total += contribute_users(host, &users);

    Ok(json!({ "indexed": total }))
}

// ---------------------------------------------------------------------------
// datasource contribution
// ---------------------------------------------------------------------------

/// Contribute `slack.channel` records from a `conversations.list` reply; returns the number indexed.
fn contribute_channels(host: &mut Host, v: &Value) -> usize {
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
fn contribute_users(host: &mut Host, v: &Value) -> usize {
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

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Plugin {
        manifest_builder().build()
    }

    fn host() -> MockHost {
        MockHost::default()
            .with_secret("bot_token", "xoxb")
            .with_secret("user_token", "xoxp")
    }

    #[test]
    fn auth_test_probes_both_tokens() {
        let mut h = host().with_http(
            "auth.test",
            json!({ "ok": true, "team": "Acme", "user": "bot", "team_id": "T1", "user_id": "U1" }),
        );
        let out = plugin().call("slack.test", json!({}), &mut h).unwrap();
        assert_eq!(out["status"], "ok");
        assert_eq!(out["count"], 2);
        assert_eq!(out["tokens"][0]["role"], "user");
        assert_eq!(out["tokens"][1]["role"], "bot");
    }

    #[test]
    fn info_reports_identity() {
        let mut h = host().with_http(
            "auth.test",
            json!({ "ok": true, "team": "Acme", "user_id": "U1" }),
        );
        let out = plugin().call("slack.info", json!({}), &mut h).unwrap();
        assert_eq!(out["tokens"][0]["team"], "Acme");
    }

    #[test]
    fn message_send_posts_and_returns_the_ts() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "123.45" }));
        let out = plugin()
            .call(
                "slack.message.send",
                json!({ "channel": "C1", "text": "hello", "thread_ts": "100.1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ts"], "123.45");
    }

    #[test]
    fn message_list_reads_history() {
        let mut h = host().with_http(
            "conversations.history",
            json!({ "ok": true, "messages": [{ "ts": "1.1", "text": "hi" }] }),
        );
        let out = plugin()
            .call(
                "slack.message.list",
                json!({ "channel": "C1", "limit": 5 }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"][0]["text"], "hi");
    }

    #[test]
    fn message_edit_resolves_ref() {
        let mut h = host().with_http("chat.update", json!({ "ok": true, "ts": "1.1" }));
        let out = plugin()
            .call(
                "slack.message.edit",
                json!({ "ref": "C9:1.1", "text": "fixed" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn message_delete_uses_channel_and_ts() {
        let mut h = host().with_http("chat.delete", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.message.delete",
                json!({ "channel": "C1", "ts": "1.1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn thread_reads_replies_from_permalink_ref() {
        let mut h = host().with_http(
            "conversations.replies",
            json!({ "ok": true, "messages": [{ "ts": "1.1" }, { "ts": "1.2" }] }),
        );
        let out = plugin()
            .call(
                "slack.thread",
                json!({ "ref": "https://acme.slack.com/archives/C0123ABCD/p1718031600123456" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn search_uses_the_user_token() {
        let mut h = host().with_http(
            "search.messages",
            json!({ "ok": true, "messages": { "matches": [{ "text": "found" }], "total": 1 } }),
        );
        let out = plugin()
            .call("slack.search", json!({ "query": "deploy" }), &mut h)
            .unwrap();
        assert_eq!(out["messages"]["matches"][0]["text"], "found");
    }

    #[test]
    fn mentions_classifies_handling_status() {
        // The matched mention sits in a thread; the bot identity (U_me) authored a later reply,
        // so the mention classifies as `replied`. The search match carries channel + ts +
        // permalink (with a thread_ts), and own-identity resolution comes from auth.test.
        // Use a current ts: the default (empty) `since` floors to today's midnight, so stale
        // timestamps would be dropped (matching the reference's `mentionSince` semantics).
        let now = unix_now();
        let root = format!("{now}.000000");
        let matched = format!("{now}.000001");
        let later = format!("{now}.000002");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_me> please look",
                    "permalink": format!("https://acme.slack.com/archives/C1/p1001000000?thread_ts={root}"),
                    "channel": { "id": "C1", "name": "dev" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": true, "messages": [
                    { "ts": root, "user": "U2", "text": "root" },
                    { "ts": matched, "user": "U2", "text": "<@U_me> please look" },
                    { "ts": later, "user": "U_me", "text": "on it" }
                ] }),
            );
        let out = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap();
        assert_eq!(out["target"], "U_me");
        assert_eq!(out["count"], 1);
        assert_eq!(out["total"], 1);
        assert_eq!(out["mentions"][0]["channel"], "C1");
        assert_eq!(out["mentions"][0]["ts"], matched);
        assert_eq!(out["mentions"][0]["thread_ts"], root);
        assert_eq!(out["mentions"][0]["status"], "replied");
        // New residual fields are always present in the envelope.
        assert_eq!(out["since"], "");
        assert_eq!(out["unhandled"], false);
        assert!(out["tickets"].as_array().unwrap().is_empty());
    }

    #[test]
    fn mentions_unhandled_filters_out_handled() {
        // Two current-ts matches: the first is replied-to by U_me (→ filtered out by `unhandled`);
        // the second has an empty channel so classification short-circuits to `pending` w/o HTTP.
        let now = unix_now();
        let root = format!("{now}.000000");
        let handled = format!("{now}.000001");
        let later = format!("{now}.000002");
        let pending = format!("{now}.000003");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 2, "matches": [
                    {
                        "ts": handled, "user": "U2", "text": "<@U_me> a",
                        "permalink": format!("https://acme.slack.com/archives/C1/p1001000000?thread_ts={root}"),
                        "channel": { "id": "C1" }
                    },
                    {
                        "ts": pending, "user": "U2", "text": "<@U_me> b",
                        "permalink": "https://acme.slack.com/archives/C2/p2001000000",
                        "channel": { "id": "" }
                    }
                ] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": true, "messages": [
                    { "ts": root, "user": "U2", "text": "root" },
                    { "ts": handled, "user": "U2", "text": "<@U_me> a" },
                    { "ts": later, "user": "U_me", "text": "on it" }
                ] }),
            );
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "unhandled": true }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["unhandled"], true);
        assert_eq!(out["count"], 1);
        assert_eq!(out["mentions"][0]["ts"], pending);
        assert_eq!(out["mentions"][0]["status"], "pending");
        // Total still reflects the raw search total, not the filtered count.
        assert_eq!(out["total"], 2);
    }

    #[test]
    fn mentions_since_drops_older_matches() {
        // A `since` of 1h yields a unix lower bound; the old match (ts 1.0) is dropped, the
        // recent one (now) is kept. (The `after:` search term is exercised by the unit test.)
        let now = unix_now();
        let recent = format!("{now}.000100");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 2, "matches": [
                    {
                        "ts": "1.0", "user": "U2", "text": "<@U_me> old",
                        "permalink": "https://acme.slack.com/archives/C1/p1000000",
                        "channel": { "id": "" }
                    },
                    {
                        "ts": recent, "user": "U2", "text": "<@U_me> new",
                        "permalink": "https://acme.slack.com/archives/C1/pnew",
                        "channel": { "id": "" }
                    }
                ] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "since": "1h" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["since"], "1h");
        assert_eq!(out["count"], 1, "the >1h-old match must be dropped");
        assert_eq!(out["mentions"][0]["text"], "<@U_me> new");
    }

    #[test]
    fn mention_since_builds_after_term() {
        // Empty since → floor to UTC midnight; the `after:` term is one day earlier.
        let (unix, after) = mention_since("").unwrap();
        assert_eq!(unix % 86_400, 0, "empty since floors to UTC midnight");
        assert_eq!(after, civil_date(unix - 86_400));
        // A duration since → now - duration; after term is a valid YYYY-MM-DD.
        let (unix2, after2) = mention_since("7d").unwrap();
        assert!(unix2 > 0);
        assert_eq!(after2.len(), 10);
        assert!(mention_since("bogus").is_err());
    }

    #[test]
    fn unread_since_defaults_to_14d() {
        let (unix, label) = unread_since("").unwrap();
        assert_eq!(label, "14d");
        assert!(unix > 0);
        let (_, label2) = unread_since("1h").unwrap();
        assert_eq!(label2, "1h");
        assert!(unread_since("nope").is_err());
    }

    #[test]
    fn mentions_extracts_tickets() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        // Empty channel → classify short-circuits to pending (no thread HTTP needed).
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched, "user": "U2",
                    "text": "<@U_me> see DEV-42 and TEL-7, also dev-99",
                    "permalink": "https://acme.slack.com/archives/C1/p1001000000",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "tickets": true }),
                &mut h,
            )
            .unwrap();
        // Per-item tickets: default rule is case-sensitive uppercase, so `dev-99` is NOT matched
        // (mirrors fluxplane's non-(?i) default); uppercased, deduped, sorted.
        assert_eq!(out["mentions"][0]["tickets"], json!(["DEV-42", "TEL-7"]));
        // Aggregate: one record per key, sorted, with the permalink.
        let agg = out["tickets"].as_array().unwrap();
        assert_eq!(agg.len(), 2);
        assert_eq!(agg[0]["key"], "DEV-42");
        assert_eq!(agg[0]["mentions"], 1);
        assert_eq!(
            agg[0]["permalinks"][0],
            "https://acme.slack.com/archives/C1/p1001000000"
        );
    }

    #[test]
    fn mentions_tickets_honour_explicit_keys() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched, "user": "U2",
                    "text": "dev-1 ABC-2 TEL-3",
                    "permalink": "https://x",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "tickets": true, "ticket_keys": ["dev", "tel"] }),
                &mut h,
            )
            .unwrap();
        // Keyed rule is case-insensitive: `dev-1` matches DEV; ABC-2 is excluded.
        assert_eq!(out["mentions"][0]["tickets"], json!(["DEV-1", "TEL-3"]));
    }

    #[test]
    fn extract_tickets_matches_reference_rule() {
        // Default rule: `[A-Z][A-Z0-9]+-<digits>` on word boundaries, deduped + sorted.
        assert_eq!(
            extract_tickets("FLUX-1 flux-1 NOPE A-1 X9-12 trailing FOO-12bar", &[]),
            // A-1 needs ≥2 prefix chars → excluded; FOO-12bar has a trailing word char → excluded.
            json!(["FLUX-1", "X9-12"]).as_array().unwrap().to_vec()
        );
        // Keyed rule is case-insensitive on the prefix.
        assert_eq!(
            extract_tickets("dev-5 OTHER-9", &["DEV".to_string()]),
            vec!["DEV-5".to_string()]
        );
    }

    #[test]
    fn mentions_pending_when_unanswered() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_me> ping",
                    "permalink": "https://acme.slack.com/archives/C1/p2001000000",
                    "channel": { "id": "C1" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": true, "messages": [
                    { "ts": matched, "user": "U2", "text": "<@U_me> ping" }
                ] }),
            );
        let out = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap();
        assert_eq!(out["mentions"][0]["status"], "pending");
        assert_eq!(out["mentions"][0]["thread_ts"], "");
    }

    #[test]
    fn unreads_counts_genuine_unreads_after_last_read() {
        // The channel's `last_read` cursor drives the `oldest` history window so only messages
        // genuinely after the cursor count; Slack returns newest-first so we reverse them.
        let mut h = host()
            .with_http(
                "users.conversations",
                json!({ "ok": true, "channels": [{ "id": "C1", "name": "dev", "last_read": "1.0" }] }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": true, "messages": [
                    { "ts": "1.3", "text": "newest" },
                    { "ts": "1.2", "text": "middle" }
                ] }),
            );
        let out = plugin().call("slack.unreads", json!({}), &mut h).unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["since"], "14d"); // default window echoed
        assert_eq!(out["channels"][0]["unread_count"], 2);
        assert_eq!(out["channels"][0]["last_read"], "1.0");
        // chronological order after reversing Slack's newest-first history
        assert_eq!(out["channels"][0]["messages"][0]["ts"], "1.2");
        assert_eq!(out["channels"][0]["messages"][1]["ts"], "1.3");
    }

    #[test]
    fn unreads_echoes_explicit_since_label() {
        // An explicit `since` is echoed verbatim; the cursor math (last_read) is unchanged.
        let mut h = host()
            .with_http(
                "users.conversations",
                json!({ "ok": true, "channels": [{ "id": "C1", "name": "dev", "last_read": "1.0" }] }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": true, "messages": [ { "ts": "1.3", "text": "x" } ] }),
            );
        let out = plugin()
            .call("slack.unreads", json!({ "since": "7d" }), &mut h)
            .unwrap();
        assert_eq!(out["since"], "7d");
        assert_eq!(out["channels"][0]["last_read"], "1.0");
        assert_eq!(out["channels"][0]["unread_count"], 1);
    }

    #[test]
    fn reaction_add_posts_name_and_timestamp() {
        let mut h = host().with_http("reactions.add", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.reaction.add",
                json!({ "channel": "C1", "ts": "1.1", "emoji": ":tada:" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn reaction_remove_posts() {
        let mut h = host().with_http("reactions.remove", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.reaction.remove",
                json!({ "channel": "C1", "ts": "1.1", "emoji": "tada" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn channel_list_calls_the_api_and_contributes_records() {
        let mut h = host().with_http(
            "conversations.list",
            json!({
                "ok": true,
                "channels": [{ "id": "C1", "name": "dev-team", "topic": { "value": "eng" } }]
            }),
        );
        let out = plugin()
            .call("slack.channel.list", json!({}), &mut h)
            .unwrap();
        assert_eq!(out["channels"][0]["id"], "C1");
        let recs = h.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "slack.channel");
        assert_eq!(recs[0].id, "C1");
        assert_eq!(recs[0].title, "dev-team");
        assert_eq!(recs[0].body, "eng");
    }

    #[test]
    fn channel_join_posts() {
        let mut h = host().with_http(
            "conversations.join",
            json!({ "ok": true, "channel": { "id": "C1" } }),
        );
        let out = plugin()
            .call("slack.channel.join", json!({ "channel": "C1" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn channel_mark_posts() {
        let mut h = host().with_http("conversations.mark", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.channel.mark-read",
                json!({ "channel": "C1", "ts": "1.1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn file_upload_reads_blob_and_runs_the_external_flow() {
        let mut h = host()
            .with_http(
                "files.getUploadURLExternal",
                json!({ "ok": true, "upload_url": "https://files.slack.test/up", "file_id": "F1" }),
            )
            .with_http("files.slack.test/up", json!({ "ok": true }))
            .with_http(
                "files.completeUploadExternal",
                json!({ "ok": true, "files": [{ "id": "F1", "title": "hello.txt" }] }),
            );
        // Stage non-UTF-8 source bytes directly into the host's blob store, then upload by ref —
        // the byte-exact `http_bytes` PUT must carry them verbatim (no `from_utf8_lossy`).
        let raw: Vec<u8> = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
        h.blobs
            .borrow_mut()
            .insert("blob-1".into(), ("hello.bin".into(), raw.clone()));
        let out = plugin()
            .call(
                "slack.file.upload",
                json!({ "channel": "C1", "blob_ref": "blob-1", "filename": "hello.bin" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["file_id"], "F1");
        assert_eq!(out["size"], raw.len());
        assert_eq!(out["files"][0]["id"], "F1");
    }

    #[test]
    fn file_download_stages_bytes_into_a_blob_byte_exact() {
        // Non-UTF-8 bytes prove the binary path round-trips without `from_utf8_lossy` corruption.
        let raw: Vec<u8> = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
        let mut h = host()
            .with_http(
                "files.info",
                json!({ "ok": true, "file": { "id": "F1", "name": "report.bin", "url_private_download": "https://files.slack.test/dl/F1" } }),
            )
            .with_http_bytes("files.slack.test/dl", raw.clone());
        let out = plugin()
            .call("slack.file.download", json!({ "file_id": "F1" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["filename"], "report.bin");
        assert_eq!(out["size"], raw.len());
        let blob_ref = out["blob_ref"].as_str().unwrap();
        let blobs = h.blobs.borrow();
        let (_, stored) = blobs.get(blob_ref).expect("blob staged");
        assert_eq!(stored, &raw, "downloaded bytes must round-trip byte-exact");
    }

    #[test]
    fn download_alias_works() {
        let mut h = host()
            .with_http(
                "files.info",
                json!({ "ok": true, "file": { "id": "F2", "name": "a.txt", "url_private": "https://files.slack.test/p/F2" } }),
            )
            .with_http_bytes("files.slack.test/p", b"bytes".to_vec());
        let out = plugin()
            .call("slack.download", json!({ "file_id": "F2" }), &mut h)
            .unwrap();
        assert_eq!(out["file_id"], "F2");
    }

    #[test]
    fn file_info_reads() {
        let mut h = host().with_http("files.info", json!({ "ok": true, "file": { "id": "F1" } }));
        let out = plugin()
            .call("slack.file.info", json!({ "file_id": "F1" }), &mut h)
            .unwrap();
        assert_eq!(out["file"]["id"], "F1");
    }

    #[test]
    fn file_list_reads() {
        let mut h = host().with_http(
            "files.list",
            json!({ "ok": true, "files": [{ "id": "F1" }] }),
        );
        let out = plugin()
            .call("slack.file.list", json!({ "channel": "C1" }), &mut h)
            .unwrap();
        assert_eq!(out["files"][0]["id"], "F1");
    }

    #[test]
    fn file_delete_posts() {
        let mut h = host().with_http("files.delete", json!({ "ok": true }));
        let out = plugin()
            .call("slack.file.delete", json!({ "file_id": "F1" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn bookmark_add_posts() {
        let mut h = host().with_http(
            "bookmarks.add",
            json!({ "ok": true, "bookmark": { "id": "Bk1" } }),
        );
        let out = plugin()
            .call(
                "slack.bookmark.add",
                json!({ "channel": "C1", "title": "Docs", "link": "https://x", "emoji": ":book:" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["bookmark"]["id"], "Bk1");
    }

    #[test]
    fn bookmark_edit_posts() {
        let mut h = host().with_http(
            "bookmarks.edit",
            json!({ "ok": true, "bookmark": { "id": "Bk1" } }),
        );
        let out = plugin()
            .call(
                "slack.bookmark.edit",
                json!({ "channel": "C1", "bookmark_id": "Bk1", "title": "New" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["bookmark"]["id"], "Bk1");
    }

    #[test]
    fn bookmark_delete_posts() {
        let mut h = host().with_http("bookmarks.remove", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.bookmark.delete",
                json!({ "channel": "C1", "bookmark_id": "Bk1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn bookmark_list_reads() {
        let mut h = host().with_http(
            "bookmarks.list",
            json!({ "ok": true, "bookmarks": [{ "id": "Bk1" }] }),
        );
        let out = plugin()
            .call("slack.bookmark.list", json!({ "channel": "C1" }), &mut h)
            .unwrap();
        assert_eq!(out["bookmarks"][0]["id"], "Bk1");
    }

    #[test]
    fn user_list_contributes_records() {
        let mut h = host().with_http(
            "users.list",
            json!({ "ok": true, "members": [{ "id": "U1", "name": "alice", "profile": { "real_name": "Alice A" } }] }),
        );
        let out = plugin().call("slack.user.list", json!({}), &mut h).unwrap();
        assert_eq!(out["members"][0]["id"], "U1");
        let recs = h.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "slack.user");
        assert_eq!(recs[0].body, "Alice A");
    }

    #[test]
    fn presence_get_reads() {
        let mut h = host().with_http(
            "users.getPresence",
            json!({ "ok": true, "presence": "active" }),
        );
        let out = plugin()
            .call("slack.presence.get", json!({ "user": "U1" }), &mut h)
            .unwrap();
        assert_eq!(out["presence"], "active");
    }

    #[test]
    fn presence_set_posts() {
        let mut h = host().with_http("users.setPresence", json!({ "ok": true }));
        let out = plugin()
            .call("slack.presence.set", json!({ "presence": "away" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn emoji_list_reads() {
        let mut h = host().with_http(
            "emoji.list",
            json!({ "ok": true, "emoji": { "party": "https://x" } }),
        );
        let out = plugin()
            .call("slack.emoji.list", json!({}), &mut h)
            .unwrap();
        assert_eq!(out["emoji"]["party"], "https://x");
    }

    #[test]
    fn index_build_contributes_channels_and_users() {
        let mut h = host()
            .with_http(
                "conversations.list",
                json!({ "ok": true, "channels": [{ "id": "C1", "name": "dev" }] }),
            )
            .with_http(
                "users.list",
                json!({ "ok": true, "members": [{ "id": "U1", "name": "alice" }] }),
            );
        let out = plugin()
            .call("slack.index.build", json!({}), &mut h)
            .unwrap();
        assert_eq!(out["indexed"], 2);
        let recs = h.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().any(|r| r.entity == "slack.channel"));
        assert!(recs.iter().any(|r| r.entity == "slack.user"));
    }

    #[test]
    fn falsey_ok_surfaces_the_error() {
        let mut h = host().with_http(
            "conversations.history",
            json!({ "ok": false, "error": "channel_not_found" }),
        );
        let err = plugin()
            .call("slack.message.list", json!({ "channel": "C9" }), &mut h)
            .unwrap_err();
        assert!(err.contains("channel_not_found"), "got: {err}");
    }

    #[test]
    fn manifest_declares_ops_auth_and_datasources() {
        let m = plugin().manifest();
        assert_eq!(m.operations.len(), 30);
        assert_eq!(m.auth[0].purpose, "bot_token");
        assert!(m.auth.iter().any(|a| a.purpose == "user_token"));
        assert!(m.capabilities.blob);
        assert!(m.datasources.iter().any(|d| d.entity == "slack.channel"));
        assert!(m.datasources.iter().any(|d| d.entity == "slack.user"));
        assert!(m
            .datasources
            .iter()
            .all(|d| d.capabilities.iter().any(|c| c == "index")));
    }
}
