//! `gitlab` — a flux integration plugin for the GitLab REST API (v4): projects, merge requests, issues,
//! pipelines, CI/CD, code review, and releases. Authenticates with a personal access token via the
//! `PRIVATE-TOKEN` header; requests address the `gitlab.endpoint` **by reference** — the host
//! resolves the base URL (env-configured, defaulting to gitlab.com host-side) and it never crosses
//! to the plugin (D-32). List ops
//! contribute datasource records (`gitlab.project` / `gitlab.merge_request` / `gitlab.issue`) so the
//! agent can search them; `gitlab.index.build` drives that contribution exhaustively over the surface.
//!
//! This is the reference template for the HTTP-API integration plugins: every read/list/get/search op
//! is a `read_op` and every create/update/delete/mutate op is a `write_op`; all REST verbs go through
//! the DRY `gl_get`/`gl_post`/`gl_put`/`gl_delete` helpers (PRIVATE-TOKEN header, ref + `/api/v4 + path`,
//! is_success check, JSON parse); `gitlab.repository.archive` stages the downloaded bytes through the
//! host `blob` capability.

use host_kit::*;
use regex::Regex;
use serde_json::{json, Map, Value};

use schemars::JsonSchema;
use serde::Deserialize;

// ─── op input schemas (D-36) ───────────────────────────────────────────────
// Each op's `input_schema` is schemars-derived (`host_kit::read_op_typed::<T>` /
// `write_op_typed::<T>`) instead of a hand-written `so(json!{...}, json![...])` literal,
// so the schema the model sees cannot drift. The structs are schema-only: handlers
// keep their existing `flex_str`/`flex_i64`/`Value` extraction (D-34 precedent).
// The enum/bound/typed-element constraints below are enforced by host-kit's shared
// preflight in BOTH `--dry-run` and runtime dispatch (D-88), so a green dry-run can
// no longer fail the same check at runtime.

/// Issue state filter (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum IssueStateFilter {
    Opened,
    Closed,
    All,
}

/// Project/snippet visibility level (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum Visibility {
    Private,
    Internal,
    Public,
}

/// Release asset link type (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum LinkType {
    Other,
    Runbook,
    Image,
    Package,
}

/// CI/pipeline variable type (GL-011).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum VariableType {
    EnvVar,
    File,
}

/// Merge-request state filter (GL-038).
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum MrStateFilter {
    Opened,
    Closed,
    Locked,
    Merged,
    All,
}

/// CI job status scope entry (GL-033) — typed so a non-string or unknown entry is rejected
/// instead of silently skipped.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum JobScope {
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
enum ArchiveFormat {
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
struct CommitAction {
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
enum CommitActionKind {
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
enum ContentEncoding {
    Text,
    Base64,
}

/// One `gitlab.snippet.create` file (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct SnippetFile {
    file_path: String,
    content: String,
}

/// One `gitlab.pipeline.create` variable (GL-012).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PipelineVariable {
    key: String,
    value: Option<Value>,
    variable_type: Option<VariableType>,
}

/// `gitlab.project.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ProjectListInput {
    search: Option<String>,
    query: Option<String>,
    order_by: Option<String>,
    sort: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
    membership: Option<bool>,
    /// Feed the results into the local search index as `gitlab.project` records (GL-015). Off by
    /// default so a plain read is a pure read with no datasource side effects; use `index.build`
    /// (or pass `contribute=true`) to index deliberately.
    contribute: Option<bool>,
}

/// `gitlab.project.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ProjectShowInput {
    project: String,
}

/// `gitlab.mr.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MrListInput {
    project: String,
    state: Option<MrStateFilter>,
    search: Option<String>,
    query: Option<String>,
    order_by: Option<String>,
    sort: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    /// Feed the results into the local search index as `gitlab.merge_request` records (GL-015).
    /// Off by default so a plain read has no datasource side effects; `index.build` indexes
    /// deliberately.
    contribute: Option<bool>,
}

/// `gitlab.mr.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MrShowInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
}

/// `gitlab.issue.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IssueListInput {
    project: String,
    state: Option<IssueStateFilter>,
    search: Option<String>,
    query: Option<String>,
    order_by: Option<String>,
    sort: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<i64>,
    /// Alias of `limit` (GL-009); `limit` wins when both are set.
    #[schemars(range(min = 1, max = 100))]
    per_page: Option<i64>,
    /// 1-based results page (GL-019) — walk a list beyond a capped first page.
    #[schemars(range(min = 1))]
    page: Option<i64>,
    /// Feed the results into the local search index as `gitlab.issue` records (GL-015). Off by
    /// default so a plain read has no datasource side effects; `index.build` indexes deliberately.
    contribute: Option<bool>,
}

/// `gitlab.pipeline.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PipelineListInput {
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
struct TestInput {}

