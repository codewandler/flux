//! Schemars-derived input contracts for the Jira operation catalog.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ===========================================================================
// Schema-only op input structs (D-36)
// ===========================================================================
// Each op's `input_schema` is derived from the structs below via schemars
// (`host_kit::read_op_typed::<T>` / `host_kit::write_op_typed::<T>`), instead of a hand-written
// `json!({...})` object, so the schema the model sees cannot drift from a separately-maintained
// literal. The structs are schema-only: handlers keep their existing `opt_str` / `clamp_limit`
// / `issue_key` extractors (the D-34 schema-only precedent).

/// How rich-text bodies (issue descriptions, comments) are rendered. The default keeps agents
/// away from raw ADF by returning readable Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum BodyFormat {
    /// Render rich-text bodies as Markdown (default).
    Markdown,
    /// Return the raw Atlassian Document Format object.
    Adf,
    /// Return both Markdown (`description` / `body`) and the raw ADF object
    /// (`description_adf` / `body_adf`).
    Both,
}

impl BodyFormat {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "adf" => BodyFormat::Adf,
            "both" => BodyFormat::Both,
            _ => BodyFormat::Markdown,
        }
    }
}

pub(super) fn body_format_from_input(input: &Value) -> BodyFormat {
    match input.get("body_format").and_then(|v| v.as_str()) {
        Some(s) => BodyFormat::parse(s),
        None => BodyFormat::Markdown,
    }
}

/// `jira.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct TestInput {}

/// `jira.index.build`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IndexBuildInput {
    /// Issue JQL query.
    issue_jql: Option<String>,
    /// Issue text query.
    issue_query: Option<String>,
    /// Issue page size (max 100).
    issue_limit: Option<i64>,
    /// Issue project key filter.
    project: Option<String>,
    /// Issue status filter.
    status: Option<String>,
    /// User search query.
    user_query: Option<String>,
    /// User page size (max 100).
    user_limit: Option<i64>,
}

/// `jira.issue.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueCreateInput {
    /// Project key such as DEV.
    project_key: String,
    /// Alias for project_key.
    project: Option<String>,
    /// Issue type name such as Task or Bug.
    issue_type: String,
    /// Issue summary.
    summary: String,
    /// Description as Markdown (converted to Jira ADF).
    description_markdown: Option<String>,
    /// Labels to set.
    labels: Option<Vec<String>>,
    /// Assignee Atlassian account ID.
    assignee_account_id: Option<String>,
    /// Reporter Atlassian account ID.
    reporter_account_id: Option<String>,
    /// Priority name.
    priority: Option<String>,
    /// Parent issue key for subtasks.
    parent_key: Option<String>,
    /// Raw Jira fields. Explicit typed inputs override matching fields.
    fields: Option<Map<String, Value>>,
    /// Raw Jira update instructions.
    update: Option<Map<String, Value>>,
}

/// `jira.issue.edit`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueEditInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Issue summary.
    summary: Option<String>,
    /// Description as Markdown (converted to Jira ADF).
    description_markdown: Option<String>,
    /// Labels to set.
    labels: Option<Vec<String>>,
    /// Assignee Atlassian account ID.
    assignee_account_id: Option<String>,
    /// Priority name.
    priority: Option<String>,
    /// Parent issue key to reparent under.
    parent_key: Option<String>,
    /// Raw Jira fields. Explicit typed inputs override matching fields.
    fields: Option<Map<String, Value>>,
    /// Raw Jira update instructions.
    update: Option<Map<String, Value>>,
}

/// `jira.issue.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueDeleteInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Also delete subtasks when deleting a parent issue.
    delete_subtasks: Option<bool>,
}

/// `jira.issue.search`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueSearchInput {
    /// JQL query.
    jql: Option<String>,
    /// Project key filter.
    project: Option<String>,
    /// Status filter.
    status: Option<String>,
    /// Free-text filter (JQL `text ~`).
    query: Option<String>,
    /// JQL order-by expression (default `updated DESC`).
    order_by: Option<String>,
    /// Max results (default 25, cap 100).
    max: Option<i64>,
    /// Jira fields to request on issues.
    fields: Option<Vec<String>>,
    /// Rich-text body format for issue descriptions.
    body_format: Option<BodyFormat>,
}

