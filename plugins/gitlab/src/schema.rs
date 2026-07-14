//! Schemars-derived input contracts for the GitLab operation catalog.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

// ─── op input schemas (D-36) ───────────────────────────────────────────────
// Each op's `input_schema` is schemars-derived (`host_kit::read_op_typed::<T>` /
// `write_op_typed::<T>`) instead of a hand-written `so(json!{...}, json![...])` literal,
// so the schema the model sees cannot drift. Most structs remain schema-only while their bounded
// executable migrations are pending; C-74 makes the project/MR/issue list+show contracts live.
// The enum/bound/typed-element constraints below are enforced by host-kit's shared
// preflight in BOTH `--dry-run` and runtime dispatch (D-88), so a green dry-run can
// no longer fail the same check at runtime.

/// Issue state filter (GL-011).
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum IssueStateFilter {
    Opened,
    Closed,
    All,
}

impl IssueStateFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Closed => "closed",
            Self::All => "all",
        }
    }
}

/// Project/snippet visibility level (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum Visibility {
    Private,
    Internal,
    Public,
}

/// Release asset link type (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum LinkType {
    Other,
    Runbook,
    Image,
    Package,
}

/// CI/pipeline variable type (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(super) enum VariableType {
    EnvVar,
    File,
}

/// Merge-request state filter (GL-038).
#[derive(Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum MrStateFilter {
    Opened,
    Closed,
    Locked,
    Merged,
    All,
}

impl MrStateFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Closed => "closed",
            Self::Locked => "locked",
            Self::Merged => "merged",
            Self::All => "all",
        }
    }
}

/// CI job status scope entry (GL-033) — typed so a non-string or unknown entry is rejected
/// instead of silently skipped.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(super) enum JobScope {
    Created,
    Pending,
    Running,
    Failed,
    Success,
    Canceled,
    Skipped,
    WaitingForResource,
    Manual,
}

/// Repository archive format (GL-022) — a closed set, so an arbitrary string is never
/// interpolated into the archive URL/filename.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) enum ArchiveFormat {
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "tar.bz2")]
    TarBz2,
    #[serde(rename = "tbz")]
    Tbz,
    #[serde(rename = "tbz2")]
    Tbz2,
    #[serde(rename = "tb2")]
    Tb2,
    #[serde(rename = "bz2")]
    Bz2,
    #[serde(rename = "tar")]
    Tar,
    #[serde(rename = "zip")]
    Zip,
}

/// One `gitlab.repository.commit.create` action (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(super) struct CommitAction {
    action: CommitActionKind,
    file_path: String,
    /// File content (`create`/`update`).
    content: Option<String>,
    /// Source path (`move`).
    previous_path: Option<String>,
    /// `text` (default) or `base64`.
    encoding: Option<ContentEncoding>,
    execute_filemode: Option<bool>,
    last_commit_id: Option<String>,
}

/// A commit action verb (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum CommitActionKind {
    Create,
    Delete,
    Move,
    Update,
    Chmod,
}

/// Commit-action content encoding (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub(super) enum ContentEncoding {
    Text,
    Base64,
}

/// One `gitlab.snippet.create` file (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(super) struct SnippetFile {
    file_path: String,
    content: String,
}

/// One `gitlab.pipeline.create` variable (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(super) struct PipelineVariable {
    key: String,
    value: Option<Value>,
    variable_type: Option<VariableType>,
}

/// `gitlab.project.list`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectListInput {
    pub(crate) search: Option<String>,
    pub(crate) query: Option<String>,
    pub(crate) order_by: Option<String>,
    pub(crate) sort: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub(crate) limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    pub(crate) per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    pub(crate) page: Option<i64>,
    pub(crate) membership: Option<bool>,
    /// Feed the results into the local search index as `gitlab.project` records (GL-015). Off by
    /// default so a plain read is a pure read with no datasource side effects; use `index.build`
    /// (or pass `contribute=true`) to index deliberately.
    pub(crate) contribute: Option<bool>,
}

/// `gitlab.project.show`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectShowInput {
    #[serde(alias = "project_id", alias = "path")]
    pub(crate) project: String,
}