/// `gitlab.index.build`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IndexBuildInput {
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
struct ProjectCreateInput {
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
struct ProjectDeleteInput {
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
struct MrCreateInput {
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
struct MrUpdateInput {
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
struct MrApproveInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    sha: Option<String>,
}

/// `gitlab.mr.merge`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MrMergeInput {
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
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IssueShowInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
}

/// `gitlab.issue.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IssueCreateInput {
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
struct IssueUpdateInput {
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
struct IssueNoteListInput {
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
struct IssueNoteCreateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    body: String,
}

/// `gitlab.branch.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct BranchCreateInput {
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
struct BranchDeleteInput {
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
struct BranchDeleteMergedInput {
    project: String,
    /// Fat-finger guard (GL-005): when set it must equal `project`. This is a BULK sweep of every
    /// merged branch, so confirming the project guards against running it against the wrong repo.
    confirm_project: Option<String>,
}

/// `gitlab.repository.file.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RepositoryFileCreateInput {
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
struct RepositoryFileUpdateInput {
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
struct RepositoryFileDeleteInput {
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
struct RepositoryFileShowInput {
    project: String,
    path: String,
    r#ref: Option<String>,
    #[schemars(range(min = 1))]
    max_bytes: Option<i64>,
}

/// `gitlab.repository.tree`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RepositoryTreeInput {
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
struct RepositoryCommitCreateInput {
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
struct RepositoryCommitListInput {
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
struct RepositoryTagCreateInput {
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
struct RepositoryTagListInput {
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
struct RepositoryTagShowInput {
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
struct RepositoryTagDeleteInput {
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
struct SnippetCreateInput {
    title: String,
    description: Option<String>,
    visibility: Option<Visibility>,
    #[schemars(length(min = 1))]
    files: Vec<SnippetFile>,
}

/// `gitlab.snippet.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SnippetDeleteInput {
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
struct SearchBlobsInput {
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
struct MrChangesInput {
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
struct MrDiffLinesInput {
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
struct CompareInput {
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
struct MrDiscussionListInput {
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
struct MrNoteCreateInput {
    r#ref: Option<String>,
    project: Option<String>,
    #[schemars(range(min = 1))]
    iid: Option<i64>,
    body: String,
}

/// `gitlab.mr.discussion.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct MrDiscussionCreateInput {
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
struct MrDiscussionReplyInput {
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
struct MrDiscussionResolveInput {
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
struct CiVariableCreateInput {
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
struct CiVariableUpdateInput {
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
struct CiVariableDeleteInput {
    project: String,
    key: String,
    environment_scope: Option<String>,
    /// Fat-finger guard (GL-005): when set it must equal `key`.
    confirm_key: Option<String>,
}

/// `gitlab.pipeline.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PipelineCreateInput {
    project: String,
    r#ref: String,
    variables: Option<Vec<PipelineVariable>>,
}

/// `gitlab.pipeline.retry`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PipelineRetryInput {
    project: String,
    #[schemars(range(min = 1))]
    pipeline_id: i64,
}

/// `gitlab.pipeline.cancel`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PipelineCancelInput {
    project: String,
    #[schemars(range(min = 1))]
    pipeline_id: i64,
}

/// `gitlab.job.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct JobListInput {
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
struct EnvironmentListInput {
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
struct DeploymentListInput {
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
struct ReleaseListInput {
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
struct ReleaseCreateInput {
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
struct ReleaseShowInput {
    project: String,
    /// The release tag (or use the `tag` alias). One of the two is required.
    tag_name: Option<String>,
    /// Alias of `tag_name` (GL-028).
    tag: Option<String>,
}

/// `gitlab.release.update`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ReleaseUpdateInput {
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
struct ReleaseDeleteInput {
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
struct ReleaseLinkListInput {
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
struct ReleaseLinkCreateInput {
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
struct ReleaseLinkUpdateInput {
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
struct ReleaseLinkDeleteInput {
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
struct RepositoryChangelogGenerateInput {
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
struct RepositoryChangelogAddInput {
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
struct RepositoryArchiveInput {
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
struct CiJobTokenScopeShowInput {
    project: String,
}

/// `gitlab.ci.job_token.scope.set`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenScopeSetInput {
    project: String,
    enabled: bool,
}

/// `gitlab.ci.job_token.allowlist.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenAllowlistListInput {
    project: String,
}

/// `gitlab.ci.job_token.allowlist.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenAllowlistAddInput {
    project: String,
    target_project_id: i64,
}

/// `gitlab.ci.job_token.allowlist.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenAllowlistRemoveInput {
    project: String,
    target_project_id: i64,
    confirm_target_project_id: Option<i64>,
}

/// `gitlab.ci.job_token.groups_allowlist.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenGroupsAllowlistListInput {
    project: String,
}

/// `gitlab.ci.job_token.groups_allowlist.add`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenGroupsAllowlistAddInput {
    project: String,
    target_group_id: i64,
}

/// `gitlab.ci.job_token.groups_allowlist.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CiJobTokenGroupsAllowlistRemoveInput {
    project: String,
    target_group_id: i64,
    confirm_target_group_id: Option<i64>,
}

/// `gitlab.repository.protected_tag.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RepositoryProtectedTagListInput {
    project: String,
}

/// `gitlab.repository.protected_tag.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RepositoryProtectedTagShowInput {
    project: String,
    name: String,
}

/// `gitlab.repository.protected_tag.protect`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RepositoryProtectedTagProtectInput {
    project: String,
    name: String,
    create_access_level: Option<i64>,
}

/// `gitlab.repository.protected_tag.unprotect`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RepositoryProtectedTagUnprotectInput {
    project: String,
    name: String,
    confirm_name: Option<String>,
}

/// `gitlab.deploy_token.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeployTokenListInput {
    project: String,
}

/// `gitlab.deploy_token.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeployTokenCreateInput {
    project: String,
    name: String,
    scopes: Vec<Value>,
    expires_at: Option<String>,
    username: Option<String>,
}

/// `gitlab.deploy_token.revoke`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeployTokenRevokeInput {
    project: String,
    token_id: i64,
    confirm_token_id: Option<i64>,
}

/// Mark an op's secret-like fields for host-side masking (GL-031 / D-93): their values are redacted
/// wherever flux echoes this op's input or result — the `flux plugin call` dry-run input preview,
/// the live result echo, and the stringified tool result the model sees. Used for the CI/pipeline
/// variable `value` fields, which are matched by name at any depth (so a pipeline's
/// `variables[].value` array is masked element-wise too).
fn redacting(mut op: OperationSpec, fields: &[&str]) -> OperationSpec {
    op.redact_fields = fields.iter().map(|s| s.to_string()).collect();
    op
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("gitlab", env!("CARGO_PKG_VERSION"))
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["gitlab.com".into()],
            private_hosts: vec!["*".into()],
            blob: true,
            secrets: vec![
                "GITLAB_PERSONAL_TOKEN".into(),
                "GITLAB_PERSONAL_ACCESS_TOKEN".into(),
            ],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "personal_token".into(),
            env: vec![
                "GITLAB_PERSONAL_TOKEN".into(),
                "GITLAB_PERSONAL_ACCESS_TOKEN".into(),
            ],
            description: "GitLab personal access token".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "gitlab.endpoint".into(),
            env: vec!["GITLAB_URL".into(), "GITLAB_BASE_URL".into()],
            http_hosts: vec!["gitlab.com".into()],
            description: "GitLab base URL (default https://gitlab.com)".into(),
            // Host-side default (D-32): when no env key is set the host resolves gitlab.com
            // itself — the fallback that used to live plugin-side in `gl_base_token`.
            default: Some("https://gitlab.com".into()),
            ..Default::default()
        })
        .datasource(ds("gitlab.projects", "gitlab.project", "GitLab projects."))
        .datasource(ds(
            "gitlab.merge_requests",
            "gitlab.merge_request",
            "GitLab merge requests.",
        ))
        .datasource(ds("gitlab.issues", "gitlab.issue", "GitLab issues."))
        // ---- reads: projects / merge requests / issues / pipelines ----
        .operation(
            read_op_typed::<ProjectListInput>(
                "gitlab.project.list",
                "List/search projects you are a MEMBER of by default (membership=true); pass membership=false to widen to every project the token can see.",
            ),
            project_list,
        )
        .operation(
            read_op_typed::<ProjectShowInput>(
                "gitlab.project.show",
                "Show one project by id or path.",
            ),
            project_show,
        )
        .operation(
            read_op_typed::<MrListInput>(
                "gitlab.mr.list",
                "List a project's merge requests (state: opened|closed|locked|merged|all). Defaults to state=opened — pass state=all to include closed/merged (index.build indexes all states).",
            ),
            mr_list,
        )
        .operation(
            read_op_typed::<MrShowInput>(
                "gitlab.mr.show",
                "Show one merge request by ref (PROJECT!IID) or project + iid.",
            ),
            mr_show,
        )
        .operation(
            read_op_typed::<IssueListInput>(
                "gitlab.issue.list",
                "List a project's issues (state: opened|closed|all). Defaults to state=opened — pass state=all to include closed issues (index.build indexes all states).",
            ),
            issue_list,
        )
        .operation(
            read_op_typed::<PipelineListInput>(
                "gitlab.pipeline.list",
                "List a project's recent CI pipelines.",
            ),
            pipeline_list,
        )
        // ---- auth test + index ----
        .operation(
            read_op_typed::<TestInput>(
                "gitlab.test",
                "Test GitLab authentication by fetching the current user.",
            ),
            auth_test,
        )
        .operation(
            read_op_typed::<IndexBuildInput>(
                "gitlab.index.build",
                "Build GitLab index records across projects, merge requests, and issues.",
            ),
            index_build,
        )
        // ---- project create / delete ----
        .operation(
            write_op_typed::<ProjectCreateInput>(
                "gitlab.project.create",
                "Create a project, optionally inside a group namespace (resolved by path).",
            ),
            project_create,
        )
        .operation(
            risked(
                write_op_typed::<ProjectDeleteInput>(
                    "gitlab.project.delete",
                    "DELETE a project (destructive, irreversible). Pass confirm_path equal to project (or confirm_project_id equal to its numeric id) to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            project_delete,
        )
        // ---- merge request writes ----
        .operation(
            write_op_typed::<MrCreateInput>(
                "gitlab.mr.create",
                "Create a GitLab merge request.",
            ),
            mr_create,
        )
        .operation(
            write_op_typed::<MrUpdateInput>(
                "gitlab.mr.update",
                "Update merge request fields (title, description, target branch, labels) or close/reopen via state_event.",
            ),
            mr_update,
        )
        .operation(
            write_op_typed::<MrApproveInput>(
                "gitlab.mr.approve",
                "Approve a GitLab merge request.",
            ),
            mr_approve,
        )
        .operation(
            write_op_typed::<MrMergeInput>(
                "gitlab.mr.merge",
                "Merge a GitLab merge request.",
            ),
            mr_merge,
        )
        // ---- issues ----
        .operation(
            read_op_typed::<IssueShowInput>(
                "gitlab.issue.show",
                "Show one GitLab issue, including its Markdown description.",
            ),
            issue_show,
        )
        .operation(
            write_op_typed::<IssueCreateInput>(
                "gitlab.issue.create",
                "Create a GitLab issue. Description is GitLab-flavored Markdown.",
            ),
            issue_create,
        )
        .operation(
            write_op_typed::<IssueUpdateInput>(
                "gitlab.issue.update",
                "Update a GitLab issue (title/description/labels/assignees) or transition it via state_event.",
            ),
            issue_update,
        )
        .operation(
            read_op_typed::<IssueNoteListInput>(
                "gitlab.issue.note.list",
                "List comments (notes) on a GitLab issue. Bodies are Markdown.",
            ),
            issue_note_list,
        )
        .operation(
            write_op_typed::<IssueNoteCreateInput>(
                "gitlab.issue.note.create",
                "Add a comment (note) to a GitLab issue. Body is Markdown.",
            ),
            issue_note_create,
        )
        // ---- branches ----
        .operation(
            write_op_typed::<BranchCreateInput>(
                "gitlab.branch.create",
                "Create a GitLab repository branch.",
            ),
            branch_create,
        )
        .operation(
            risked(
                write_op_typed::<BranchDeleteInput>(
                    "gitlab.branch.delete",
                    "Delete a GitLab repository branch. Pass confirm_branch equal to the branch to guard against mistakes.",
                ),
                Risk::High,
            ),
            branch_delete,
        )
        .operation(
            risked(
                write_op_typed::<BranchDeleteMergedInput>(
                    "gitlab.branch.delete_merged",
                    "BULK-delete every merged branch in a GitLab project (destructive, sweeps many branches at once). Pass confirm_project equal to project to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            branch_delete_merged,
        )
        // ---- repository files ----
        .operation(
            write_op_typed::<RepositoryFileCreateInput>(
                "gitlab.repository.file.create",
                "Create a file in a GitLab repository.",
            ),
            repo_file_create,
        )
        .operation(
            write_op_typed::<RepositoryFileUpdateInput>(
                "gitlab.repository.file.update",
                "Update a file in a GitLab repository.",
            ),
            repo_file_update,
        )
        .operation(
            risked(
                write_op_typed::<RepositoryFileDeleteInput>(
                    "gitlab.repository.file.delete",
                    "Delete a file from a GitLab repository (destructive). Pass confirm_file_path equal to file_path to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            repo_file_delete,
        )
        .operation(
            read_op_typed::<RepositoryFileShowInput>(
                "gitlab.repository.file.show",
                "Read a file's content at a ref (default branch when omitted).",
            ),
            repo_file_show,
        )
        .operation(
            read_op_typed::<RepositoryTreeInput>(
                "gitlab.repository.tree",
                "List a repository tree at a ref (optionally recursive).",
            ),
            repo_tree,
        )
        // ---- commits ----
        .operation(
            write_op_typed::<RepositoryCommitCreateInput>(
                "gitlab.repository.commit.create",
                "Create a GitLab commit with one or more file actions.",
            ),
            commit_create,
        )
        .operation(
            read_op_typed::<RepositoryCommitListInput>(
                "gitlab.repository.commit.list",
                "List a ref's commit history, newest first; filter by path, author, or a since/until window.",
            ),
            commit_list,
        )
        // ---- tags ----
        .operation(
            write_op_typed::<RepositoryTagCreateInput>(
                "gitlab.repository.tag.create",
                "Create a GitLab repository tag.",
            ),
            tag_create,
        )
        .operation(
            read_op_typed::<RepositoryTagListInput>(
                "gitlab.repository.tag.list",
                "List a project's git tags with their target commits, newest first.",
            ),
            tag_list,
        )
        .operation(
            read_op_typed::<RepositoryTagShowInput>(
                "gitlab.repository.tag.show",
                "Show one git tag with its target commit and any annotation message.",
            ),
            tag_show,
        )
        .operation(
            risked(
                write_op_typed::<RepositoryTagDeleteInput>(
                    "gitlab.repository.tag.delete",
                    "Delete a git tag from a project (destructive). Pass confirm_tag_name equal to the tag to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            tag_delete,
        )
        // ---- snippets ----
        .operation(
            write_op_typed::<SnippetCreateInput>(
                "gitlab.snippet.create",
                "Create a personal GitLab snippet.",
            ),
            snippet_create,
        )
        .operation(
            risked(
                write_op_typed::<SnippetDeleteInput>(
                    "gitlab.snippet.delete",
                    "Delete a personal GitLab snippet (destructive). Pass confirm_snippet_id equal to the snippet id to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            snippet_delete,
        )
        // ---- search ----
        .operation(
            read_op_typed::<SearchBlobsInput>(
                "gitlab.search.blobs",
                "Search file contents (GitLab scope=blobs) in ONE scope: a project (supports ref), a group (no ref), or — with neither — the whole instance, which requires GitLab advanced/exact code search (Elasticsearch/Zoekt) and fails on instances without it.",
            ),
            search_blobs,
        )
        // ---- review / diff ----
        .operation(
            read_op_typed::<MrChangesInput>(
                "gitlab.mr.changes",
                "List a merge request's changed files with bounded unified diffs, plus the base/start/head diff refs.",
            ),
            mr_changes,
        )
        .operation(
            read_op_typed::<MrDiffLinesInput>(
                "gitlab.mr.diff.lines",
                "Parse one changed file's diff into typed lines (added/deleted/context with old/new line numbers).",
            ),
            mr_diff_lines,
        )
        .operation(
            read_op_typed::<CompareInput>(
                "gitlab.compare",
                "Compare two refs (branches, tags, or commits): commits between them and bounded file diffs.",
            ),
            compare,
        )
        .operation(
            read_op_typed::<MrDiscussionListInput>(
                "gitlab.mr.discussion.list",
                "List a merge request's discussion threads with resolution state and inline line positions.",
            ),
            mr_discussion_list,
        )
        .operation(
            write_op_typed::<MrNoteCreateInput>(
                "gitlab.mr.note.create",
                "Post a top-level merge request note.",
            ),
            mr_note_create,
        )
        .operation(
            write_op_typed::<MrDiscussionCreateInput>(
                "gitlab.mr.discussion.create",
                "Open a merge request discussion, optionally anchored to a diff line (path + new_line/old_line). dry_run=true is a SERVER-SIDE preview: it resolves the line anchor via the GitLab API and returns the would-be position without posting (the CLI's --dry-run flag, by contrast, only validates the input locally).",
            ),
            mr_discussion_create,
        )
        .operation(
            write_op_typed::<MrDiscussionReplyInput>(
                "gitlab.mr.discussion.reply",
                "Reply into an existing merge request discussion thread.",
            ),
            mr_discussion_reply,
        )
        .operation(
            write_op_typed::<MrDiscussionResolveInput>(
                "gitlab.mr.discussion.resolve",
                "Resolve (or unresolve with resolved=false) a merge request discussion thread.",
            ),
            mr_discussion_resolve,
        )
        // ---- CI/CD ----
        .operation(
            redacting(
                write_op_typed::<CiVariableCreateInput>(
                    "gitlab.ci.variable.create",
                    "Create a GitLab project CI/CD variable.",
                ),
                &["value"],
            ),
            ci_variable_create,
        )
        .operation(
            redacting(
                write_op_typed::<CiVariableUpdateInput>(
                    "gitlab.ci.variable.update",
                    "Update a GitLab project CI/CD variable.",
                ),
                &["value"],
            ),
            ci_variable_update,
        )
        .operation(
            risked(
                write_op_typed::<CiVariableDeleteInput>(
                    "gitlab.ci.variable.delete",
                    "Delete a GitLab project CI/CD variable (destructive). Pass confirm_key equal to key to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            ci_variable_delete,
        )
        .operation(
            redacting(
                write_op_typed::<PipelineCreateInput>(
                    "gitlab.pipeline.create",
                    "Create a GitLab CI pipeline.",
                ),
                &["value"],
            ),
            pipeline_create,
        )
        .operation(
            write_op_typed::<PipelineRetryInput>(
                "gitlab.pipeline.retry",
                "Retry a GitLab CI pipeline.",
            ),
            pipeline_retry,
        )
        .operation(
            write_op_typed::<PipelineCancelInput>(
                "gitlab.pipeline.cancel",
                "Cancel a GitLab CI pipeline.",
            ),
            pipeline_cancel,
        )
        .operation(
            read_op_typed::<JobListInput>(
                "gitlab.job.list",
                "List one pipeline's jobs with stage, status, duration, and failure_reason.",
            ),
            job_list,
        )
        .operation(
            read_op_typed::<EnvironmentListInput>(
                "gitlab.environment.list",
                "List a project's environments with state, tier, external URL, and last deployment.",
            ),
            environment_list,
        )
        .operation(
            read_op_typed::<DeploymentListInput>(
                "gitlab.deployment.list",
                "List a project's deployments, newest first, filterable by environment and status.",
            ),
            deployment_list,
        )
        // ---- releases ----
        .operation(
            read_op_typed::<ReleaseListInput>(
                "gitlab.release.list",
                "List a project's releases, newest first.",
            ),
            release_list,
        )
        .operation(
            write_op_typed::<ReleaseCreateInput>(
                "gitlab.release.create",
                "Create a GitLab release for a tag, cutting the tag from ref when it does not yet exist.",
            ),
            release_create,
        )
        .operation(
            read_op_typed::<ReleaseShowInput>(
                "gitlab.release.show",
                "Show one GitLab release with its description, milestones, and asset links.",
            ),
            release_show,
        )
        .operation(
            write_op_typed::<ReleaseUpdateInput>(
                "gitlab.release.update",
                "Update a GitLab release's title, notes, milestones, or release date.",
            ),
            release_update,
        )
        .operation(
            risked(
                write_op_typed::<ReleaseDeleteInput>(
                    "gitlab.release.delete",
                    "Delete a GitLab release (destructive; the underlying git tag is left in place). Pass confirm_tag_name equal to the tag to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            release_delete,
        )
        .operation(
            read_op_typed::<ReleaseLinkListInput>(
                "gitlab.release.link.list",
                "List the asset links attached to a release.",
            ),
            release_link_list,
        )
        .operation(
            write_op_typed::<ReleaseLinkCreateInput>(
                "gitlab.release.link.create",
                "Attach a new asset link (a download or related URL) to a release.",
            ),
            release_link_create,
        )
        .operation(
            write_op_typed::<ReleaseLinkUpdateInput>(
                "gitlab.release.link.update",
                "Edit an existing release asset link.",
            ),
            release_link_update,
        )
        .operation(
            write_op_typed::<ReleaseLinkDeleteInput>(
                "gitlab.release.link.delete",
                "Remove an asset link from a release.",
            ),
            release_link_delete,
        )
        // ---- changelog ----
        .operation(
            read_op_typed::<RepositoryChangelogGenerateInput>(
                "gitlab.repository.changelog.generate",
                "Generate Markdown release notes from the commits between two refs without committing.",
            ),
            changelog_generate,
        )
        .operation(
            write_op_typed::<RepositoryChangelogAddInput>(
                "gitlab.repository.changelog.add",
                "Generate a changelog section and commit it into the repository's changelog file (default CHANGELOG.md).",
            ),
            changelog_add,
        )
        // ---- archive (blob) ----
        .operation(
            read_op_typed::<RepositoryArchiveInput>(
                "gitlab.repository.archive",
                "Download a repository archive (tar.gz/zip/tar) at a ref into the host blob store. Refuses archives over max_bytes (default 50 MiB) — raise it explicitly for bigger repos.",
            ),
            repository_archive,
        )
        // ---- CI/CD job-token scope (inbound token access allowlist) ----
        .operation(
            read_op_typed::<CiJobTokenScopeShowInput>(
                "gitlab.ci.job_token.scope.show",
                "Show a project's CI/CD job-token inbound/outbound access scope settings.",
            ),
            ci_job_token_scope_show,
        )
        .operation(
            write_op_typed::<CiJobTokenScopeSetInput>(
                "gitlab.ci.job_token.scope.set",
                "Enable/disable a project's inbound CI/CD job-token access enforcement (enabled=true restricts to the allowlist).",
            ),
            ci_job_token_scope_set,
        )
        .operation(
            read_op_typed::<CiJobTokenAllowlistListInput>(
                "gitlab.ci.job_token.allowlist.list",
                "List the projects allowed to use their CI_JOB_TOKEN to access this project.",
            ),
            ci_job_token_allowlist_list,
        )
        .operation(
            write_op_typed::<CiJobTokenAllowlistAddInput>(
                "gitlab.ci.job_token.allowlist.add",
                "Add a project (by numeric id) to this project's CI job-token allowlist, letting its CI clone/access this project via CI_JOB_TOKEN.",
            ),
            ci_job_token_allowlist_add,
        )
        .operation(
            risked(
                write_op_typed::<CiJobTokenAllowlistRemoveInput>(
                    "gitlab.ci.job_token.allowlist.remove",
                    "Remove a project from this project's CI job-token allowlist (may break that project's CI access). Pass confirm_target_project_id to guard against mistakes.",
                ),
                Risk::High,
            ),
            ci_job_token_allowlist_remove,
        )
        .operation(
            read_op_typed::<CiJobTokenGroupsAllowlistListInput>(
                "gitlab.ci.job_token.groups_allowlist.list",
                "List the groups allowed to use their CI_JOB_TOKEN to access this project.",
            ),
            ci_job_token_groups_allowlist_list,
        )
        .operation(
            write_op_typed::<CiJobTokenGroupsAllowlistAddInput>(
                "gitlab.ci.job_token.groups_allowlist.add",
                "Add a group (by numeric id) to this project's CI job-token groups allowlist.",
            ),
            ci_job_token_groups_allowlist_add,
        )
        .operation(
            risked(
                write_op_typed::<CiJobTokenGroupsAllowlistRemoveInput>(
                    "gitlab.ci.job_token.groups_allowlist.remove",
                    "Remove a group from this project's CI job-token groups allowlist. Pass confirm_target_group_id to guard against mistakes.",
                ),
                Risk::High,
            ),
            ci_job_token_groups_allowlist_remove,
        )
        // ---- protected tags ----
        .operation(
            read_op_typed::<RepositoryProtectedTagListInput>(
                "gitlab.repository.protected_tag.list",
                "List a project's protected tags with their create-access levels.",
            ),
            protected_tag_list,
        )
        .operation(
            read_op_typed::<RepositoryProtectedTagShowInput>(
                "gitlab.repository.protected_tag.show",
                "Show one protected tag (by name or wildcard pattern).",
            ),
            protected_tag_show,
        )
        .operation(
            write_op_typed::<RepositoryProtectedTagProtectInput>(
                "gitlab.repository.protected_tag.protect",
                "Protect a tag or wildcard pattern (create_access_level defaults to 40 = maintainer).",
            ),
            protected_tag_protect,
        )
        .operation(
            risked(
                write_op_typed::<RepositoryProtectedTagUnprotectInput>(
                    "gitlab.repository.protected_tag.unprotect",
                    "Unprotect a tag or wildcard pattern. Pass confirm_name equal to name to guard against mistakes.",
                ),
                Risk::High,
            ),
            protected_tag_unprotect,
        )
        // ---- deploy tokens ----
        .operation(
            read_op_typed::<DeployTokenListInput>(
                "gitlab.deploy_token.list",
                "List a project's deploy tokens (metadata only; the secret is never returned by list).",
            ),
            deploy_token_list,
        )
        .operation(
            risked(
                write_op_typed::<DeployTokenCreateInput>(
                    "gitlab.deploy_token.create",
                    "Create a project deploy token (scopes e.g. read_repository, read_registry). The response `token` is a secret returned ONCE — store it now.",
                ),
                Risk::High,
            ),
            deploy_token_create,
        )
        .operation(
            risked(
                write_op_typed::<DeployTokenRevokeInput>(
                    "gitlab.deploy_token.revoke",
                    "Revoke (delete) a project deploy token by numeric id. Pass confirm_token_id equal to token_id to guard against mistakes.",
                ),
                Risk::High,
            ),
            deploy_token_revoke,
        )
        // ---- custom preflight rules (D-88) ----
        // Conditional targets, aliases, regex validity, and empty-update guards the schemas
        // cannot express; host-kit runs them in both --dry-run and runtime dispatch.
        .preflight("gitlab.mr.show", pf_mr_address)
        .preflight("gitlab.mr.update", |i| {
            let mut p = pf_mr_address(i);
            p.extend(pf_any_update(
                i,
                &["title", "description", "target_branch", "state_event", "labels"],
            ));
            p
        })
        .preflight("gitlab.mr.approve", pf_mr_address)
        .preflight("gitlab.mr.merge", pf_mr_address)
        .preflight("gitlab.mr.changes", pf_mr_address)
        .preflight("gitlab.mr.diff.lines", pf_mr_diff_lines)
        .preflight("gitlab.mr.discussion.list", pf_mr_address)
        .preflight("gitlab.mr.note.create", pf_mr_address)
        .preflight("gitlab.mr.discussion.create", pf_mr_discussion_create)
        .preflight("gitlab.mr.discussion.reply", pf_mr_address)
        .preflight("gitlab.mr.discussion.resolve", pf_mr_address)
        .preflight("gitlab.issue.show", pf_issue_address)
        .preflight("gitlab.issue.update", |i| {
            let mut p = pf_issue_address(i);
            p.extend(pf_any_update(
                i,
                &[
                    "title",
                    "description",
                    "labels",
                    "add_labels",
                    "remove_labels",
                    "state_event",
                    "assignee_ids",
                ],
            ));
            p
        })
        .preflight("gitlab.issue.note.list", pf_issue_address)
        .preflight("gitlab.issue.note.create", pf_issue_address)
        .preflight("gitlab.branch.create", pf_branch)
        .preflight("gitlab.branch.delete", pf_branch)
        .preflight("gitlab.repository.tag.create", pf_tag_name)
        .preflight("gitlab.repository.tag.show", pf_tag_name)
        .preflight("gitlab.repository.tag.delete", pf_tag_name)
        .preflight("gitlab.snippet.delete", pf_snippet_delete)
        .preflight("gitlab.search.blobs", pf_search_blobs)
        .preflight("gitlab.index.build", pf_index_build)
        .preflight("gitlab.release.show", pf_release_tag)
        .preflight("gitlab.release.update", |i| {
            let mut p = pf_release_tag(i);
            p.extend(pf_any_update(
                i,
                &["name", "description", "milestones", "released_at"],
            ));
            p
        })
        .preflight("gitlab.release.delete", pf_release_tag)
        .preflight("gitlab.release.link.list", pf_release_tag)
        .preflight("gitlab.release.link.create", pf_release_tag)
        .preflight("gitlab.release.link.update", pf_release_tag)
        .preflight("gitlab.release.link.delete", pf_release_tag)
}

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

// ---------------------------------------------------------------------------
// HTTP plumbing — every REST verb funnels through `gl_request` (PRIVATE-TOKEN
// header, `gitlab.endpoint` ref + /api/v4 + path, is_success check) so
// auth/encoding stay DRY. The base URL is resolved host-side only (env or the
// manifest's gitlab.com default) — the plugin never holds it (D-32). The
// manifest's `personal_token` auth method is not Header-scheme, so the token
// is still fetched via `host.secret` and sent explicitly as `PRIVATE-TOKEN`.
// ---------------------------------------------------------------------------

fn gl_request(
    host: &mut Host,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<HttpResponse, String> {
    let token = host.secret("personal_token")?;
    let mut headers: Vec<(&str, &str)> = vec![("PRIVATE-TOKEN", token.as_str())];
    let body_str;
    let body_ref = match body {
        Some(b) => {
            body_str = serde_json::to_string(b).map_err(|e| e.to_string())?;
            headers.push(("content-type", "application/json"));
            Some(body_str.as_bytes())
        }
        None => None,
    };
    let resp = host.http_ref(
        "gitlab.endpoint",
        method,
        &format!("/api/v4{path}"),
        None,
        &headers,
        body_ref,
    )?;
    if !resp.is_success() {
        return Err(format!(
            "gitlab {method} {path} → {} {}",
            resp.status, resp.body
        ));
    }
    Ok(resp)
}

/// GET `/api/v4{path}` on the `gitlab.endpoint` ref and return the parsed JSON.
fn gl_get(host: &mut Host, path: &str) -> Result<Value, String> {
    gl_request(host, "GET", path, None)?.json()
}

/// POST a JSON body and return the parsed JSON response.
fn gl_post(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    gl_request(host, "POST", path, Some(body))?.json()
}

/// PUT a JSON body and return the parsed JSON response.
fn gl_put(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    gl_request(host, "PUT", path, Some(body))?.json()
}

/// DELETE a path; GitLab replies 204 (no body), so nothing is parsed.
fn gl_delete(host: &mut Host, path: &str) -> Result<(), String> {
    gl_request(host, "DELETE", path, None)?;
    Ok(())
}

/// GET raw bytes (for binary downloads like the repository archive) — byte-exact via
/// `http_bytes_ref`, so an archive never round-trips through a UTF-8 string body.
fn gl_get_bytes(host: &mut Host, path: &str) -> Result<Vec<u8>, String> {
    let token = host.secret("personal_token")?;
    let resp = host.http_bytes_ref(
        "gitlab.endpoint",
        "GET",
        &format!("/api/v4{path}"),
        None,
        &[("PRIVATE-TOKEN", token.as_str())],
        None,
        true,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("gitlab GET {path} → {}", resp.status));
    }
    Ok(resp.bytes)
}

// ---------------------------------------------------------------------------
// Input helpers.
// ---------------------------------------------------------------------------

/// Percent-encode an id/path/value so `group/app` → `group%2Fapp` for a URL segment or query value.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A trimmed string for `key`, accepting a JSON string or number; `None` when absent/empty.
fn flex_str(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// The first present integer across `keys`, accepting a JSON integer or numeric string.
fn flex_i64(input: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        match input.get(*key) {
            Some(Value::Number(n)) => {
                if let Some(i) = n.as_i64() {
                    return Some(i);
                }
            }
            Some(Value::String(s)) => {
                if let Ok(i) = s.trim().parse::<i64>() {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The project id/path from `project`/`project_id`/`path` aliases.
fn req_project(input: &Value) -> Result<String, String> {
    for key in ["project", "project_id", "path"] {
        if let Some(s) = flex_str(input, key) {
            return Ok(s);
        }
    }
    Err("`project` (string) required".into())
}

/// Resolve `project` (already numeric, or a `namespace/path`) to its numeric project id.
///
/// GitLab's `job_token_scope/allowlist` and `groups_allowlist` POST/DELETE handlers reject the
/// URL-encoded `namespace%2Fproject` path form with `400 {"error":"id is invalid"}`, even though the
/// matching GET accepts it — they want the numeric id. Resolve it via `/projects/:id` (which does
/// accept the encoded path) rather than encoding a path into these endpoints.
fn resolve_project_id(host: &mut Host, project: &str) -> Result<i64, String> {
    if let Ok(id) = project.parse::<i64>() {
        return Ok(id);
    }
    let obj = gl_get(host, &format!("/projects/{}", enc(project)))?;
    obj.get("id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("could not resolve project `{project}` to a numeric id"))
}

/// Resolve a merge request to (project, iid) from a `ref`/`id` (PROJECT!IID) or project + iid.
fn mr_address(input: &Value) -> Result<(String, i64), String> {
    if let Some(r) = flex_str(input, "ref").or_else(|| flex_str(input, "id")) {
        let (p, iid) = r
            .split_once('!')
            .ok_or("merge request ref must be PROJECT!IID")?;
        let iid = iid
            .trim()
            .parse::<i64>()
            .map_err(|_| "merge request ref must be PROJECT!IID".to_string())?;
        if p.trim().is_empty() || iid <= 0 {
            return Err("merge request ref must be PROJECT!IID".into());
        }
        return Ok((p.trim().to_string(), iid));
    }
    let project = req_project(input)?;
    let iid = flex_i64(input, &["iid", "merge_request_iid"]).ok_or("`iid` (integer) required")?;
    Ok((project, iid))
}

/// Resolve an issue to (project, iid) from a `ref`/`id` (PROJECT#IID) or project + iid.
fn issue_address(input: &Value) -> Result<(String, i64), String> {
    if let Some(r) = flex_str(input, "ref").or_else(|| flex_str(input, "id")) {
        let (p, iid) = r.split_once('#').ok_or("issue ref must be PROJECT#IID")?;
        let iid = iid
            .trim()
            .parse::<i64>()
            .map_err(|_| "issue ref must be PROJECT#IID".to_string())?;
        if p.trim().is_empty() || iid <= 0 {
            return Err("issue ref must be PROJECT#IID".into());
        }
        return Ok((p.trim().to_string(), iid));
    }
    let project = req_project(input)?;
    let iid = flex_i64(input, &["iid", "issue_iid"]).ok_or("`iid` (integer) required")?;
    Ok((project, iid))
}

// ─── custom preflight rules (D-88) ──────────────────────────────────────────
// Constraints the generated schemas cannot express, attached via `PluginBuilder::preflight` so
// host-kit runs them in BOTH `--dry-run` (`plugin.validate`) and runtime dispatch. Each rule
// reuses the SAME resolution helper its handler calls, so the two verdicts cannot drift.

/// GL-004: a merge-request target — `ref`/`id` (PROJECT!IID) or `project` + `iid`.
fn pf_mr_address(input: &Value) -> Vec<String> {
    mr_address(input).err().into_iter().collect()
}

/// GL-004: an issue target — `ref`/`id` (PROJECT#IID) or `project` + `iid`.
fn pf_issue_address(input: &Value) -> Vec<String> {
    issue_address(input).err().into_iter().collect()
}

/// GL-021: an update op must carry at least one updatable field.
fn pf_any_update(input: &Value, keys: &[&str]) -> Vec<String> {
    if body_from(input, keys).is_empty() {
        vec![format!("nothing to update: pass {}", keys.join(", "))]
    } else {
        Vec::new()
    }
}

/// GL-027: `mr.diff.lines` — the MR target, plus `search` must be a compilable regex.
fn pf_mr_diff_lines(input: &Value) -> Vec<String> {
    let mut problems = pf_mr_address(input);
    if let Some(s) = flex_str(input, "search") {
        if let Err(e) = Regex::new(&s) {
            problems.push(format!("search: {e}"));
        }
    }
    problems
}

/// GL-036: `mr.discussion.create` — the MR target, plus the line-anchor conditionals
/// (`path` + `new_line`/`old_line` travel together).
fn pf_mr_discussion_create(input: &Value) -> Vec<String> {
    let mut problems = pf_mr_address(input);
    let path = flex_str(input, "path");
    let new_line = flex_i64(input, &["new_line"]);
    let old_line = flex_i64(input, &["old_line"]);
    if path.is_some() || new_line.is_some() || old_line.is_some() {
        if path.is_none() {
            problems.push("`path` is required for a line-level comment".into());
        }
        if new_line.is_none() && old_line.is_none() {
            problems.push("`new_line` or `old_line` is required for a line-level comment".into());
        }
    }
    problems
}

/// GL-029: `snippet.delete` — `snippet_id` (or its `id` alias) is required.
fn pf_snippet_delete(input: &Value) -> Vec<String> {
    if flex_i64(input, &["snippet_id", "id"]).is_none() {
        vec!["`snippet_id` (integer) required".into()]
    } else {
        Vec::new()
    }
}

/// GL-028: `branch` (or its `name` alias) is required.
fn pf_branch(input: &Value) -> Vec<String> {
    if flex_str(input, "branch")
        .or_else(|| flex_str(input, "name"))
        .is_none()
    {
        vec!["`branch` (string) required".into()]
    } else {
        Vec::new()
    }
}

/// GL-028: a tag op's `tag_name` (or a documented alias) is required.
fn pf_tag_name(input: &Value) -> Vec<String> {
    tag_name(input).err().into_iter().collect()
}

/// GL-028: a release op's `tag_name`/`tag` is required (`name` is a display name, never the tag).
fn pf_release_tag(input: &Value) -> Vec<String> {
    release_tag(input).err().into_iter().collect()
}

/// GL-032/GL-041: blob-search scope must be unambiguous — `project` OR `group`, never both —
/// and `ref` only exists project-scoped.
fn pf_search_blobs(input: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let project = flex_str(input, "project");
    let group = flex_str(input, "group");
    if project.is_some() && group.is_some() {
        problems.push("pass `project` OR `group`, not both (ambiguous search scope)".into());
    }
    if group.is_some() && flex_str(input, "ref").is_some() {
        problems.push(
            "`ref` is not supported for group-scoped blob search (project scope only)".into(),
        );
    }
    problems
}

/// GL-034: index selectors must resolve to at least one known index.
fn pf_index_build(input: &Value) -> Vec<String> {
    index_include(input).err().into_iter().collect()
}

/// GL-015: whether a plain read/list op should feed its results into the local search index.
/// Off by default so a pure read has no datasource side effects (no records, no stderr
/// `(N record(s) contributed)` line — the host prints it only when records are contributed);
/// `index.build` is the deliberate indexing path.
fn wants_contribution(input: &Value) -> bool {
    input
        .get("contribute")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// `&page=N` when the caller asked for a specific 1-based results page (GL-019), else "".
fn page_qs(input: &Value) -> String {
    flex_i64(input, &["page"])
        .map(|p| format!("&page={p}"))
        .unwrap_or_default()
}

/// Clamp a 1-based `limit` to `[1, max]`, falling back to `default` when unset/non-positive.
fn clamp(value: i64, default: i64, max: i64) -> i64 {
    if value <= 0 {
        default
    } else if value > max {
        max
    } else {
        value
    }
}

/// Copy each present, non-null `key` from `input` into a fresh body map.
fn body_from(input: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut m = Map::new();
    for key in keys {
        if let Some(v) = input.get(*key) {
            if !v.is_null() {
                m.insert((*key).to_string(), v.clone());
            }
        }
    }
    m
}

/// Build `?k=v&...` (values percent-encoded); empty values are dropped, empty result is "".
fn qs(pairs: &[(&str, String)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

// ---------------------------------------------------------------------------
// Reads: projects / merge requests / issues / pipelines (the original surface).
// ---------------------------------------------------------------------------

fn project_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let membership = input
        .get("membership")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let search = flex_str(&input, "search")
        .or_else(|| flex_str(&input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(&input, "order_by").unwrap_or_else(|| "last_activity_at".into());
    let sort = flex_str(&input, "sort").unwrap_or_else(|| "desc".into());
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let pairs = [
        (
            "membership",
            if membership {
                "true".into()
            } else {
                "false".into()
            },
        ),
        ("search", search),
        ("order_by", order_by),
        ("sort", sort),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
    ];
    let projects = gl_get(host, &format!("/projects{}", qs(&pairs)))?;
    if wants_contribution(&input) {
        contribute_projects(host, &projects);
    }
    Ok(projects)
}

fn project_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}", enc(&project)))
}

fn mr_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let state = flex_str(&input, "state").unwrap_or_else(|| "opened".into());
    let search = flex_str(&input, "search")
        .or_else(|| flex_str(&input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(&input, "order_by").unwrap_or_else(|| "updated_at".into());
    let sort = flex_str(&input, "sort").unwrap_or_else(|| "desc".into());
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let source_branch = flex_str(&input, "source_branch").unwrap_or_default();
    let target_branch = flex_str(&input, "target_branch").unwrap_or_default();
    let pairs = [
        ("state", state),
        ("search", search),
        ("order_by", order_by),
        ("sort", sort),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("source_branch", source_branch),
        ("target_branch", target_branch),
    ];
    let mrs = gl_get(
        host,
        &format!("/projects/{}/merge_requests{}", enc(&project), qs(&pairs)),
    )?;
    if wants_contribution(&input) {
        contribute_list(host, &mrs, "gitlab.merge_request", &project);
    }
    Ok(mrs)
}

fn mr_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
    )
}

fn issue_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let state = flex_str(&input, "state").unwrap_or_else(|| "opened".into());
    let search = flex_str(&input, "search")
        .or_else(|| flex_str(&input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(&input, "order_by").unwrap_or_default();
    let sort = flex_str(&input, "sort").unwrap_or_default();
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let pairs = [
        ("state", state),
        ("search", search),
        ("order_by", order_by),
        ("sort", sort),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
    ];
    let issues = gl_get(
        host,
        &format!("/projects/{}/issues{}", enc(&project), qs(&pairs)),
    )?;
    if wants_contribution(&input) {
        contribute_list(host, &issues, "gitlab.issue", &project);
    }
    Ok(issues)
}

fn pipeline_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let status = flex_str(&input, "status").unwrap_or_default();
    let git_ref = flex_str(&input, "ref").unwrap_or_default();
    let source = flex_str(&input, "source").unwrap_or_default();
    let username = flex_str(&input, "username").unwrap_or_default();
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("status", status),
        ("ref", git_ref),
        ("source", source),
        ("username", username),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
    ];
    gl_get(
        host,
        &format!("/projects/{}/pipelines{}", enc(&project), qs(&pairs)),
    )
}

// ---------------------------------------------------------------------------
// Auth test + index build.
// ---------------------------------------------------------------------------

fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let user = gl_get(host, "/user")?;
    // GL-016: an auth smoke check needs only enough identity to confirm *which* account the token
    // authenticates as — id/username/name. The full `GET /user` (~50 keys: email, public/commit
    // email, two-factor status, last-sign-in timestamps, …) is sensitive and must never be echoed
    // for a health check, so pin the result to a minimal, documented identity subset.
    let pick = |key: &str| user.get(key).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "status": "ok",
        "text": "GitLab auth OK",
        "user": {
            "id": pick("id"),
            "username": pick("username"),
            "name": pick("name"),
        },
    }))
}

/// Which datasource categories the current `index.build` call should populate.
#[derive(Default)]
struct IndexInclude {
    projects: bool,
    merge_requests: bool,
    issues: bool,
}

fn index_include(input: &Value) -> Result<IndexInclude, String> {
    let mut raw = Vec::new();
    for key in ["index", "indexes", "entity", "entities"] {
        match input.get(key) {
            Some(Value::String(s)) => {
                for part in s.split(',') {
                    raw.push(part.trim().to_lowercase());
                }
            }
            Some(Value::Array(arr)) => {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        raw.push(s.trim().to_lowercase());
                    }
                }
            }
            _ => {}
        }
    }
    let raw: Vec<String> = raw.into_iter().filter(|s| !s.is_empty()).collect();
    if raw.is_empty() {
        return Ok(IndexInclude {
            projects: true,
            merge_requests: true,
            issues: true,
        });
    }
    let mut inc = IndexInclude::default();
    let mut unknown = Vec::new();
    for v in raw {
        match v.as_str() {
            "projects" | "project" | "gitlab.projects" | "gitlab.project" => inc.projects = true,
            "merge_requests"
            | "merge_request"
            | "mr"
            | "mrs"
            | "gitlab.merge_requests"
            | "gitlab.merge_request" => inc.merge_requests = true,
            "issues" | "issue" | "gitlab.issues" | "gitlab.issue" => inc.issues = true,
            other => unknown.push(other.to_string()),
        }
    }
    // GL-034: a selector typo must be an error, not an empty `indexed: 0` success.
    if !unknown.is_empty() {
        return Err(format!(
            "unknown index selector(s): {} (known: projects, merge_requests/mrs, issues)",
            unknown.join(", ")
        ));
    }
    Ok(inc)
}

/// Resolve a 1-based `limit` into `(all_pages, per_page)` for index paging.
/// A positive limit yields a single page of up to `max_per_page` items; otherwise all pages are fetched with `per_page`.
fn page_plan(input: &Value, limit_key: &str, max_per_page: i64) -> (bool, i64) {
    match flex_i64(input, &[limit_key]) {
        Some(v) if v > 0 => (false, clamp(v, 1, max_per_page)),
        _ => (true, max_per_page),
    }
}

/// Drive datasource contribution over the requested selectors. Each category pages via
/// `per_page`/`page` unless a datasource-specific limit pins it to a single page.
fn index_build(input: Value, host: &mut Host) -> Result<Value, String> {
    let include = index_include(&input)?;
    // GL-017: a dry-run scope estimate — describe the breadth WITHOUT crawling or contributing,
    // so a no-argument `index.build` is never a silent instance-wide sweep.
    if input
        .get("estimate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(index_estimate(&input, &include));
    }
    let mut total = 0;
    if include.projects {
        total += index_projects(host, &input);
    }
    if include.merge_requests {
        total += index_merge_requests(host, &input);
    }
    if include.issues {
        total += index_issues(host, &input);
    }
    Ok(json!({ "indexed": total }))
}

/// GL-017: describe the crawl `index.build` is about to run — which datasources, and each one's
/// scope (a named project vs the whole instance) — without any HTTP or contribution. The operator
/// runs this first, sees the breadth, then reruns without `estimate` to actually index.
fn index_estimate(input: &Value, include: &IndexInclude) -> Value {
    let mut would_crawl = Vec::new();
    let mut scopes = Map::new();
    let mut instance_wide = false;
    if include.projects {
        would_crawl.push(json!("projects"));
        let membership = input
            .get("membership")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        // Projects are always instance-scoped (there is no project selector to narrow them).
        instance_wide = true;
        scopes.insert(
            "projects".into(),
            json!(if membership {
                "instance-wide (projects you are a member of)"
            } else {
                "instance-wide (every visible project)"
            }),
        );
    }
    if include.merge_requests {
        would_crawl.push(json!("merge_requests"));
        let project = flex_str(input, "mr_project")
            .or_else(|| flex_str(input, "project"))
            .or_else(|| flex_str(input, "project_id"))
            .or_else(|| flex_str(input, "path"));
        match project {
            Some(p) => {
                scopes.insert("merge_requests".into(), json!(format!("project {p}")));
            }
            None => {
                instance_wide = true;
                scopes.insert(
                    "merge_requests".into(),
                    json!("instance-wide (every visible merge request)"),
                );
            }
        }
    }
    if include.issues {
        would_crawl.push(json!("issues"));
        let project = flex_str(input, "issue_project")
            .or_else(|| flex_str(input, "project"))
            .or_else(|| flex_str(input, "project_id"))
            .or_else(|| flex_str(input, "path"));
        match project {
            Some(p) => {
                scopes.insert("issues".into(), json!(format!("project {p}")));
            }
            None => {
                instance_wide = true;
                scopes.insert(
                    "issues".into(),
                    json!("instance-wide (every visible issue)"),
                );
            }
        }
    }
    let note = if instance_wide {
        "This crawls instance-wide datasources — potentially every visible project/MR/issue. Scope it with a project (project/mr_project/issue_project) or narrow with index/entities, then rerun without estimate to index."
    } else {
        "Rerun without estimate to index the scoped datasources above."
    };
    json!({
        "estimate": true,
        "would_crawl": would_crawl,
        "scopes": Value::Object(scopes),
        "instance_wide": instance_wide,
        "note": note,
    })
}

fn index_projects(host: &mut Host, input: &Value) -> usize {
    let membership = input
        .get("membership")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let search = flex_str(input, "search")
        .or_else(|| flex_str(input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(input, "order_by").unwrap_or_else(|| "last_activity_at".into());
    let sort = flex_str(input, "sort").unwrap_or_else(|| "desc".into());
    let (all_pages, per_page) = page_plan(input, "limit", 100);
    let mut pairs = vec![(
        "membership",
        if membership {
            "true".into()
        } else {
            "false".into()
        },
    )];
    if !search.is_empty() {
        pairs.push(("search", search));
    }
    pairs.push(("order_by", order_by));
    pairs.push(("sort", sort));
    let base = format!("/projects{}", qs(&pairs));
    page_index(host, &base, per_page, all_pages, contribute_projects)
}

fn index_merge_requests(host: &mut Host, input: &Value) -> usize {
    let project = flex_str(input, "mr_project")
        .or_else(|| flex_str(input, "project"))
        .or_else(|| flex_str(input, "project_id"))
        .or_else(|| flex_str(input, "path"));
    let state = flex_str(input, "mr_state").unwrap_or_else(|| "all".into());
    let search = flex_str(input, "mr_search").unwrap_or_default();
    let order_by = flex_str(input, "mr_order_by").unwrap_or_else(|| "updated_at".into());
    let sort = flex_str(input, "mr_sort").unwrap_or_else(|| "desc".into());
    let (all_pages, per_page) = page_plan(input, "mr_limit", 100);
    let mut pairs = vec![("scope", "all".into())];
    if !state.is_empty() {
        pairs.push(("state", state));
    }
    if !search.is_empty() {
        pairs.push(("search", search));
    }
    pairs.push(("order_by", order_by));
    pairs.push(("sort", sort));
    let base = if let Some(project) = project {
        format!("/projects/{}/merge_requests{}", enc(&project), qs(&pairs))
    } else {
        format!("/merge_requests{}", qs(&pairs))
    };
    page_index(host, &base, per_page, all_pages, |h, page| {
        contribute_refs(h, page, "gitlab.merge_request")
    })
}

fn index_issues(host: &mut Host, input: &Value) -> usize {
    // GL-040: honor a project scope for issues, matching MR indexing — `issue_project` (or the
    // shared `project`). Without one, issues are crawled instance-wide.
    let project = flex_str(input, "issue_project")
        .or_else(|| flex_str(input, "project"))
        .or_else(|| flex_str(input, "project_id"))
        .or_else(|| flex_str(input, "path"));
    let state = flex_str(input, "issue_state").unwrap_or_else(|| "all".into());
    let search = flex_str(input, "issue_search").unwrap_or_default();
    let order_by = flex_str(input, "issue_order_by").unwrap_or_else(|| "updated_at".into());
    let sort = flex_str(input, "issue_sort").unwrap_or_else(|| "desc".into());
    let (all_pages, per_page) = page_plan(input, "issue_limit", 100);
    let mut pairs = vec![("scope", "all".into())];
    if !state.is_empty() {
        pairs.push(("state", state));
    }
    if !search.is_empty() {
        pairs.push(("search", search));
    }
    pairs.push(("order_by", order_by));
    pairs.push(("sort", sort));
    let base = if let Some(project) = project {
        format!("/projects/{}/issues{}", enc(&project), qs(&pairs))
    } else {
        format!("/issues{}", qs(&pairs))
    };
    page_index(host, &base, per_page, all_pages, |h, page| {
        contribute_refs(h, page, "gitlab.issue")
    })
}

/// Page `base_path` until exhausted (or a single page when `all_pages` is false),
/// contributing each page and returning the number of records indexed.
fn page_index(
    host: &mut Host,
    base_path: &str,
    per_page: i64,
    all_pages: bool,
    contribute: impl Fn(&mut Host, &Value) -> usize,
) -> usize {
    let mut total = 0;
    let mut page = 1;
    loop {
        let sep = if base_path.contains('?') { "&" } else { "?" };
        let path = format!("{base_path}{sep}per_page={per_page}&page={page}");
        let items = match gl_get(host, &path) {
            Ok(v) => v,
            Err(_) => break,
        };
        let len = items.as_array().map(|a| a.len()).unwrap_or(0);
        if len == 0 {
            break;
        }
        total += contribute(host, &items);
        if !all_pages || len < per_page as usize {
            break;
        }
        page += 1;
    }
    total
}

// ---------------------------------------------------------------------------
// Project / merge request / issue writes.
// ---------------------------------------------------------------------------

fn project_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    let mut body = body_from(
        &input,
        &[
            "path",
            "description",
            "visibility",
            "initialize_with_readme",
        ],
    );
    body.insert("name".into(), json!(name));
    // Resolve a group namespace path → namespace_id (GL-026/GL-046).
    if let Some(namespace) = flex_str(&input, "namespace") {
        let id = resolve_namespace_id(host, &namespace)?;
        body.insert("namespace_id".into(), id);
    }
    gl_post(host, "/projects", &Value::Object(body))
}

/// Resolve a group `namespace` to its numeric id for `project.create` (GL-026/GL-046).
///
/// Robust against the two beta findings: it **paginates** the `/groups` search beyond the first
/// page (the old code capped at `per_page=20`, so a group past the first 20 hits was invisible),
/// and it resolves **unambiguously**. An exact `full_path` match wins deterministically; otherwise
/// a bare basename (`path`) match is used only when it is unique — a basename shared by several
/// nested groups is an error asking for the full path, never a silent first-wins pick.
fn resolve_namespace_id(host: &mut Host, namespace: &str) -> Result<Value, String> {
    let mut exact_full: Option<Value> = None;
    let mut basename: Vec<(String, Value)> = Vec::new();
    let mut page = 1;
    loop {
        let groups = gl_get(
            host,
            &format!("/groups?search={}&per_page=100&page={page}", enc(namespace)),
        )?;
        let arr = match groups.as_array() {
            Some(a) if !a.is_empty() => a.clone(),
            _ => break,
        };
        let len = arr.len();
        for g in &arr {
            let full = g.get("full_path").and_then(|v| v.as_str()).unwrap_or("");
            let path = g.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let Some(id) = g.get("id").filter(|v| !v.is_null()).cloned() else {
                continue;
            };
            if full.eq_ignore_ascii_case(namespace) {
                // First exact full_path match wins deterministically.
                exact_full.get_or_insert(id);
            } else if path.eq_ignore_ascii_case(namespace)
                && !basename.iter().any(|(f, _)| f.eq_ignore_ascii_case(full))
            {
                basename.push((full.to_string(), id));
            }
        }
        if len < 100 {
            break;
        }
        page += 1;
        if page > 50 {
            break; // safety cap: never loop unboundedly on a pathological search
        }
    }
    if let Some(id) = exact_full {
        return Ok(id);
    }
    match basename.len() {
        0 => Err(format!("group {namespace:?} not found")),
        1 => Ok(basename.into_iter().next().unwrap().1),
        _ => {
            let names: Vec<String> = basename.into_iter().map(|(f, _)| f).collect();
            Err(format!(
                "namespace {namespace:?} is ambiguous — it matches multiple groups: {}. Pass the full group path.",
                names.join(", ")
            ))
        }
    }
}

fn project_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    // GL-005: fat-finger guards — a supplied confirm must match, an absent one stays ergonomic.
    confirm_str(&input, "confirm_path", &project)?;
    if flex_i64(&input, &["confirm_project_id"]).is_some() {
        let id = resolve_project_id(host, &project)?;
        confirm_i64(&input, "confirm_project_id", id)?;
    }
    gl_delete(host, &format!("/projects/{}", enc(&project)))?;
    Ok(json!({ "project": project, "message": "project deleted" }))
}

fn mr_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    for key in ["title", "source_branch", "target_branch"] {
        if flex_str(&input, key).is_none() {
            return Err(format!("`{key}` (string) required"));
        }
    }
    let body = body_from(
        &input,
        &[
            "title",
            "source_branch",
            "target_branch",
            "description",
            "labels",
            "assignee_id",
            "assignee_ids",
            "reviewer_ids",
            "target_project_id",
            "milestone_id",
            "remove_source_branch",
            "squash",
            "allow_collaboration",
        ],
    );
    gl_post(
        host,
        &format!("/projects/{}/merge_requests", enc(&project)),
        &Value::Object(body),
    )
}

fn mr_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let body = body_from(
        &input,
        &[
            "title",
            "description",
            "target_branch",
            "state_event",
            "labels",
        ],
    );
    if body.is_empty() {
        return Err(
            "nothing to update: pass title, description, target_branch, state_event, or labels"
                .into(),
        );
    }
    gl_put(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
        &Value::Object(body),
    )
}

fn mr_approve(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let body = body_from(&input, &["sha"]);
    gl_post(
        host,
        &format!("/projects/{}/merge_requests/{iid}/approve", enc(&project)),
        &Value::Object(body),
    )
}

fn mr_merge(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let mut body = body_from(
        &input,
        &[
            "merge_commit_message",
            "squash_commit_message",
            "squash",
            "should_remove_source_branch",
            "sha",
        ],
    );
    if body.get("should_remove_source_branch").is_none() {
        if let Some(v) = input.get("remove_source_branch") {
            if !v.is_null() {
                body.insert("should_remove_source_branch".into(), v.clone());
            }
        }
    }
    // GitLab's modern accept-MR parameter is `auto_merge` (the older
    // `merge_when_pipeline_succeeds` is deprecated), matching the reference.
    if let Some(v) = input.get("auto_merge") {
        if !v.is_null() {
            body.insert("auto_merge".into(), v.clone());
        }
    }
    gl_put(
        host,
        &format!("/projects/{}/merge_requests/{iid}/merge", enc(&project)),
        &Value::Object(body),
    )
}

fn issue_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    gl_get(host, &format!("/projects/{}/issues/{iid}", enc(&project)))
}

fn issue_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    if flex_str(&input, "title").is_none() {
        return Err("`title` (string) required".into());
    }
    let body = body_from(
        &input,
        &[
            "title",
            "description",
            "labels",
            "assignee_ids",
            "milestone_id",
            "confidential",
        ],
    );
    gl_post(
        host,
        &format!("/projects/{}/issues", enc(&project)),
        &Value::Object(body),
    )
}

fn issue_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    let body = body_from(
        &input,
        &[
            "title",
            "description",
            "labels",
            "add_labels",
            "remove_labels",
            "state_event",
            "assignee_ids",
        ],
    );
    gl_put(
        host,
        &format!("/projects/{}/issues/{iid}", enc(&project)),
        &Value::Object(body),
    )
}

fn issue_note_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("sort", flex_str(&input, "sort").unwrap_or_default()),
        ("order_by", flex_str(&input, "order_by").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!(
            "/projects/{}/issues/{iid}/notes{}",
            enc(&project),
            qs(&pairs)
        ),
    )
}

fn issue_note_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    let body = flex_str(&input, "body").ok_or("`body` (string) required")?;
    gl_post(
        host,
        &format!("/projects/{}/issues/{iid}/notes", enc(&project)),
        &json!({ "body": body }),
    )
}

// ---------------------------------------------------------------------------
// Branches.
// ---------------------------------------------------------------------------

fn branch_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let branch = flex_str(&input, "branch")
        .or_else(|| flex_str(&input, "name"))
        .ok_or("`branch` (string) required")?;
    let git_ref = flex_str(&input, "ref").ok_or("`ref` (string) required")?;
    gl_post(
        host,
        &format!("/projects/{}/repository/branches", enc(&project)),
        &json!({ "branch": branch, "ref": git_ref }),
    )
}

fn branch_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let branch = flex_str(&input, "branch")
        .or_else(|| flex_str(&input, "name"))
        .ok_or("`branch` (string) required")?;
    confirm_str(&input, "confirm_branch", &branch)?;
    gl_delete(
        host,
        &format!(
            "/projects/{}/repository/branches/{}",
            enc(&project),
            enc(&branch)
        ),
    )?;
    Ok(json!({ "project": project, "branch": branch, "message": "branch deleted" }))
}

fn branch_delete_merged(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    confirm_str(&input, "confirm_project", &project)?;
    gl_delete(
        host,
        &format!("/projects/{}/repository/merged_branches", enc(&project)),
    )?;
    Ok(json!({ "project": project, "message": "merged branches deletion requested" }))
}

// ---------------------------------------------------------------------------
// Repository files + tree.
// ---------------------------------------------------------------------------

fn repo_file_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, file_path) = repo_file_target(&input)?;
    require_keys(&input, &["branch", "content", "commit_message"])?;
    let body = body_from(
        &input,
        &[
            "branch",
            "content",
            "commit_message",
            "encoding",
            "start_branch",
            "author_email",
            "author_name",
            "execute_filemode",
        ],
    );
    gl_post(
        host,
        &format!(
            "/projects/{}/repository/files/{}",
            enc(&project),
            enc(&file_path)
        ),
        &Value::Object(body),
    )
}

fn repo_file_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, file_path) = repo_file_target(&input)?;
    require_keys(&input, &["branch", "content", "commit_message"])?;
    let body = body_from(
        &input,
        &[
            "branch",
            "content",
            "commit_message",
            "encoding",
            "start_branch",
            "author_email",
            "author_name",
            "last_commit_id",
            "execute_filemode",
        ],
    );
    gl_put(
        host,
        &format!(
            "/projects/{}/repository/files/{}",
            enc(&project),
            enc(&file_path)
        ),
        &Value::Object(body),
    )
}

fn repo_file_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, file_path) = repo_file_target(&input)?;
    confirm_str(&input, "confirm_file_path", &file_path)?;
    require_keys(&input, &["branch", "commit_message"])?;
    let body = body_from(
        &input,
        &[
            "branch",
            "commit_message",
            "start_branch",
            "author_email",
            "author_name",
            "last_commit_id",
        ],
    );
    // The delete-file endpoint takes the commit params in the body.
    gl_request(
        host,
        "DELETE",
        &format!(
            "/projects/{}/repository/files/{}",
            enc(&project),
            enc(&file_path)
        ),
        Some(&Value::Object(body)),
    )?;
    Ok(json!({
        "project": project,
        "file_path": file_path,
        "branch": flex_str(&input, "branch"),
        "message": "repository file deleted"
    }))
}

fn repo_file_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let path = flex_str(&input, "path").ok_or("`path` (string) required")?;
    let git_ref = match flex_str(&input, "ref") {
        Some(r) => r,
        None => {
            // The files API needs an explicit ref — fall back to the project default branch.
            let project_obj = gl_get(host, &format!("/projects/{}", enc(&project)))?;
            project_obj
                .get("default_branch")
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or("project has no default branch — pass ref explicitly")?
        }
    };
    let mut file = gl_get(
        host,
        &format!(
            "/projects/{}/repository/files/{}?ref={}",
            enc(&project),
            enc(&path),
            enc(&git_ref)
        ),
    )?;
    if let Some(max_bytes) = flex_i64(&input, &["max_bytes"]) {
        if max_bytes > 0 {
            let max = max_bytes as usize;
            let is_b64 = file.get("encoding").and_then(|v| v.as_str()) == Some("base64");
            let mut truncated = false;
            if let Some(Value::String(content)) = file.get_mut("content") {
                if is_b64 {
                    // GL-013: the cap applies to DECODED bytes and the prefix is re-encoded, so
                    // `content` stays valid base64 — truncating the base64 string itself would
                    // hand back an undecodable fragment.
                    use base64::Engine as _;
                    let engine = base64::engine::general_purpose::STANDARD;
                    let compact: String = content.split_whitespace().collect();
                    if let Ok(decoded) = engine.decode(compact) {
                        if decoded.len() > max {
                            *content = engine.encode(&decoded[..max]);
                            truncated = true;
                        }
                    }
                } else if content.len() > max {
                    let mut end = max;
                    while end > 0 && !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    *content = content[..end].to_string();
                    truncated = true;
                }
            }
            if truncated {
                file["truncated"] = json!(true);
            }
        }
    }
    // GL-006: convenience decoded text for UTF-8 files. GitLab returns file content base64-encoded;
    // agents and CLI users almost always want the text. Decode the (post-`max_bytes`) base64 into
    // `decoded_content` when it is valid UTF-8, leaving the raw `content`/`encoding` untouched for
    // existing consumers. Binary files (and a truncation that split a multi-byte char) simply omit
    // the field, so nothing breaks.
    let is_b64 = file.get("encoding").and_then(|v| v.as_str()) == Some("base64");
    if is_b64 {
        if let Some(content) = file.get("content").and_then(|v| v.as_str()) {
            use base64::Engine as _;
            let engine = base64::engine::general_purpose::STANDARD;
            let compact: String = content.split_whitespace().collect();
            if let Ok(decoded) = engine.decode(compact) {
                if let Ok(text) = String::from_utf8(decoded) {
                    file["decoded_content"] = json!(text);
                }
            }
        }
    }
    Ok(file)
}

fn repo_tree(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        200,
        2000,
    );
    let recursive = input
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("path", flex_str(&input, "path").unwrap_or_default()),
        ("ref", flex_str(&input, "ref").unwrap_or_default()),
        (
            "recursive",
            if recursive {
                "true".into()
            } else {
                String::new()
            },
        ),
    ];
    gl_get(
        host,
        &format!("/projects/{}/repository/tree{}", enc(&project), qs(&pairs)),
    )
}

/// (project, file_path) for the repository-file write ops.
fn repo_file_target(input: &Value) -> Result<(String, String), String> {
    let project = req_project(input)?;
    let file_path = flex_str(input, "file_path").ok_or("`file_path` (string) required")?;
    Ok((project, file_path))
}

fn require_keys(input: &Value, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        if input.get(*key).map(|v| v.is_null()).unwrap_or(true) {
            return Err(format!("`{key}` required"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commits.
// ---------------------------------------------------------------------------

fn commit_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    require_keys(&input, &["branch", "commit_message"])?;
    let actions = input
        .get("actions")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`actions` (non-empty array) required")?;
    let mut body = body_from(
        &input,
        &[
            "branch",
            "commit_message",
            "start_branch",
            "start_sha",
            "start_project",
            "author_email",
            "author_name",
            "force",
        ],
    );
    body.insert("actions".into(), json!(actions));
    gl_post(
        host,
        &format!("/projects/{}/repository/commits", enc(&project)),
        &Value::Object(body),
    )
}

fn commit_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("ref_name", flex_str(&input, "ref").unwrap_or_default()),
        ("path", flex_str(&input, "file_path").unwrap_or_default()),
        ("author", flex_str(&input, "author").unwrap_or_default()),
        ("since", flex_str(&input, "since").unwrap_or_default()),
        ("until", flex_str(&input, "until").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!(
            "/projects/{}/repository/commits{}",
            enc(&project),
            qs(&pairs)
        ),
    )
}

// ---------------------------------------------------------------------------
// Tags.
// ---------------------------------------------------------------------------

fn tag_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag_name = flex_str(&input, "tag_name")
        .or_else(|| flex_str(&input, "name"))
        .ok_or("`tag_name` (string) required")?;
    let git_ref = flex_str(&input, "ref").ok_or("`ref` (string) required")?;
    let mut body = json!({ "tag_name": tag_name, "ref": git_ref });
    if let Some(msg) = flex_str(&input, "message") {
        body["message"] = json!(msg);
    }
    gl_post(
        host,
        &format!("/projects/{}/repository/tags", enc(&project)),
        &body,
    )
}

fn tag_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("search", flex_str(&input, "search").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!("/projects/{}/repository/tags{}", enc(&project), qs(&pairs)),
    )
}

fn tag_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/repository/tags/{}", enc(&project), enc(&tag)),
    )
}

fn tag_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
    confirm_str(&input, "confirm_tag_name", &tag)?;
    gl_delete(
        host,
        &format!("/projects/{}/repository/tags/{}", enc(&project), enc(&tag)),
    )?;
    Ok(json!({ "project": project, "tag_name": tag, "message": "tag deleted" }))
}

/// A tag name from `tag_name`/`tag`/`name` aliases (tag ops only — see [`release_tag`]).
fn tag_name(input: &Value) -> Result<String, String> {
    flex_str(input, "tag_name")
        .or_else(|| flex_str(input, "tag"))
        .or_else(|| flex_str(input, "name"))
        .ok_or_else(|| "`tag_name` (string) required".into())
}

/// The release tag from `tag_name`/`tag` — deliberately NOT `name`, which is the release/link
/// display-name field on the release ops (GL-028: the old `name` fallback could silently treat
/// a display name as the tag).
fn release_tag(input: &Value) -> Result<String, String> {
    flex_str(input, "tag_name")
        .or_else(|| flex_str(input, "tag"))
        .ok_or_else(|| "`tag_name` (string) required".into())
}

// ---------------------------------------------------------------------------
// Snippets.
// ---------------------------------------------------------------------------

fn snippet_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let title = flex_str(&input, "title").ok_or("`title` (string) required")?;
    let files = input
        .get("files")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`files` (non-empty array) required")?;
    let visibility = flex_str(&input, "visibility").unwrap_or_else(|| "private".into());
    let mut body = json!({ "title": title, "visibility": visibility, "files": files });
    if let Some(desc) = flex_str(&input, "description") {
        body["description"] = json!(desc);
    }
    gl_post(host, "/snippets", &body)
}

fn snippet_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = flex_i64(&input, &["snippet_id", "id"]).ok_or("`snippet_id` (integer) required")?;
    confirm_i64(&input, "confirm_snippet_id", id)?;
    gl_delete(host, &format!("/snippets/{id}"))?;
    Ok(json!({ "snippet_id": id, "message": "snippet deleted" }))
}

// ---------------------------------------------------------------------------
// Search.
// ---------------------------------------------------------------------------

fn search_blobs(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = flex_str(&input, "query").ok_or("`query` (string) required")?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let project = flex_str(&input, "project");
    let group = flex_str(&input, "group");
    let git_ref = flex_str(&input, "ref").unwrap_or_default();
    let page = page_qs(&input);
    let scope = format!("?scope=blobs&search={}&per_page={limit}{page}", enc(&query));
    let path = if let Some(p) = project {
        let r = if git_ref.is_empty() {
            String::new()
        } else {
            format!("&ref={}", enc(&git_ref))
        };
        format!("/projects/{}/search{scope}{r}", enc(&p))
    } else if let Some(g) = group {
        format!("/groups/{}/search{scope}", enc(&g))
    } else {
        format!("/search{scope}")
    };
    let mut matches = gl_get(host, &path)?;
    if let Some(max_data_bytes) = flex_i64(&input, &["max_data_bytes"]) {
        if max_data_bytes > 0 {
            if let Some(arr) = matches.as_array_mut() {
                let max = max_data_bytes as usize;
                for m in arr {
                    if let Some(Value::String(data)) = m.get_mut("data") {
                        if data.len() > max {
                            // The cap includes the marker (GL-035): the returned string never
                            // exceeds the requested max_data_bytes.
                            const MARKER: &str = "\n[snippet truncated]";
                            let budget = max.saturating_sub(MARKER.len());
                            let mut end = budget;
                            while end > 0 && !data.is_char_boundary(end) {
                                end -= 1;
                            }
                            *data = if end == 0 {
                                let mut bare = max.min(data.len());
                                while bare > 0 && !data.is_char_boundary(bare) {
                                    bare -= 1;
                                }
                                data[..bare].to_string()
                            } else {
                                format!("{}{MARKER}", &data[..end])
                            };
                            m["data_truncated"] = json!(true);
                        }
                    }
                }
            }
        }
    }
    Ok(matches)
}

// ---------------------------------------------------------------------------
// Review: changes / diff lines / compare / discussions.
// ---------------------------------------------------------------------------

fn mr_changes(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let max_files = clamp(flex_i64(&input, &["max_files"]).unwrap_or(0), 50, 200) as usize;
    let max_diff_bytes = clamp(
        flex_i64(&input, &["max_diff_bytes"]).unwrap_or(0),
        16384,
        262144,
    ) as usize;
    let file_filter = flex_str(&input, "file");
    let mut files = Vec::new();
    let mut files_truncated = false;
    // Paginate the diff list (unique `/diffs` substring, fetched before the MR detail) and apply
    // the `file` filter BEFORE the file cap (GL-042) — asking for a specific file can never
    // return empty just because it sits beyond the first page (GL-043).
    let mut page = 1;
    loop {
        let diffs = gl_get(
            host,
            &format!(
                "/projects/{}/merge_requests/{iid}/diffs?per_page=100&page={page}",
                enc(&project)
            ),
        )?;
        let arr = diffs.as_array().cloned().unwrap_or_default();
        let page_len = arr.len();
        for f in &arr {
            if let Some(ff) = &file_filter {
                let np = f.get("new_path").and_then(|v| v.as_str()).unwrap_or("");
                let op = f.get("old_path").and_then(|v| v.as_str()).unwrap_or("");
                if np != ff && op != ff {
                    continue;
                }
            }
            if files.len() >= max_files {
                // GL-044: the file-count cut has its own top-level flag, distinct from the
                // per-file `diff_truncated`.
                files_truncated = true;
                break;
            }
            let mut fc = f.clone();
            if let Some(d) = f.get("diff").and_then(|v| v.as_str()) {
                if let Some(capped) = cap_bytes(d, max_diff_bytes) {
                    fc["diff"] = json!(capped);
                    fc["diff_truncated"] = json!(true);
                }
            }
            files.push(fc);
        }
        let filter_satisfied = file_filter.is_some() && !files.is_empty();
        if files_truncated || filter_satisfied || page_len < 100 {
            break;
        }
        page += 1;
    }
    let detail = gl_get(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
    )?;
    let diff_refs = detail.get("diff_refs").cloned().unwrap_or(Value::Null);
    let count = files.len();
    Ok(json!({
        "project": project, "iid": iid, "diff_refs": diff_refs, "files": files,
        "count": count, "files_truncated": files_truncated
    }))
}

fn mr_diff_lines(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let file = flex_str(&input, "file").ok_or("`file` (string) required")?;
    let fd = fetch_file_diff(host, &project, iid, &file)?
        .ok_or_else(|| format!("file {file:?} is not part of this merge request"))?;
    let parsed = parse_unified_diff(fd.get("diff").and_then(|v| v.as_str()).unwrap_or(""));
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 200, 2000) as usize;
    let mut lines = Vec::new();
    let mut truncated = false;
    // Anchor on a new-file `line`, or an old-file `old_line` (GL-047 — deleted/context lines);
    // `line` wins when both are set.
    let anchor = flex_i64(&input, &["line"])
        .map(|t| (t, false))
        .or_else(|| flex_i64(&input, &["old_line"]).map(|t| (t, true)));
    if let Some((target, on_old)) = anchor {
        let ctx = flex_i64(&input, &["context"]).unwrap_or(3).max(0) as usize;
        let pos = if on_old {
            parsed
                .iter()
                .position(|l| l.old_line == target && l.kind != "added")
        } else {
            parsed
                .iter()
                .position(|l| l.new_line == target && l.kind != "deleted")
        };
        match pos {
            Some(idx) => {
                let start = idx.saturating_sub(ctx);
                let end = (idx + ctx + 1).min(parsed.len());
                for (i, l) in parsed[start..end].iter().enumerate() {
                    let mut o = diff_line_json(l);
                    if start + i == idx {
                        o["target"] = json!(true);
                    }
                    lines.push(o);
                }
            }
            None => {
                let side = if on_old { "old-file" } else { "new-file" };
                return Ok(json!({
                    "project": project, "iid": iid, "file": file, "lines": [], "count": 0,
                    "hint": format!("{side} line {target} is not part of this file's diff")
                }));
            }
        }
    } else if let Some(search) = flex_str(&input, "search") {
        // Regex search over line content (matching the reference's `SearchLines`),
        // not a plain substring scan.
        let re = Regex::new(&search).map_err(|e| format!("search: {e}"))?;
        for l in &parsed {
            if re.is_match(&l.content) {
                if lines.len() >= limit {
                    truncated = true;
                    break;
                }
                lines.push(diff_line_json(l));
            }
        }
    } else {
        for l in &parsed {
            if lines.len() >= limit {
                truncated = true;
                break;
            }
            lines.push(diff_line_json(l));
        }
    }
    let count = lines.len();
    Ok(json!({
        "project": project, "iid": iid, "file": file,
        "old_path": fd.get("old_path"), "new_path": fd.get("new_path"),
        "lines": lines, "count": count, "truncated": truncated
    }))
}

fn compare(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let from = flex_str(&input, "from").ok_or("`from` (string) required")?;
    let to = flex_str(&input, "to").ok_or("`to` (string) required")?;
    let straight = input
        .get("straight")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_files = clamp(flex_i64(&input, &["max_files"]).unwrap_or(0), 50, 200) as usize;
    let max_diff_bytes = clamp(
        flex_i64(&input, &["max_diff_bytes"]).unwrap_or(0),
        16384,
        262144,
    ) as usize;
    let result = gl_get(
        host,
        &format!(
            "/projects/{}/repository/compare?from={}&to={}{}",
            enc(&project),
            enc(&from),
            enc(&to),
            if straight { "&straight=true" } else { "" }
        ),
    )?;
    let max_commits = clamp(flex_i64(&input, &["max_commits"]).unwrap_or(0), 50, 500) as usize;
    let commit_arr = result
        .get("commits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // GL-045: commits are capped with their own marker; `commit_count` stays the full total.
    let commit_count = commit_arr.len();
    let commits_truncated = commit_count > max_commits;
    let commits: Vec<Value> = commit_arr.into_iter().take(max_commits).collect();
    let mut files = Vec::new();
    let mut files_truncated = false;
    let mut any_diff_truncated = false;
    if let Some(arr) = result.get("diffs").and_then(|v| v.as_array()) {
        for f in arr {
            if files.len() >= max_files {
                files_truncated = true;
                break;
            }
            let mut fc = f.clone();
            if let Some(d) = f.get("diff").and_then(|v| v.as_str()) {
                if let Some(capped) = cap_bytes(d, max_diff_bytes) {
                    fc["diff"] = json!(capped);
                    fc["diff_truncated"] = json!(true);
                    any_diff_truncated = true;
                }
            }
            files.push(fc);
        }
    }
    let file_count = files.len();
    // GL-014: the top-level flag is true when ANYTHING was cut — dropped files, a capped
    // per-file diff, or capped commits — with per-cause flags alongside.
    Ok(json!({
        "project": project, "from": from, "to": to,
        "web_url": result.get("web_url"),
        "commits": commits, "commit_count": commit_count,
        "commits_truncated": commits_truncated,
        "files": files, "file_count": file_count,
        "files_truncated": files_truncated,
        "truncated": files_truncated || any_diff_truncated || commits_truncated
    }))
}

fn mr_discussion_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        50,
        200,
    );
    let page = page_qs(&input);
    gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions?per_page={limit}{page}",
            enc(&project)
        ),
    )
}

fn mr_note_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let body = flex_str(&input, "body").ok_or("`body` (string) required")?;
    gl_post(
        host,
        &format!("/projects/{}/merge_requests/{iid}/notes", enc(&project)),
        &json!({ "body": body }),
    )
}

