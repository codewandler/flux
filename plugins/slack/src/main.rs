//! `slack` — a flux integration plugin for the Slack Web API: token info, messaging, threads, search,
//! reactions, channels, files (via host blobs), bookmarks, users, presence, and emoji. Authenticates
//! with tokens injected as bearer headers — purpose `bot_token` for most calls, `user_token` for the
//! search-scoped reads (search/mentions/unreads) and presence writes. Every call goes through the host
//! by endpoint reference (`slack.endpoint`, base URL from the required `SLACK_API_URL` env) plus the
//! method path — the plugin never composes a URL. List ops contribute datasource records
//! (`slack.channel` / `slack.user`) so the agent can search them; `slack.index.build` rebuilds both.
//!
//! Slack replies are JSON carrying an `"ok": bool`; a falsey `ok` is surfaced as an error built from the
//! response's `"error"` field. File ops never inline base64 — `slack.file.upload` reads its bytes from a
//! host `blob_ref`, and the download ops stage the fetched bytes back into the blob store, returning a ref.

use base64::Engine as _;
use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

// ─── op input schemas (D-36) ───────────────────────────────────────────────
// Each op's `input_schema` is schemars-derived (`host_kit::read_op_typed::<T>` /
// `write_op_typed::<T>`) instead of an inline `json!({"type":"object",...})` literal,
// so the schema cannot drift. The structs are schema-only: handlers keep their
// existing `opt_str`/`Value` extraction (D-34 precedent).
/// `slack.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TestInput {}

/// `slack.info`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct InfoInput {}

/// `slack.message.send`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MessageSendInput {
    channel: String,
    text: Option<String>,
    markdown: Option<String>,
    blocks: Option<Vec<Value>>,
    thread_ts: Option<String>,
    reply_broadcast: Option<bool>,
    unfurl_links: Option<bool>,
    unfurl_media: Option<bool>,
    parse: Option<String>,
}

/// `slack.message.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MessageListInput {
    channel: String,
    limit: Option<i64>,
    cursor: Option<String>,
    oldest: Option<String>,
    latest: Option<String>,
    text_format: Option<String>,
}

/// `slack.message.edit`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MessageEditInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    text: Option<String>,
    markdown: Option<String>,
    blocks: Option<Vec<Value>>,
    unfurl_links: Option<bool>,
    unfurl_media: Option<bool>,
    parse: Option<String>,
}

/// `slack.message.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MessageDeleteInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
}

/// `slack.thread`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ThreadInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    limit: Option<i64>,
    max_bytes: Option<i64>,
    text_format: Option<String>,
}

/// `slack.search`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SearchInput {
    query: String,
    limit: Option<i64>,
    tickets: Option<bool>,
    ticket_keys: Option<Vec<String>>,
}

/// `slack.mentions`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MentionsInput {
    user: Option<String>,
    bot: Option<bool>,
    since: Option<String>,
    limit: Option<i64>,
    unhandled: Option<bool>,
    max_thread: Option<i64>,
    tickets: Option<bool>,
    ticket_keys: Option<Vec<String>>,
}

/// `slack.unreads`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct UnreadsInput {
    channel: Option<String>,
    since: Option<String>,
    limit: Option<i64>,
}

/// `slack.reaction.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ReactionAddInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    emoji: String,
}

/// `slack.reaction.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ReactionRemoveInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    emoji: String,
}

/// `slack.channel.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ChannelListInput {
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.channel.join`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ChannelJoinInput {
    channel: String,
}

/// `slack.channel.mark-read`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ChannelMarkReadInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
}

/// `slack.file.upload`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileUploadInput {
    channel: String,
    blob_ref: Option<String>,
    content_bytes: Option<String>,
    filename: Option<String>,
    thread_ts: Option<String>,
    initial_comment: Option<String>,
    alt_text: Option<String>,
}

/// `slack.file.download`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileDownloadInput {
    file_id: String,
    blob_ref: Option<String>,
    filename: Option<String>,
}

/// `slack.download`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DownloadInput {
    file_id: String,
    blob_ref: Option<String>,
    filename: Option<String>,
}

/// `slack.file.info`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileInfoInput {
    file_id: String,
}

/// `slack.file.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileListInput {
    channel: Option<String>,
    user: Option<String>,
    types: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.file.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileDeleteInput {
    file_id: String,
}

/// `slack.bookmark.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct BookmarkAddInput {
    channel: String,
    title: String,
    link: String,
    emoji: Option<String>,
}

/// `slack.bookmark.edit`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct BookmarkEditInput {
    channel: String,
    bookmark_id: String,
    title: Option<String>,
    link: Option<String>,
    emoji: Option<String>,
}

/// `slack.bookmark.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct BookmarkDeleteInput {
    channel: String,
    bookmark_id: String,
}