/// `gitlab.mr.list`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MrListInput {
    #[serde(alias = "project_id", alias = "path")]
    pub(crate) project: String,
    pub(crate) state: Option<MrStateFilter>,
    pub(crate) search: Option<String>,
    pub(crate) query: Option<String>,
    pub(crate) order_by: Option<String>,
    pub(crate) sort: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub(crate) limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    pub(crate) per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    pub(crate) page: Option<i64>,
    pub(crate) source_branch: Option<String>,
    pub(crate) target_branch: Option<String>,
    /// Feed the results into the local search index as `gitlab.merge_request` records (GL-015).
    /// Off by default so a plain read has no datasource side effects; `index.build` indexes
    /// deliberately.
    pub(crate) contribute: Option<bool>,
}

/// `gitlab.mr.show`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MrShowInput {
    #[serde(alias = "id")]
    pub(crate) r#ref: Option<String>,
    #[serde(alias = "project_id", alias = "path")]
    pub(crate) project: Option<String>,
    #[schemars(range(min = 1))]
    #[serde(alias = "merge_request_iid")]
    pub(crate) iid: Option<i64>,
}

/// `gitlab.issue.list`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct IssueListInput {
    #[serde(alias = "project_id", alias = "path")]
    pub(crate) project: String,
    pub(crate) state: Option<IssueStateFilter>,
    pub(crate) search: Option<String>,
    pub(crate) query: Option<String>,
    pub(crate) order_by: Option<String>,
    pub(crate) sort: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub(crate) limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    pub(crate) per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    pub(crate) page: Option<i64>,
    /// Feed the results into the local search index as `gitlab.issue` records (GL-015). Off by
    /// default so a plain read has no datasource side effects; `index.build` indexes deliberately.
    pub(crate) contribute: Option<bool>,
}

// C-74 output contracts. GitLab evolves these objects by adding fields, so each executable output
// retains the vendor object as an exact map. `schemars(with = ...)` projects the stable fields flux
// consumes without making the open vendor tail disappear from either the schema or the result.

#[derive(JsonSchema)]
#[allow(dead_code)]
struct GitLabProjectSchema {
    id: Option<i64>,
    name: Option<String>,
    path: Option<String>,
    path_with_namespace: Option<String>,
    name_with_namespace: Option<String>,
    description: Option<String>,
    web_url: Option<String>,
    default_branch: Option<String>,
    visibility: Option<String>,
    archived: Option<bool>,
    last_activity_at: Option<String>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub(super) struct GitLabProject(
    #[schemars(with = "GitLabProjectSchema")] pub(crate) Map<String, Value>,
);

#[derive(JsonSchema)]
#[allow(dead_code)]
struct GitLabMergeRequestSchema {
    id: Option<i64>,
    iid: Option<i64>,
    project_id: Option<i64>,
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    draft: Option<bool>,
    web_url: Option<String>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    merged_at: Option<String>,
    closed_at: Option<String>,
    labels: Option<Vec<String>>,
    author: Option<Value>,
    assignees: Option<Vec<Value>>,
    references: Option<Value>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub(super) struct GitLabMergeRequest(
    #[schemars(with = "GitLabMergeRequestSchema")] pub(crate) Map<String, Value>,
);

#[derive(JsonSchema)]
#[allow(dead_code)]
struct GitLabIssueSchema {
    id: Option<i64>,
    iid: Option<i64>,
    project_id: Option<i64>,
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    confidential: Option<bool>,
    web_url: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    closed_at: Option<String>,
    labels: Option<Vec<String>>,
    author: Option<Value>,
    assignees: Option<Vec<Value>>,
    references: Option<Value>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub(super) struct GitLabIssue(
    #[schemars(with = "GitLabIssueSchema")] pub(crate) Map<String, Value>,
);

pub(super) type ProjectListOutput = Vec<GitLabProject>;
pub(super) type ProjectShowOutput = GitLabProject;
pub(super) type MrListOutput = Vec<GitLabMergeRequest>;
pub(super) type MrShowOutput = GitLabMergeRequest;
pub(super) type IssueListOutput = Vec<GitLabIssue>;
pub(super) type IssueShowOutput = GitLabIssue;

/// `gitlab.pipeline.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct PipelineListInput {
    project: String,
    status: Option<String>,
    r#ref: Option<String>,
    source: Option<String>,
    username: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct TestInput {}

/// `gitlab.index.build`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IndexBuildInput {
    index: Option<String>,
    indexes: Option<Vec<String>>,
    entity: Option<String>,
    entities: Option<Vec<String>>,
    #[schemars(range(min = 1))]
    limit: Option<i64>,
    search: Option<String>,
    query: Option<String>,
    order_by: Option<String>,
    sort: Option<String>,
    membership: Option<bool>,
    /// Preview the crawl breadth WITHOUT indexing (GL-017): returns which datasources would be
    /// crawled and each one's scope (project vs instance-wide), contributing no records. Run this
    /// first when calling with no selectors, so a broad instance-wide crawl is never silent.
    estimate: Option<bool>,
    /// Scope issue indexing to one project (GL-040), matching `mr_project` for merge requests;
    /// falls back to `project`. Without it, issues are crawled instance-wide.
    issue_project: Option<String>,
    #[schemars(range(min = 1))]
    issue_limit: Option<i64>,
    issue_search: Option<String>,
    issue_state: Option<String>,
    issue_order_by: Option<String>,
    issue_sort: Option<String>,
    mr_project: Option<String>,
    #[schemars(range(min = 1))]
    mr_limit: Option<i64>,
    mr_search: Option<String>,
    mr_state: Option<String>,
    mr_order_by: Option<String>,
    mr_sort: Option<String>,
}

/// `gitlab.project.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ProjectCreateInput {
    name: String,
    path: Option<String>,
    namespace: Option<String>,
    description: Option<String>,
    visibility: Option<Visibility>,
    initialize_with_readme: Option<bool>,
}

/// `gitlab.project.delete` (GL-001) — a destructive, plugin-native repo-lifecycle counterpart to
/// `project.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ProjectDeleteInput {
    project: String,
    /// Fat-finger guard: when set it must equal `project` (the path/id you passed) or the delete
    /// is refused.
    confirm_path: Option<String>,
    /// Fat-finger guard: when set it must equal the project's resolved numeric id or the delete is
    /// refused.
    #[schemars(range(min = 1))]
    confirm_project_id: Option<i64>,
}

