//! Plugin manifest and operation-to-handler catalog.

use super::*;

pub(super) fn manifest_builder() -> PluginBuilder {
    let builder = PluginBuilder::new("slack", env!("CARGO_PKG_VERSION"))
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
            default: Some("https://slack.com/api".into()),
            ..Default::default()
        })
        .datasource(ds("slack.channels", "slack.channel", "Slack channels."))
        .datasource(ds("slack.users", "slack.user", "Slack workspace users."))
        // -- auth / identity ------------------------------------------------
        .operation_flexible(
            read_op_typed::<TestInput>(
                "slack.test",
                "Test Slack user and bot token authentication.",
            ),
            auth_test,
        )
        .operation_flexible(
            read_op_typed::<InfoInput>(
                "slack.info",
                "Show Slack token identity and workspace information.",
            ),
            auth_test,
        )
        // -- messages -------------------------------------------------------
        .operation_flexible(
            write_op_typed::<MessageSendInput>(
                "slack.message.send",
                "Send a message to a channel (channel id or DM channel; optionally as a thread reply).",
            ),
            message_send,
        )
        .operation_flexible(
            read_op_typed::<MessageListInput>(
                "slack.message.list",
                "Read recent messages from a channel (conversations.history); paginate with next_cursor.",
            ),
            message_list,
        )
        .operation_flexible(
            write_op_typed::<MessageEditInput>(
                "slack.message.edit",
                "Edit a Slack message. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
            ),
            message_edit,
        )
        .operation_flexible(
            write_op_typed::<MessageDeleteInput>(
                "slack.message.delete",
                "Delete a Slack message. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
            ),
            message_delete,
        )
        .operation_flexible(
            read_op_typed::<ThreadInput>(
                "slack.thread",
                "View a Slack thread. Provide `ref` (permalink or channel:ts) OR `channel`+`ts`.",
            ),
            thread,
        )
        // -- search / mentions / unreads (user token) -----------------------
        .operation_flexible(
            read_op_typed::<SearchInput>(
                "slack.search",
                "Search Slack messages (search.messages; requires a user token).",
            ),
            search,
        )
        .operation_flexible(
            read_op_typed::<MentionsInput>(
                "slack.mentions",
                "Search Slack mentions of a user and classify whether each was handled (search.messages \\
                 + per-mention thread inspection; requires a user token).",
            ),
            mentions,
        )
        .operation_flexible(
            read_op_typed::<UnreadsInput>(
                "slack.unreads",
                "List Slack conversations with recent (unread) messages (requires a user token).",
            ),
            unreads,
        )
        // -- reactions ------------------------------------------------------
        .operation_flexible(
            write_op_typed::<ReactionAddInput>(
                "slack.reaction.add",
                "Add a reaction to a Slack message. Provide `ref` OR `channel`+`ts`, plus `emoji`.",
            ),
            reaction_add,
        )
        .operation_flexible(
            write_op_typed::<ReactionRemoveInput>(
                "slack.reaction.remove",
                "Remove a reaction from a Slack message. Provide `ref` OR `channel`+`ts`, plus `emoji`.",
            ),
            reaction_remove,
        )
        // -- channels -------------------------------------------------------
        .operation_flexible(
            read_op_typed::<ChannelListInput>(
                "slack.channel.list",
                "List public and private channels (plus group/direct conversations) in the workspace.",
            ),
            channel_list,
        )
        .operation_flexible(
            write_op_typed::<ChannelJoinInput>(
                "slack.channel.join",
                "Join a Slack public channel.",
            ),
            channel_join,
        )
        .operation_flexible(
            write_op_typed::<ChannelMarkReadInput>(
                "slack.channel.mark_read",
                "Mark a Slack channel read through a timestamp. Provide `ref` OR `channel`+`ts`.",
            ),
            channel_mark,
        )
        // -- files (blobs) --------------------------------------------------
        .operation_flexible(
            write_op_typed::<FileUploadInput>(
                "slack.file.upload",
                "Upload a file to a Slack channel, DM, or thread. Bytes come from a host `blob_ref`.",
            ),
            file_upload,
        )
        .operation_flexible(
            write_op_typed::<FileDownloadInput>(
                "slack.file.download",
                "Download a Slack file to a host blob; returns the `blob_ref`.",
            ),
            file_download,
        )
        .operation_flexible(
            write_op_typed::<DownloadInput>(
                "slack.download",
                "Download a Slack file to a host blob; returns the `blob_ref`.",
            ),
            file_download,
        )
        .operation_flexible(
            read_op_typed::<FileInfoInput>(
                "slack.file.info",
                "Show Slack file information.",
            ),
            file_info,
        )
        .operation_flexible(
            read_op_typed::<FileListInput>(
                "slack.file.list",
                "List Slack files (optionally filtered by channel/user/type).",
            ),
            file_list,
        )
        .operation_flexible(
            write_op_typed::<FileDeleteInput>(
                "slack.file.delete",
                "Delete a Slack file.",
            ),
            file_delete,
        )
        // -- bookmarks ------------------------------------------------------
        .operation_flexible(
            write_op_typed::<BookmarkAddInput>(
                "slack.bookmark.add",
                "Add a Slack channel bookmark.",
            ),
            bookmark_add,
        )
        .operation_flexible(
            write_op_typed::<BookmarkEditInput>(
                "slack.bookmark.edit",
                "Edit a Slack channel bookmark.",
            ),
            bookmark_edit,
        )
        .operation_flexible(
            write_op_typed::<BookmarkDeleteInput>(
                "slack.bookmark.delete",
                "Delete a Slack channel bookmark.",
            ),
            bookmark_delete,
        )
        .operation_flexible(
            read_op_typed::<BookmarkListInput>(
                "slack.bookmark.list",
                "List Slack channel bookmarks.",
            ),
            bookmark_list,
        )
        // -- users / presence / emoji ---------------------------------------
        .operation_flexible(
            read_op_typed::<UserListInput>(
                "slack.user.list",
                "List users in the workspace.",
            ),
            user_list,
        )
        .operation_flexible(
            read_op_typed::<PresenceGetInput>(
                "slack.presence.get",
                "Get Slack user presence.",
            ),
            presence_get,
        )
        .operation_flexible(
            write_op_typed::<PresenceSetInput>(
                "slack.presence.set",
                "Set Slack user presence (auto|away; requires a user token).",
            ),
            presence_set,
        )
        .operation_flexible(
            read_op_typed::<EmojiListInput>(
                "slack.emoji.list",
                "List Slack custom emoji.",
            ),
            emoji_list,
        )
        // -- index ----------------------------------------------------------
        .operation_flexible(
            read_op_typed::<IndexBuildInput>(
                "slack.index.build",
                "Build the Slack channel and user reverse-lookup indexes.",
            ),
            index_build,
        );
    let mut tools = builder
        .manifest()
        .operations
        .into_iter()
        .map(|operation| operation.name)
        .collect::<Vec<_>>();
    tools.push(VALIDATE_OP.into());
    builder.group(ToolGroup {
        name: "plugin.slack".into(),
        description: "Slack company-chat messaging, channels, users, files, and search.".into(),
        tools,
        surface_when: ["slack", "chat", "company chat", "team chat", "slack.com"]
            .into_iter()
            .map(|signal| SignalMatch {
                kind: KIND_TURN_INTENT.into(),
                signal: Some(signal.into()),
            })
            .collect(),
    })
}

/// A contributing datasource: searchable, gettable, and feedable by `slack.index.build`.
pub(super) fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}