fn mr_discussion_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let comment = flex_str(&input, "body").ok_or("`body` (string) required")?;
    let dry_run = input
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = flex_str(&input, "path");
    let new_line = flex_i64(&input, &["new_line"]);
    let old_line = flex_i64(&input, &["old_line"]);
    let positioned = path.is_some() || new_line.is_some() || old_line.is_some();

    let mut position = Value::Null;
    if positioned {
        let path = path.ok_or("`path` is required for a line-level comment")?;
        if new_line.is_none() && old_line.is_none() {
            return Err("`new_line` or `old_line` is required for a line-level comment".into());
        }
        let detail = gl_get(
            host,
            &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
        )?;
        let refs = detail.get("diff_refs").cloned().unwrap_or(Value::Null);
        let fd = fetch_file_diff(host, &project, iid, &path)?
            .ok_or_else(|| format!("file {path:?} is not part of this merge request"))?;
        let old_path = fd
            .get("old_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string();
        let new_path = fd
            .get("new_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string();
        // Derive the missing side for context lines so GitLab accepts the anchor.
        let parsed = parse_unified_diff(fd.get("diff").and_then(|v| v.as_str()).unwrap_or(""));
        let (mut nl, mut ol) = (new_line, old_line);
        if let (Some(n), None) = (new_line, old_line) {
            if let Some(l) = parsed
                .iter()
                .find(|l| l.new_line == n && l.kind == "context")
            {
                ol = Some(l.old_line);
            }
        } else if let (None, Some(o)) = (new_line, old_line) {
            if let Some(l) = parsed
                .iter()
                .find(|l| l.old_line == o && l.kind == "context")
            {
                nl = Some(l.new_line);
            }
        }
        let mut pos = Map::new();
        pos.insert("position_type".into(), json!("text"));
        pos.insert(
            "base_sha".into(),
            refs.get("base_sha").cloned().unwrap_or(Value::Null),
        );
        pos.insert(
            "start_sha".into(),
            refs.get("start_sha").cloned().unwrap_or(Value::Null),
        );
        pos.insert(
            "head_sha".into(),
            refs.get("head_sha").cloned().unwrap_or(Value::Null),
        );
        pos.insert("old_path".into(), json!(old_path));
        pos.insert("new_path".into(), json!(new_path));
        if let Some(n) = nl {
            pos.insert("new_line".into(), json!(n));
        }
        if let Some(o) = ol {
            pos.insert("old_line".into(), json!(o));
        }
        position = Value::Object(pos);
    }

    if dry_run {
        return Ok(json!({
            "project": project, "iid": iid, "posted": false, "dry_run": true, "position": position
        }));
    }

    let mut body = json!({ "body": comment });
    if !position.is_null() {
        body["position"] = position;
    }
    let discussion = gl_post(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions",
            enc(&project)
        ),
        &body,
    )?;
    Ok(json!({ "project": project, "iid": iid, "posted": true, "discussion": discussion }))
}