/// `jira.issue.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueShowInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Rich-text body format for the issue description.
    body_format: Option<BodyFormat>,
}

/// `jira.issue.create_meta`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueCreateMetaInput {
    /// Project key filter.
    project_key: Option<String>,
    /// Issue type name filter.
    issue_type: Option<String>,
}

/// `jira.issue.edit_meta`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueEditMetaInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
}

/// `jira.issue.transition.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueTransitionListInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
}

/// `jira.issue.transition.run`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueTransitionRunInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Jira transition ID to apply.
    transition_id: Option<String>,
    /// Jira transition name to apply.
    transition_name: Option<String>,
    /// Desired status name or ID.
    target_status: Option<String>,
    /// Take intermediate transitions to reach target_status.
    auto_transition: Option<bool>,
    /// Maximum transitions for auto_transition (default 5, max 20).
    max_steps: Option<i64>,
}

/// `jira.issue.comment.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CommentAddInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Comment body as Markdown (converted to Jira ADF).
    body_markdown: String,
}

/// `jira.issue.comment.edit`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CommentEditInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Jira comment ID.
    comment_id: String,
    /// Comment body as Markdown (converted to Jira ADF).
    body_markdown: String,
}

/// `jira.issue.comment.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CommentDeleteInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Jira comment ID.
    comment_id: String,
}

/// `jira.issue.comment.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CommentListInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Max comments (default 20, cap 100).
    limit: Option<i64>,
    /// Zero-based pagination offset.
    start_at: Option<i64>,
    /// Sort order by creation time: `created` (oldest first) or `-created` (newest first).
    order: Option<String>,
    /// Rich-text body format for comments.
    body_format: Option<BodyFormat>,
}

/// `jira.issue.attachment.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct AttachmentAddInput {
    /// Issue key (e.g. PROJ-123).
    key: String,
    /// Alias for key.
    id: Option<String>,
    /// Alias for key.
    issue_key: Option<String>,
    /// Host blob ref to upload. Mutually exclusive with content_bytes.
    blob_ref: Option<String>,
    /// Base64-encoded inline bytes. Mutually exclusive with blob_ref.
    content_bytes: Option<String>,
    /// Filename shown in Jira.
    filename: Option<String>,
    /// Attachment MIME type.
    content_type: Option<String>,
}

/// `jira.issue.attachment.list`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AttachmentListInput {
    /// Issue key (e.g. PROJ-123).
    pub(super) key: String,
    /// Compatibility alias for key.
    pub(super) id: Option<String>,
    /// Compatibility alias for key.
    pub(super) issue_key: Option<String>,
}

/// `jira.issue.attachment.get`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AttachmentGetInput {
    /// Jira attachment ID.
    pub(super) attachment_id: String,
    /// Optional filename metadata.
    pub(super) filename: Option<String>,
    /// Optional MIME type metadata.
    pub(super) mime_type: Option<String>,
    /// Optional host blob ref for downloaded attachment bytes.
    pub(super) blob_ref: Option<String>,
}

/// Stable result envelope for `jira.issue.attachment.list`. Jira attachment objects are retained
/// verbatim because Atlassian adds vendor fields over time; the list envelope itself is typed.
#[derive(Serialize, JsonSchema)]
pub(super) struct AttachmentListOutput {
    pub(super) issue_key: String,
    pub(super) count: usize,
    pub(super) attachments: Vec<Value>,
}

/// Stable result contract for `jira.issue.attachment.get`.
#[derive(Serialize, JsonSchema)]
pub(super) struct AttachmentGetOutput {
    pub(super) id: String,
    pub(super) filename: String,
    pub(super) mime_type: String,
    pub(super) size: usize,
    pub(super) blob_ref: String,
}

/// `jira.issue.attachment.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct AttachmentDeleteInput {
    /// Jira attachment ID.
    attachment_id: String,
}

/// `jira.issue.link.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueLinkAddInput {
    /// Issue key on the verb side of the link (the blocker in Blocks).
    key: String,
    /// Issue key the verb points at (the blocked issue in Blocks).
    to_key: String,
    /// Link type name such as Blocks or Relates.
    r#type: String,
}

/// `jira.user.search`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct UserSearchInput {
    /// User search query.
    query: Option<String>,
    /// Max users (default 20, cap 100).
    limit: Option<i64>,
}