/// `gitlab.mr.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrCreateInput {
    project: String,
    title: String,
    source_branch: String,
    target_branch: String,
    description: Option<String>,
    labels: Option<Vec<String>>,
    #[schemars(range(min = 1))]
    assignee_id: Option<i64>,
    assignee_ids: Option<Vec<i64>>,
    reviewer_ids: Option<Vec<i64>>,
    #[schemars(range(min = 1))]
    target_project_id: Option<i64>,
    #[schemars(range(min = 1))]
    milestone_id: Option<i64>,
    remove_source_branch: Option<bool>,
    squash: Option<bool>,
    allow_collaboration: Option<bool>,
}

/// `gitlab.mr.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrUpdateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    title: Option<String>,
    description: Option<String>,
    target_branch: Option<String>,
    state_event: Option<String>,
    labels: Option<Vec<String>>,
}

/// `gitlab.mr.approve`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrApproveInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    sha: Option<String>,
}

/// `gitlab.mr.merge`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrMergeInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    auto_merge: Option<bool>,
    merge_commit_message: Option<String>,
    squash_commit_message: Option<String>,
    squash: Option<bool>,
    should_remove_source_branch: Option<bool>,
    remove_source_branch: Option<bool>,
    sha: Option<String>,
}

/// `gitlab.issue.show`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct IssueShowInput {
    #[serde(alias = "id")]
    pub(crate) r#ref: Option<String>,
    #[serde(alias = "project_id", alias = "path")]
    pub(crate) project: Option<String>,
    #[schemars(range(min = 1))]
    #[serde(alias = "issue_iid")]
    pub(crate) iid: Option<i64>,
}

/// `gitlab.issue.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueCreateInput {
    project: String,
    title: String,
    description: Option<String>,
    labels: Option<Vec<String>>,
    assignee_ids: Option<Vec<i64>>,
    #[schemars(range(min = 1))]
    milestone_id: Option<i64>,
    confidential: Option<bool>,
}

/// `gitlab.issue.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueUpdateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    title: Option<String>,
    description: Option<String>,
    labels: Option<Vec<String>>,
    add_labels: Option<Vec<String>>,
    remove_labels: Option<Vec<String>>,
    state_event: Option<String>,
    assignee_ids: Option<Vec<i64>>,
}

/// `gitlab.issue.note.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueNoteListInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    sort: Option<String>,
    order_by: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.issue.note.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct IssueNoteCreateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    body: String,
}

/// `gitlab.branch.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BranchCreateInput {
    project: String,
    /// The new branch name (or use the `name` alias). One of the two is required.
    branch: Option<String>,
    /// Alias of `branch` (GL-028).
    name: Option<String>,
    r#ref: String,
}