fn mr_discussion_reply(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let discussion_id =
        flex_str(&input, "discussion_id").ok_or("`discussion_id` (string) required")?;
    let body = flex_str(&input, "body").ok_or("`body` (string) required")?;
    gl_post(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions/{}/notes",
            enc(&project),
            enc(&discussion_id)
        ),
        &json!({ "body": body }),
    )
}

fn mr_discussion_resolve(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let discussion_id =
        flex_str(&input, "discussion_id").ok_or("`discussion_id` (string) required")?;
    let resolved = input
        .get("resolved")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    gl_put(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions/{}",
            enc(&project),
            enc(&discussion_id)
        ),
        &json!({ "resolved": resolved }),
    )
}

// ---------------------------------------------------------------------------
// CI/CD: variables / pipelines / jobs / environments / deployments.
// ---------------------------------------------------------------------------

fn ci_variable_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    require_keys(&input, &["key", "value"])?;
    let body = body_from(
        &input,
        &[
            "key",
            "value",
            "description",
            "environment_scope",
            "masked",
            "masked_and_hidden",
            "protected",
            "raw",
            "variable_type",
        ],
    );
    gl_post(
        host,
        &format!("/projects/{}/variables", enc(&project)),
        &Value::Object(body),
    )
}

fn ci_variable_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let key = flex_str(&input, "key").ok_or("`key` (string) required")?;
    require_keys(&input, &["value"])?;
    let body = body_from(
        &input,
        &[
            "value",
            "description",
            "environment_scope",
            "masked",
            "protected",
            "raw",
            "variable_type",
        ],
    );
    gl_put(
        host,
        &format!(
            "/projects/{}/variables/{}{}",
            enc(&project),
            enc(&key),
            env_scope_filter(&input)
        ),
        &Value::Object(body),
    )
}

fn ci_variable_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let key = flex_str(&input, "key").ok_or("`key` (string) required")?;
    confirm_str(&input, "confirm_key", &key)?;
    gl_delete(
        host,
        &format!(
            "/projects/{}/variables/{}{}",
            enc(&project),
            enc(&key),
            env_scope_filter(&input)
        ),
    )?;
    Ok(json!({ "project": project, "key": key, "message": "ci variable deleted" }))
}

/// `?filter[environment_scope]=<scope>` when an environment_scope is supplied, else "".
fn env_scope_filter(input: &Value) -> String {
    match flex_str(input, "environment_scope") {
        Some(scope) => format!("?filter[environment_scope]={}", enc(&scope)),
        None => String::new(),
    }
}

fn pipeline_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let git_ref = flex_str(&input, "ref").ok_or("`ref` (string) required")?;
    let mut body = json!({ "ref": git_ref });
    if let Some(vars) = input.get("variables").and_then(|v| v.as_array()) {
        let variables = validate_pipeline_variables(vars)?;
        body["variables"] = json!(variables);
    }
    gl_post(
        host,
        &format!("/projects/{}/pipeline", enc(&project)),
        &body,
    )
}

/// Validate and normalize pipeline `variables` (matching the reference): each entry needs a
/// non-empty `key`, and `variable_type` must be one of `env_var`/`file` when given; the forwarded
/// object carries `key`/`value`/`variable_type`.
fn validate_pipeline_variables(vars: &[Value]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(vars.len());
    for (i, v) in vars.iter().enumerate() {
        let key = flex_str(v, "key").ok_or_else(|| format!("variables[{i}]: key is required"))?;
        let variable_type = match flex_str(v, "variable_type") {
            Some(t) if t == "env_var" || t == "file" => Some(t),
            Some(t) => return Err(format!("variables[{i}]: invalid variable_type {t:?}")),
            None => None,
        };
        let mut entry = Map::new();
        entry.insert("key".into(), json!(key));
        entry.insert(
            "value".into(),
            v.get("value").cloned().unwrap_or(Value::Null),
        );
        if let Some(t) = variable_type {
            entry.insert("variable_type".into(), json!(t));
        }
        out.push(Value::Object(entry));
    }
    Ok(out)
}

fn pipeline_retry(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let id = flex_i64(&input, &["pipeline_id"]).ok_or("`pipeline_id` (integer) required")?;
    gl_post(
        host,
        &format!("/projects/{}/pipelines/{id}/retry", enc(&project)),
        &json!({}),
    )
}

fn pipeline_cancel(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let id = flex_i64(&input, &["pipeline_id"]).ok_or("`pipeline_id` (integer) required")?;
    gl_post(
        host,
        &format!("/projects/{}/pipelines/{id}/cancel", enc(&project)),
        &json!({}),
    )
}

fn job_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let id = flex_i64(&input, &["pipeline_id"]).ok_or("`pipeline_id` (integer) required")?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        50,
        200,
    );
    let page = page_qs(&input);
    let mut path = format!(
        "/projects/{}/pipelines/{id}/jobs?per_page={limit}{page}",
        enc(&project)
    );
    if let Some(scopes) = input.get("scope").and_then(|v| v.as_array()) {
        for s in scopes {
            if let Some(st) = s.as_str() {
                path.push_str(&format!("&scope[]={}", enc(st)));
            }
        }
    }
    gl_get(host, &path)
}

fn environment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("search", flex_str(&input, "search").unwrap_or_default()),
        ("states", flex_str(&input, "states").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!("/projects/{}/environments{}", enc(&project), qs(&pairs)),
    )
}

fn deployment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("order_by", "created_at".to_string()),
        ("sort", "desc".to_string()),
        (
            "environment",
            flex_str(&input, "environment").unwrap_or_default(),
        ),
        ("status", flex_str(&input, "status").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!("/projects/{}/deployments{}", enc(&project), qs(&pairs)),
    )
}

// ---------------------------------------------------------------------------
// Releases + asset links + changelog.
// ---------------------------------------------------------------------------

fn release_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let page = page_qs(&input);
    gl_get(
        host,
        &format!(
            "/projects/{}/releases?per_page={limit}{page}",
            enc(&project)
        ),
    )
}

fn release_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = flex_str(&input, "tag_name").ok_or("`tag_name` (string) required")?;
    let mut body = body_from(
        &input,
        &[
            "ref",
            "name",
            "description",
            "tag_message",
            "milestones",
            "released_at",
        ],
    );
    body.insert("tag_name".into(), json!(tag));
    if let Some(links) = input.get("assets_links").and_then(|v| v.as_array()) {
        body.insert("assets".into(), json!({ "links": links }));
    }
    gl_post(
        host,
        &format!("/projects/{}/releases", enc(&project)),
        &Value::Object(body),
    )
}

fn release_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
    )
}

fn release_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let body = body_from(
        &input,
        &["name", "description", "milestones", "released_at"],
    );
    gl_put(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
        &Value::Object(body),
    )
}

fn release_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    confirm_str(&input, "confirm_tag_name", &tag)?;
    gl_delete(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
    )?;
    Ok(json!({ "project": project, "tag_name": tag, "message": "release deleted" }))
}

fn release_link_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let page = page_qs(&input);
    gl_get(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links?per_page={limit}{page}",
            enc(&project),
            enc(&tag)
        ),
    )
}

fn release_link_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    require_keys(&input, &["name", "url"])?;
    let body = body_from(&input, &["name", "url", "direct_asset_path", "link_type"]);
    gl_post(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links",
            enc(&project),
            enc(&tag)
        ),
        &Value::Object(body),
    )
}

fn release_link_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let link_id = flex_i64(&input, &["link_id"]).ok_or("`link_id` (integer) required")?;
    let body = body_from(&input, &["name", "url", "direct_asset_path", "link_type"]);
    gl_put(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links/{link_id}",
            enc(&project),
            enc(&tag)
        ),
        &Value::Object(body),
    )
}

fn release_link_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let link_id = flex_i64(&input, &["link_id"]).ok_or("`link_id` (integer) required")?;
    gl_delete(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links/{link_id}",
            enc(&project),
            enc(&tag)
        ),
    )?;
    Ok(
        json!({ "project": project, "tag_name": tag, "link_id": link_id, "message": "release link deleted" }),
    )
}

fn changelog_generate(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let version = flex_str(&input, "version").ok_or("`version` (string) required")?;
    let pairs = [
        ("version", version),
        ("from", flex_str(&input, "from").unwrap_or_default()),
        ("to", flex_str(&input, "to").unwrap_or_default()),
        ("date", flex_str(&input, "date").unwrap_or_default()),
        ("trailer", flex_str(&input, "trailer").unwrap_or_default()),
        (
            "config_file",
            flex_str(&input, "config_file").unwrap_or_default(),
        ),
    ];
    gl_get(
        host,
        &format!(
            "/projects/{}/repository/changelog{}",
            enc(&project),
            qs(&pairs)
        ),
    )
}

fn changelog_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let version = flex_str(&input, "version").ok_or("`version` (string) required")?;
    // GL-037: require an explicit target branch — never let GitLab default this write to the
    // repo's default branch. (The schema also marks `branch` required, so the shared preflight
    // rejects a missing/blank branch in both --dry-run and runtime.)
    let branch = flex_str(&input, "branch").ok_or(
        "`branch` (string) required — name the branch to commit the changelog onto, rather than silently writing the default branch",
    )?;
    let mut body = body_from(
        &input,
        &[
            "branch",
            "file",
            "from",
            "to",
            "date",
            "message",
            "trailer",
            "config_file",
        ],
    );
    body.insert("branch".into(), json!(branch));
    body.insert("version".into(), json!(version.clone()));
    // The add-changelog endpoint returns no body.
    gl_request(
        host,
        "POST",
        &format!("/projects/{}/repository/changelog", enc(&project)),
        Some(&Value::Object(body)),
    )?;
    let file = flex_str(&input, "file").unwrap_or_else(|| "CHANGELOG.md".into());
    Ok(json!({
        "project": project, "version": version,
        "branch": flex_str(&input, "branch"), "file": file, "message": "changelog committed"
    }))
}

// ---------------------------------------------------------------------------
// Archive (blob): download then stage through the host blob store.
// ---------------------------------------------------------------------------

fn repository_archive(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let format = flex_str(&input, "format").unwrap_or_else(|| "tar.gz".into());
    let git_ref = flex_str(&input, "ref");
    let sub = flex_str(&input, "path");
    let pairs = [
        ("sha", git_ref.clone().unwrap_or_default()),
        ("path", sub.unwrap_or_default()),
    ];
    let path = format!(
        "/projects/{}/repository/archive.{format}{}",
        enc(&project),
        qs(&pairs)
    );
    let bytes = gl_get_bytes(host, &path)?;
    // GL-023: an "archive read" must not stage an unbounded blob — refuse oversized results
    // (the caller raises max_bytes explicitly to accept a bigger archive).
    let max_bytes = flex_i64(&input, &["max_bytes"])
        .filter(|v| *v > 0)
        .unwrap_or(52_428_800) as usize;
    if bytes.len() > max_bytes {
        return Err(format!(
            "archive is {} bytes, exceeding max_bytes {max_bytes} — pass a larger max_bytes to accept it",
            bytes.len()
        ));
    }
    let mut name = project.replace(['/', ' '], "-");
    if let Some(r) = &git_ref {
        name.push('-');
        name.push_str(&r.replace(['/', ' '], "-"));
    }
    let filename = format!("{name}.{format}");
    let blob_ref = host.blob_put(&filename, &bytes)?;
    Ok(json!({
        "project": project, "ref": git_ref, "format": format,
        "blob_ref": blob_ref, "filename": filename, "bytes": bytes.len()
    }))
}

// ---------------------------------------------------------------------------
// Datasource contribution.
// ---------------------------------------------------------------------------

/// Contribute `gitlab.project` records keyed by `path_with_namespace`; returns the count contributed.
fn contribute_projects(host: &mut Host, projects: &Value) -> usize {
    let Some(arr) = projects.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|p| {
            let id = p.get("path_with_namespace").and_then(|v| v.as_str())?;
            Some(Record::new(
                Source::new("gitlab"),
                "gitlab.project",
                id,
                p.get("name_with_namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id),
                p.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    let n = records.len();
    if n > 0 {
        let _ = host.contribute(&records);
    }
    n
}

/// Contribute project-scoped MR/issue list items keyed by `<project>!<iid>` with title/description;
/// returns the count contributed.
fn contribute_list(host: &mut Host, items: &Value, entity: &str, project: &str) -> usize {
    let Some(arr) = items.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let id = it.get("iid").map(|v| v.to_string())?;
            Some(Record::new(
                Source::new("gitlab"),
                entity,
                format!("{project}!{}", id.trim_matches('"')),
                it.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    let n = records.len();
    if n > 0 {
        let _ = host.contribute(&records);
    }
    n
}

/// Contribute global MR/issue list items, deriving the `project!iid` / `project#iid` id from each
/// item's `references.full` (falling back to the numeric id); returns the count contributed.
fn contribute_refs(host: &mut Host, items: &Value, entity: &str) -> usize {
    let Some(arr) = items.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let id = it
                .get("references")
                .and_then(|r| r.get("full"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| {
                    it.get("id")
                        .map(|v| v.to_string().trim_matches('"').to_string())
                })?;
            Some(Record::new(
                Source::new("gitlab"),
                entity,
                id,
                it.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    let n = records.len();
    if n > 0 {
        let _ = host.contribute(&records);
    }
    n
}

// ---------------------------------------------------------------------------
// Unified-diff parsing (for mr.diff.lines and mr.discussion.create anchoring).
// ---------------------------------------------------------------------------

/// One parsed diff line: `kind` is `added` | `deleted` | `context`; line numbers are 1-based (0 = N/A).
struct DiffLine {
    kind: &'static str,
    old_line: i64,
    new_line: i64,
    content: String,
}

fn diff_line_json(l: &DiffLine) -> Value {
    json!({ "type": l.kind, "old_line": l.old_line, "new_line": l.new_line, "content": l.content })
}

/// Parse a unified diff body (hunks; no `diff --git`/`---`/`+++` file headers expected from GitLab).
fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut old_no = 0i64;
    let mut new_no = 0i64;
    for line in diff.split('\n') {
        if line.starts_with("@@") {
            if let Some(header) = line.strip_prefix("@@").and_then(|r| r.split_once("@@")) {
                for tok in header.0.split_whitespace() {
                    if let Some(t) = tok.strip_prefix('-') {
                        old_no = t.split(',').next().unwrap_or("0").parse().unwrap_or(0);
                    } else if let Some(t) = tok.strip_prefix('+') {
                        new_no = t.split(',').next().unwrap_or("0").parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("diff ")
            || line.starts_with('\\')
        {
            continue;
        }
        match line.chars().next() {
            Some('+') => {
                out.push(DiffLine {
                    kind: "added",
                    old_line: 0,
                    new_line: new_no,
                    content: line[1..].to_string(),
                });
                new_no += 1;
            }
            Some('-') => {
                out.push(DiffLine {
                    kind: "deleted",
                    old_line: old_no,
                    new_line: 0,
                    content: line[1..].to_string(),
                });
                old_no += 1;
            }
            Some(' ') => {
                out.push(DiffLine {
                    kind: "context",
                    old_line: old_no,
                    new_line: new_no,
                    content: line[1..].to_string(),
                });
                old_no += 1;
                new_no += 1;
            }
            _ => {}
        }
    }
    out
}

/// Find one file's diff object within an MR/compare change set by `new_path` or `old_path`.
/// The diff entry for `file`, paginating the MR diff list past a single page (GL-043) — a file
/// beyond the first page of changed files is still addressable. `None` when the file is not part
/// of the merge request.
fn fetch_file_diff(
    host: &mut Host,
    project: &str,
    iid: i64,
    file: &str,
) -> Result<Option<Value>, String> {
    let mut page = 1;
    loop {
        let diffs = gl_get(
            host,
            &format!(
                "/projects/{}/merge_requests/{iid}/diffs?per_page=100&page={page}",
                enc(project)
            ),
        )?;
        if let Some(fd) = find_file_diff(&diffs, file) {
            return Ok(Some(fd.clone()));
        }
        if diffs.as_array().map(|a| a.len()).unwrap_or(0) < 100 {
            return Ok(None);
        }
        page += 1;
    }
}

fn find_file_diff<'a>(diffs: &'a Value, file: &str) -> Option<&'a Value> {
    diffs.as_array()?.iter().find(|f| {
        f.get("new_path").and_then(|v| v.as_str()) == Some(file)
            || f.get("old_path").and_then(|v| v.as_str()) == Some(file)
    })
}

/// Truncate `s` so the RESULT — marker included — is at most `max` bytes on a char boundary;
/// `None` if it fits. The cap is a promise about the returned string (GL-035); when `max` is too
/// small to fit the marker, the bare capped prefix is returned (the caller's `*_truncated` flag
/// still signals the cut).
fn cap_bytes(s: &str, max: usize) -> Option<String> {
    const MARKER: &str = "\n[diff truncated]";
    if max == 0 || s.len() <= max {
        return None;
    }
    let budget = max.saturating_sub(MARKER.len());
    let mut end = budget;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        let mut bare = max.min(s.len());
        while bare > 0 && !s.is_char_boundary(bare) {
            bare -= 1;
        }
        return Some(s[..bare].to_string());
    }
    Some(format!("{}{MARKER}", &s[..end]))
}

// ---------------------------------------------------------------------------
// CI/CD job-token scope, protected tags, deploy tokens (CI governance).
// ---------------------------------------------------------------------------

/// Fat-finger guard for a destructive op: when a `confirm_*` integer field is supplied it must equal
/// the target, else the op is refused; an absent confirm is allowed (so automation stays ergonomic).
fn confirm_i64(input: &Value, field: &str, expected: i64) -> Result<(), String> {
    match flex_i64(input, &[field]) {
        Some(c) if c == expected => Ok(()),
        Some(_) => Err(format!(
            "`{field}` does not match the target — refusing to proceed"
        )),
        None => Ok(()),
    }
}

/// String counterpart of [`confirm_i64`].
fn confirm_str(input: &Value, field: &str, expected: &str) -> Result<(), String> {
    match flex_str(input, field) {
        Some(c) if c == expected => Ok(()),
        Some(_) => Err(format!(
            "`{field}` does not match the target — refusing to proceed"
        )),
        None => Ok(()),
    }
}

fn ci_job_token_scope_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/job_token_scope", enc(&project)),
    )
}

fn ci_job_token_scope_set(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let enabled = input
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or("`enabled` (boolean) required")?;
    // GitLab replies 204 No Content to this PATCH, so synthesize the confirmation.
    gl_request(
        host,
        "PATCH",
        &format!("/projects/{}/job_token_scope", enc(&project)),
        Some(&json!({ "enabled": enabled })),
    )?;
    Ok(json!({ "project": project, "enabled": enabled, "message": "job token scope updated" }))
}

fn ci_job_token_allowlist_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/job_token_scope/allowlist", enc(&project)),
    )
}

fn ci_job_token_allowlist_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_project_id"]).ok_or("`target_project_id` (integer) required")?;
    let project_id = resolve_project_id(host, &project)?;
    gl_post(
        host,
        &format!("/projects/{project_id}/job_token_scope/allowlist"),
        &json!({ "target_project_id": target }),
    )
}

fn ci_job_token_allowlist_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_project_id"]).ok_or("`target_project_id` (integer) required")?;
    confirm_i64(&input, "confirm_target_project_id", target)?;
    let project_id = resolve_project_id(host, &project)?;
    gl_delete(
        host,
        &format!("/projects/{project_id}/job_token_scope/allowlist/{target}"),
    )?;
    Ok(json!({
        "project": project,
        "target_project_id": target,
        "message": "removed from job token allowlist"
    }))
}

fn ci_job_token_groups_allowlist_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!(
            "/projects/{}/job_token_scope/groups_allowlist",
            enc(&project)
        ),
    )
}

fn ci_job_token_groups_allowlist_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_group_id"]).ok_or("`target_group_id` (integer) required")?;
    let project_id = resolve_project_id(host, &project)?;
    gl_post(
        host,
        &format!("/projects/{project_id}/job_token_scope/groups_allowlist"),
        &json!({ "target_group_id": target }),
    )
}

