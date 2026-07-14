//! Plugin manifest and operation-to-handler catalog.

use super::*;

pub(super) fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("jira", env!("CARGO_PKG_VERSION"))
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["api.atlassian.com".into()],
            private_hosts: vec!["*".into()],
            blob: true,
            secrets: vec!["JIRA_API_TOKEN".into(), "ATLASSIAN_API_TOKEN".into()],
            ..Default::default()
        })
        // PRIMARY (reference): Bearer with the `api_token` purpose. Used against the cloud_id gateway
        // when a cloud_id is configured, and against the site URL otherwise.
        .auth(AuthMethod::bearer(
            "api_token",
            vec!["JIRA_API_TOKEN".into(), "ATLASSIAN_API_TOKEN".into()],
        ))
        // FALLBACK: Basic (email:token) against the site URL — for setups without a cloud_id/OAuth
        // gateway. The email is the username half (config, via user_env); the token the secret half.
        .auth(AuthMethod::basic(
            "basic",
            vec!["JIRA_EMAIL".into(), "ATLASSIAN_EMAIL".into()],
            vec!["JIRA_API_TOKEN".into(), "ATLASSIAN_API_TOKEN".into()],
        ))
        .endpoint(EndpointSpec {
            name: "jira.endpoint".into(),
            env: vec![
                "JIRA_URL".into(),
                "ATLASSIAN_URL".into(),
                "ATLASSIAN_SITE_URL".into(),
            ],
            description: "Jira Cloud site URL (e.g. https://site.atlassian.net)".into(),
            ..Default::default()
        })
        // The OAuth-gateway base, HOST-composed from the `cloud_id` config value — the plugin
        // addresses it by name (`jira.gateway`) and never holds the composed URL.
        .endpoint(EndpointSpec {
            name: "jira.gateway".into(),
            template: Some("https://api.atlassian.com/ex/jira/{cloud_id}".into()),
            http_hosts: vec!["api.atlassian.com".into()],
            description: "Atlassian OAuth gateway base, composed host-side from cloud_id".into(),
            ..Default::default()
        })
        // The cloud_id (gated non-secret config) selects gateway mode + Bearer. Absent → site modes.
        .config(ConfigSpec {
            name: "cloud_id".into(),
            env: vec!["ATLASSIAN_CLOUD_ID".into(), "JIRA_CLOUD_ID".into()],
            description: "Atlassian Cloud ID; when set, calls go through the OAuth gateway".into(),
        })
        // The email (gated non-secret config) selects the Basic fallback when no cloud_id is set.
        .config(ConfigSpec {
            name: "email".into(),
            env: vec!["JIRA_EMAIL".into(), "ATLASSIAN_EMAIL".into()],
            description: "Atlassian account email; enables the Basic auth fallback".into(),
        })
        .datasource(ds("jira.issues", "jira.issue", "Jira issues."))
        .datasource(ds("jira.users", "jira.user", "Jira users."))
        // --- auth + index -------------------------------------------------------------------------
        .operation_flexible(
            read_op_typed::<TestInput>(
                "jira.test",
                "Test Jira authentication by fetching the current user.",
            ),
            auth_test,
        )
        .operation_flexible(
            read_op_typed::<IndexBuildInput>(
                "jira.index.build",
                "Build Jira issue and user index records for reverse lookup.",
            ),
            index_build,
        )
        // --- issue CRUD ---------------------------------------------------------------------------
        .operation_flexible(
            write_op_typed::<IssueCreateInput>(
                "jira.issue.create",
                "Create a Jira issue from structured fields and Markdown. Raw `fields` and `update` \
                 maps are passed through; typed inputs override matching raw fields.",
            ),
            issue_create,
        )
        .operation_flexible(
            write_op_typed::<IssueEditInput>(
                "jira.issue.edit",
                "Edit a Jira issue's structured fields and Markdown, including reparenting via parent_key. \
                 Raw `fields` and `update` maps are passed through; typed inputs override matching raw fields.",
            ),
            issue_edit,
        )
        .operation_flexible(
            write_op_typed::<IssueDeleteInput>(
                "jira.issue.delete",
                "Delete a Jira issue.",
            ),
            issue_delete,
        )
        .operation_flexible(
            read_op_typed::<IssueSearchInput>(
                "jira.issue.search",
                "Search issues with a JQL query (or project/status/query filters).",
            ),
            issue_search,
        )
        .operation_flexible(
            read_op_typed::<IssueShowInput>(
                "jira.issue.show",
                "Show one issue by key (e.g. PROJ-123).",
            ),
            issue_show,
        )
        .operation_flexible(
            read_op_typed::<IssueCreateMetaInput>(
                "jira.issue.create_meta",
                "Show Jira issue create metadata (settable fields per project/issue type).",
            ),
            create_meta,
        )
        .operation_flexible(
            read_op_typed::<IssueEditMetaInput>(
                "jira.issue.edit_meta",
                "Show Jira issue edit metadata (settable fields for one issue).",
            ),
            edit_meta,
        )
        // --- transitions --------------------------------------------------------------------------
        .operation_flexible(
            read_op_typed::<IssueTransitionListInput>(
                "jira.issue.transition.list",
                "Show a Jira issue's current status and currently available transitions.",
            ),
            transition_list,
        )
        .operation_flexible(
            write_op_typed::<IssueTransitionRunInput>(
                "jira.issue.transition.run",
                "Run a Jira issue transition. Provide exactly one of transition_id, transition_name, or \
                 target_status. With auto_transition, walks intermediate transitions until target_status.",
            ),
            transition_run,
        )
        // --- comments -----------------------------------------------------------------------------
        .operation_flexible(
            write_op_typed::<CommentAddInput>(
                "jira.issue.comment.add",
                "Add a Markdown comment to a Jira issue.",
            ),
            comment_add,
        )
        .operation_flexible(
            write_op_typed::<CommentEditInput>(
                "jira.issue.comment.edit",
                "Edit a Jira issue comment with Markdown.",
            ),
            comment_edit,
        )
        .operation_flexible(
            write_op_typed::<CommentDeleteInput>(
                "jira.issue.comment.delete",
                "Delete a Jira issue comment.",
            ),
            comment_delete,
        )
        .operation_flexible(
            read_op_typed::<CommentListInput>(
                "jira.issue.comment.list",
                "List comments on a Jira issue as Markdown, with raw ADF available via body_format.",
            ),
            comment_list,
        )
        // --- attachments (blob, byte-exact via http_bytes) ----------------------------------------
        .operation_flexible(
            write_op_typed::<AttachmentAddInput>(
                "jira.issue.attachment.add",
                "Upload an attachment to a Jira issue. Provide exactly one of blob_ref or content_bytes.",
            ),
            attachment_add,
        )
        .operation_typed::<AttachmentGetInput, AttachmentGetOutput>(
            read_op_typed::<AttachmentGetInput>(
                "jira.issue.attachment.get",
                "Download a Jira attachment into the host blob store and return its ref.",
            ),
            attachment_get,
        )
        .operation_typed::<AttachmentListInput, AttachmentListOutput>(
            read_op_typed::<AttachmentListInput>(
                "jira.issue.attachment.list",
                "List a Jira issue's attachments.",
            ),
            attachment_list,
        )
        .operation_flexible(
            write_op_typed::<AttachmentDeleteInput>(
                "jira.issue.attachment.delete",
                "Delete a Jira issue attachment.",
            ),
            attachment_delete,
        )
        // --- links + users ------------------------------------------------------------------------
        .operation_flexible(
            write_op_typed::<IssueLinkAddInput>(
                "jira.issue.link.add",
                "Link two Jira issues (key <type-verb> to_key, e.g. DEV-1 blocks DEV-2 with type Blocks). \
                 Returns the issue's links read back from Jira so the new link is verified.",
            ),
            issue_link_add,
        )
        .operation_flexible(
            read_op_typed::<UserSearchInput>(
                "jira.user.search",
                "Search Jira users.",
            ),
            user_search,
        )
}

pub(super) fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}