/// `gitlab.branch.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BranchDeleteInput {
    project: String,
    /// The branch to delete (or use the `name` alias). One of the two is required.
    branch: Option<String>,
    /// Alias of `branch` (GL-028).
    name: Option<String>,
    /// Fat-finger guard (GL-005): when set it must equal the branch being deleted.
    confirm_branch: Option<String>,
}

/// `gitlab.branch.delete_merged`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct BranchDeleteMergedInput {
    project: String,
    /// Fat-finger guard (GL-005): when set it must equal `project`. This is a BULK sweep of every
    /// merged branch, so confirming the project guards against running it against the wrong repo.
    confirm_project: Option<String>,
}

/// `gitlab.repository.file.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryFileCreateInput {
    project: String,
    file_path: String,
    branch: String,
    content: String,
    commit_message: String,
    encoding: Option<String>,
    start_branch: Option<String>,
    author_email: Option<String>,
    author_name: Option<String>,
    execute_filemode: Option<bool>,
}

/// `gitlab.repository.file.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryFileUpdateInput {
    project: String,
    file_path: String,
    branch: String,
    content: String,
    commit_message: String,
    encoding: Option<String>,
    start_branch: Option<String>,
    author_email: Option<String>,
    author_name: Option<String>,
    last_commit_id: Option<String>,
    execute_filemode: Option<bool>,
}

/// `gitlab.repository.file.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryFileDeleteInput {
    project: String,
    file_path: String,
    branch: String,
    commit_message: String,
    start_branch: Option<String>,
    author_email: Option<String>,
    author_name: Option<String>,
    last_commit_id: Option<String>,
    /// Fat-finger guard (GL-005): when set it must equal `file_path`.
    confirm_file_path: Option<String>,
}

/// `gitlab.repository.file.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryFileShowInput {
    project: String,
    path: String,
    r#ref: Option<String>,
    #[schemars(range(min = 1))]
    max_bytes: Option<i64>,
}

/// `gitlab.repository.tree`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryTreeInput {
    project: String,
    path: Option<String>,
    r#ref: Option<String>,
    recursive: Option<bool>,
    #[schemars(range(min = 1, max = 2000))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 2000))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.repository.commit.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryCommitCreateInput {
    project: String,
    branch: String,
    commit_message: String,
    #[schemars(length(min = 1))]
    actions: Vec<CommitAction>,
    start_branch: Option<String>,
    start_sha: Option<String>,
    start_project: Option<String>,
    author_email: Option<String>,
    author_name: Option<String>,
    force: Option<bool>,
}

/// `gitlab.repository.commit.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryCommitListInput {
    project: String,
    r#ref: Option<String>,
    file_path: Option<String>,
    author: Option<String>,
    since: Option<String>,
    until: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.repository.tag.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryTagCreateInput {
    project: String,
    /// The new tag name (or use the `name` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    name: Option<String>,
    r#ref: String,
    message: Option<String>,
}

/// `gitlab.repository.tag.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryTagListInput {
    project: String,
    search: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.repository.tag.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryTagShowInput {
    project: String,
    /// The tag name (or use the `tag`/`name` aliases). One of the three is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    /// Alias of `tag_name` (GL-028).
    name: Option<String>,
}

/// `gitlab.repository.tag.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryTagDeleteInput {
    project: String,
    /// The tag to delete (or use the `tag`/`name` aliases). One of the three is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    /// Alias of `tag_name` (GL-028).
    name: Option<String>,
    /// Fat-finger guard (GL-005): when set it must equal the resolved tag name.
    confirm_tag_name: Option<String>,
}

/// `gitlab.snippet.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct SnippetCreateInput {
    title: String,
    description: Option<String>,
    visibility: Option<Visibility>,
    #[schemars(length(min = 1))]
    files: Vec<SnippetFile>,
}

/// `gitlab.snippet.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct SnippetDeleteInput {
    /// The snippet id (or use the `id` alias). One of the two is required (GL-029).
    #[schemars(range(min = 1))]
    snippet_id: Option<i64>,
    /// Alias of `snippet_id` (GL-028).
    #[schemars(range(min = 1))]
    id: Option<i64>,
    /// Fat-finger guard (GL-005): when set it must equal the resolved snippet id.
    #[schemars(range(min = 1))]
    confirm_snippet_id: Option<i64>,
}

/// `gitlab.search.blobs`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct SearchBlobsInput {
    query: String,
    project: Option<String>,
    group: Option<String>,
    r#ref: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
    #[schemars(range(min = 1))]
    max_data_bytes: Option<i64>,
}