/// `slack.bookmark.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct BookmarkListInput {
    channel: String,
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.user.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct UserListInput {
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.presence.get`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PresenceGetInput {
    user: Option<String>,
}

/// `slack.presence.set`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PresenceSetInput {
    presence: String,
}

/// `slack.emoji.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EmojiListInput {
    query: Option<String>,
    limit: Option<i64>,
    mode: Option<String>,
    include_aliases: Option<bool>,
}

/// `slack.index.build`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IndexBuildInput {}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("slack", "0.1.0")
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["slack.com".into(), "*.slack.com".into()],
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
            http_hosts: vec!["slack.com".into()],
            description: "Slack Web API base URL (default https://slack.com/api)".into(),
        })
        .datasource(ds("slack.channels", "slack.channel", "Slack channels."))
        .datasource(ds("slack.users", "slack.user", "Slack workspace users."))
        // -- auth / identity ------------------------------------------------
        .operation(
            read_op_typed::<TestInput>(
                "slack.test",
                "Test Slack user and bot token authentication.",
            ),
            auth_test,
        )
        .operation(
            read_op_typed::<InfoInput>(
                "slack.info",
                "Show Slack token identity and workspace information.",
            ),
            auth_test,
        )
        // -- messages -------------------------------------------------------
        .operation(
            write_op_typed::<MessageSendInput>(
                "slack.message.send",
                "Send a message to a channel (channel id or DM channel; optionally as a thread reply).",
            ),
            message_send,
        )
        .operation(
            read_op_typed::<MessageListInput>(
                "slack.message.list",
                "Read recent messages from a channel (conversations.history); paginate with next_cursor.",
            ),
            message_list,
        )
        .operation(
            write_op_typed::<MessageEditInput>(
                "slack.message.edit",
                "Edit a Slack message. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
            ),
            message_edit,
        )
        .operation(
            write_op_typed::<MessageDeleteInput>(
                "slack.message.delete",
                "Delete a Slack message. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
            ),
            message_delete,
        )
        .operation(
            read_op_typed::<ThreadInput>(
                "slack.thread",
                "View a Slack thread. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
            ),
            thread,
        )
        // -- search / mentions / unreads (user token) -----------------------
        .operation(
            read_op_typed::<SearchInput>(
                "slack.search",
                "Search Slack messages (search.messages; requires a user token).",
            ),
            search,
        )
        .operation(
            read_op_typed::<MentionsInput>(
                "slack.mentions",
                "Search Slack mentions of a user and classify whether each was handled (search.messages \\
                 + per-mention thread inspection; requires a user token).",
            ),
            mentions,
        )
        .operation(
            read_op_typed::<UnreadsInput>(
                "slack.unreads",
                "List Slack conversations with recent (unread) messages (requires a user token).",
            ),
            unreads,
        )
        // -- reactions ------------------------------------------------------
        .operation(
            write_op_typed::<ReactionAddInput>(
                "slack.reaction.add",
                "Add a reaction to a Slack message. Provide `ref` OR `channel`+`ts`, plus `emoji`.",
            ),
            reaction_add,
        )
        .operation(
            write_op_typed::<ReactionRemoveInput>(
                "slack.reaction.remove",
                "Remove a reaction from a Slack message. Provide `ref` OR `channel`+`ts`, plus `emoji`.",
            ),
            reaction_remove,
        )
        // -- channels -------------------------------------------------------
        .operation(
            read_op_typed::<ChannelListInput>(
                "slack.channel.list",
                "List public and private channels (plus group/direct conversations) in the workspace.",
            ),
            channel_list,
        )
        .operation(
            write_op_typed::<ChannelJoinInput>(
                "slack.channel.join",
                "Join a Slack public channel.",
            ),
            channel_join,
        )
        .operation(
            write_op_typed::<ChannelMarkReadInput>(
                "slack.channel.mark-read",
                "Mark a Slack channel read through a timestamp. Provide `ref` OR `channel`+`ts`.",
            ),
            channel_mark,
        )
        // -- files (blobs) --------------------------------------------------
        .operation(
            write_op_typed::<FileUploadInput>(
                "slack.file.upload",
                "Upload a file to a Slack channel, DM, or thread. Bytes come from a host `blob_ref`.",
            ),
            file_upload,
        )
        .operation(
            write_op_typed::<FileDownloadInput>(
                "slack.file.download",
                "Download a Slack file to a host blob; returns the `blob_ref`.",
            ),
            file_download,
        )
        .operation(
            write_op_typed::<DownloadInput>(
                "slack.download",
                "Download a Slack file to a host blob; returns the `blob_ref`.",
            ),
            file_download,
        )
        .operation(
            read_op_typed::<FileInfoInput>(
                "slack.file.info",
                "Show Slack file information.",
            ),
            file_info,
        )
        .operation(
            read_op_typed::<FileListInput>(
                "slack.file.list",
                "List Slack files (optionally filtered by channel/user/type).",
            ),
            file_list,
        )
        .operation(
            write_op_typed::<FileDeleteInput>(
                "slack.file.delete",
                "Delete a Slack file.",
            ),
            file_delete,
        )
        // -- bookmarks ------------------------------------------------------
        .operation(
            write_op_typed::<BookmarkAddInput>(
                "slack.bookmark.add",
                "Add a Slack channel bookmark.",
            ),
            bookmark_add,
        )
        .operation(
            write_op_typed::<BookmarkEditInput>(
                "slack.bookmark.edit",
                "Edit a Slack channel bookmark.",
            ),
            bookmark_edit,
        )
        .operation(
            write_op_typed::<BookmarkDeleteInput>(
                "slack.bookmark.delete",
                "Delete a Slack channel bookmark.",
            ),
            bookmark_delete,
        )
        .operation(
            read_op_typed::<BookmarkListInput>(
                "slack.bookmark.list",
                "List Slack channel bookmarks.",
            ),
            bookmark_list,
        )
        // -- users / presence / emoji ---------------------------------------
        .operation(
            read_op_typed::<UserListInput>(
                "slack.user.list",
                "List users in the workspace.",
            ),
            user_list,
        )
        .operation(
            read_op_typed::<PresenceGetInput>(
                "slack.presence.get",
                "Get Slack user presence.",
            ),
            presence_get,
        )
        .operation(
            write_op_typed::<PresenceSetInput>(
                "slack.presence.set",
                "Set Slack user presence (auto|away; requires a user token).",
            ),
            presence_set,
        )
        .operation(
            read_op_typed::<EmojiListInput>(
                "slack.emoji.list",
                "List Slack custom emoji.",
            ),
            emoji_list,
        )
        // -- index ----------------------------------------------------------
        .operation(
            read_op_typed::<IndexBuildInput>(
                "slack.index.build",
                "Build the Slack channel and user reverse-lookup indexes.",
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

/// GET a Slack API `path` (joined onto the host-resolved `slack.endpoint` base) and parse the JSON.
/// The host holds the URL; the plugin only ever names the endpoint ref and the method path.
fn sl_get(host: &mut Host, path: &str, auth: Option<&str>) -> Result<Value, String> {
    host.get_json_ref("slack.endpoint", path, auth)
}

/// Send a JSON body to a Slack API `path` (joined onto the host-resolved `slack.endpoint` base) and
/// parse the response. The ref-based mirror of `host.send_json` — the URL stays host-side.
fn sl_send(
    host: &mut Host,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: &Value,
) -> Result<Value, String> {
    host.send_json_ref("slack.endpoint", method, path, auth, body)
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

/// Resolved message-content payload: a text fallback plus optional Block Kit blocks,
/// plus the Slack message-options that control unfurling/parsing.
#[derive(Default)]
struct MessageContent {
    text: String,
    blocks: Vec<Value>,
    unfurl_links: Option<bool>,
    unfurl_media: Option<bool>,
    parse: String,
}

/// Build a message-content payload from `text`, `markdown`, or `blocks` (mutually exclusive
/// carriers), mirroring fluxplane's `messageContent`. A `blocks` payload still requires a
/// `text` fallback string.
fn message_content(input: &Value) -> Result<MessageContent, String> {
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
fn channel_matches_query(channel: &Value, query: &str) -> bool {
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
fn user_matches_query(user: &Value, query: &str) -> bool {
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
fn bookmark_matches_query(bookmark: &Value, query: &str) -> bool {
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
fn file_matches_query(file: &Value, query: &str) -> bool {
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
fn collect_search_ticket_mentions(mentions: &[Value]) -> Vec<Value> {
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
fn markdown_section_block(markdown: &str) -> Value {
    json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": markdown }
    })
}

/// Text rendering mode for message reads, matching fluxplane's `textFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextFormat {
    Markdown,
    Mrkdwn,
    Both,
}

/// Parse the `text_format` enum (`markdown`/`mrkdwn`/`both`, default `markdown`).
fn parse_text_format(raw: &str) -> TextFormat {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mrkdwn" => TextFormat::Mrkdwn,
        "both" => TextFormat::Both,
        _ => TextFormat::Markdown,
    }
}

/// Apply the requested `text_format` to a raw Slack message object in-place:
/// `markdown` returns readable Markdown, `mrkdwn` keeps raw mrkdwn, `both`
/// returns both forms as `text` and `text_mrkdwn`.
fn render_message_text(message: &mut Value, format: TextFormat) {
    let raw = message
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match format {
        TextFormat::Mrkdwn => {
            message["text_mrkdwn"] = Value::Null;
        }
        TextFormat::Both => {
            message["text"] = json!(mrkdwn_to_markdown(&raw));
            message["text_mrkdwn"] = json!(raw);
        }
        TextFormat::Markdown => {
            message["text"] = json!(mrkdwn_to_markdown(&raw));
            message["text_mrkdwn"] = Value::Null;
        }
    }
}

/// Best-effort Slack mrkdwn → Markdown renderer. Links, mentions, channels, and
/// subteam/special broadcasts are translated; bold/italic/strike and HTML
/// entity decoding are applied outside code spans.
fn mrkdwn_to_markdown(text: &str) -> String {
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
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// auth / identity
// ---------------------------------------------------------------------------

fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
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

fn message_send(input: Value, host: &mut Host) -> Result<Value, String> {
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

fn message_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
    let mut path = format!(
        "/conversations.history?channel={}&limit={limit}&inclusive=true",
        urlencode(channel),
    );
    for key in ["cursor", "oldest", "latest"] {
        if let Some(val) = opt_str(&input, key) {
            path.push_str(&format!("&{key}={}", urlencode(val)));
        }
    }
    let format = parse_text_format(opt_str(&input, "text_format").unwrap_or(""));
    let mut v = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    if let Some(messages) = v.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for message in messages {
            render_message_text(message, format);
        }
    }
    Ok(v)
}

fn message_edit(input: Value, host: &mut Host) -> Result<Value, String> {
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

fn message_delete(input: Value, host: &mut Host) -> Result<Value, String> {
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

fn thread(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    // max_bytes gates per-image downloads in fluxplane; this handler still
    // surfaces the raw message envelope, but records the cap for callers.
    let _max_bytes = input
        .get("max_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(10_485_760);
    let path = format!(
        "/conversations.replies?channel={}&ts={}&limit={limit}&inclusive=true",
        urlencode(&channel),
        urlencode(&ts),
    );
    let format = parse_text_format(opt_str(&input, "text_format").unwrap_or(""));
    let mut v = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    if let Some(messages) = v.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for message in messages {
            render_message_text(message, format);
        }
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// search / mentions / unreads (user token)
// ---------------------------------------------------------------------------

fn search(input: Value, host: &mut Host) -> Result<Value, String> {
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

fn mentions(input: Value, host: &mut Host) -> Result<Value, String> {
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

fn unreads(input: Value, host: &mut Host) -> Result<Value, String> {
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
    check_ok(sl_send(
        host,
        "POST",
        &format!("/{method}"),
        Some("bot_token"),
        &body,
    )?)
}

// ---------------------------------------------------------------------------
// channels
// ---------------------------------------------------------------------------

fn channel_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let v = check_ok(sl_get(
        host,
        "/conversations.list?types=public_channel,private_channel,mpim,im&limit=200",
        Some("bot_token"),
    )?)?;
    contribute_channels(host, &v);
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0);
    if query.is_empty() && limit.is_none() {
        return Ok(v);
    }
    let mut v = v;
    if let Some(channels) = v.get_mut("channels").and_then(|c| c.as_array_mut()) {
        if !query.is_empty() {
            channels.retain(|c| channel_matches_query(c, &query));
        }
        if let Some(n) = limit {
            if channels.len() > n as usize {
                channels.truncate(n as usize);
            }
        }
    }
    Ok(v)
}

fn channel_join(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    check_ok(sl_send(
        host,
        "POST",
        "/conversations.join",
        Some("bot_token"),
        &json!({ "channel": channel }),
    )?)
}

fn channel_mark(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    check_ok(sl_send(
        host,
        "POST",
        "/conversations.mark",
        Some("bot_token"),
        &json!({ "channel": channel, "ts": ts }),
    )?)
}

// ---------------------------------------------------------------------------
// files (host blobs)
// ---------------------------------------------------------------------------

fn file_upload(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?.to_string();

    // Bytes come from either an inline base64 payload or a host blob_ref (exactly one).
    let has_blob_ref = opt_str(&input, "blob_ref").is_some();
    let has_content_bytes = opt_str(&input, "content_bytes").is_some();
    if has_blob_ref == has_content_bytes {
        return Err("provide exactly one of blob_ref or content_bytes".into());
    }
    let bytes = if let Some(b64) = opt_str(&input, "content_bytes") {
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| format!("content_bytes is not valid base64: {e}"))?
    } else {
        let blob_ref = req_str(&input, "blob_ref")?.to_string();
        host.blob_get(&blob_ref)?
    };

    let filename = opt_str(&input, "filename")
        .map(str::to_string)
        .unwrap_or_else(|| "upload.bin".into());
    if bytes.is_empty() {
        return Err("file content is empty".into());
    }

    // 1. Reserve an external upload URL.
    let reserve_path = format!(
        "/files.getUploadURLExternal?filename={}&length={}",
        urlencode(&filename),
        bytes.len(),
    );
    let reserved = check_ok(sl_get(host, &reserve_path, Some("bot_token"))?)?;
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
    let mut file_entry = json!({ "id": file_id, "title": filename });
    if let Some(alt) = opt_str(&input, "alt_text") {
        file_entry["alt_text"] = json!(alt);
    }
    let mut complete = json!({
        "files": [file_entry],
        "channel_id": channel,
    });
    if let Some(ts) = opt_str(&input, "thread_ts") {
        complete["thread_ts"] = json!(ts);
    }
    if let Some(comment) = opt_str(&input, "initial_comment") {
        complete["initial_comment"] = json!(comment);
    }
    let done = check_ok(sl_send(
        host,
        "POST",
        "/files.completeUploadExternal",
        Some("bot_token"),
        &complete,
    )?)?;
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
    let info_path = format!("/files.info?file={}", urlencode(&file_id));
    let info = check_ok(sl_get(host, &info_path, Some("bot_token"))?)?;
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
    // If the caller provided a blob_ref seed, use it as the returned reference (the host's
    // blob store receives the content under that name). Mirrors fluxplane's BlobWrite.Ref.
    let blob_ref = if let Some(seed) = opt_str(&input, "blob_ref") {
        host.blob_put(seed, &bytes)?;
        seed.to_string()
    } else {
        host.blob_put(&filename, &bytes)?
    };
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
    let path = format!("/files.info?file={}", urlencode(file_id));
    check_ok(sl_get(host, &path, Some("bot_token"))?)
}

fn file_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    let mut path = format!("/files.list?count={limit}&page=1");
    for key in ["channel", "user", "types"] {
        if let Some(val) = opt_str(&input, key) {
            path.push_str(&format!("&{key}={}", urlencode(val)));
        }
    }
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut v = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    if let Some(files) = v.get_mut("files").and_then(|f| f.as_array_mut()) {
        if !query.is_empty() {
            files.retain(|f| file_matches_query(f, &query));
        }
        let cap = input
            .get("limit")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0);
        if let Some(n) = cap {
            if files.len() > n as usize {
                files.truncate(n as usize);
            }
        }
    }
    Ok(v)
}

fn file_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?;
    check_ok(sl_send(
        host,
        "POST",
        "/files.delete",
        Some("bot_token"),
        &json!({ "file": file_id }),
    )?)
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
    check_ok(sl_send(
        host,
        "POST",
        "/bookmarks.add",
        Some("bot_token"),
        &body,
    )?)
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
    check_ok(sl_send(
        host,
        "POST",
        "/bookmarks.edit",
        Some("bot_token"),
        &body,
    )?)
}

fn bookmark_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let bookmark_id = req_str(&input, "bookmark_id")?;
    let body = json!({ "channel_id": channel, "bookmark_id": bookmark_id });
    check_ok(sl_send(
        host,
        "POST",
        "/bookmarks.remove",
        Some("bot_token"),
        &body,
    )?)
}

fn bookmark_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let path = format!("/bookmarks.list?channel_id={}", urlencode(channel));
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0);
    let mut v = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    if let Some(bookmarks) = v.get_mut("bookmarks").and_then(|b| b.as_array_mut()) {
        if !query.is_empty() {
            bookmarks.retain(|b| bookmark_matches_query(b, &query));
        }
        if let Some(n) = limit {
            if bookmarks.len() > n as usize {
                bookmarks.truncate(n as usize);
            }
        }
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// users / presence / emoji
// ---------------------------------------------------------------------------

fn user_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let v = check_ok(sl_get(host, "/users.list?limit=200", Some("bot_token"))?)?;
    contribute_users(host, &v);
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0);
    if query.is_empty() && limit.is_none() {
        return Ok(v);
    }
    let mut v = v;
    if let Some(members) = v.get_mut("members").and_then(|m| m.as_array_mut()) {
        if !query.is_empty() {
            members.retain(|u| user_matches_query(u, &query));
        }
        if let Some(n) = limit {
            if members.len() > n as usize {
                members.truncate(n as usize);
            }
        }
    }
    Ok(v)
}

fn presence_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut path = String::from("/users.getPresence");
    if let Some(user) = opt_str(&input, "user") {
        path.push_str(&format!("?user={}", urlencode(user)));
    }
    check_ok(sl_get(host, &path, Some("bot_token"))?)
}

fn presence_set(input: Value, host: &mut Host) -> Result<Value, String> {
    let presence = req_str(&input, "presence")?;
    check_ok(sl_send(
        host,
        "POST",
        "/users.setPresence",
        Some("user_token"),
        &json!({ "presence": presence }),
    )?)
}

fn emoji_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mode = opt_str(&input, "mode")
        .unwrap_or("custom")
        .trim()
        .to_ascii_lowercase();
    if mode != "custom" && mode != "builtin" && mode != "all" {
        return Err("mode must be custom, builtin, or all".into());
    }
    let include_aliases = input
        .get("include_aliases")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0);
    let unfiltered = query.is_empty() && limit.is_none() && !include_aliases && mode == "custom";

    let mut v = check_ok(sl_get(host, "/emoji.list", Some("bot_token"))?)?;
    if unfiltered {
        // No client-side filtering requested: keep Slack's native emoji map shape.
        return Ok(v);
    }

    let mut out: Vec<Value> = Vec::new();

    if mode == "custom" || mode == "all" {
        if let Some(emoji) = v.get("emoji").and_then(|e| e.as_object()) {
            let mut names: Vec<&String> = emoji.keys().collect();
            names.sort();
            for name in names {
                let value = emoji
                    .get(name)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let (is_alias, alias_for) = value
                    .strip_prefix("alias:")
                    .map(|rest| (true, rest.trim()))
                    .unwrap_or((false, ""));
                if is_alias && !include_aliases {
                    continue;
                }
                if name.to_ascii_lowercase().contains(&query) {
                    let mut entry = json!({ "name": name, "source": "custom" });
                    if is_alias {
                        entry["alias_for"] = json!(alias_for);
                    } else if !value.is_empty() {
                        entry["url"] = json!(value);
                    }
                    out.push(entry);
                }
            }
        }
    }

    if mode == "builtin" || mode == "all" {
        if let Some(categories) = v.get("categories").and_then(|c| c.as_array()) {
            for category in categories {
                let category_name = category
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if let Some(names) = category.get("emoji_names").and_then(|v| v.as_array()) {
                    for name in names {
                        let name = name
                            .as_str()
                            .map(str::trim)
                            .map(|s| s.trim_matches(':'))
                            .unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        if name.to_ascii_lowercase().contains(&query) {
                            out.push(json!({
                                "name": name,
                                "source": "builtin",
                                "category": category_name.clone(),
                            }));
                        }
                    }
                }
            }
            if mode == "all" {
                // Sort combined custom+builtin by name for stable output.
                out.sort_by(|a, b| {
                    let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    a_name.cmp(b_name)
                });
            }
        }
    }

    if let Some(n) = limit {
        if out.len() > n as usize {
            out.truncate(n as usize);
        }
    }

    v["emoji"] = json!(out);
    Ok(v)
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