fn ci_job_token_groups_allowlist_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_group_id"]).ok_or("`target_group_id` (integer) required")?;
    confirm_i64(&input, "confirm_target_group_id", target)?;
    let project_id = resolve_project_id(host, &project)?;
    gl_delete(
        host,
        &format!("/projects/{project_id}/job_token_scope/groups_allowlist/{target}"),
    )?;
    Ok(json!({
        "project": project,
        "target_group_id": target,
        "message": "removed from job token groups allowlist"
    }))
}

fn protected_tag_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}/protected_tags", enc(&project)))
}

fn protected_tag_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    gl_get(
        host,
        &format!("/projects/{}/protected_tags/{}", enc(&project), enc(&name)),
    )
}

fn protected_tag_protect(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    let create_access_level = flex_i64(&input, &["create_access_level"]).unwrap_or(40);
    gl_post(
        host,
        &format!("/projects/{}/protected_tags", enc(&project)),
        &json!({ "name": name, "create_access_level": create_access_level }),
    )
}

fn protected_tag_unprotect(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    confirm_str(&input, "confirm_name", &name)?;
    gl_delete(
        host,
        &format!("/projects/{}/protected_tags/{}", enc(&project), enc(&name)),
    )?;
    Ok(json!({ "project": project, "name": name, "message": "tag unprotected" }))
}

fn deploy_token_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}/deploy_tokens", enc(&project)))
}

fn deploy_token_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    let scopes = input
        .get("scopes")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`scopes` (non-empty array, e.g. [\"read_repository\"]) required")?;
    let mut body = body_from(&input, &["expires_at", "username"]);
    body.insert("name".into(), json!(name));
    body.insert("scopes".into(), json!(scopes));
    gl_post(
        host,
        &format!("/projects/{}/deploy_tokens", enc(&project)),
        &Value::Object(body),
    )
}

