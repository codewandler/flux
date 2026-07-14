//! Schemars-derived input contracts for the Slack operation catalog.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

// ─── op input schemas (D-36) ───────────────────────────────────────────────
// Each op's `input_schema` is schemars-derived (`host_kit::read_op_typed::<T>` /
// `write_op_typed::<T>`) instead of an inline `json!({"type":"object",...})` literal,
// so the schema cannot drift. The structs are schema-only: handlers keep their
// existing `opt_str`/`Value` extraction (D-34 precedent).
/// `slack.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct TestInput {}

/// `slack.info`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct InfoInput {}

/// `slack.message.send`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MessageSendInput {
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
pub(super) struct MessageListInput {
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
pub(super) struct MessageEditInput {
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
pub(super) struct MessageDeleteInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
}

/// `slack.thread`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ThreadInput {
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
pub(super) struct SearchInput {
    query: String,
    limit: Option<i64>,
    tickets: Option<bool>,
    ticket_keys: Option<Vec<String>>,
}

/// `slack.mentions`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MentionsInput {
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
pub(super) struct UnreadsInput {
    channel: Option<String>,
    since: Option<String>,
    limit: Option<i64>,
}

/// `slack.reaction.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReactionAddInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    emoji: String,
}

/// `slack.reaction.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReactionRemoveInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    emoji: String,
}

/// `slack.channel.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ChannelListInput {
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.channel.join`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ChannelJoinInput {
    channel: String,
}

/// `slack.channel.mark_read`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ChannelMarkReadInput {
    r#ref: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
}

/// `slack.file.upload`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct FileUploadInput {
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
pub(super) struct FileDownloadInput {
    file_id: String,
    blob_ref: Option<String>,
    filename: Option<String>,
}

/// `slack.download`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct DownloadInput {
    file_id: String,
    blob_ref: Option<String>,
    filename: Option<String>,
}

/// `slack.file.info`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct FileInfoInput {
    file_id: String,
}

/// `slack.file.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct FileListInput {
    channel: Option<String>,
    user: Option<String>,
    types: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.file.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct FileDeleteInput {
    file_id: String,
}

/// `slack.bookmark.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BookmarkAddInput {
    channel: String,
    title: String,
    link: String,
    emoji: Option<String>,
}

/// `slack.bookmark.edit`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BookmarkEditInput {
    channel: String,
    bookmark_id: String,
    title: Option<String>,
    link: Option<String>,
    emoji: Option<String>,
}

/// `slack.bookmark.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BookmarkDeleteInput {
    channel: String,
    bookmark_id: String,
}

/// `slack.bookmark.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BookmarkListInput {
    channel: String,
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.user.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct UserListInput {
    query: Option<String>,
    limit: Option<i64>,
}

/// `slack.presence.get`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct PresenceGetInput {
    user: Option<String>,
}

/// `slack.presence.set`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct PresenceSetInput {
    presence: String,
}

/// `slack.emoji.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct EmojiListInput {
    query: Option<String>,
    limit: Option<i64>,
    mode: Option<String>,
    include_aliases: Option<bool>,
}

/// `slack.index.build`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IndexBuildInput {}