/// `gitlab.mr.changes`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrChangesInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    file: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    max_files: Option<i64>,
    #[schemars(range(min = 1, max = 262144))]
    max_diff_bytes: Option<i64>,
}

/// `gitlab.mr.diff.lines`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrDiffLinesInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    file: String,
    /// New-file line to anchor on (wins over `old_line` when both are set).
    #[schemars(range(min = 1))]
    line: Option<i64>,
    /// Old-file line to anchor on — addresses deleted/context lines (GL-047).
    #[schemars(range(min = 1))]
    old_line: Option<i64>,
    context: Option<i64>,
    search: Option<String>,
    #[schemars(range(min = 1, max = 2000))]
    limit: Option<i64>,
}

/// `gitlab.compare`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CompareInput {
    project: String,
    from: String,
    to: String,
    straight: Option<bool>,
    #[schemars(range(min = 1, max = 200))]
    max_files: Option<i64>,
    #[schemars(range(min = 1, max = 262144))]
    max_diff_bytes: Option<i64>,
    /// Cap on returned commits (default 50, max 500); `commits_truncated` reports a cut (GL-045).
    #[schemars(range(min = 1, max = 500))]
    max_commits: Option<i64>,
}

/// `gitlab.mr.discussion.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrDiscussionListInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.mr.note.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrNoteCreateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    body: String,
}

/// `gitlab.mr.discussion.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrDiscussionCreateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    body: String,
    /// File to anchor a line-level comment to; required when `new_line`/`old_line` is set.
    path: Option<String>,
    /// New-file line for the anchor; `path` plus `new_line` or `old_line` is required for a
    /// line-level comment.
    #[schemars(range(min = 1))]
    new_line: Option<i64>,
    /// Old-file line for the anchor (deleted/context lines).
    #[schemars(range(min = 1))]
    old_line: Option<i64>,
    /// Server-side preview (GL-025): resolve the line anchor via the GitLab API and return the
    /// would-be discussion `position` WITHOUT posting. Distinct from the CLI's `--dry-run`,
    /// which only validates the input locally and never contacts GitLab.
    dry_run: Option<bool>,
}

/// `gitlab.mr.discussion.reply`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrDiscussionReplyInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    discussion_id: String,
    body: String,
}

/// `gitlab.mr.discussion.resolve`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct MrDiscussionResolveInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    discussion_id: String,
    resolved: Option<bool>,
}

/// `gitlab.ci.variable.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiVariableCreateInput {
    project: String,
    key: String,
    value: String,
    description: Option<String>,
    environment_scope: Option<String>,
    masked: Option<bool>,
    masked_and_hidden: Option<bool>,
    protected: Option<bool>,
    raw: Option<bool>,
    variable_type: Option<VariableType>,
}

/// `gitlab.ci.variable.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiVariableUpdateInput {
    project: String,
    key: String,
    value: String,
    description: Option<String>,
    environment_scope: Option<String>,
    masked: Option<bool>,
    protected: Option<bool>,
    raw: Option<bool>,
    variable_type: Option<VariableType>,
}

/// `gitlab.ci.variable.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiVariableDeleteInput {
    project: String,
    key: String,
    environment_scope: Option<String>,
    /// Fat-finger guard (GL-005): when set it must equal `key`.
    confirm_key: Option<String>,
}

/// `gitlab.pipeline.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct PipelineCreateInput {
    project: String,
    r#ref: String,
    variables: Option<Vec<PipelineVariable>>,
}

/// `gitlab.pipeline.retry`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct PipelineRetryInput {
    project: String,
    #[schemars(range(min = 1))]
    pipeline_id: i64,
}

/// `gitlab.pipeline.cancel`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct PipelineCancelInput {
    project: String,
    #[schemars(range(min = 1))]
    pipeline_id: i64,
}

/// `gitlab.job.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct JobListInput {
    project: String,
    #[schemars(range(min = 1))]
    pipeline_id: i64,
    scope: Option<Vec<JobScope>>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.environment.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct EnvironmentListInput {
    project: String,
    search: Option<String>,
    states: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.deployment.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct DeploymentListInput {
    project: String,
    environment: Option<String>,
    status: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.release.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseListInput {
    project: String,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.release.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseCreateInput {
    project: String,
    tag_name: String,
    r#ref: Option<String>,
    name: Option<String>,
    description: Option<String>,
    tag_message: Option<String>,
    milestones: Option<Vec<String>>,
    released_at: Option<String>,
    assets_links: Option<Vec<Value>>,
}

/// `gitlab.release.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseShowInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
}

/// `gitlab.release.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseUpdateInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required — `name` is the
    /// release's display name, never the tag.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    name: Option<String>,
    description: Option<String>,
    milestones: Option<Vec<String>>,
    released_at: Option<String>,
}

/// `gitlab.release.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseDeleteInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    /// Fat-finger guard (GL-005): when set it must equal the resolved release tag.
    confirm_tag_name: Option<String>,
}