fn deploy_token_revoke(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let token_id = flex_i64(&input, &["token_id", "id"]).ok_or("`token_id` (integer) required")?;
    confirm_i64(&input, "confirm_token_id", token_id)?;
    gl_delete(
        host,
        &format!("/projects/{}/deploy_tokens/{token_id}", enc(&project)),
    )?;
    Ok(json!({ "project": project, "token_id": token_id, "message": "deploy token revoked" }))
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> MockHost {
        MockHost::default()
            .with_endpoint_ref("gitlab.endpoint", "https://gl.example.com")
            .with_secret("personal_token", "tok")
    }

    fn run(op: &str, input: Value, host: &mut MockHost) -> Value {
        manifest_builder().build().call(op, input, host).unwrap()
    }

    /// The verdict the CLI `--dry-run` path gets from the auto-registered `plugin.validate`
    /// (D-88): `(valid, problems, warnings)` for one op input.
    fn validate(op: &str, input: Value) -> (bool, Vec<String>, Vec<String>) {
        let mut host = MockHost::default();
        let v = manifest_builder()
            .build()
            .call(
                VALIDATE_OP,
                json!({ "operation": op, "input": input }),
                &mut host,
            )
            .expect("plugin.validate answers");
        let strings = |key: &str| -> Vec<String> {
            v[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        };
        (
            v["valid"].as_bool().unwrap(),
            strings("problems"),
            strings("warnings"),
        )
    }

    // ---- D-88: shared dry-run/runtime preflight ----

    /// The keystone failing-first pair: inputs that used to pass the schema-only dry-run and
    /// then fail at runtime now fail `plugin.validate` too — with the SAME problem the runtime
    /// dispatch reports, and without any HTTP.
    #[test]
    fn preflight_dry_run_and_runtime_share_one_verdict() {
        // GL-021: an empty mr.update passed the old dry-run, then the handler rejected it.
        let empty_update = json!({ "project": "group/app", "iid": 5 });
        let (valid, problems, _) = validate("gitlab.mr.update", empty_update.clone());
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("nothing to update")),
            "{problems:?}"
        );
        let runtime_err = manifest_builder()
            .build()
            .call("gitlab.mr.update", empty_update, &mut MockHost::default())
            .unwrap_err();
        assert!(runtime_err.contains("nothing to update"), "{runtime_err}");

        // GL-030: a blank required string passed the old dry-run, then failed flex extraction.
        let (valid, problems, _) = validate("gitlab.project.show", json!({ "project": "   " }));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("`project` is blank")),
            "{problems:?}"
        );
    }

    /// GL-011/GL-022: enum-like fields validate against their allowed set locally.
    #[test]
    fn preflight_enforces_enum_values() {
        let (valid, problems, _) = validate(
            "gitlab.issue.list",
            json!({ "project": "g/a", "state": "wontfix" }),
        );
        assert!(!valid);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("state") && p.contains("must be one of")),
            "{problems:?}"
        );
        let (valid, _, _) = validate(
            "gitlab.issue.list",
            json!({ "project": "g/a", "state": "opened" }),
        );
        assert!(valid);
        // The archive format is a closed set, so an arbitrary string can never reach the URL.
        let (valid, _, _) = validate(
            "gitlab.repository.archive",
            json!({ "project": "g/a", "format": "../../etc" }),
        );
        assert!(!valid);
        let (valid, _, _) = validate(
            "gitlab.repository.archive",
            json!({ "project": "g/a", "format": "zip" }),
        );
        assert!(valid);
        // The remaining GL-011 enum fields reject out-of-set values the same way.
        for (op, input) in [
            (
                "gitlab.project.create",
                json!({ "name": "app", "visibility": "hidden" }),
            ),
            (
                "gitlab.ci.variable.create",
                json!({ "project": "g/a", "key": "K", "value": "v", "variable_type": "blob" }),
            ),
            (
                "gitlab.release.link.create",
                json!({ "project": "g/a", "tag_name": "v1", "name": "installer", "url": "https://x", "link_type": "binary" }),
            ),
        ] {
            let (valid, problems, _) = validate(op, input);
            assert!(!valid, "{op}");
            assert!(
                problems.iter().any(|p| p.contains("must be one of")),
                "{op}: {problems:?}"
            );
        }
    }

    /// GL-020/GL-012: non-empty arrays and typed nested payload elements.
    #[test]
    fn preflight_enforces_arrays_and_nested_payloads() {
        let base = json!({ "project": "g/a", "branch": "main", "commit_message": "msg" });
        let with = |actions: Value| {
            let mut v = base.clone();
            v["actions"] = actions;
            v
        };
        let (valid, problems, _) = validate("gitlab.repository.commit.create", with(json!([])));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("at least 1 item")),
            "{problems:?}"
        );
        let (valid, problems, _) = validate(
            "gitlab.repository.commit.create",
            with(json!([{ "action": "explode", "file_path": "a" }])),
        );
        assert!(!valid);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("action") && p.contains("must be one of")),
            "{problems:?}"
        );
        let (valid, _, _) = validate(
            "gitlab.repository.commit.create",
            with(json!([{ "action": "create", "file_path": "a", "content": "x" }])),
        );
        assert!(valid);
        let (valid, problems, _) = validate(
            "gitlab.snippet.create",
            json!({ "title": "t", "files": [] }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("at least 1 item")),
            "{problems:?}"
        );
    }

    /// GL-024/GL-029: non-positive ids are rejected; snippet.delete requires an id at all.
    #[test]
    fn preflight_enforces_positive_ids_and_snippet_target() {
        let (valid, problems, _) =
            validate("gitlab.mr.show", json!({ "project": "g/a", "iid": 0 }));
        assert!(!valid);
        assert!(problems.iter().any(|p| p.contains(">= 1")), "{problems:?}");
        let (valid, problems, _) = validate("gitlab.snippet.delete", json!({}));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("`snippet_id`")),
            "{problems:?}"
        );
        let (valid, _, _) = validate("gitlab.snippet.delete", json!({ "id": 12 }));
        assert!(valid, "the documented `id` alias satisfies the target");
    }

    /// GL-004: conditional targets — `ref` OR `project`+`iid` — are enforced locally.
    #[test]
    fn preflight_enforces_conditional_targets() {
        let (valid, problems, _) = validate("gitlab.mr.show", json!({}));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("`project`")),
            "{problems:?}"
        );
        let (valid, _, _) = validate("gitlab.mr.show", json!({ "ref": "group/app!5" }));
        assert!(valid);
        let (valid, _, _) = validate("gitlab.issue.show", json!({ "project": "g/a", "iid": 3 }));
        assert!(valid);
        let (valid, problems, _) = validate("gitlab.issue.show", json!({ "project": "g/a" }));
        assert!(!valid);
        assert!(problems.iter().any(|p| p.contains("`iid`")), "{problems:?}");
    }

    /// GL-027: an invalid `mr.diff.lines` search regex is reported at validate time.
    #[test]
    fn preflight_compiles_diff_lines_regex() {
        let (valid, problems, _) = validate(
            "gitlab.mr.diff.lines",
            json!({ "ref": "g/a!5", "file": "src/x.rs", "search": "[unclosed" }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.starts_with("search:")),
            "{problems:?}"
        );
        let (valid, _, _) = validate(
            "gitlab.mr.diff.lines",
            json!({ "ref": "g/a!5", "file": "src/x.rs", "search": "fn \\w+" }),
        );
        assert!(valid);
    }

    /// GL-036: MR line-anchor conditionals — `path` + `new_line`/`old_line` travel together.
    #[test]
    fn preflight_enforces_line_anchor_conditionals() {
        let (valid, problems, _) = validate(
            "gitlab.mr.discussion.create",
            json!({ "ref": "g/a!5", "body": "hm", "new_line": 3 }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("`path` is required")),
            "{problems:?}"
        );
        let (valid, problems, _) = validate(
            "gitlab.mr.discussion.create",
            json!({ "ref": "g/a!5", "body": "hm", "path": "src/x.rs" }),
        );
        assert!(!valid);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`new_line` or `old_line`")),
            "{problems:?}"
        );
        let (valid, _, _) = validate(
            "gitlab.mr.discussion.create",
            json!({ "ref": "g/a!5", "body": "hm", "path": "src/x.rs", "new_line": 3 }),
        );
        assert!(valid);
    }

    /// GL-021: issue.update and release.update gained the empty-update guard mr.update had.
    #[test]
    fn preflight_empty_update_guards_are_consistent() {
        for (op, target) in [
            ("gitlab.issue.update", json!({ "ref": "g/a#3" })),
            (
                "gitlab.release.update",
                json!({ "project": "g/a", "tag_name": "v1" }),
            ),
        ] {
            let (valid, problems, _) = validate(op, target);
            assert!(!valid, "{op} accepts an empty update");
            assert!(
                problems.iter().any(|p| p.contains("nothing to update")),
                "{op}: {problems:?}"
            );
        }
    }

    /// GL-028: the documented aliases satisfy their targets — and `name` is never a release tag.
    #[test]
    fn preflight_alias_requirements() {
        let (valid, _, _) = validate(
            "gitlab.repository.tag.show",
            json!({ "project": "g/a", "tag": "v1" }),
        );
        assert!(valid);
        let (valid, problems, _) =
            validate("gitlab.repository.tag.show", json!({ "project": "g/a" }));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("`tag_name`")),
            "{problems:?}"
        );
        let (valid, _, _) = validate(
            "gitlab.branch.create",
            json!({ "project": "g/a", "name": "feat/x", "ref": "main" }),
        );
        assert!(valid, "`name` is a documented branch alias");
        // release.update: `name` is the display name — it must NOT satisfy the tag target
        // (the old `name` fallback silently treated a display name as the tag).
        let (valid, problems, _) = validate(
            "gitlab.release.update",
            json!({ "project": "g/a", "name": "Renamed release" }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("`tag_name`")),
            "{problems:?}"
        );
        let runtime_err = manifest_builder()
            .build()
            .call(
                "gitlab.release.update",
                json!({ "project": "g/a", "name": "Renamed release" }),
                &mut MockHost::default(),
            )
            .unwrap_err();
        assert!(runtime_err.contains("`tag_name`"), "{runtime_err}");
    }

    /// GL-008: unknown fields are clearly warned (the schema is open — handlers may read
    /// undocumented aliases — so this is advisory, not a rejection).
    #[test]
    fn preflight_warns_on_unknown_fields() {
        let (valid, problems, warnings) = validate(
            "gitlab.issue.list",
            json!({ "project": "g/a", "stat": "opened" }),
        );
        assert!(valid, "{problems:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("`stat`") && w.contains("not in the op schema")),
            "{warnings:?}"
        );
    }

    // ---- D-89: honest read defaults ----

    /// GL-010 failing-first: a non-positive `limit`/`per_page`/`max_bytes` is REJECTED — it no
    /// longer silently expands to the default page size / no limit.
    #[test]
    fn preflight_rejects_non_positive_limits() {
        for (op, input) in [
            ("gitlab.mr.list", json!({ "project": "g/a", "limit": 0 })),
            (
                "gitlab.issue.list",
                json!({ "project": "g/a", "per_page": -1 }),
            ),
            (
                "gitlab.repository.file.show",
                json!({ "project": "g/a", "path": "x", "max_bytes": 0 }),
            ),
            ("gitlab.index.build", json!({ "mr_limit": -5 })),
        ] {
            let (valid, problems, _) = validate(op, input);
            assert!(!valid, "{op} accepted a non-positive limit");
            assert!(
                problems.iter().any(|p| p.contains(">= 1")),
                "{op}: {problems:?}"
            );
        }
        // Runtime dispatch agrees.
        let err = manifest_builder()
            .build()
            .call(
                "gitlab.mr.list",
                json!({ "project": "g/a", "limit": 0 }),
                &mut MockHost::default(),
            )
            .unwrap_err();
        assert!(err.contains(">= 1"), "{err}");
    }

    /// GL-009: `per_page` is honored as a documented alias of `limit`, not silently dropped.
    #[test]
    fn per_page_is_honored_as_limit_alias() {
        let mut host = base().with_http(
            "/repository/tags?per_page=7",
            json!([{ "name": "v1", "message": "" }]),
        );
        let out = run(
            "gitlab.repository.tag.list",
            json!({ "project": "g/a", "per_page": 7 }),
            &mut host,
        );
        assert_eq!(out[0]["name"], "v1", "per_page drove the query");
        // `limit` wins when both are set.
        let mut host = base().with_http(
            "/repository/tags?per_page=3",
            json!([{ "name": "v2", "message": "" }]),
        );
        let out = run(
            "gitlab.repository.tag.list",
            json!({ "project": "g/a", "limit": 3, "per_page": 9 }),
            &mut host,
        );
        assert_eq!(out[0]["name"], "v2");
    }

    /// GL-032/GL-041: blob-search scope is unambiguous, and `ref` is project-scope only.
    #[test]
    fn search_blobs_scope_is_unambiguous() {
        let (valid, problems, _) = validate(
            "gitlab.search.blobs",
            json!({ "query": "q", "project": "g/a", "group": "g" }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("not both")),
            "{problems:?}"
        );
        let (valid, problems, _) = validate(
            "gitlab.search.blobs",
            json!({ "query": "q", "group": "g", "ref": "main" }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("group-scoped")),
            "{problems:?}"
        );
        let (valid, _, _) = validate("gitlab.search.blobs", json!({ "query": "q", "group": "g" }));
        assert!(valid);
        let (valid, _, _) = validate(
            "gitlab.search.blobs",
            json!({ "query": "q", "project": "g/a", "ref": "main" }),
        );
        assert!(valid);
    }

    /// GL-033: `job.list scope` entries are validated — a non-string or unknown status is a
    /// problem, not a silently-skipped entry.
    #[test]
    fn job_list_scope_entries_are_validated() {
        let target = json!({ "project": "g/a", "pipeline_id": 5 });
        let with_scope = |scope: Value| {
            let mut v = target.clone();
            v["scope"] = scope;
            v
        };
        let (valid, problems, _) = validate("gitlab.job.list", with_scope(json!(["running", 5])));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("scope[1]")),
            "{problems:?}"
        );
        let (valid, problems, _) =
            validate("gitlab.job.list", with_scope(json!(["running", "bogus"])));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("must be one of")),
            "{problems:?}"
        );
        let (valid, _, _) = validate(
            "gitlab.job.list",
            with_scope(json!(["running", "waiting_for_resource"])),
        );
        assert!(valid);
    }

    /// GL-034: an index selector typo is a validation error, never an `indexed: 0` success.
    #[test]
    fn index_build_rejects_unknown_selectors() {
        let (valid, problems, _) =
            validate("gitlab.index.build", json!({ "indexes": ["porjects"] }));
        assert!(!valid);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("porjects") && p.contains("unknown index selector")),
            "{problems:?}"
        );
        // A mixed list with a typo is also rejected — partial silent under-indexing is the trap.
        let (valid, _, _) = validate(
            "gitlab.index.build",
            json!({ "indexes": ["projects", "porjects"] }),
        );
        assert!(!valid);
        // Runtime dispatch agrees (no HTTP happens).
        let err = manifest_builder()
            .build()
            .call(
                "gitlab.index.build",
                json!({ "indexes": ["porjects"] }),
                &mut MockHost::default(),
            )
            .unwrap_err();
        assert!(err.contains("unknown index selector"), "{err}");
        // Known selectors and the empty default remain valid.
        let (valid, _, _) = validate("gitlab.index.build", json!({ "indexes": ["mrs"] }));
        assert!(valid);
        let (valid, _, _) = validate("gitlab.index.build", json!({}));
        assert!(valid);
    }

    // ---- original surface ----

    #[test]
    fn mr_list_calls_the_api_and_contributes_records() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/merge_requests",
            json!([{ "iid": 7, "title": "Fix warm transfer", "description": "MR body" }]),
        );
        let out = run(
            "gitlab.mr.list",
            // GL-015: contribution is opt-in — a plain read stays pure (see
            // `reads_are_pure_unless_contribution_is_opted_in`).
            json!({ "project": "group/app", "state": "opened", "contribute": true }),
            &mut host,
        );
        assert_eq!(out[0]["iid"], 7);
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "gitlab.merge_request");
        assert_eq!(recs[0].id, "group/app!7");
        assert_eq!(recs[0].title, "Fix warm transfer");
    }

    #[test]
    fn project_show_encodes_the_path() {
        // The gitlab.com fallback now lives host-side (`EndpointSpec.default`, D-32); the mock
        // has no manifest knowledge, so pin the ref to that default and assert the composed URL.
        let mut host = MockHost::default()
            .with_endpoint_ref("gitlab.endpoint", "https://gitlab.com")
            .with_secret("personal_token", "tok")
            .with_http(
                "gitlab.com/api/v4/projects/group%2Fapp",
                json!({ "id": 1, "name": "app" }),
            );
        let out = run(
            "gitlab.project.show",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(out["name"], "app");
    }

    #[test]
    fn issue_list_contributes_issue_records() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/issues",
            json!([{ "iid": 3, "title": "Bug", "description": "details" }]),
        );
        let out = run(
            "gitlab.issue.list",
            json!({ "project": "group/app", "contribute": true }),
            &mut host,
        );
        assert_eq!(out[0]["iid"], 3);
        assert_eq!(host.contributed.borrow()[0].id, "group/app!3");
    }

    // ---- auth test + index ----

    #[test]
    fn auth_test_fetches_current_user() {
        let mut host = base().with_http("/api/v4/user", json!({ "username": "agent" }));
        let out = run("gitlab.test", json!({}), &mut host);
        assert_eq!(out["status"], "ok");
        assert_eq!(out["user"]["username"], "agent");
    }

    #[test]
    fn auth_test_returns_minimal_identity() {
        // GL-016: `gitlab.test` is an auth smoke check — it must return only a minimal identity
        // (id/username/name), never the sensitive full `GET /user` profile (email, public/commit
        // email, two-factor status, sign-in timestamps/IPs).
        let full_user = json!({
            "id": 7,
            "username": "agent",
            "name": "Agent Smith",
            "email": "agent@example.com",
            "commit_email": "commit@example.com",
            "public_email": "public@example.com",
            "two_factor_enabled": true,
            "last_sign_in_at": "2026-07-14T00:00:00Z",
            "current_sign_in_ip": "10.0.0.1",
        });
        let mut host = base().with_http("/api/v4/user", full_user);
        let out = run("gitlab.test", json!({}), &mut host);
        assert_eq!(out["status"], "ok");
        assert_eq!(out["user"]["id"], 7);
        assert_eq!(out["user"]["username"], "agent");
        assert_eq!(out["user"]["name"], "Agent Smith");
        // The user object is pinned to EXACTLY the three identity keys.
        let keys: Vec<&str> = out["user"]
            .as_object()
            .expect("user is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), 3, "user pinned to a minimal key set: {keys:?}");
        for leaked in [
            "email",
            "commit_email",
            "public_email",
            "two_factor_enabled",
            "last_sign_in_at",
            "current_sign_in_ip",
        ] {
            assert!(
                out["user"].get(leaked).is_none(),
                "sensitive `{leaked}` must not be echoed by an auth smoke check"
            );
        }
    }

    #[test]
    fn variable_ops_declare_value_as_secret() {
        // GL-031: the CI/pipeline variable ops carry the redaction metadata the host masks on, so a
        // dry-run/echo of a variable write never leaks the `value` to scrollback/logs/transcripts.
        let m = manifest_builder().build().manifest();
        for op_name in [
            "gitlab.ci.variable.create",
            "gitlab.ci.variable.update",
            "gitlab.pipeline.create",
        ] {
            let op = m
                .operations
                .iter()
                .find(|o| o.name == op_name)
                .unwrap_or_else(|| panic!("op {op_name} present"));
            assert_eq!(
                op.redact_fields,
                vec!["value".to_string()],
                "{op_name} must mark `value` secret"
            );
        }
        // `ci.variable.delete` carries no secret value field, so it declares none.
        let del = m
            .operations
            .iter()
            .find(|o| o.name == "gitlab.ci.variable.delete")
            .expect("delete op present");
        assert!(del.redact_fields.is_empty());
    }

    #[test]
    fn index_build_pages_all_three_datasources() {
        let mut host = base()
            .with_http(
                "/projects?membership",
                json!([{ "path_with_namespace": "group/app", "name_with_namespace": "Group / App" }]),
            )
            .with_http(
                "/merge_requests?scope=all",
                json!([{ "iid": 7, "title": "MR", "references": { "full": "group/app!7" } }]),
            )
            .with_http(
                "/issues?scope=all",
                json!([{ "iid": 3, "title": "Issue", "references": { "full": "group/app#3" } }]),
            );
        let out = run("gitlab.index.build", json!({}), &mut host);
        assert_eq!(out["indexed"], 3);
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 3);
        assert!(recs.iter().any(|r| r.id == "group/app!7"));
        assert!(recs.iter().any(|r| r.id == "group/app#3"));
    }

    // ---- project / mr writes ----

    #[test]
    fn project_create_resolves_namespace() {
        let mut host = base()
            .with_http(
                "/groups?search=testing",
                json!([{ "id": 42, "full_path": "testing", "path": "testing" }]),
            )
            .with_http("/api/v4/projects", json!({ "id": 9, "name": "dummy" }));
        let out = run(
            "gitlab.project.create",
            json!({ "name": "dummy", "namespace": "testing", "initialize_with_readme": true }),
            &mut host,
        );
        assert_eq!(out["id"], 9);
    }

    #[test]
    fn mr_create_posts_to_the_project() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/merge_requests",
            json!({ "iid": 12, "title": "Add feature" }),
        );
        let out = run(
            "gitlab.mr.create",
            json!({ "project": "group/app", "title": "Add feature", "source_branch": "feat", "target_branch": "main" }),
            &mut host,
        );
        assert_eq!(out["iid"], 12);
    }

    #[test]
    fn mr_update_via_ref() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/merge_requests/7",
            json!({ "iid": 7, "state": "closed" }),
        );
        let out = run(
            "gitlab.mr.update",
            json!({ "ref": "group/app!7", "state_event": "close" }),
            &mut host,
        );
        assert_eq!(out["state"], "closed");
    }

    #[test]
    fn mr_approve_and_merge() {
        let mut host = base()
            .with_http("/merge_requests/7/approve", json!({ "id": 1 }))
            .with_http(
                "/merge_requests/7/merge",
                json!({ "iid": 7, "state": "merged" }),
            );
        let approved = run(
            "gitlab.mr.approve",
            json!({ "ref": "group/app!7" }),
            &mut host,
        );
        assert_eq!(approved["id"], 1);
        let merged = run(
            "gitlab.mr.merge",
            json!({ "project": "group/app", "iid": 7, "auto_merge": true }),
            &mut host,
        );
        assert_eq!(merged["state"], "merged");
    }

    // ---- issues ----

    #[test]
    fn issue_show_create_update() {
        let mut host = base()
            .with_http(
                "/projects/group%2Fapp/issues/3",
                json!({ "iid": 3, "title": "Bug" }),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/issues",
                json!({ "iid": 4, "title": "New" }),
            );
        let shown = run(
            "gitlab.issue.show",
            json!({ "ref": "group/app#3" }),
            &mut host,
        );
        assert_eq!(shown["iid"], 3);
        let created = run(
            "gitlab.issue.create",
            json!({ "project": "group/app", "title": "New" }),
            &mut host,
        );
        assert_eq!(created["iid"], 4);
        let updated = run(
            "gitlab.issue.update",
            json!({ "ref": "group/app#3", "state_event": "close" }),
            &mut host,
        );
        assert_eq!(updated["iid"], 3);
    }

    #[test]
    fn issue_notes_list_and_create() {
        let mut host = base()
            .with_http(
                "/issues/3/notes?per_page",
                json!([{ "id": 1, "body": "hi" }]),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/issues/3/notes",
                json!({ "id": 2, "body": "reply" }),
            );
        let listed = run(
            "gitlab.issue.note.list",
            json!({ "ref": "group/app#3" }),
            &mut host,
        );
        assert_eq!(listed[0]["id"], 1);
        let created = run(
            "gitlab.issue.note.create",
            json!({ "ref": "group/app#3", "body": "reply" }),
            &mut host,
        );
        assert_eq!(created["id"], 2);
    }

    // ---- branches ----

    #[test]
    fn branch_lifecycle() {
        let mut host = base()
            .with_http("/repository/branches/feat%2Fx", json!({}))
            .with_http("/repository/merged_branches", json!({}))
            .with_http(
                "/api/v4/projects/group%2Fapp/repository/branches",
                json!({ "name": "feat/x" }),
            );
        let created = run(
            "gitlab.branch.create",
            json!({ "project": "group/app", "branch": "feat/x", "ref": "main" }),
            &mut host,
        );
        assert_eq!(created["name"], "feat/x");
        let deleted = run(
            "gitlab.branch.delete",
            json!({ "project": "group/app", "branch": "feat/x" }),
            &mut host,
        );
        assert_eq!(deleted["message"], "branch deleted");
        let merged = run(
            "gitlab.branch.delete_merged",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert!(merged["message"].as_str().unwrap().contains("merged"));
    }

    // ---- repo files + tree ----

    #[test]
    fn repo_file_create_update_delete_show() {
        let mut host = base()
            .with_http(
                "/repository/files/src%2Fmain.rs?ref",
                json!({ "file_path": "src/main.rs", "content": "Zm9v", "encoding": "base64" }),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/repository/files/src%2Fmain.rs",
                json!({ "file_path": "src/main.rs", "branch": "main" }),
            );
        let created = run(
            "gitlab.repository.file.create",
            json!({ "project": "group/app", "file_path": "src/main.rs", "branch": "main", "content": "foo", "commit_message": "add" }),
            &mut host,
        );
        assert_eq!(created["file_path"], "src/main.rs");
        let updated = run(
            "gitlab.repository.file.update",
            json!({ "project": "group/app", "file_path": "src/main.rs", "branch": "main", "content": "bar", "commit_message": "up" }),
            &mut host,
        );
        assert_eq!(updated["branch"], "main");
        let deleted = run(
            "gitlab.repository.file.delete",
            json!({ "project": "group/app", "file_path": "src/main.rs", "branch": "main", "commit_message": "rm" }),
            &mut host,
        );
        assert_eq!(deleted["message"], "repository file deleted");
        let shown = run(
            "gitlab.repository.file.show",
            json!({ "project": "group/app", "path": "src/main.rs", "ref": "main" }),
            &mut host,
        );
        assert_eq!(shown["encoding"], "base64");
    }

    #[test]
    fn repo_tree_lists_entries() {
        let mut host = base().with_http(
            "/repository/tree",
            json!([{ "path": "src", "name": "src", "type": "tree" }]),
        );
        let out = run(
            "gitlab.repository.tree",
            json!({ "project": "group/app", "recursive": true }),
            &mut host,
        );
        assert_eq!(out[0]["name"], "src");
    }

    // ---- commits ----

    #[test]
    fn commit_create_and_list() {
        let mut host = base()
            .with_http(
                "/repository/commits?per_page",
                json!([{ "id": "abc", "title": "c" }]),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/repository/commits",
                json!({ "id": "def", "title": "new" }),
            );
        let listed = run(
            "gitlab.repository.commit.list",
            json!({ "project": "group/app", "ref": "main" }),
            &mut host,
        );
        assert_eq!(listed[0]["id"], "abc");
        let created = run(
            "gitlab.repository.commit.create",
            json!({ "project": "group/app", "branch": "main", "commit_message": "new", "actions": [{ "action": "create", "file_path": "a", "content": "x" }] }),
            &mut host,
        );
        assert_eq!(created["id"], "def");
    }

    // ---- tags ----

    #[test]
    fn tag_lifecycle() {
        let mut host = base()
            .with_http("/repository/tags?per_page", json!([{ "name": "v1.0.0" }]))
            .with_http("/repository/tags/v1.0.0", json!({ "name": "v1.0.0" }))
            .with_http(
                "/api/v4/projects/group%2Fapp/repository/tags",
                json!({ "name": "v1.1.0" }),
            );
        let created = run(
            "gitlab.repository.tag.create",
            json!({ "project": "group/app", "tag_name": "v1.1.0", "ref": "main" }),
            &mut host,
        );
        assert_eq!(created["name"], "v1.1.0");
        let listed = run(
            "gitlab.repository.tag.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(listed[0]["name"], "v1.0.0");
        let shown = run(
            "gitlab.repository.tag.show",
            json!({ "project": "group/app", "tag_name": "v1.0.0" }),
            &mut host,
        );
        assert_eq!(shown["name"], "v1.0.0");
        let deleted = run(
            "gitlab.repository.tag.delete",
            json!({ "project": "group/app", "tag_name": "v1.0.0" }),
            &mut host,
        );
        assert_eq!(deleted["message"], "tag deleted");
    }

    // ---- snippets ----

    #[test]
    fn snippet_create_and_delete() {
        let mut host = base().with_http("/snippets", json!({ "id": 5, "title": "snip" }));
        let created = run(
            "gitlab.snippet.create",
            json!({ "title": "snip", "files": [{ "file_path": "a.txt", "content": "hi" }] }),
            &mut host,
        );
        assert_eq!(created["id"], 5);
        let deleted = run(
            "gitlab.snippet.delete",
            json!({ "snippet_id": 5 }),
            &mut host,
        );
        assert_eq!(deleted["message"], "snippet deleted");
    }

    // ---- search ----

    #[test]
    fn search_blobs_scopes_to_project() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/search?scope=blobs",
            json!([{ "path": "src/main.rs", "data": "fn main" }]),
        );
        let out = run(
            "gitlab.search.blobs",
            json!({ "query": "fn main", "project": "group/app", "ref": "main" }),
            &mut host,
        );
        assert_eq!(out[0]["path"], "src/main.rs");
    }

    // ---- parity tests (D-36) ----

    #[test]
    fn project_list_pagination_and_filters() {
        let mut host = base().with_http(
            "/projects?membership=false&search=foo&order_by=last_activity_at&sort=desc&per_page=5",
            json!([{ "path_with_namespace": "group/app", "name_with_namespace": "Group / App" }]),
        );
        let out = run(
            "gitlab.project.list",
            json!({ "query": "foo", "membership": false, "limit": 5 }),
            &mut host,
        );
        assert_eq!(out[0]["path_with_namespace"], "group/app");
    }

    #[test]
    fn mr_list_pagination_and_branch_filters() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/merge_requests?state=opened&order_by=updated_at&sort=desc&per_page=5&source_branch=feat&target_branch=main",
            json!([{ "iid": 1, "title": "MR" }]),
        );
        let out = run(
            "gitlab.mr.list",
            json!({
                "project": "group/app",
                "limit": 5,
                "source_branch": "feat",
                "target_branch": "main"
            }),
            &mut host,
        );
        assert_eq!(out[0]["iid"], 1);
    }

    #[test]
    fn issue_list_pagination_and_search() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/issues?state=all&search=bug&order_by=created_at&sort=asc&per_page=5",
            json!([{ "iid": 2, "title": "Bug" }]),
        );
        let out = run(
            "gitlab.issue.list",
            json!({
                "project": "group/app",
                "state": "all",
                "query": "bug",
                "order_by": "created_at",
                "sort": "asc",
                "limit": 5
            }),
            &mut host,
        );
        assert_eq!(out[0]["iid"], 2);
    }

    #[test]
    fn pipeline_list_passes_filters() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/pipelines?status=success&ref=main&source=push&username=agent&per_page=50",
            json!([{ "id": 9, "status": "success" }]),
        );
        let out = run(
            "gitlab.pipeline.list",
            json!({
                "project": "group/app",
                "status": "success",
                "ref": "main",
                "source": "push",
                "username": "agent",
                "limit": 50
            }),
            &mut host,
        );
        assert_eq!(out[0]["id"], 9);
    }

    #[test]
    fn index_build_selects_only_requested_entities() {
        let mut host = base().with_http(
            "/projects?membership=true&order_by=last_activity_at&sort=desc&per_page=100&page=1",
            json!([{ "path_with_namespace": "group/app", "name_with_namespace": "Group / App" }]),
        );
        let out = run(
            "gitlab.index.build",
            json!({ "entities": ["projects"] }),
            &mut host,
        );
        assert_eq!(out["indexed"], 1);
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "gitlab.project");
    }

    #[test]
    fn repo_file_show_respects_max_bytes() {
        // GL-013: `max_bytes` caps the DECODED bytes and re-encodes, so the returned `content`
        // is always valid base64 (the old byte-cap on the base64 string yielded an undecodable
        // fragment).
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;
        let content = engine.encode("x".repeat(1000));
        let mut host = base().with_http(
            "/repository/files/big.txt?ref=main",
            json!({ "file_path": "big.txt", "content": content, "encoding": "base64" }),
        );
        let out = run(
            "gitlab.repository.file.show",
            json!({
                "project": "group/app",
                "path": "big.txt",
                "ref": "main",
                "max_bytes": 100
            }),
            &mut host,
        );
        let got = out["content"].as_str().unwrap();
        let decoded = engine
            .decode(got)
            .expect("capped content stays valid base64");
        assert_eq!(decoded.len(), 100, "cap applies to decoded bytes");
        assert_eq!(decoded, "x".repeat(100).into_bytes());
        assert_eq!(out["truncated"], true);
    }

    #[test]
    fn repo_file_show_caps_plain_text_on_byte_boundary() {
        let mut host = base().with_http(
            "/repository/files/notes.txt?ref=main",
            json!({ "file_path": "notes.txt", "content": "b".repeat(500), "encoding": "text" }),
        );
        let out = run(
            "gitlab.repository.file.show",
            json!({ "project": "group/app", "path": "notes.txt", "ref": "main", "max_bytes": 64 }),
            &mut host,
        );
        assert_eq!(out["content"].as_str().unwrap().len(), 64);
        assert_eq!(out["truncated"], true);
    }

    #[test]
    fn search_blobs_truncates_match_data() {
        let data = "a".repeat(100);
        let mut host = base().with_http(
            "/projects/group%2Fapp/search?scope=blobs",
            json!([{ "path": "src/main.rs", "data": data }]),
        );
        let out = run(
            "gitlab.search.blobs",
            json!({
                "query": "fn main",
                "project": "group/app",
                "max_data_bytes": 40
            }),
            &mut host,
        );
        // GL-035: the cap includes the marker — the returned string never exceeds the max.
        let got = out[0]["data"].as_str().unwrap();
        assert!(got.len() <= 40, "data len {} > 40", got.len());
        got.strip_suffix("\n[snippet truncated]")
            .expect("truncated data should end with marker");
        assert_eq!(out[0]["data_truncated"], true);
    }

    #[test]
    fn mr_merge_accepts_remove_source_branch_alias() {
        let mut host = base().with_http(
            "/merge_requests/7/merge",
            json!({ "iid": 7, "state": "merged" }),
        );
        let out = run(
            "gitlab.mr.merge",
            json!({
                "project": "group/app",
                "iid": 7,
                "remove_source_branch": true
            }),
            &mut host,
        );
        assert_eq!(out["state"], "merged");
    }

    #[test]
    fn mr_merge_schema_has_remove_source_branch() {
        let m = manifest_builder().build().manifest();
        let spec = m
            .operations
            .iter()
            .find(|o| o.name == "gitlab.mr.merge")
            .unwrap();
        let props = spec
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(props.contains_key("remove_source_branch"));
    }

    // ---- review ----

    #[test]
    fn mr_changes_returns_files_and_diff_refs() {
        let mut host = base()
            .with_http(
                "/merge_requests/7/diffs",
                json!([{ "new_path": "a.rs", "old_path": "a.rs", "diff": "@@ -1 +1 @@\n-x\n+y\n" }]),
            )
            .with_http(
                "/merge_requests/7",
                json!({ "iid": 7, "diff_refs": { "base_sha": "b", "start_sha": "s", "head_sha": "h" } }),
            );
        let out = run(
            "gitlab.mr.changes",
            json!({ "ref": "group/app!7" }),
            &mut host,
        );
        assert_eq!(out["count"], 1);
        assert_eq!(out["diff_refs"]["head_sha"], "h");
        assert_eq!(out["files"][0]["new_path"], "a.rs");
    }

    #[test]
    fn mr_diff_lines_parses_the_diff() {
        let mut host = base().with_http(
            "/merge_requests/7/diffs",
            json!([{ "new_path": "a.rs", "old_path": "a.rs", "diff": "@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n" }]),
        );
        let out = run(
            "gitlab.mr.diff.lines",
            json!({ "ref": "group/app!7", "file": "a.rs" }),
            &mut host,
        );
        let lines = out["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "context");
        assert_eq!(lines[1]["type"], "deleted");
        assert_eq!(lines[2]["type"], "added");
        assert_eq!(lines[2]["new_line"], 2);
    }

    #[test]
    fn mr_diff_lines_search_is_regex_not_substring() {
        let mut host = base().with_http(
            "/merge_requests/7/diffs",
            json!([{
                "new_path": "a.rs", "old_path": "a.rs",
                "diff": "@@ -1,3 +1,3 @@\n let foo = 1;\n-let bar = 2;\n+let baz = 3;\n"
            }]),
        );
        // Anchored regex matches only the line starting with "let baz".
        let out = run(
            "gitlab.mr.diff.lines",
            json!({ "ref": "group/app!7", "file": "a.rs", "search": "^let ba[xz]" }),
            &mut host,
        );
        let lines = out["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["content"], "let baz = 3;");
        assert_eq!(lines[0]["type"], "added");
        assert_eq!(out["count"], 1);
    }

    #[test]
    fn mr_diff_lines_search_rejects_a_bad_regex() {
        let mut host = base().with_http(
            "/merge_requests/7/diffs",
            json!([{ "new_path": "a.rs", "old_path": "a.rs", "diff": "@@ -1 +1 @@\n+x\n" }]),
        );
        let err = manifest_builder()
            .build()
            .call(
                "gitlab.mr.diff.lines",
                json!({ "ref": "group/app!7", "file": "a.rs", "search": "(" }),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("search:"), "unexpected error: {err}");
    }

    // ---- D-90: pagination & truncation truth ----

    /// GL-042/GL-043: a `file` filter is applied BEFORE the file cap, paginating past the first
    /// diff page — asking for a specific file can never return empty because of a hidden page
    /// limit.
    #[test]
    fn mr_changes_finds_a_filtered_file_beyond_the_first_page() {
        let page1: Vec<Value> = (0..100)
            .map(|i| json!({ "new_path": format!("f{i}.rs"), "old_path": format!("f{i}.rs"), "diff": "@@\n" }))
            .collect();
        let host = base()
            .with_http_seq("/diffs?per_page=100&page=1", json!(page1))
            .with_http_seq(
                "/diffs?per_page=100&page=2",
                json!([{ "new_path": "deep.rs", "old_path": "deep.rs", "diff": "@@\n+x\n" }]),
            );
        let mut host = host.with_http(
            "/merge_requests/7",
            json!({ "diff_refs": { "head_sha": "h" } }),
        );
        let out = run(
            "gitlab.mr.changes",
            json!({ "ref": "group/app!7", "file": "deep.rs" }),
            &mut host,
        );
        assert_eq!(out["count"], 1, "{out}");
        assert_eq!(out["files"][0]["new_path"], "deep.rs");
        assert_eq!(out["files_truncated"], false);
    }

    /// GL-044: the file-count cut has its own top-level flag, distinct from per-file
    /// `diff_truncated`.
    #[test]
    fn mr_changes_reports_files_truncated() {
        let page: Vec<Value> = (0..60)
            .map(|i| json!({ "new_path": format!("f{i}.rs"), "old_path": format!("f{i}.rs"), "diff": "@@\n" }))
            .collect();
        let mut host = base()
            .with_http("/diffs?per_page=100&page=1", json!(page))
            .with_http("/merge_requests/7", json!({ "diff_refs": Value::Null }));
        let out = run(
            "gitlab.mr.changes",
            json!({ "ref": "group/app!7", "max_files": 50 }),
            &mut host,
        );
        assert_eq!(out["count"], 50);
        assert_eq!(out["files_truncated"], true);
    }

    /// GL-045/GL-014: commits are capped with an honest marker, and the top-level `truncated`
    /// is true when ANY part of the result was cut.
    #[test]
    fn compare_caps_commits_and_reports_truncation_truth() {
        let commits: Vec<Value> = (0..60).map(|i| json!({ "id": format!("c{i}") })).collect();
        let mut host = base().with_http(
            "/repository/compare",
            json!({ "web_url": "u", "commits": commits, "diffs": [] }),
        );
        let out = run(
            "gitlab.compare",
            json!({ "project": "group/app", "from": "main", "to": "feat", "max_commits": 10 }),
            &mut host,
        );
        assert_eq!(out["commits"].as_array().unwrap().len(), 10);
        assert_eq!(out["commit_count"], 60, "full total survives the cap");
        assert_eq!(out["commits_truncated"], true);
        assert_eq!(out["truncated"], true);

        // A capped per-file diff also flips the aggregate flag (GL-014) — and the returned diff
        // never exceeds the requested byte cap, marker included (GL-035). The byte cap floor is
        // 16384, so build a diff bigger than that.
        let big_diff = format!("@@\n+{}\n", "y".repeat(20000));
        let mut host = base().with_http(
            "/repository/compare",
            json!({ "web_url": "u", "commits": [], "diffs": [{ "new_path": "a.rs", "diff": big_diff }] }),
        );
        let out = run(
            "gitlab.compare",
            json!({ "project": "group/app", "from": "main", "to": "feat", "max_diff_bytes": 16384 }),
            &mut host,
        );
        assert_eq!(out["files"][0]["diff_truncated"], true);
        assert!(
            out["files"][0]["diff"].as_str().unwrap().len() <= 16384,
            "cap includes the marker"
        );
        assert_eq!(out["files_truncated"], false);
        assert_eq!(out["truncated"], true, "aggregate reflects the diff cut");
    }

    /// GL-047: a deleted line is addressable via `old_line` (it has no new-file number).
    #[test]
    fn mr_diff_lines_anchors_on_old_line() {
        let mut host = base().with_http(
            "/merge_requests/7/diffs",
            json!([{ "new_path": "a.rs", "old_path": "a.rs", "diff": "@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n" }]),
        );
        let out = run(
            "gitlab.mr.diff.lines",
            json!({ "ref": "group/app!7", "file": "a.rs", "old_line": 2, "context": 0 }),
            &mut host,
        );
        let lines = out["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1, "{out}");
        assert_eq!(lines[0]["type"], "deleted");
        assert_eq!(lines[0]["target"], true);
        // A missing old line reports an old-file hint, not a silent empty set.
        let mut host = base().with_http(
            "/merge_requests/7/diffs",
            json!([{ "new_path": "a.rs", "old_path": "a.rs", "diff": "@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n" }]),
        );
        let out = run(
            "gitlab.mr.diff.lines",
            json!({ "ref": "group/app!7", "file": "a.rs", "old_line": 99 }),
            &mut host,
        );
        assert!(out["hint"].as_str().unwrap().contains("old-file line 99"));
    }

    /// GL-043: diff-line file resolution paginates past the first page of changed files.
    #[test]
    fn mr_diff_lines_resolves_a_file_beyond_the_first_page() {
        let page1: Vec<Value> = (0..100)
            .map(|i| json!({ "new_path": format!("f{i}.rs"), "old_path": format!("f{i}.rs"), "diff": "@@\n" }))
            .collect();
        let mut host = base()
            .with_http_seq("/diffs?per_page=100&page=1", json!(page1))
            .with_http_seq(
                "/diffs?per_page=100&page=2",
                json!([{ "new_path": "deep.rs", "old_path": "deep.rs", "diff": "@@ -1,1 +1,1 @@\n ctx\n" }]),
            );
        let out = run(
            "gitlab.mr.diff.lines",
            json!({ "ref": "group/app!7", "file": "deep.rs" }),
            &mut host,
        );
        assert_eq!(out["count"], 1, "{out}");
    }

    /// GL-023: an oversized archive is refused instead of staged.
    #[test]
    fn repository_archive_refuses_an_oversized_download() {
        let mut host = base().with_http_bytes("/repository/archive.tar.gz", vec![0u8; 4096]);
        let err = manifest_builder()
            .build()
            .call(
                "gitlab.repository.archive",
                json!({ "project": "group/app", "max_bytes": 1024 }),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("exceeding max_bytes 1024"), "{err}");
    }

    /// GL-019: `page` walks beyond a capped first page, and an over-cap `limit` is rejected
    /// instead of silently clamped.
    #[test]
    fn list_paging_is_explicit_and_over_cap_limits_reject() {
        let mut host = base().with_http(
            "/repository/tags?per_page=20&page=3",
            json!([{ "name": "v3", "message": "" }]),
        );
        let out = run(
            "gitlab.repository.tag.list",
            json!({ "project": "group/app", "page": 3 }),
            &mut host,
        );
        assert_eq!(out[0]["name"], "v3", "page reached the query");

        let (valid, problems, _) =
            validate("gitlab.mr.list", json!({ "project": "g/a", "limit": 500 }));
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("<= 100")),
            "{problems:?}"
        );
    }

    #[test]
    fn compare_returns_commits_and_files() {
        let mut host = base().with_http(
            "/repository/compare",
            json!({ "web_url": "u", "commits": [{ "id": "c1" }], "diffs": [{ "new_path": "a.rs", "diff": "@@\n" }] }),
        );
        let out = run(
            "gitlab.compare",
            json!({ "project": "group/app", "from": "main", "to": "feat" }),
            &mut host,
        );
        assert_eq!(out["commit_count"], 1);
        assert_eq!(out["file_count"], 1);
    }

    #[test]
    fn mr_discussion_list_note_reply_resolve() {
        let mut host = base()
            .with_http(
                "/merge_requests/7/discussions/abc/notes",
                json!({ "id": 2, "body": "reply" }),
            )
            .with_http(
                "/merge_requests/7/discussions/abc",
                json!({ "id": "abc", "resolved": true }),
            )
            .with_http(
                "/merge_requests/7/discussions?per_page",
                json!([{ "id": "abc" }]),
            )
            .with_http(
                "/merge_requests/7/notes",
                json!({ "id": 1, "body": "note" }),
            );
        let listed = run(
            "gitlab.mr.discussion.list",
            json!({ "ref": "group/app!7" }),
            &mut host,
        );
        assert_eq!(listed[0]["id"], "abc");
        let note = run(
            "gitlab.mr.note.create",
            json!({ "ref": "group/app!7", "body": "note" }),
            &mut host,
        );
        assert_eq!(note["id"], 1);
        let reply = run(
            "gitlab.mr.discussion.reply",
            json!({ "ref": "group/app!7", "discussion_id": "abc", "body": "reply" }),
            &mut host,
        );
        assert_eq!(reply["id"], 2);
        let resolved = run(
            "gitlab.mr.discussion.resolve",
            json!({ "ref": "group/app!7", "discussion_id": "abc" }),
            &mut host,
        );
        assert_eq!(resolved["resolved"], true);
    }

    #[test]
    fn mr_discussion_create_dry_run_builds_position() {
        let mut host = base()
            .with_http(
                "/merge_requests/7/diffs",
                json!([{ "new_path": "a.rs", "old_path": "a.rs", "diff": "@@ -1,2 +1,2 @@\n ctx\n+added\n" }]),
            )
            .with_http(
                "/merge_requests/7",
                json!({ "iid": 7, "diff_refs": { "base_sha": "b", "start_sha": "s", "head_sha": "h" } }),
            );
        let out = run(
            "gitlab.mr.discussion.create",
            json!({ "ref": "group/app!7", "body": "comment", "path": "a.rs", "new_line": 2, "dry_run": true }),
            &mut host,
        );
        assert_eq!(out["posted"], false);
        assert_eq!(out["position"]["new_line"], 2);
        assert_eq!(out["position"]["head_sha"], "h");
        assert_eq!(out["position"]["position_type"], "text");
    }

    // ---- CI/CD ----

    #[test]
    fn ci_variable_create_update_delete() {
        let mut host = base()
            .with_http(
                "/variables/KEY?filter",
                json!({ "key": "KEY", "value": "v2" }),
            )
            .with_http("/variables/KEY", json!({}))
            .with_http(
                "/api/v4/projects/group%2Fapp/variables",
                json!({ "key": "KEY", "value": "v1" }),
            );
        let created = run(
            "gitlab.ci.variable.create",
            json!({ "project": "group/app", "key": "KEY", "value": "v1" }),
            &mut host,
        );
        assert_eq!(created["value"], "v1");
        let updated = run(
            "gitlab.ci.variable.update",
            json!({ "project": "group/app", "key": "KEY", "value": "v2", "environment_scope": "prod" }),
            &mut host,
        );
        assert_eq!(updated["value"], "v2");
        let deleted = run(
            "gitlab.ci.variable.delete",
            json!({ "project": "group/app", "key": "KEY" }),
            &mut host,
        );
        assert_eq!(deleted["message"], "ci variable deleted");
    }

    #[test]
    fn pipeline_create_retry_cancel() {
        let mut host = base()
            .with_http(
                "/pipelines/5/retry",
                json!({ "id": 5, "status": "running" }),
            )
            .with_http(
                "/pipelines/5/cancel",
                json!({ "id": 5, "status": "canceled" }),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/pipeline",
                json!({ "id": 5, "status": "pending" }),
            );
        let created = run(
            "gitlab.pipeline.create",
            json!({ "project": "group/app", "ref": "main" }),
            &mut host,
        );
        assert_eq!(created["id"], 5);
        let retried = run(
            "gitlab.pipeline.retry",
            json!({ "project": "group/app", "pipeline_id": 5 }),
            &mut host,
        );
        assert_eq!(retried["status"], "running");
        let canceled = run(
            "gitlab.pipeline.cancel",
            json!({ "project": "group/app", "pipeline_id": 5 }),
            &mut host,
        );
        assert_eq!(canceled["status"], "canceled");
    }

    #[test]
    fn pipeline_create_validates_variables() {
        let mut host = base().with_http(
            "/api/v4/projects/group%2Fapp/pipeline",
            json!({ "id": 6, "status": "pending" }),
        );
        // A well-formed variable is accepted.
        let ok = run(
            "gitlab.pipeline.create",
            json!({ "project": "group/app", "ref": "main", "variables": [{ "key": "K", "value": "v", "variable_type": "file" }] }),
            &mut host,
        );
        assert_eq!(ok["id"], 6);
        // A missing key is rejected before any HTTP call (by the shared preflight since D-88).
        let bad_key = manifest_builder()
            .build()
            .call(
                "gitlab.pipeline.create",
                json!({ "project": "group/app", "ref": "main", "variables": [{ "value": "v" }] }),
                &mut host,
            )
            .unwrap_err();
        assert!(
            bad_key.contains("missing required field `key`"),
            "got: {bad_key}"
        );
        // An invalid variable_type is rejected.
        let bad_type = manifest_builder()
            .build()
            .call(
                "gitlab.pipeline.create",
                json!({ "project": "group/app", "ref": "main", "variables": [{ "key": "K", "variable_type": "nope" }] }),
                &mut host,
            )
            .unwrap_err();
        assert!(
            bad_type.contains("variable_type") && bad_type.contains("must be one of"),
            "got: {bad_type}"
        );
    }

    #[test]
    fn job_environment_deployment_lists() {
        let mut host = base()
            .with_http(
                "/pipelines/5/jobs",
                json!([{ "id": 1, "name": "build", "status": "failed" }]),
            )
            .with_http("/environments", json!([{ "id": 2, "name": "production" }]))
            .with_http("/deployments", json!([{ "id": 3, "status": "success" }]));
        let jobs = run(
            "gitlab.job.list",
            json!({ "project": "group/app", "pipeline_id": 5, "scope": ["failed"] }),
            &mut host,
        );
        assert_eq!(jobs[0]["name"], "build");
        let envs = run(
            "gitlab.environment.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(envs[0]["name"], "production");
        let deps = run(
            "gitlab.deployment.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(deps[0]["status"], "success");
    }

    // ---- releases ----

    #[test]
    fn release_lifecycle() {
        let mut host = base()
            .with_http(
                "/releases/v1.0.0",
                json!({ "tag_name": "v1.0.0", "name": "1.0" }),
            )
            .with_http("/releases?per_page", json!([{ "tag_name": "v1.0.0" }]))
            .with_http(
                "/api/v4/projects/group%2Fapp/releases",
                json!({ "tag_name": "v1.0.0", "name": "1.0" }),
            );
        let created = run(
            "gitlab.release.create",
            json!({ "project": "group/app", "tag_name": "v1.0.0", "ref": "main", "name": "1.0" }),
            &mut host,
        );
        assert_eq!(created["tag_name"], "v1.0.0");
        let listed = run(
            "gitlab.release.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(listed[0]["tag_name"], "v1.0.0");
        let shown = run(
            "gitlab.release.show",
            json!({ "project": "group/app", "tag_name": "v1.0.0" }),
            &mut host,
        );
        assert_eq!(shown["name"], "1.0");
        let updated = run(
            "gitlab.release.update",
            json!({ "project": "group/app", "tag_name": "v1.0.0", "description": "notes" }),
            &mut host,
        );
        assert_eq!(updated["tag_name"], "v1.0.0");
        let deleted = run(
            "gitlab.release.delete",
            json!({ "project": "group/app", "tag_name": "v1.0.0" }),
            &mut host,
        );
        assert_eq!(deleted["message"], "release deleted");
    }

    #[test]
    fn release_link_lifecycle() {
        let mut host = base()
            .with_http(
                "/assets/links/7",
                json!({ "id": 7, "name": "Binary (signed)" }),
            )
            .with_http(
                "/assets/links?per_page",
                json!([{ "id": 7, "name": "Binary" }]),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/releases/v1.0.0/assets/links",
                json!({ "id": 7, "name": "Binary" }),
            );
        let created = run(
            "gitlab.release.link.create",
            json!({ "project": "group/app", "tag_name": "v1.0.0", "name": "Binary", "url": "https://x/y.zip" }),
            &mut host,
        );
        assert_eq!(created["id"], 7);
        let listed = run(
            "gitlab.release.link.list",
            json!({ "project": "group/app", "tag_name": "v1.0.0" }),
            &mut host,
        );
        assert_eq!(listed[0]["name"], "Binary");
        let updated = run(
            "gitlab.release.link.update",
            json!({ "project": "group/app", "tag_name": "v1.0.0", "link_id": 7, "name": "Binary (signed)" }),
            &mut host,
        );
        assert_eq!(updated["name"], "Binary (signed)");
        let deleted = run(
            "gitlab.release.link.delete",
            json!({ "project": "group/app", "tag_name": "v1.0.0", "link_id": 7 }),
            &mut host,
        );
        assert_eq!(deleted["message"], "release link deleted");
    }

    // ---- changelog ----

    #[test]
    fn changelog_generate_and_add() {
        let mut host = base()
            .with_http(
                "/repository/changelog?version",
                json!({ "notes": "## 1.2.0" }),
            )
            .with_http(
                "/api/v4/projects/group%2Fapp/repository/changelog",
                json!({}),
            );
        let generated = run(
            "gitlab.repository.changelog.generate",
            json!({ "project": "group/app", "version": "1.2.0" }),
            &mut host,
        );
        assert_eq!(generated["notes"], "## 1.2.0");
        let added = run(
            "gitlab.repository.changelog.add",
            json!({ "project": "group/app", "version": "1.2.0", "branch": "main" }),
            &mut host,
        );
        assert_eq!(added["message"], "changelog committed");
        assert_eq!(added["file"], "CHANGELOG.md");
    }

    // ---- archive (blob) ----

    #[test]
    fn repository_archive_stages_a_blob() {
        let mut host =
            base().with_http_bytes("/repository/archive.tar.gz", b"ARCHIVE-BYTES".to_vec());
        let out = run(
            "gitlab.repository.archive",
            json!({ "project": "group/app", "ref": "main" }),
            &mut host,
        );
        assert_eq!(out["blob_ref"], "mockblob-1");
        assert_eq!(out["filename"], "group-app-main.tar.gz");
        assert!(out["bytes"].as_u64().unwrap() > 0);
        assert!(host.blobs.borrow().contains_key("mockblob-1"));
    }

    // ---- manifest ----

    #[test]
    fn manifest_declares_ops_auth_and_datasources() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.iter().filter(|o| !o.internal).count(), 80);
        assert_eq!(m.auth[0].purpose, "personal_token");
        assert_eq!(m.endpoints[0].name, "gitlab.endpoint");
        assert_eq!(
            m.endpoints[0].default.as_deref(),
            Some("https://gitlab.com")
        );
        assert!(m.capabilities.blob);
        assert!(m
            .datasources
            .iter()
            .all(|d| d.capabilities.iter().any(|c| c == "index")));
        assert!(m
            .datasources
            .iter()
            .any(|d| d.entity == "gitlab.merge_request"));
    }

    // ---- CI governance: job-token scope, protected tags, deploy tokens ----

    #[test]
    fn job_token_scope_show_and_set() {
        let mut host = base().with_http(
            "/api/v4/projects/group%2Fapp/job_token_scope",
            json!({ "inbound_enabled": true, "outbound_enabled": false }),
        );
        let shown = run(
            "gitlab.ci.job_token.scope.show",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(shown["inbound_enabled"], true);
        // PATCH replies 204 → the op synthesizes a confirmation.
        let set = run(
            "gitlab.ci.job_token.scope.set",
            json!({ "project": "group/app", "enabled": true }),
            &mut host,
        );
        assert_eq!(set["enabled"], true);
        assert_eq!(set["message"], "job token scope updated");
    }

    #[test]
    fn job_token_allowlist_lifecycle() {
        // add/remove resolve `project` to its numeric id first (GitLab's allowlist POST/DELETE
        // reject the URL-encoded `namespace%2Fproject` path form — see `resolve_project_id`);
        // list is unaffected and still hits the encoded path directly.
        let mut host = base()
            .with_http("/api/v4/projects/group%2Fapp", json!({ "id": 42 }))
            .with_http("/job_token_scope/allowlist/123", json!({ "removed": true }))
            .with_http_seq(
                "/job_token_scope/allowlist",
                json!({ "target_project_id": 123, "target_project_path": "grp/b" }),
            )
            .with_http_seq(
                "/api/v4/projects/group%2Fapp/job_token_scope/allowlist",
                json!([{ "target_project_id": 123 }]),
            );
        let added = run(
            "gitlab.ci.job_token.allowlist.add",
            json!({ "project": "group/app", "target_project_id": 123 }),
            &mut host,
        );
        assert_eq!(added["target_project_id"], 123);
        let listed = run(
            "gitlab.ci.job_token.allowlist.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(listed[0]["target_project_id"], 123);
        let removed = run(
            "gitlab.ci.job_token.allowlist.remove",
            json!({ "project": "group/app", "target_project_id": 123, "confirm_target_project_id": 123 }),
            &mut host,
        );
        assert_eq!(removed["message"], "removed from job token allowlist");
    }

    #[test]
    fn job_token_allowlist_add_uses_numeric_project_id_not_encoded_path() {
        // Regression for the reported bug: GitLab 400s `{"error":"id is invalid"}` on this write
        // endpoint when given the URL-encoded path form, so the POST must target the numeric id
        // resolved via `/projects/:id`, not `/projects/group%2Fapp/...`.
        let mut host = base()
            .with_http("/api/v4/projects/group%2Fapp", json!({ "id": 42 }))
            .with_http(
                "/api/v4/projects/42/job_token_scope/allowlist",
                json!({ "target_project_id": 123 }),
            );
        let added = run(
            "gitlab.ci.job_token.allowlist.add",
            json!({ "project": "group/app", "target_project_id": 123 }),
            &mut host,
        );
        assert_eq!(added["target_project_id"], 123);
    }

    #[test]
    fn job_token_groups_allowlist_lifecycle() {
        // add/remove resolve `project` to its numeric id first (see `resolve_project_id`); list is
        // unaffected. The write mocks below are project-agnostic (bare suffix), so they match
        // regardless of whether the numeric id or the encoded path is used in the URL.
        let mut host = base()
            .with_http("/api/v4/projects/group%2Fapp", json!({ "id": 42 }))
            .with_http(
                "/job_token_scope/groups_allowlist/456",
                json!({ "removed": true }),
            )
            .with_http_seq(
                "/job_token_scope/groups_allowlist",
                json!({ "target_group_id": 456 }),
            )
            .with_http_seq(
                "/job_token_scope/groups_allowlist",
                json!([{ "target_group_id": 456 }]),
            );
        let added = run(
            "gitlab.ci.job_token.groups_allowlist.add",
            json!({ "project": "group/app", "target_group_id": 456 }),
            &mut host,
        );
        assert_eq!(added["target_group_id"], 456);
        let listed = run(
            "gitlab.ci.job_token.groups_allowlist.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(listed[0]["target_group_id"], 456);
        let removed = run(
            "gitlab.ci.job_token.groups_allowlist.remove",
            json!({ "project": "group/app", "target_group_id": 456, "confirm_target_group_id": 456 }),
            &mut host,
        );
        assert_eq!(
            removed["message"],
            "removed from job token groups allowlist"
        );
    }

    #[test]
    fn protected_tag_lifecycle() {
        // `v*` percent-encodes to `v%2A`; show/unprotect target the encoded suffixed path.
        let mut host = base()
            .with_http("/protected_tags/v%2A", json!({ "name": "v*" }))
            .with_http_seq(
                "/protected_tags",
                json!({ "name": "v*", "create_access_level": 40 }),
            )
            .with_http_seq("/protected_tags", json!([{ "name": "v*" }]));
        let protected = run(
            "gitlab.repository.protected_tag.protect",
            json!({ "project": "group/app", "name": "v*" }),
            &mut host,
        );
        assert_eq!(protected["name"], "v*");
        let listed = run(
            "gitlab.repository.protected_tag.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(listed[0]["name"], "v*");
        let shown = run(
            "gitlab.repository.protected_tag.show",
            json!({ "project": "group/app", "name": "v*" }),
            &mut host,
        );
        assert_eq!(shown["name"], "v*");
        let unprotected = run(
            "gitlab.repository.protected_tag.unprotect",
            json!({ "project": "group/app", "name": "v*", "confirm_name": "v*" }),
            &mut host,
        );
        assert_eq!(unprotected["message"], "tag unprotected");
    }

    #[test]
    fn deploy_token_lifecycle_surfaces_token_once() {
        let mut host = base()
            .with_http("/deploy_tokens/7", json!({ "revoked": true }))
            .with_http_seq(
                "/deploy_tokens",
                json!({ "id": 7, "name": "ci", "token": "gLdeadbeef", "scopes": ["read_repository"] }),
            )
            .with_http_seq("/deploy_tokens", json!([{ "id": 7, "name": "ci" }]));
        let created = run(
            "gitlab.deploy_token.create",
            json!({ "project": "group/app", "name": "ci", "scopes": ["read_repository"] }),
            &mut host,
        );
        // The one-time secret is surfaced to the operator (this is the deliverable).
        assert_eq!(created["token"], "gLdeadbeef");
        let listed = run(
            "gitlab.deploy_token.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert_eq!(listed[0]["id"], 7);
        let revoked = run(
            "gitlab.deploy_token.revoke",
            json!({ "project": "group/app", "token_id": 7, "confirm_token_id": 7 }),
            &mut host,
        );
        assert_eq!(revoked["message"], "deploy token revoked");
    }

    #[test]
    fn destructive_confirm_guard_blocks_mismatch() {
        // Canned responses are present, so the ONLY reason these fail is the confirm guard firing
        // before the HTTP call — a matching confirm proceeds.
        let mut host = base()
            .with_http("/api/v4/projects/group%2Fapp", json!({ "id": 42 }))
            .with_http("/job_token_scope/allowlist/123", json!({ "ok": true }))
            .with_http("/deploy_tokens/7", json!({ "ok": true }))
            .with_http("/protected_tags/v%2A", json!({ "ok": true }));
        let built = manifest_builder().build();

        let bad = built.call(
            "gitlab.ci.job_token.allowlist.remove",
            json!({ "project": "group/app", "target_project_id": 123, "confirm_target_project_id": 999 }),
            &mut host,
        );
        assert!(bad.is_err());
        assert!(bad.unwrap_err().contains("confirm_target_project_id"));

        let bad_tok = built.call(
            "gitlab.deploy_token.revoke",
            json!({ "project": "group/app", "token_id": 7, "confirm_token_id": 8 }),
            &mut host,
        );
        assert!(bad_tok.is_err());

        let bad_tag = built.call(
            "gitlab.repository.protected_tag.unprotect",
            json!({ "project": "group/app", "name": "v*", "confirm_name": "nope" }),
            &mut host,
        );
        assert!(bad_tag.is_err());

        // A matching confirm proceeds (guard does not block).
        let ok = built.call(
            "gitlab.ci.job_token.allowlist.remove",
            json!({ "project": "group/app", "target_project_id": 123, "confirm_target_project_id": 123 }),
            &mut host,
        );
        assert!(ok.is_ok());
    }

    // ==== D-91: destructive-op risk metadata, confirm fields & project.delete ====

    /// GL-005: the destructive/bulk ops are no longer flat `Medium` — deletes are `Destructive`
    /// (the bulk merged-branch sweep too), a single branch delete is `High`, and each delete's
    /// `confirm_*` guard blocks a mismatched confirmation before any HTTP.
    #[test]
    fn delete_ops_carry_finer_risk_and_confirm_guards() {
        let m = manifest_builder().build().manifest();
        let risk = |name: &str| {
            m.operations
                .iter()
                .find(|o| o.name == name)
                .unwrap_or_else(|| panic!("op {name}"))
                .risk
        };
        assert_eq!(risk("gitlab.branch.delete"), Some(Risk::High));
        assert_eq!(risk("gitlab.branch.delete_merged"), Some(Risk::Destructive));
        assert_eq!(
            risk("gitlab.repository.tag.delete"),
            Some(Risk::Destructive)
        );
        assert_eq!(risk("gitlab.release.delete"), Some(Risk::Destructive));
        assert_eq!(risk("gitlab.ci.variable.delete"), Some(Risk::Destructive));
        assert_eq!(
            risk("gitlab.repository.file.delete"),
            Some(Risk::Destructive)
        );
        assert_eq!(risk("gitlab.snippet.delete"), Some(Risk::Destructive));
        // Ordinary reversible writes stay Medium — the differentiation the finding asked for.
        assert_eq!(risk("gitlab.mr.update"), Some(Risk::Medium));

        // A mismatched confirm is refused before the mutating call (canned responses present, so
        // the ONLY failure reason is the guard). Each guard names its own field.
        let built = manifest_builder().build();
        let mut host = base()
            .with_http("/repository/branches/feat", json!({ "ok": true }))
            .with_http("/repository/tags/v1.0.0", json!({ "ok": true }))
            .with_http("/releases/v1.0.0", json!({ "ok": true }))
            .with_http("/variables/KEY", json!({ "ok": true }))
            .with_http("/snippets/5", json!({ "ok": true }));
        let mismatches = [
            (
                "gitlab.branch.delete",
                json!({ "project": "group/app", "branch": "feat", "confirm_branch": "nope" }),
                "confirm_branch",
            ),
            (
                "gitlab.repository.tag.delete",
                json!({ "project": "group/app", "tag_name": "v1.0.0", "confirm_tag_name": "wrong" }),
                "confirm_tag_name",
            ),
            (
                "gitlab.release.delete",
                json!({ "project": "group/app", "tag_name": "v1.0.0", "confirm_tag_name": "wrong" }),
                "confirm_tag_name",
            ),
            (
                "gitlab.ci.variable.delete",
                json!({ "project": "group/app", "key": "KEY", "confirm_key": "OTHER" }),
                "confirm_key",
            ),
            (
                "gitlab.snippet.delete",
                json!({ "snippet_id": 5, "confirm_snippet_id": 6 }),
                "confirm_snippet_id",
            ),
        ];
        for (op, input, field) in mismatches {
            let err = built.call(op, input, &mut host).unwrap_err();
            assert!(
                err.contains(field),
                "{op}: expected {field} guard, got {err}"
            );
        }
        // A matching confirm proceeds.
        let ok = built.call(
            "gitlab.repository.tag.delete",
            json!({ "project": "group/app", "tag_name": "v1.0.0", "confirm_tag_name": "v1.0.0" }),
            &mut host,
        );
        assert_eq!(ok.unwrap()["message"], "tag deleted");
    }

    /// GL-001: a plugin-native `project.delete` exists, round-trips, and its `confirm_path` guard
    /// blocks a mismatched confirmation.
    #[test]
    fn project_delete_round_trips_with_confirm_guard() {
        let built = manifest_builder().build();
        let mut host = base().with_http("/api/v4/projects/group%2Fapp", json!({ "ok": true }));
        // Mismatched confirm_path → refused before the DELETE.
        let bad = built.call(
            "gitlab.project.delete",
            json!({ "project": "group/app", "confirm_path": "group/other" }),
            &mut host,
        );
        assert!(bad.unwrap_err().contains("confirm_path"));
        // Matching confirm_path → deletes.
        let ok = run(
            "gitlab.project.delete",
            json!({ "project": "group/app", "confirm_path": "group/app" }),
            &mut host,
        );
        assert_eq!(ok["message"], "project deleted");
        assert_eq!(ok["project"], "group/app");
    }

    /// GL-037: `changelog.add` refuses to run without an explicit `branch`, so it can never
    /// silently commit the generated section to the repo's default branch. (Before, `branch` was
    /// optional and GitLab defaulted it.)
    #[test]
    fn changelog_add_requires_explicit_branch() {
        // The shared preflight (dry-run AND runtime) rejects the missing required field.
        let (valid, problems, _) = validate(
            "gitlab.repository.changelog.add",
            json!({ "project": "group/app", "version": "1.2.0" }),
        );
        assert!(!valid);
        assert!(
            problems.iter().any(|p| p.contains("branch")),
            "{problems:?}"
        );
        // Runtime dispatch refuses it too, even past the preflight — belt and suspenders.
        let mut host = base().with_http(
            "/api/v4/projects/group%2Fapp/repository/changelog",
            json!({}),
        );
        let err = manifest_builder()
            .build()
            .call(
                "gitlab.repository.changelog.add",
                json!({ "project": "group/app", "version": "1.2.0" }),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("branch"), "{err}");
    }

    // ==== D-92: index scoping correctness & scope estimate ====

    /// GL-040: issue indexing honors a project scope (`issue_project`), hitting the project-scoped
    /// endpoint instead of the instance-wide `/issues?scope=all`. Only the project-scoped response
    /// is canned, so an instance-wide crawl would index nothing.
    #[test]
    fn index_issues_honors_project_scope() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/issues",
            json!([{ "iid": 3, "title": "Bug", "references": { "full": "group/app#3" } }]),
        );
        let out = run(
            "gitlab.index.build",
            json!({ "entities": ["issues"], "issue_project": "group/app" }),
            &mut host,
        );
        assert_eq!(out["indexed"], 1);
        assert_eq!(host.contributed.borrow()[0].id, "group/app#3");
    }

    /// GL-017: `index.build {estimate:true}` reports the crawl breadth (which datasources, each
    /// one's scope) WITHOUT crawling or contributing a single record — so a no-argument call is
    /// never a silent instance-wide sweep.
    #[test]
    fn index_build_estimate_reports_scope_without_crawling() {
        // No HTTP is canned: the estimate must not touch the network.
        let mut host = MockHost::default();
        let out = run("gitlab.index.build", json!({ "estimate": true }), &mut host);
        assert_eq!(out["estimate"], true);
        assert_eq!(out["instance_wide"], true);
        let crawl = out["would_crawl"].as_array().unwrap();
        assert!(crawl.iter().any(|v| v == "projects"));
        assert!(crawl.iter().any(|v| v == "merge_requests"));
        assert!(crawl.iter().any(|v| v == "issues"));
        assert!(out["scopes"]["issues"]
            .as_str()
            .unwrap()
            .contains("instance-wide"));
        assert!(
            host.contributed.borrow().is_empty(),
            "estimate must be pure"
        );

        // A project-scoped estimate reports the narrower scope and is not flagged instance-wide.
        let mut host2 = MockHost::default();
        let scoped = run(
            "gitlab.index.build",
            json!({ "entities": ["merge_requests"], "mr_project": "group/app", "estimate": true }),
            &mut host2,
        );
        assert_eq!(scoped["instance_wide"], false);
        assert!(scoped["scopes"]["merge_requests"]
            .as_str()
            .unwrap()
            .contains("group/app"));
    }

    /// GL-039: the never-implemented `user_*`/`group_*` `index.build` inputs are gone from the
    /// schema (the surface no longer advertises support that does not exist), while the new
    /// `issue_project`/`estimate` scoping inputs are present.
    #[test]
    fn index_build_schema_drops_unimplemented_user_and_group_inputs() {
        let m = manifest_builder().build().manifest();
        let op = m
            .operations
            .iter()
            .find(|o| o.name == "gitlab.index.build")
            .unwrap();
        let props = op.input_schema["properties"].as_object().unwrap();
        for gone in [
            "user_limit",
            "user_search",
            "active_users",
            "group_limit",
            "group_search",
            "group_order_by",
            "group_sort",
            "active_groups",
            "all_visible_groups",
        ] {
            assert!(
                !props.contains_key(gone),
                "index.build still exposes {gone}"
            );
        }
        assert!(props.contains_key("issue_project"));
        assert!(props.contains_key("estimate"));
    }

    /// GL-026: namespace resolution paginates with `per_page=100` (not the old first-20-only
    /// `per_page=20`). The canned response only matches a `per_page=100` request, so the old
    /// behavior would fail to resolve.
    #[test]
    fn project_create_namespace_resolution_is_paginated() {
        let mut host = base()
            .with_http(
                "/groups?search=team&per_page=100",
                json!([{ "id": 5, "full_path": "team", "path": "team" }]),
            )
            .with_http("/api/v4/projects", json!({ "id": 9, "name": "dummy" }));
        let out = run(
            "gitlab.project.create",
            json!({ "name": "dummy", "namespace": "team" }),
            &mut host,
        );
        assert_eq!(out["id"], 9);
    }

    /// GL-046: a bare basename that matches multiple nested groups is a hard error asking for the
    /// full path — not a silent first-wins pick.
    #[test]
    fn project_create_rejects_ambiguous_namespace() {
        let mut host = base().with_http(
            "/groups?search=team",
            json!([
                { "id": 1, "full_path": "alpha/team", "path": "team" },
                { "id": 2, "full_path": "beta/team", "path": "team" }
            ]),
        );
        let err = manifest_builder()
            .build()
            .call(
                "gitlab.project.create",
                json!({ "name": "dummy", "namespace": "team" }),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(
            err.contains("alpha/team") && err.contains("beta/team"),
            "{err}"
        );
    }

    /// GL-046: an exact `full_path` match disambiguates deterministically even when a basename is
    /// shared, so resolution succeeds instead of erroring.
    #[test]
    fn project_create_prefers_exact_full_path_over_basename() {
        let mut host = base()
            .with_http(
                "/groups?search=team",
                json!([
                    { "id": 1, "full_path": "alpha/team", "path": "team" },
                    { "id": 2, "full_path": "team", "path": "team" }
                ]),
            )
            .with_http("/api/v4/projects", json!({ "id": 9 }));
        let out = run(
            "gitlab.project.create",
            json!({ "name": "dummy", "namespace": "team" }),
            &mut host,
        );
        assert_eq!(out["id"], 9);
    }

    // ==== D-94: output ergonomics & pure-read side-effects ====

    /// GL-006: `repository.file.show` adds a `decoded_content` text field for UTF-8 files while
    /// keeping the raw base64 `content`/`encoding` for existing consumers.
    #[test]
    fn repo_file_show_adds_decoded_content_for_utf8_text() {
        // "Zm9v" is base64 for "foo".
        let mut host = base().with_http(
            "/repository/files/src%2Fmain.rs?ref=main",
            json!({ "file_path": "src/main.rs", "content": "Zm9v", "encoding": "base64" }),
        );
        let out = run(
            "gitlab.repository.file.show",
            json!({ "project": "group/app", "path": "src/main.rs", "ref": "main" }),
            &mut host,
        );
        assert_eq!(out["decoded_content"], "foo");
        // Raw fields are untouched.
        assert_eq!(out["content"], "Zm9v");
        assert_eq!(out["encoding"], "base64");
    }

    /// GL-015: a plain read/list is pure by default — no datasource records are contributed (so
    /// the host prints no `(N record(s) contributed)` stderr line). Contribution is opt-in via
    /// `contribute:true`, which `index.build` does deliberately.
    #[test]
    fn reads_are_pure_unless_contribution_is_opted_in() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/merge_requests",
            json!([{ "iid": 7, "title": "MR", "description": "body" }]),
        );
        // Default: pure read, nothing contributed.
        run(
            "gitlab.mr.list",
            json!({ "project": "group/app" }),
            &mut host,
        );
        assert!(
            host.contributed.borrow().is_empty(),
            "a plain read must not contribute records"
        );
        // Opt-in: records are contributed.
        run(
            "gitlab.mr.list",
            json!({ "project": "group/app", "contribute": true }),
            &mut host,
        );
        assert_eq!(host.contributed.borrow().len(), 1);
        assert_eq!(host.contributed.borrow()[0].id, "group/app!7");
    }
}

// ===========================================================================
// D-36: schema-derivation contract test.
//
// Each op's `input_schema` now comes from a schemars-derived struct
// (`read_op_typed::<T>` / `write_op_typed::<T>`) instead of a hand-written
// `so(json!{...}, json![...])` literal. schemars represents optional fields as
// `type: ["T","null"]` and `Vec<Value>` arrays as `{"type":"array","items":{}}`,
// so the derived JSON is not byte-identical to the legacy literal — but the
// *contract* (which fields exist, which are required, their base type) must be
// unchanged. This test encodes the legacy `so(...)` contract per op (transcribed
// from the pre-migration source) and asserts the derived schema matches after
// normalizing schemars' nullable representation. A change here is a real
// contract change. (The legacy `so(props, required)` form wrote
// `{"type":"<T>"}` per field + `"required": [...]`; arrays were untyped
// `{"type":"array"}` with no `items`.)
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
        /// A closed string set (D-88): the schema carries `enum`, so dry-run and runtime both
        /// reject out-of-set values locally.
        Enum(Vec<String>),
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
    fn en(name: &'static str, values: &[&str]) -> Prop {
        p(
            name,
            Kind::Enum(values.iter().map(|s| s.to_string()).collect()),
        )
    }
    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            (
                "gitlab.project.list",
                c(
                    vec![
                        p("search", Kind::Str),
                        p("query", Kind::Str),
                        p("order_by", Kind::Str),
                        p("sort", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                        p("membership", Kind::Bool),
                        p("contribute", Kind::Bool),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.project.show",
                c(vec![p("project", Kind::Str)], vec!["project"]),
            ),
            (
                "gitlab.mr.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        en("state", &["opened", "closed", "locked", "merged", "all"]),
                        p("search", Kind::Str),
                        p("query", Kind::Str),
                        p("order_by", Kind::Str),
                        p("sort", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                        p("source_branch", Kind::Str),
                        p("target_branch", Kind::Str),
                        p("contribute", Kind::Bool),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.mr.show",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.issue.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        en("state", &["opened", "closed", "all"]),
                        p("search", Kind::Str),
                        p("query", Kind::Str),
                        p("order_by", Kind::Str),
                        p("sort", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                        p("contribute", Kind::Bool),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.pipeline.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("status", Kind::Str),
                        p("ref", Kind::Str),
                        p("source", Kind::Str),
                        p("username", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            ("gitlab.test", c(vec![], vec![])),
            (
                "gitlab.index.build",
                c(
                    vec![
                        p("index", Kind::Str),
                        p("indexes", Kind::ArrayAny),
                        p("entity", Kind::Str),
                        p("entities", Kind::ArrayAny),
                        p("limit", Kind::Int),
                        p("search", Kind::Str),
                        p("query", Kind::Str),
                        p("order_by", Kind::Str),
                        p("sort", Kind::Str),
                        p("membership", Kind::Bool),
                        p("estimate", Kind::Bool),
                        p("issue_project", Kind::Str),
                        p("issue_limit", Kind::Int),
                        p("issue_search", Kind::Str),
                        p("issue_state", Kind::Str),
                        p("issue_order_by", Kind::Str),
                        p("issue_sort", Kind::Str),
                        p("mr_project", Kind::Str),
                        p("mr_limit", Kind::Int),
                        p("mr_search", Kind::Str),
                        p("mr_state", Kind::Str),
                        p("mr_order_by", Kind::Str),
                        p("mr_sort", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.project.create",
                c(
                    vec![
                        p("name", Kind::Str),
                        p("path", Kind::Str),
                        p("namespace", Kind::Str),
                        p("description", Kind::Str),
                        en("visibility", &["private", "internal", "public"]),
                        p("initialize_with_readme", Kind::Bool),
                    ],
                    vec!["name"],
                ),
            ),
            (
                "gitlab.project.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("confirm_path", Kind::Str),
                        p("confirm_project_id", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.mr.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("title", Kind::Str),
                        p("source_branch", Kind::Str),
                        p("target_branch", Kind::Str),
                        p("description", Kind::Str),
                        p("labels", Kind::ArrayAny),
                        p("assignee_id", Kind::Int),
                        p("assignee_ids", Kind::ArrayAny),
                        p("reviewer_ids", Kind::ArrayAny),
                        p("target_project_id", Kind::Int),
                        p("milestone_id", Kind::Int),
                        p("remove_source_branch", Kind::Bool),
                        p("squash", Kind::Bool),
                        p("allow_collaboration", Kind::Bool),
                    ],
                    vec!["project", "title", "source_branch", "target_branch"],
                ),
            ),
            (
                "gitlab.mr.update",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("title", Kind::Str),
                        p("description", Kind::Str),
                        p("target_branch", Kind::Str),
                        p("state_event", Kind::Str),
                        p("labels", Kind::ArrayAny),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.mr.approve",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("sha", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.mr.merge",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("auto_merge", Kind::Bool),
                        p("merge_commit_message", Kind::Str),
                        p("squash_commit_message", Kind::Str),
                        p("squash", Kind::Bool),
                        p("should_remove_source_branch", Kind::Bool),
                        p("remove_source_branch", Kind::Bool),
                        p("sha", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.issue.show",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.issue.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("title", Kind::Str),
                        p("description", Kind::Str),
                        p("labels", Kind::ArrayAny),
                        p("assignee_ids", Kind::ArrayAny),
                        p("milestone_id", Kind::Int),
                        p("confidential", Kind::Bool),
                    ],
                    vec!["project", "title"],
                ),
            ),
            (
                "gitlab.issue.update",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("title", Kind::Str),
                        p("description", Kind::Str),
                        p("labels", Kind::ArrayAny),
                        p("add_labels", Kind::ArrayAny),
                        p("remove_labels", Kind::ArrayAny),
                        p("state_event", Kind::Str),
                        p("assignee_ids", Kind::ArrayAny),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.issue.note.list",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("sort", Kind::Str),
                        p("order_by", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.issue.note.create",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("body", Kind::Str),
                    ],
                    vec!["body"],
                ),
            ),
            (
                "gitlab.branch.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("branch", Kind::Str),
                        p("name", Kind::Str),
                        p("ref", Kind::Str),
                    ],
                    // `branch` is conditionally required (or its `name` alias) via the
                    // custom preflight (GL-028).
                    vec!["project", "ref"],
                ),
            ),
            (
                "gitlab.branch.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("branch", Kind::Str),
                        p("name", Kind::Str),
                        p("confirm_branch", Kind::Str),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.branch.delete_merged",
                c(
                    vec![p("project", Kind::Str), p("confirm_project", Kind::Str)],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.repository.file.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("file_path", Kind::Str),
                        p("branch", Kind::Str),
                        p("content", Kind::Str),
                        p("commit_message", Kind::Str),
                        p("encoding", Kind::Str),
                        p("start_branch", Kind::Str),
                        p("author_email", Kind::Str),
                        p("author_name", Kind::Str),
                        p("execute_filemode", Kind::Bool),
                    ],
                    vec![
                        "project",
                        "file_path",
                        "branch",
                        "content",
                        "commit_message",
                    ],
                ),
            ),
            (
                "gitlab.repository.file.update",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("file_path", Kind::Str),
                        p("branch", Kind::Str),
                        p("content", Kind::Str),
                        p("commit_message", Kind::Str),
                        p("encoding", Kind::Str),
                        p("start_branch", Kind::Str),
                        p("author_email", Kind::Str),
                        p("author_name", Kind::Str),
                        p("last_commit_id", Kind::Str),
                        p("execute_filemode", Kind::Bool),
                    ],
                    vec![
                        "project",
                        "file_path",
                        "branch",
                        "content",
                        "commit_message",
                    ],
                ),
            ),
            (
                "gitlab.repository.file.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("file_path", Kind::Str),
                        p("branch", Kind::Str),
                        p("commit_message", Kind::Str),
                        p("start_branch", Kind::Str),
                        p("author_email", Kind::Str),
                        p("author_name", Kind::Str),
                        p("last_commit_id", Kind::Str),
                        p("confirm_file_path", Kind::Str),
                    ],
                    vec!["project", "file_path", "branch", "commit_message"],
                ),
            ),
            (
                "gitlab.repository.file.show",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("path", Kind::Str),
                        p("ref", Kind::Str),
                        p("max_bytes", Kind::Int),
                    ],
                    vec!["project", "path"],
                ),
            ),
            (
                "gitlab.repository.tree",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("path", Kind::Str),
                        p("ref", Kind::Str),
                        p("recursive", Kind::Bool),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.repository.commit.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("branch", Kind::Str),
                        p("commit_message", Kind::Str),
                        p("actions", Kind::ArrayAny),
                        p("start_branch", Kind::Str),
                        p("start_sha", Kind::Str),
                        p("start_project", Kind::Str),
                        p("author_email", Kind::Str),
                        p("author_name", Kind::Str),
                        p("force", Kind::Bool),
                    ],
                    vec!["project", "branch", "commit_message", "actions"],
                ),
            ),
            (
                "gitlab.repository.commit.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("ref", Kind::Str),
                        p("file_path", Kind::Str),
                        p("author", Kind::Str),
                        p("since", Kind::Str),
                        p("until", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.repository.tag.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("name", Kind::Str),
                        p("ref", Kind::Str),
                        p("message", Kind::Str),
                    ],
                    // `tag_name` is conditionally required (or its `name` alias) via the
                    // custom preflight (GL-028).
                    vec!["project", "ref"],
                ),
            ),
            (
                "gitlab.repository.tag.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("search", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.repository.tag.show",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("name", Kind::Str),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.repository.tag.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("name", Kind::Str),
                        p("confirm_tag_name", Kind::Str),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.snippet.create",
                c(
                    vec![
                        p("title", Kind::Str),
                        p("description", Kind::Str),
                        en("visibility", &["private", "internal", "public"]),
                        p("files", Kind::ArrayAny),
                    ],
                    vec!["title", "files"],
                ),
            ),
            (
                "gitlab.snippet.delete",
                c(
                    vec![
                        p("snippet_id", Kind::Int),
                        p("id", Kind::Int),
                        p("confirm_snippet_id", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.search.blobs",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("project", Kind::Str),
                        p("group", Kind::Str),
                        p("ref", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                        p("max_data_bytes", Kind::Int),
                    ],
                    vec!["query"],
                ),
            ),
            (
                "gitlab.mr.changes",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("file", Kind::Str),
                        p("max_files", Kind::Int),
                        p("max_diff_bytes", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.mr.diff.lines",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("file", Kind::Str),
                        p("line", Kind::Int),
                        p("old_line", Kind::Int),
                        p("context", Kind::Int),
                        p("search", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec!["file"],
                ),
            ),
            (
                "gitlab.compare",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("from", Kind::Str),
                        p("to", Kind::Str),
                        p("straight", Kind::Bool),
                        p("max_files", Kind::Int),
                        p("max_diff_bytes", Kind::Int),
                        p("max_commits", Kind::Int),
                    ],
                    vec!["project", "from", "to"],
                ),
            ),
            (
                "gitlab.mr.discussion.list",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "gitlab.mr.note.create",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("body", Kind::Str),
                    ],
                    vec!["body"],
                ),
            ),
            (
                "gitlab.mr.discussion.create",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("body", Kind::Str),
                        p("path", Kind::Str),
                        p("new_line", Kind::Int),
                        p("old_line", Kind::Int),
                        p("dry_run", Kind::Bool),
                    ],
                    vec!["body"],
                ),
            ),
            (
                "gitlab.mr.discussion.reply",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("discussion_id", Kind::Str),
                        p("body", Kind::Str),
                    ],
                    vec!["discussion_id", "body"],
                ),
            ),
            (
                "gitlab.mr.discussion.resolve",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("project", Kind::Str),
                        p("iid", Kind::Int),
                        p("discussion_id", Kind::Str),
                        p("resolved", Kind::Bool),
                    ],
                    vec!["discussion_id"],
                ),
            ),
            (
                "gitlab.ci.variable.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("key", Kind::Str),
                        p("value", Kind::Str),
                        p("description", Kind::Str),
                        p("environment_scope", Kind::Str),
                        p("masked", Kind::Bool),
                        p("masked_and_hidden", Kind::Bool),
                        p("protected", Kind::Bool),
                        p("raw", Kind::Bool),
                        en("variable_type", &["env_var", "file"]),
                    ],
                    vec!["project", "key", "value"],
                ),
            ),
            (
                "gitlab.ci.variable.update",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("key", Kind::Str),
                        p("value", Kind::Str),
                        p("description", Kind::Str),
                        p("environment_scope", Kind::Str),
                        p("masked", Kind::Bool),
                        p("protected", Kind::Bool),
                        p("raw", Kind::Bool),
                        en("variable_type", &["env_var", "file"]),
                    ],
                    vec!["project", "key", "value"],
                ),
            ),
            (
                "gitlab.ci.variable.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("key", Kind::Str),
                        p("environment_scope", Kind::Str),
                        p("confirm_key", Kind::Str),
                    ],
                    vec!["project", "key"],
                ),
            ),
            (
                "gitlab.pipeline.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("ref", Kind::Str),
                        p("variables", Kind::ArrayAny),
                    ],
                    vec!["project", "ref"],
                ),
            ),
            (
                "gitlab.pipeline.retry",
                c(
                    vec![p("project", Kind::Str), p("pipeline_id", Kind::Int)],
                    vec!["project", "pipeline_id"],
                ),
            ),
            (
                "gitlab.pipeline.cancel",
                c(
                    vec![p("project", Kind::Str), p("pipeline_id", Kind::Int)],
                    vec!["project", "pipeline_id"],
                ),
            ),
            (
                "gitlab.job.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("pipeline_id", Kind::Int),
                        p("scope", Kind::ArrayAny),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project", "pipeline_id"],
                ),
            ),
            (
                "gitlab.environment.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("search", Kind::Str),
                        p("states", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.deployment.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("environment", Kind::Str),
                        p("status", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.release.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.release.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("ref", Kind::Str),
                        p("name", Kind::Str),
                        p("description", Kind::Str),
                        p("tag_message", Kind::Str),
                        p("milestones", Kind::ArrayAny),
                        p("released_at", Kind::Str),
                        p("assets_links", Kind::ArrayAny),
                    ],
                    vec!["project", "tag_name"],
                ),
            ),
            (
                "gitlab.release.show",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.release.update",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("name", Kind::Str),
                        p("description", Kind::Str),
                        p("milestones", Kind::ArrayAny),
                        p("released_at", Kind::Str),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.release.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("confirm_tag_name", Kind::Str),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.release.link.list",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("limit", Kind::Int),
                        p("per_page", Kind::Int),
                        p("page", Kind::Int),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.release.link.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("name", Kind::Str),
                        p("url", Kind::Str),
                        p("direct_asset_path", Kind::Str),
                        en("link_type", &["other", "runbook", "image", "package"]),
                    ],
                    vec!["project", "name", "url"],
                ),
            ),
            (
                "gitlab.release.link.update",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("link_id", Kind::Int),
                        p("name", Kind::Str),
                        p("url", Kind::Str),
                        p("direct_asset_path", Kind::Str),
                        en("link_type", &["other", "runbook", "image", "package"]),
                    ],
                    vec!["project", "link_id"],
                ),
            ),
            (
                "gitlab.release.link.delete",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("tag_name", Kind::Str),
                        p("tag", Kind::Str),
                        p("link_id", Kind::Int),
                    ],
                    vec!["project", "link_id"],
                ),
            ),
            (
                "gitlab.repository.changelog.generate",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("version", Kind::Str),
                        p("from", Kind::Str),
                        p("to", Kind::Str),
                        p("date", Kind::Str),
                        p("trailer", Kind::Str),
                        p("config_file", Kind::Str),
                    ],
                    vec!["project", "version"],
                ),
            ),
            (
                "gitlab.repository.changelog.add",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("version", Kind::Str),
                        p("branch", Kind::Str),
                        p("file", Kind::Str),
                        p("from", Kind::Str),
                        p("to", Kind::Str),
                        p("date", Kind::Str),
                        p("message", Kind::Str),
                        p("trailer", Kind::Str),
                        p("config_file", Kind::Str),
                    ],
                    vec!["project", "version", "branch"],
                ),
            ),
            (
                "gitlab.repository.archive",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("ref", Kind::Str),
                        p("path", Kind::Str),
                        p("max_bytes", Kind::Int),
                        en(
                            "format",
                            &[
                                "tar.gz", "tar.bz2", "tbz", "tbz2", "tb2", "bz2", "tar", "zip",
                            ],
                        ),
                    ],
                    vec!["project"],
                ),
            ),
            (
                "gitlab.ci.job_token.scope.show",
                c(vec![p("project", Kind::Str)], vec!["project"]),
            ),
            (
                "gitlab.ci.job_token.scope.set",
                c(
                    vec![p("project", Kind::Str), p("enabled", Kind::Bool)],
                    vec!["project", "enabled"],
                ),
            ),
            (
                "gitlab.ci.job_token.allowlist.list",
                c(vec![p("project", Kind::Str)], vec!["project"]),
            ),
            (
                "gitlab.ci.job_token.allowlist.add",
                c(
                    vec![p("project", Kind::Str), p("target_project_id", Kind::Int)],
                    vec!["project", "target_project_id"],
                ),
            ),
            (
                "gitlab.ci.job_token.allowlist.remove",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("target_project_id", Kind::Int),
                        p("confirm_target_project_id", Kind::Int),
                    ],
                    vec!["project", "target_project_id"],
                ),
            ),
            (
                "gitlab.ci.job_token.groups_allowlist.list",
                c(vec![p("project", Kind::Str)], vec!["project"]),
            ),
            (
                "gitlab.ci.job_token.groups_allowlist.add",
                c(
                    vec![p("project", Kind::Str), p("target_group_id", Kind::Int)],
                    vec!["project", "target_group_id"],
                ),
            ),
            (
                "gitlab.ci.job_token.groups_allowlist.remove",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("target_group_id", Kind::Int),
                        p("confirm_target_group_id", Kind::Int),
                    ],
                    vec!["project", "target_group_id"],
                ),
            ),
            (
                "gitlab.repository.protected_tag.list",
                c(vec![p("project", Kind::Str)], vec!["project"]),
            ),
            (
                "gitlab.repository.protected_tag.show",
                c(
                    vec![p("project", Kind::Str), p("name", Kind::Str)],
                    vec!["project", "name"],
                ),
            ),
            (
                "gitlab.repository.protected_tag.protect",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("name", Kind::Str),
                        p("create_access_level", Kind::Int),
                    ],
                    vec!["project", "name"],
                ),
            ),
            (
                "gitlab.repository.protected_tag.unprotect",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("name", Kind::Str),
                        p("confirm_name", Kind::Str),
                    ],
                    vec!["project", "name"],
                ),
            ),
            (
                "gitlab.deploy_token.list",
                c(vec![p("project", Kind::Str)], vec!["project"]),
            ),
            (
                "gitlab.deploy_token.create",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("name", Kind::Str),
                        p("scopes", Kind::ArrayAny),
                        p("expires_at", Kind::Str),
                        p("username", Kind::Str),
                    ],
                    vec!["project", "name", "scopes"],
                ),
            ),
            (
                "gitlab.deploy_token.revoke",
                c(
                    vec![
                        p("project", Kind::Str),
                        p("token_id", Kind::Int),
                        p("confirm_token_id", Kind::Int),
                    ],
                    vec!["project", "token_id"],
                ),
            ),
        ]
    }

    fn kind_of(root: &Value, node: &Value) -> Kind {
        // Enum fields land in `$defs` behind a `$ref` (possibly wrapped in `anyOf` for Option).
        if let Some(r) = node.get("$ref").and_then(|v| v.as_str()) {
            let resolved = r
                .strip_prefix("#/$defs/")
                .and_then(|name| root.get("$defs").and_then(|d| d.get(name)))
                .unwrap_or_else(|| panic!("unresolvable $ref: {r}"));
            return kind_of(root, resolved);
        }
        if let Some(branches) = node.get("anyOf").and_then(|v| v.as_array()) {
            let branch = branches
                .iter()
                .find(|b| b.get("type").and_then(|t| t.as_str()) != Some("null"))
                .expect("anyOf with a non-null branch");
            return kind_of(root, branch);
        }
        if let Some(vals) = node.get("enum").and_then(|v| v.as_array()) {
            return Kind::Enum(
                vals.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect(),
            );
        }
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

    fn base_kind(t: &str, _node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "array" => Kind::ArrayAny,
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
                got.insert(k.as_str(), kind_of(schema, v));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got.get(*name).unwrap_or_else(|| {
                panic!("{op_name}: missing property `{name}` in derived schema")
            });
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
            .filter(|o| !o.internal)
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