fn index_build(_input: Value, host: &mut Host) -> Result<Value, String> {
    let mut total = 0usize;
    let channels = check_ok(sl_get(
        host,
        "/conversations.list?types=public_channel,private_channel,mpim,im&limit=200",
        Some("bot_token"),
    )?)?;
    total += contribute_channels(host, &channels);

    let users = check_ok(sl_get(host, "/users.list?limit=200", Some("bot_token"))?)?;
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
            .with_endpoint_ref("slack.endpoint", "https://slack.com/api")
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
    fn mentions_surfaces_search_errors() {
        let mut h = host().with_http(
            "search.messages",
            json!({ "ok": false, "error": "invalid_auth" }),
        );
        let err = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap_err();
        assert!(err.contains("invalid_auth"));
    }

    #[test]
    fn mentions_surfaces_thread_errors() {
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
                json!({ "ok": false, "error": "ratelimited" }),
            );
        let err = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap_err();
        assert!(err.contains("ratelimited"));
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
    fn unreads_surfaces_history_errors() {
        let mut h = host()
            .with_http(
                "users.conversations",
                json!({ "ok": true, "channels": [{
                    "id": "C1",
                    "name": "dev",
                    "last_read": "1.0",
                    "latest": { "ts": "2.0" }
                }] }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": false, "error": "ratelimited" }),
            );
        let err = plugin()
            .call("slack.unreads", json!({}), &mut h)
            .unwrap_err();
        assert!(err.contains("ratelimited"));
    }

    #[test]
    fn unreads_paginates_conversations() {
        let mut h = host()
            .with_http(
                "cursor=page-2",
                json!({
                    "ok": true,
                    "channels": [{
                        "id": "C2",
                        "name": "ops",
                        "last_read": "1.0",
                        "latest": { "ts": "2.0" }
                    }],
                    "response_metadata": { "next_cursor": "" }
                }),
            )
            .with_http(
                "users.conversations",
                json!({
                    "ok": true,
                    "channels": [{
                        "id": "C1",
                        "name": "dev",
                        "last_read": "1.0",
                        "latest": { "ts": "1.0" }
                    }],
                    "response_metadata": { "next_cursor": "page-2" }
                }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": true, "messages": [{ "ts": "2.0", "text": "page two" }] }),
            );
        let out = plugin().call("slack.unreads", json!({}), &mut h).unwrap();
        assert_eq!(out["scanned"], 2);
        assert_eq!(out["count"], 1);
        assert_eq!(out["channels"][0]["id"], "C2");
    }

    #[test]
    fn unreads_does_not_treat_missing_last_read_as_empty() {
        let mut h = host().with_http(
            "users.conversations",
            json!({ "ok": true, "channels": [{
                "id": "C1",
                "name": "dev",
                "latest": { "ts": "2.0" }
            }] }),
        );
        let out = plugin().call("slack.unreads", json!({}), &mut h).unwrap();
        assert_eq!(out["count"], 0);
        assert_eq!(out["skipped"][0]["id"], "C1");
        assert_eq!(out["skipped"][0]["reason"], "missing_last_read");
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
    fn message_send_blocks_requires_no_text_fails_without_fallback() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "1.0" }));
        let err = plugin()
            .call(
                "slack.message.send",
                json!({
                    "channel": "C1",
                    "blocks": [{ "type": "divider" }]
                }),
                &mut h,
            )
            .unwrap_err();
        assert!(err.contains("text fallback"), "got: {err}");
    }

    #[test]
    fn message_send_blocks_and_text_posts_blocks() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "1.0" }));
        let out = plugin()
            .call(
                "slack.message.send",
                json!({
                    "channel": "C1",
                    "text": "fallback",
                    "blocks": [{ "type": "divider" }],
                    "unfurl_links": false,
                    "unfurl_media": false,
                    "parse": "none"
                }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ts"], "1.0");
    }

    #[test]
    fn message_send_markdown_posts_mrkdwn_block() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "1.0" }));
        let out = plugin()
            .call(
                "slack.message.send",
                json!({"channel": "C1", "markdown": "hello *world*" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ts"], "1.0");
    }

    #[test]
    fn message_edit_blocks_without_text_is_rejected() {
        let mut h = host().with_http("chat.update", json!({ "ok": true, "ts": "1.0" }));
        let err = plugin()
            .call(
                "slack.message.edit",
                json!({ "ref": "C1:1.0", "blocks": [{ "type": "divider" }] }),
                &mut h,
            )
            .unwrap_err();
        assert!(err.contains("text fallback"), "got: {err}");
    }

    #[test]
    fn message_list_text_format_mrkdwn_keeps_raw() {
        let mut h = host().with_http(
            "conversations.history",
            json!({ "ok": true, "messages": [{ "ts": "1.1", "text": "<https://x|link> plain" }] }),
        );
        let out = plugin()
            .call(
                "slack.message.list",
                json!({ "channel": "C1", "text_format": "mrkdwn" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"][0]["text"], "<https://x|link> plain");
        assert!(out["messages"][0]["text_mrkdwn"].is_null());
    }

    #[test]
    fn thread_text_format_default_renders_markdown() {
        let mut h = host().with_http(
            "conversations.replies",
            json!({ "ok": true, "messages": [{ "ts": "1.1", "text": "<https://x|link>" }] }),
        );
        let out = plugin()
            .call(
                "slack.thread",
                json!({ "channel": "C1", "ts": "1.0" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"][0]["text"], "[link](https://x)");
    }

    #[test]
    fn search_extracts_tickets() {
        let mut h = host().with_http(
            "search.messages",
            json!({
                "ok": true,
                "messages": {
                    "total": 1,
                    "matches": [{
                        "text": "PROJ-123 is fixed",
                        "permalink": "https://acme.slack.com/archives/C1/p1"
                    }]
                }
            }),
        );
        let out = plugin()
            .call(
                "slack.search",
                json!({ "query": "PROJ", "tickets": true, "ticket_keys": ["PROJ"] }),
                &mut h,
            )
            .unwrap();
        assert_eq!(
            out["messages"]["matches"][0]["tickets"],
            json!(["PROJ-123"])
        );
        let tickets = out["tickets"].as_array().unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0]["key"], "PROJ-123");
        assert_eq!(tickets[0]["mentions"], 1);
    }

    #[test]
    fn mentions_uses_bot_identity_when_requested() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_bot> look",
                    "permalink": "https://acme.slack.com/archives/C1/p1001000000",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_bot" }));
        let out = plugin()
            .call("slack.mentions", json!({ "bot": true }), &mut h)
            .unwrap();
        assert_eq!(out["target"], "U_bot");
        assert_eq!(out["count"], 1);
    }

    #[test]
    fn mentions_ticket_keys_are_strings() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "dev-5 ABC-9",
                    "permalink": "https://x",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "tickets": true, "ticket_keys": ["DEV", "abc"] }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["mentions"][0]["tickets"], json!(["ABC-9", "DEV-5"]));
    }

    #[test]
    fn file_upload_content_bytes_decodes_inline_base64() {
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
        let content = b"hello bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let out = plugin()
            .call(
                "slack.file.upload",
                json!({
                    "channel": "C1",
                    "content_bytes": b64,
                    "filename": "hello.txt",
                    "alt_text": "chart"
                }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["filename"], "hello.txt");
        assert_eq!(out["size"], content.len());
    }

    #[test]
    fn file_upload_requires_exactly_one_content_source() {
        let mut h = host().with_http("files.getUploadURLExternal", json!({ "ok": false }));
        let err = plugin()
            .call(
                "slack.file.upload",
                json!({
                    "channel": "C1",
                    "blob_ref": "blob-1",
                    "content_bytes": "aGVsbG8="
                }),
                &mut h,
            )
            .unwrap_err();
        assert!(
            err.contains("exactly one of blob_ref or content_bytes"),
            "got: {err}"
        );
    }

    #[test]
    fn file_download_blob_ref_seed_returns_prefixed_ref() {
        let mut h = host()
            .with_http(
                "files.info",
                json!({ "ok": true, "file": { "id": "F1", "name": "a.txt", "url_private_download": "https://files.slack.test/dl" } }),
            )
            .with_http_bytes("files.slack.test/dl", b"data".to_vec());
        let out = plugin()
            .call(
                "slack.file.download",
                json!({ "file_id": "F1", "blob_ref": "myprefix" }),
                &mut h,
            )
            .unwrap();
        let blob_ref = out["blob_ref"].as_str().unwrap();
        assert!(blob_ref.starts_with("myprefix"), "got: {blob_ref}");
    }

    #[test]
    fn file_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "files.list",
            json!({
                "ok": true,
                "files": [
                    { "id": "F1", "name": "foo.txt" },
                    { "id": "F2", "name": "bar.txt" },
                    { "id": "F3", "name": "fooagain.txt" }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.file.list",
                json!({ "query": "foo", "limit": 2 }),
                &mut h,
            )
            .unwrap();
        let files = out["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .all(|f| f["name"].as_str().unwrap().contains("foo")));
    }

    #[test]
    fn channel_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "conversations.list",
            json!({
                "ok": true,
                "channels": [
                    { "id": "C1", "name": "team-alpha" },
                    { "id": "C2", "name": "team-beta" },
                    { "id": "C3", "name": "alpha-2" }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.channel.list",
                json!({ "query": "alpha", "limit": 1 }),
                &mut h,
            )
            .unwrap();
        let channels = out["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 1);
        assert!(channels[0]["name"].as_str().unwrap().contains("alpha"));
    }

    #[test]
    fn user_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "users.list",
            json!({
                "ok": true,
                "members": [
                    { "id": "U1", "name": "alice", "profile": { "real_name": "Alice A" } },
                    { "id": "U2", "name": "bob", "profile": { "real_name": "Bob B" } },
                    { "id": "U3", "name": "alicia", "profile": { "real_name": "Alicia C" } }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.user.list",
                json!({ "query": "ali", "limit": 2 }),
                &mut h,
            )
            .unwrap();
        let members = out["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn bookmark_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "bookmarks.list",
            json!({
                "ok": true,
                "bookmarks": [
                    { "id": "B1", "title": "Alpha docs", "link": "https://a" },
                    { "id": "B2", "title": "Beta docs", "link": "https://b" },
                    { "id": "B3", "title": "Gamma runbook", "link": "https://g" }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.bookmark.list",
                json!({ "channel": "C1", "query": "docs", "limit": 1 }),
                &mut h,
            )
            .unwrap();
        let bookmarks = out["bookmarks"].as_array().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert!(bookmarks[0]["title"].as_str().unwrap().contains("docs"));
    }

    #[test]
    fn emoji_list_custom_mode_and_query_and_limit() {
        let mut h = host().with_http(
            "emoji.list",
            json!({
                "ok": true,
                "emoji": {
                    "party": "https://x",
                    "partyparrot": "alias:party",
                    "work": "https://y"
                }
            }),
        );
        let out = plugin()
            .call(
                "slack.emoji.list",
                json!({ "query": "party", "limit": 1 }),
                &mut h,
            )
            .unwrap();
        let emoji = out["emoji"].as_array().unwrap();
        assert_eq!(emoji.len(), 1);
        assert_eq!(emoji[0]["name"], "party");
    }

    #[test]
    fn emoji_list_include_aliases_shows_alias_entry() {
        let mut h = host().with_http(
            "emoji.list",
            json!({
                "ok": true,
                "emoji": {
                    "party": "https://x",
                    "partyparrot": "alias:party"
                }
            }),
        );
        let out = plugin()
            .call(
                "slack.emoji.list",
                json!({ "include_aliases": true }),
                &mut h,
            )
            .unwrap();
        let emoji = out["emoji"].as_array().unwrap();
        assert!(emoji
            .iter()
            .any(|e| e["name"] == "partyparrot" && e["alias_for"] == "party"));
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

// ===========================================================================
// D-36: schema-derivation contract test (slack).
// Each op's `input_schema` is schemars-derived (`read_op_typed::<T>` /
// `write_op_typed::<T>`) instead of an inline `json!({"type":"object",...})`
// literal. Asserts the derived schema's fields/required/base-types match the
// legacy inline contract (transcribed pre-migration). A change here is a real
// contract change.
// ===========================================================================
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        ArrayAny,
        ArrayStr,
    }
    #[derive(Clone)]
    struct Prop {
        name: &'static str,
        kind: Kind,
    }
    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }
    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }
    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }
    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            ("slack.test", c(vec![], vec![])),
            ("slack.info", c(vec![], vec![])),
            (
                "slack.message.send",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("text", Kind::Str),
                        p("markdown", Kind::Str),
                        p("blocks", Kind::ArrayAny),
                        p("thread_ts", Kind::Str),
                        p("reply_broadcast", Kind::Bool),
                        p("unfurl_links", Kind::Bool),
                        p("unfurl_media", Kind::Bool),
                        p("parse", Kind::Str),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.message.list",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("limit", Kind::Int),
                        p("cursor", Kind::Str),
                        p("oldest", Kind::Str),
                        p("latest", Kind::Str),
                        p("text_format", Kind::Str),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.message.edit",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("text", Kind::Str),
                        p("markdown", Kind::Str),
                        p("blocks", Kind::ArrayAny),
                        p("unfurl_links", Kind::Bool),
                        p("unfurl_media", Kind::Bool),
                        p("parse", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.message.delete",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.thread",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("limit", Kind::Int),
                        p("max_bytes", Kind::Int),
                        p("text_format", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.search",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                        p("tickets", Kind::Bool),
                        p("ticket_keys", Kind::ArrayStr),
                    ],
                    vec!["query"],
                ),
            ),
            (
                "slack.mentions",
                c(
                    vec![
                        p("user", Kind::Str),
                        p("bot", Kind::Bool),
                        p("since", Kind::Str),
                        p("limit", Kind::Int),
                        p("unhandled", Kind::Bool),
                        p("max_thread", Kind::Int),
                        p("tickets", Kind::Bool),
                        p("ticket_keys", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.unreads",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("since", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.reaction.add",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["emoji"],
                ),
            ),
            (
                "slack.reaction.remove",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["emoji"],
                ),
            ),
            (
                "slack.channel.list",
                c(vec![p("query", Kind::Str), p("limit", Kind::Int)], vec![]),
            ),
            (
                "slack.channel.join",
                c(vec![p("channel", Kind::Str)], vec!["channel"]),
            ),
            (
                "slack.channel.mark-read",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.file.upload",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("blob_ref", Kind::Str),
                        p("content_bytes", Kind::Str),
                        p("filename", Kind::Str),
                        p("thread_ts", Kind::Str),
                        p("initial_comment", Kind::Str),
                        p("alt_text", Kind::Str),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.file.download",
                c(
                    vec![
                        p("file_id", Kind::Str),
                        p("blob_ref", Kind::Str),
                        p("filename", Kind::Str),
                    ],
                    vec!["file_id"],
                ),
            ),
            (
                "slack.download",
                c(
                    vec![
                        p("file_id", Kind::Str),
                        p("blob_ref", Kind::Str),
                        p("filename", Kind::Str),
                    ],
                    vec!["file_id"],
                ),
            ),
            (
                "slack.file.info",
                c(vec![p("file_id", Kind::Str)], vec!["file_id"]),
            ),
            (
                "slack.file.list",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("user", Kind::Str),
                        p("types", Kind::Str),
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.file.delete",
                c(vec![p("file_id", Kind::Str)], vec!["file_id"]),
            ),
            (
                "slack.bookmark.add",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("title", Kind::Str),
                        p("link", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["channel", "title", "link"],
                ),
            ),
            (
                "slack.bookmark.edit",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("bookmark_id", Kind::Str),
                        p("title", Kind::Str),
                        p("link", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["channel", "bookmark_id"],
                ),
            ),
            (
                "slack.bookmark.delete",
                c(
                    vec![p("channel", Kind::Str), p("bookmark_id", Kind::Str)],
                    vec!["channel", "bookmark_id"],
                ),
            ),
            (
                "slack.bookmark.list",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.user.list",
                c(vec![p("query", Kind::Str), p("limit", Kind::Int)], vec![]),
            ),
            ("slack.presence.get", c(vec![p("user", Kind::Str)], vec![])),
            (
                "slack.presence.set",
                c(vec![p("presence", Kind::Str)], vec!["presence"]),
            ),
            (
                "slack.emoji.list",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                        p("mode", Kind::Str),
                        p("include_aliases", Kind::Bool),
                    ],
                    vec![],
                ),
            ),
            ("slack.index.build", c(vec![], vec![])),
        ]
    }
    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }
    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "array" => {
                if node
                    .get("items")
                    .and_then(|v| v.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("string")
                {
                    Kind::ArrayStr
                } else {
                    Kind::ArrayAny
                }
            }
            "string" => Kind::Str,
            other => panic!("unsupported property type: {other}"),
        }
    }
    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        assert_eq!(schema["type"], "object", "{op_name}: root type");
        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                got.insert(k.as_str(), kind_of(v));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got
                .get(*name)
                .unwrap_or_else(|| panic!("{op_name}: missing property `{name}`"));
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }
        let req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut req_set: Vec<&str> = req.clone();
        req_set.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req_set, want_req, "{op_name}: required set");
    }
    #[test]
    fn derived_schemas_match_legacy_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}