/// `gitlab.release.link.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseLinkListInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 200))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
}

/// `gitlab.release.link.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseLinkCreateInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required — `name` is the
    /// link's display name, never the tag.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    name: String,
    url: String,
    direct_asset_path: Option<String>,
    link_type: Option<LinkType>,
}

/// `gitlab.release.link.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseLinkUpdateInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    #[schemars(range(min = 1))]
    link_id: i64,
    name: Option<String>,
    url: Option<String>,
    direct_asset_path: Option<String>,
    link_type: Option<LinkType>,
}

/// `gitlab.release.link.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct ReleaseLinkDeleteInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
    #[schemars(range(min = 1))]
    link_id: i64,
}

/// `gitlab.repository.changelog.generate`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryChangelogGenerateInput {
    project: String,
    version: String,
    from: Option<String>,
    to: Option<String>,
    date: Option<String>,
    trailer: Option<String>,
    config_file: Option<String>,
}

/// `gitlab.repository.changelog.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryChangelogAddInput {
    project: String,
    version: String,
    /// The branch to commit the changelog section onto — REQUIRED (GL-037). Previously optional,
    /// which let GitLab default it to the repo's default branch: a silent write to `main`. Name
    /// the target branch explicitly.
    branch: String,
    file: Option<String>,
    from: Option<String>,
    to: Option<String>,
    date: Option<String>,
    message: Option<String>,
    trailer: Option<String>,
    config_file: Option<String>,
}

/// `gitlab.repository.archive`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryArchiveInput {
    project: String,
    r#ref: Option<String>,
    path: Option<String>,
    format: Option<ArchiveFormat>,
    /// Refuse archives larger than this many bytes (default 52428800 = 50 MiB) so an
    /// "archive read" cannot stage an unbounded blob (GL-023).
    #[schemars(range(min = 1))]
    max_bytes: Option<i64>,
}

/// `gitlab.ci.job_token.scope.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenScopeShowInput {
    project: String,
}

/// `gitlab.ci.job_token.scope.set`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenScopeSetInput {
    project: String,
    enabled: bool,
}

/// `gitlab.ci.job_token.allowlist.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenAllowlistListInput {
    project: String,
}

/// `gitlab.ci.job_token.allowlist.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenAllowlistAddInput {
    project: String,
    target_project_id: i64,
}

/// `gitlab.ci.job_token.allowlist.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenAllowlistRemoveInput {
    project: String,
    target_project_id: i64,
    confirm_target_project_id: Option<i64>,
}

/// `gitlab.ci.job_token.groups_allowlist.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenGroupsAllowlistListInput {
    project: String,
}

/// `gitlab.ci.job_token.groups_allowlist.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenGroupsAllowlistAddInput {
    project: String,
    target_group_id: i64,
}

/// `gitlab.ci.job_token.groups_allowlist.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct CiJobTokenGroupsAllowlistRemoveInput {
    project: String,
    target_group_id: i64,
    confirm_target_group_id: Option<i64>,
}

/// `gitlab.repository.protected_tag.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryProtectedTagListInput {
    project: String,
}

/// `gitlab.repository.protected_tag.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryProtectedTagShowInput {
    project: String,
    name: String,
}

/// `gitlab.repository.protected_tag.protect`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryProtectedTagProtectInput {
    project: String,
    name: String,
    create_access_level: Option<i64>,
}

/// `gitlab.repository.protected_tag.unprotect`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct RepositoryProtectedTagUnprotectInput {
    project: String,
    name: String,
    confirm_name: Option<String>,
}

/// `gitlab.deploy_token.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct DeployTokenListInput {
    project: String,
}

/// `gitlab.deploy_token.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct DeployTokenCreateInput {
    project: String,
    name: String,
    scopes: Vec<Value>,
    expires_at: Option<String>,
    username: Option<String>,
}

/// `gitlab.deploy_token.revoke`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(super) struct DeployTokenRevokeInput {
    project: String,
    token_id: i64,
    confirm_token_id: Option<i64>,
}
