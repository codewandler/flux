//! `gitlab` — a flux integration plugin for the GitLab REST API (v4): projects, merge requests, issues,
//! pipelines, CI/CD, code review, and releases. Authenticates with a personal access token via the
//! `PRIVATE-TOKEN` header; the base URL is the `gitlab.endpoint` (defaults to gitlab.com). List ops
//! contribute datasource records (`gitlab.project` / `gitlab.merge_request` / `gitlab.issue`) so the
//! agent can search them; `gitlab.index.build` drives that contribution exhaustively over the surface.
//!
//! This is the reference template for the HTTP-API integration plugins: every read/list/get/search op
//! is a `read_op` and every create/update/delete/mutate op is a `write_op`; all REST verbs go through
//! the DRY `gl_get`/`gl_post`/`gl_put`/`gl_delete` helpers (PRIVATE-TOKEN header, `base + /api/v4 + path`,
//! is_success check, JSON parse); `gitlab.repository.archive` stages the downloaded bytes through the
//! host `blob` capability.

use host_kit::*;
use regex::Regex;
use serde_json::{json, Map, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("gitlab", "0.1.0")
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
            read_op(
                "gitlab.project.list",
                "List/search projects the token can see.",
                so(json!({"search": {"type": "string"}}), json!([])),
            ),
            project_list,
        )
        .operation(
            read_op(
                "gitlab.project.show",
                "Show one project by id or path.",
                so(json!({"project": {"type": "string"}}), json!(["project"])),
            ),
            project_show,
        )
        .operation(
            read_op(
                "gitlab.mr.list",
                "List a project's merge requests (state: opened|closed|merged|all).",
                so(
                    json!({"project": {"type": "string"}, "state": {"type": "string"}}),
                    json!(["project"]),
                ),
            ),
            mr_list,
        )
        .operation(
            read_op(
                "gitlab.mr.show",
                "Show one merge request by ref (PROJECT!IID) or project + iid.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}}),
                    json!([]),
                ),
            ),
            mr_show,
        )
        .operation(
            read_op(
                "gitlab.issue.list",
                "List a project's issues (state: opened|closed|all).",
                so(
                    json!({"project": {"type": "string"}, "state": {"type": "string"}}),
                    json!(["project"]),
                ),
            ),
            issue_list,
        )
        .operation(
            read_op(
                "gitlab.pipeline.list",
                "List a project's recent CI pipelines.",
                so(json!({"project": {"type": "string"}}), json!(["project"])),
            ),
            pipeline_list,
        )
        // ---- auth test + index ----
        .operation(
            read_op(
                "gitlab.test",
                "Test GitLab authentication by fetching the current user.",
                so(json!({}), json!([])),
            ),
            auth_test,
        )
        .operation(
            read_op(
                "gitlab.index.build",
                "Build GitLab index records across projects, merge requests, and issues.",
                so(json!({"limit": {"type": "integer"}}), json!([])),
            ),
            index_build,
        )
        // ---- project create ----
        .operation(
            write_op(
                "gitlab.project.create",
                "Create a project, optionally inside a group namespace (resolved by path).",
                so(
                    json!({
                        "name": {"type": "string"}, "path": {"type": "string"},
                        "namespace": {"type": "string"}, "description": {"type": "string"},
                        "visibility": {"type": "string"}, "initialize_with_readme": {"type": "boolean"}
                    }),
                    json!(["name"]),
                ),
            ),
            project_create,
        )
        // ---- merge request writes ----
        .operation(
            write_op(
                "gitlab.mr.create",
                "Create a GitLab merge request.",
                so(
                    json!({
                        "project": {"type": "string"}, "title": {"type": "string"},
                        "source_branch": {"type": "string"}, "target_branch": {"type": "string"},
                        "description": {"type": "string"}, "labels": {"type": "array"},
                        "assignee_id": {"type": "integer"}, "assignee_ids": {"type": "array"},
                        "reviewer_ids": {"type": "array"}, "target_project_id": {"type": "integer"},
                        "milestone_id": {"type": "integer"}, "remove_source_branch": {"type": "boolean"},
                        "squash": {"type": "boolean"}, "allow_collaboration": {"type": "boolean"}
                    }),
                    json!(["project", "title", "source_branch", "target_branch"]),
                ),
            ),
            mr_create,
        )
        .operation(
            write_op(
                "gitlab.mr.update",
                "Update merge request fields (title, description, target branch, labels) or close/reopen via state_event.",
                so(
                    json!({
                        "ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"},
                        "title": {"type": "string"}, "description": {"type": "string"},
                        "target_branch": {"type": "string"}, "state_event": {"type": "string"},
                        "labels": {"type": "array"}
                    }),
                    json!([]),
                ),
            ),
            mr_update,
        )
        .operation(
            write_op(
                "gitlab.mr.approve",
                "Approve a GitLab merge request.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "sha": {"type": "string"}}),
                    json!([]),
                ),
            ),
            mr_approve,
        )
        .operation(
            write_op(
                "gitlab.mr.merge",
                "Merge a GitLab merge request.",
                so(
                    json!({
                        "ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"},
                        "auto_merge": {"type": "boolean"}, "merge_commit_message": {"type": "string"},
                        "squash_commit_message": {"type": "string"}, "squash": {"type": "boolean"},
                        "should_remove_source_branch": {"type": "boolean"}, "sha": {"type": "string"}
                    }),
                    json!([]),
                ),
            ),
            mr_merge,
        )
        // ---- issues ----
        .operation(
            read_op(
                "gitlab.issue.show",
                "Show one GitLab issue, including its Markdown description.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}}),
                    json!([]),
                ),
            ),
            issue_show,
        )
        .operation(
            write_op(
                "gitlab.issue.create",
                "Create a GitLab issue. Description is GitLab-flavored Markdown.",
                so(
                    json!({
                        "project": {"type": "string"}, "title": {"type": "string"},
                        "description": {"type": "string"}, "labels": {"type": "array"},
                        "assignee_ids": {"type": "array"}, "milestone_id": {"type": "integer"},
                        "confidential": {"type": "boolean"}
                    }),
                    json!(["project", "title"]),
                ),
            ),
            issue_create,
        )
        .operation(
            write_op(
                "gitlab.issue.update",
                "Update a GitLab issue (title/description/labels/assignees) or transition it via state_event.",
                so(
                    json!({
                        "ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"},
                        "title": {"type": "string"}, "description": {"type": "string"},
                        "labels": {"type": "array"}, "add_labels": {"type": "array"},
                        "remove_labels": {"type": "array"}, "state_event": {"type": "string"},
                        "assignee_ids": {"type": "array"}
                    }),
                    json!([]),
                ),
            ),
            issue_update,
        )
        .operation(
            read_op(
                "gitlab.issue.note.list",
                "List comments (notes) on a GitLab issue. Bodies are Markdown.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "sort": {"type": "string"}, "order_by": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!([]),
                ),
            ),
            issue_note_list,
        )
        .operation(
            write_op(
                "gitlab.issue.note.create",
                "Add a comment (note) to a GitLab issue. Body is Markdown.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "body": {"type": "string"}}),
                    json!(["body"]),
                ),
            ),
            issue_note_create,
        )
        // ---- branches ----
        .operation(
            write_op(
                "gitlab.branch.create",
                "Create a GitLab repository branch.",
                so(
                    json!({"project": {"type": "string"}, "branch": {"type": "string"}, "ref": {"type": "string"}}),
                    json!(["project", "branch", "ref"]),
                ),
            ),
            branch_create,
        )
        .operation(
            write_op(
                "gitlab.branch.delete",
                "Delete a GitLab repository branch.",
                so(
                    json!({"project": {"type": "string"}, "branch": {"type": "string"}}),
                    json!(["project", "branch"]),
                ),
            ),
            branch_delete,
        )
        .operation(
            write_op(
                "gitlab.branch.delete_merged",
                "Delete all merged branches in a GitLab project.",
                so(json!({"project": {"type": "string"}}), json!(["project"])),
            ),
            branch_delete_merged,
        )
        // ---- repository files ----
        .operation(
            write_op(
                "gitlab.repository.file.create",
                "Create a file in a GitLab repository.",
                so(
                    json!({
                        "project": {"type": "string"}, "file_path": {"type": "string"},
                        "branch": {"type": "string"}, "content": {"type": "string"},
                        "commit_message": {"type": "string"}, "encoding": {"type": "string"},
                        "start_branch": {"type": "string"}, "author_email": {"type": "string"},
                        "author_name": {"type": "string"}, "execute_filemode": {"type": "boolean"}
                    }),
                    json!(["project", "file_path", "branch", "content", "commit_message"]),
                ),
            ),
            repo_file_create,
        )
        .operation(
            write_op(
                "gitlab.repository.file.update",
                "Update a file in a GitLab repository.",
                so(
                    json!({
                        "project": {"type": "string"}, "file_path": {"type": "string"},
                        "branch": {"type": "string"}, "content": {"type": "string"},
                        "commit_message": {"type": "string"}, "encoding": {"type": "string"},
                        "start_branch": {"type": "string"}, "author_email": {"type": "string"},
                        "author_name": {"type": "string"}, "last_commit_id": {"type": "string"},
                        "execute_filemode": {"type": "boolean"}
                    }),
                    json!(["project", "file_path", "branch", "content", "commit_message"]),
                ),
            ),
            repo_file_update,
        )
        .operation(
            write_op(
                "gitlab.repository.file.delete",
                "Delete a file from a GitLab repository.",
                so(
                    json!({
                        "project": {"type": "string"}, "file_path": {"type": "string"},
                        "branch": {"type": "string"}, "commit_message": {"type": "string"},
                        "start_branch": {"type": "string"}, "author_email": {"type": "string"},
                        "author_name": {"type": "string"}, "last_commit_id": {"type": "string"}
                    }),
                    json!(["project", "file_path", "branch", "commit_message"]),
                ),
            ),
            repo_file_delete,
        )
        .operation(
            read_op(
                "gitlab.repository.file.show",
                "Read a file's content at a ref (default branch when omitted).",
                so(
                    json!({"project": {"type": "string"}, "path": {"type": "string"}, "ref": {"type": "string"}}),
                    json!(["project", "path"]),
                ),
            ),
            repo_file_show,
        )
        .operation(
            read_op(
                "gitlab.repository.tree",
                "List a repository tree at a ref (optionally recursive).",
                so(
                    json!({"project": {"type": "string"}, "path": {"type": "string"}, "ref": {"type": "string"}, "recursive": {"type": "boolean"}, "limit": {"type": "integer"}}),
                    json!(["project"]),
                ),
            ),
            repo_tree,
        )
        // ---- commits ----
        .operation(
            write_op(
                "gitlab.repository.commit.create",
                "Create a GitLab commit with one or more file actions.",
                so(
                    json!({
                        "project": {"type": "string"}, "branch": {"type": "string"},
                        "commit_message": {"type": "string"}, "actions": {"type": "array"},
                        "start_branch": {"type": "string"}, "start_sha": {"type": "string"},
                        "start_project": {"type": "string"}, "author_email": {"type": "string"},
                        "author_name": {"type": "string"}, "force": {"type": "boolean"}
                    }),
                    json!(["project", "branch", "commit_message", "actions"]),
                ),
            ),
            commit_create,
        )
        .operation(
            read_op(
                "gitlab.repository.commit.list",
                "List a ref's commit history, newest first; filter by path, author, or a since/until window.",
                so(
                    json!({"project": {"type": "string"}, "ref": {"type": "string"}, "file_path": {"type": "string"}, "author": {"type": "string"}, "since": {"type": "string"}, "until": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!(["project"]),
                ),
            ),
            commit_list,
        )
        // ---- tags ----
        .operation(
            write_op(
                "gitlab.repository.tag.create",
                "Create a GitLab repository tag.",
                so(
                    json!({"project": {"type": "string"}, "tag_name": {"type": "string"}, "ref": {"type": "string"}, "message": {"type": "string"}}),
                    json!(["project", "tag_name", "ref"]),
                ),
            ),
            tag_create,
        )
        .operation(
            read_op(
                "gitlab.repository.tag.list",
                "List a project's git tags with their target commits, newest first.",
                so(
                    json!({"project": {"type": "string"}, "search": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!(["project"]),
                ),
            ),
            tag_list,
        )
        .operation(
            read_op(
                "gitlab.repository.tag.show",
                "Show one git tag with its target commit and any annotation message.",
                so(
                    json!({"project": {"type": "string"}, "tag_name": {"type": "string"}}),
                    json!(["project", "tag_name"]),
                ),
            ),
            tag_show,
        )
        .operation(
            write_op(
                "gitlab.repository.tag.delete",
                "Delete a git tag from a project.",
                so(
                    json!({"project": {"type": "string"}, "tag_name": {"type": "string"}}),
                    json!(["project", "tag_name"]),
                ),
            ),
            tag_delete,
        )
        // ---- snippets ----
        .operation(
            write_op(
                "gitlab.snippet.create",
                "Create a personal GitLab snippet.",
                so(
                    json!({"title": {"type": "string"}, "description": {"type": "string"}, "visibility": {"type": "string"}, "files": {"type": "array"}}),
                    json!(["title", "files"]),
                ),
            ),
            snippet_create,
        )
        .operation(
            write_op(
                "gitlab.snippet.delete",
                "Delete a personal GitLab snippet.",
                so(json!({"snippet_id": {"type": "integer"}}), json!([])),
            ),
            snippet_delete,
        )
        // ---- search ----
        .operation(
            read_op(
                "gitlab.search.blobs",
                "Search file contents (GitLab scope=blobs) across a project, a group, or the instance.",
                so(
                    json!({"query": {"type": "string"}, "project": {"type": "string"}, "group": {"type": "string"}, "ref": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!(["query"]),
                ),
            ),
            search_blobs,
        )
        // ---- review / diff ----
        .operation(
            read_op(
                "gitlab.mr.changes",
                "List a merge request's changed files with bounded unified diffs, plus the base/start/head diff refs.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "file": {"type": "string"}, "max_files": {"type": "integer"}, "max_diff_bytes": {"type": "integer"}}),
                    json!([]),
                ),
            ),
            mr_changes,
        )
        .operation(
            read_op(
                "gitlab.mr.diff.lines",
                "Parse one changed file's diff into typed lines (added/deleted/context with old/new line numbers).",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "file": {"type": "string"}, "line": {"type": "integer"}, "context": {"type": "integer"}, "search": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!(["file"]),
                ),
            ),
            mr_diff_lines,
        )
        .operation(
            read_op(
                "gitlab.compare",
                "Compare two refs (branches, tags, or commits): commits between them and bounded file diffs.",
                so(
                    json!({"project": {"type": "string"}, "from": {"type": "string"}, "to": {"type": "string"}, "straight": {"type": "boolean"}, "max_files": {"type": "integer"}, "max_diff_bytes": {"type": "integer"}}),
                    json!(["project", "from", "to"]),
                ),
            ),
            compare,
        )
        .operation(
            read_op(
                "gitlab.mr.discussion.list",
                "List a merge request's discussion threads with resolution state and inline line positions.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "limit": {"type": "integer"}}),
                    json!([]),
                ),
            ),
            mr_discussion_list,
        )
        .operation(
            write_op(
                "gitlab.mr.note.create",
                "Post a top-level merge request note.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "body": {"type": "string"}}),
                    json!(["body"]),
                ),
            ),
            mr_note_create,
        )
        .operation(
            write_op(
                "gitlab.mr.discussion.create",
                "Open a merge request discussion, optionally anchored to a diff line (path + new_line/old_line). dry_run previews the resolved position without posting.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "body": {"type": "string"}, "path": {"type": "string"}, "new_line": {"type": "integer"}, "old_line": {"type": "integer"}, "dry_run": {"type": "boolean"}}),
                    json!(["body"]),
                ),
            ),
            mr_discussion_create,
        )
        .operation(
            write_op(
                "gitlab.mr.discussion.reply",
                "Reply into an existing merge request discussion thread.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "discussion_id": {"type": "string"}, "body": {"type": "string"}}),
                    json!(["discussion_id", "body"]),
                ),
            ),
            mr_discussion_reply,
        )
        .operation(
            write_op(
                "gitlab.mr.discussion.resolve",
                "Resolve (or unresolve with resolved=false) a merge request discussion thread.",
                so(
                    json!({"ref": {"type": "string"}, "project": {"type": "string"}, "iid": {"type": "integer"}, "discussion_id": {"type": "string"}, "resolved": {"type": "boolean"}}),
                    json!(["discussion_id"]),
                ),
            ),
            mr_discussion_resolve,
        )
        // ---- CI/CD ----
        .operation(
            write_op(
                "gitlab.ci.variable.create",
                "Create a GitLab project CI/CD variable.",
                so(
                    json!({
                        "project": {"type": "string"}, "key": {"type": "string"}, "value": {"type": "string"},
                        "description": {"type": "string"}, "environment_scope": {"type": "string"},
                        "masked": {"type": "boolean"}, "masked_and_hidden": {"type": "boolean"},
                        "protected": {"type": "boolean"}, "raw": {"type": "boolean"}, "variable_type": {"type": "string"}
                    }),
                    json!(["project", "key", "value"]),
                ),
            ),
            ci_variable_create,
        )
        .operation(
            write_op(
                "gitlab.ci.variable.update",
                "Update a GitLab project CI/CD variable.",
                so(
                    json!({
                        "project": {"type": "string"}, "key": {"type": "string"}, "value": {"type": "string"},
                        "description": {"type": "string"}, "environment_scope": {"type": "string"},
                        "masked": {"type": "boolean"}, "protected": {"type": "boolean"},
                        "raw": {"type": "boolean"}, "variable_type": {"type": "string"}
                    }),
                    json!(["project", "key", "value"]),
                ),
            ),
            ci_variable_update,
        )
        .operation(
            write_op(
                "gitlab.ci.variable.delete",
                "Delete a GitLab project CI/CD variable.",
                so(
                    json!({"project": {"type": "string"}, "key": {"type": "string"}, "environment_scope": {"type": "string"}}),
                    json!(["project", "key"]),
                ),
            ),
            ci_variable_delete,
        )
        .operation(
            write_op(
                "gitlab.pipeline.create",
                "Create a GitLab CI pipeline.",
                so(
                    json!({"project": {"type": "string"}, "ref": {"type": "string"}, "variables": {"type": "array"}}),
                    json!(["project", "ref"]),
                ),
            ),
            pipeline_create,
        )
        .operation(
            write_op(
                "gitlab.pipeline.retry",
                "Retry a GitLab CI pipeline.",
                so(
                    json!({"project": {"type": "string"}, "pipeline_id": {"type": "integer"}}),
                    json!(["project", "pipeline_id"]),
                ),
            ),
            pipeline_retry,
        )
        .operation(
            write_op(
                "gitlab.pipeline.cancel",
                "Cancel a GitLab CI pipeline.",
                so(
                    json!({"project": {"type": "string"}, "pipeline_id": {"type": "integer"}}),
                    json!(["project", "pipeline_id"]),
                ),
            ),
            pipeline_cancel,
        )
        .operation(
            read_op(
                "gitlab.job.list",
                "List one pipeline's jobs with stage, status, duration, and failure_reason.",
                so(
                    json!({"project": {"type": "string"}, "pipeline_id": {"type": "integer"}, "scope": {"type": "array"}, "limit": {"type": "integer"}}),
                    json!(["project", "pipeline_id"]),
                ),
            ),
            job_list,
        )
        .operation(
            read_op(
                "gitlab.environment.list",
                "List a project's environments with state, tier, external URL, and last deployment.",
                so(
                    json!({"project": {"type": "string"}, "search": {"type": "string"}, "states": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!(["project"]),
                ),
            ),
            environment_list,
        )
        .operation(
            read_op(
                "gitlab.deployment.list",
                "List a project's deployments, newest first, filterable by environment and status.",
                so(
                    json!({"project": {"type": "string"}, "environment": {"type": "string"}, "status": {"type": "string"}, "limit": {"type": "integer"}}),
                    json!(["project"]),
                ),
            ),
            deployment_list,
        )
        // ---- releases ----
        .operation(
            read_op(
                "gitlab.release.list",
                "List a project's releases, newest first.",
                so(json!({"project": {"type": "string"}, "limit": {"type": "integer"}}), json!(["project"])),
            ),
            release_list,
        )
        .operation(
            write_op(
                "gitlab.release.create",
                "Create a GitLab release for a tag, cutting the tag from ref when it does not yet exist.",
                so(
                    json!({
                        "project": {"type": "string"}, "tag_name": {"type": "string"}, "ref": {"type": "string"},
                        "name": {"type": "string"}, "description": {"type": "string"}, "tag_message": {"type": "string"},
                        "milestones": {"type": "array"}, "released_at": {"type": "string"}, "assets_links": {"type": "array"}
                    }),
                    json!(["project", "tag_name"]),
                ),
            ),
            release_create,
        )
        .operation(
            read_op(
                "gitlab.release.show",
                "Show one GitLab release with its description, milestones, and asset links.",
                so(json!({"project": {"type": "string"}, "tag_name": {"type": "string"}}), json!(["project", "tag_name"])),
            ),
            release_show,
        )
        .operation(
            write_op(
                "gitlab.release.update",
                "Update a GitLab release's title, notes, milestones, or release date.",
                so(
                    json!({"project": {"type": "string"}, "tag_name": {"type": "string"}, "name": {"type": "string"}, "description": {"type": "string"}, "milestones": {"type": "array"}, "released_at": {"type": "string"}}),
                    json!(["project", "tag_name"]),
                ),
            ),
            release_update,
        )
        .operation(
            write_op(
                "gitlab.release.delete",
                "Delete a GitLab release. The underlying git tag is left in place.",
                so(json!({"project": {"type": "string"}, "tag_name": {"type": "string"}}), json!(["project", "tag_name"])),
            ),
            release_delete,
        )
        .operation(
            read_op(
                "gitlab.release.link.list",
                "List the asset links attached to a release.",
                so(json!({"project": {"type": "string"}, "tag_name": {"type": "string"}, "limit": {"type": "integer"}}), json!(["project", "tag_name"])),
            ),
            release_link_list,
        )
        .operation(
            write_op(
                "gitlab.release.link.create",
                "Attach a new asset link (a download or related URL) to a release.",
                so(
                    json!({"project": {"type": "string"}, "tag_name": {"type": "string"}, "name": {"type": "string"}, "url": {"type": "string"}, "direct_asset_path": {"type": "string"}, "link_type": {"type": "string"}}),
                    json!(["project", "tag_name", "name", "url"]),
                ),
            ),
            release_link_create,
        )
        .operation(
            write_op(
                "gitlab.release.link.update",
                "Edit an existing release asset link.",
                so(
                    json!({"project": {"type": "string"}, "tag_name": {"type": "string"}, "link_id": {"type": "integer"}, "name": {"type": "string"}, "url": {"type": "string"}, "direct_asset_path": {"type": "string"}, "link_type": {"type": "string"}}),
                    json!(["project", "tag_name", "link_id"]),
                ),
            ),
            release_link_update,
        )
        .operation(
            write_op(
                "gitlab.release.link.delete",
                "Remove an asset link from a release.",
                so(json!({"project": {"type": "string"}, "tag_name": {"type": "string"}, "link_id": {"type": "integer"}}), json!(["project", "tag_name", "link_id"])),
            ),
            release_link_delete,
        )
        // ---- changelog ----
        .operation(
            read_op(
                "gitlab.repository.changelog.generate",
                "Generate Markdown release notes from the commits between two refs without committing.",
                so(
                    json!({"project": {"type": "string"}, "version": {"type": "string"}, "from": {"type": "string"}, "to": {"type": "string"}, "date": {"type": "string"}, "trailer": {"type": "string"}, "config_file": {"type": "string"}}),
                    json!(["project", "version"]),
                ),
            ),
            changelog_generate,
        )
        .operation(
            write_op(
                "gitlab.repository.changelog.add",
                "Generate a changelog section and commit it into the repository's changelog file (default CHANGELOG.md).",
                so(
                    json!({"project": {"type": "string"}, "version": {"type": "string"}, "branch": {"type": "string"}, "file": {"type": "string"}, "from": {"type": "string"}, "to": {"type": "string"}, "date": {"type": "string"}, "message": {"type": "string"}, "trailer": {"type": "string"}, "config_file": {"type": "string"}}),
                    json!(["project", "version"]),
                ),
            ),
            changelog_add,
        )
        // ---- archive (blob) ----
        .operation(
            read_op(
                "gitlab.repository.archive",
                "Download a repository archive (tar.gz/zip/tar) at a ref into the host blob store.",
                so(
                    json!({"project": {"type": "string"}, "ref": {"type": "string"}, "path": {"type": "string"}, "format": {"type": "string"}}),
                    json!(["project"]),
                ),
            ),
            repository_archive,
        )
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

/// `{ "type": "object", "properties": <props>, "required": <required> }`.
fn so(props: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

// ---------------------------------------------------------------------------
// HTTP plumbing — every REST verb funnels through `gl_request` (PRIVATE-TOKEN
// header, base + /api/v4 + path, is_success check) so auth/encoding stay DRY.
// ---------------------------------------------------------------------------

fn gl_base_token(host: &mut Host) -> Result<(String, String), String> {
    let base = host
        .endpoint("gitlab.endpoint")
        .unwrap_or_else(|_| "https://gitlab.com".into());
    let token = host.secret("personal_token")?;
    Ok((base.trim_end_matches('/').to_string(), token))
}

fn gl_request(
    host: &mut Host,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<HttpResponse, String> {
    let (base, token) = gl_base_token(host)?;
    let url = format!("{base}/api/v4{path}");
    let mut headers: Vec<(&str, &str)> = vec![("PRIVATE-TOKEN", token.as_str())];
    let body_str;
    let body_ref = match body {
        Some(b) => {
            body_str = serde_json::to_string(b).map_err(|e| e.to_string())?;
            headers.push(("content-type", "application/json"));
            Some(body_str.as_str())
        }
        None => None,
    };
    let resp = host.http(method, &url, None, &headers, body_ref)?;
    if !resp.is_success() {
        return Err(format!(
            "gitlab {method} {path} → {} {}",
            resp.status, resp.body
        ));
    }
    Ok(resp)
}

/// GET `{base}/api/v4{path}` and return the parsed JSON.
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

/// GET raw bytes (for binary downloads like the repository archive).
fn gl_get_bytes(host: &mut Host, path: &str) -> Result<Vec<u8>, String> {
    Ok(gl_request(host, "GET", path, None)?.body.into_bytes())
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
    let search = flex_str(&input, "search").unwrap_or_default();
    let path = if search.is_empty() {
        "/projects?membership=true&per_page=20&order_by=last_activity_at".to_string()
    } else {
        format!("/projects?search={}&per_page=20", enc(&search))
    };
    let projects = gl_get(host, &path)?;
    contribute_projects(host, &projects);
    Ok(projects)
}

fn project_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}", enc(&project)))
}

fn mr_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let state = flex_str(&input, "state").unwrap_or_else(|| "opened".into());
    let mrs = gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests?state={state}&per_page=20",
            enc(&project)
        ),
    )?;
    contribute_list(host, &mrs, "gitlab.merge_request", &project);
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
    let issues = gl_get(
        host,
        &format!(
            "/projects/{}/issues?state={state}&per_page=20",
            enc(&project)
        ),
    )?;
    contribute_list(host, &issues, "gitlab.issue", &project);
    Ok(issues)
}

fn pipeline_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/pipelines?per_page=20", enc(&project)),
    )
}

// ---------------------------------------------------------------------------
// Auth test + index build.
// ---------------------------------------------------------------------------

fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let user = gl_get(host, "/user")?;
    Ok(json!({ "status": "ok", "text": "GitLab auth OK", "user": user }))
}

/// Drive datasource contribution exhaustively over the global surface: projects, then merge requests,
/// then issues — paging each up to a few hundred records — and return the total `indexed` count.
fn index_build(input: Value, host: &mut Host) -> Result<Value, String> {
    let cap = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 300, 1000) as usize;
    let mut total = 0;
    total += page_index(
        host,
        "/projects?membership=true&order_by=last_activity_at",
        cap,
        contribute_projects,
    );
    total += page_index(host, "/merge_requests?scope=all", cap, |h, page| {
        contribute_refs(h, page, "gitlab.merge_request")
    });
    total += page_index(host, "/issues?scope=all", cap, |h, page| {
        contribute_refs(h, page, "gitlab.issue")
    });
    Ok(json!({ "indexed": total }))
}

/// Page `base_path` (per_page=100) until exhausted or `cap` reached, contributing each page.
fn page_index(
    host: &mut Host,
    base_path: &str,
    cap: usize,
    contribute: impl Fn(&mut Host, &Value) -> usize,
) -> usize {
    let mut total = 0;
    let mut page = 1;
    loop {
        let sep = if base_path.contains('?') { "&" } else { "?" };
        let path = format!("{base_path}{sep}per_page=100&page={page}");
        let items = match gl_get(host, &path) {
            Ok(v) => v,
            Err(_) => break,
        };
        let len = items.as_array().map(|a| a.len()).unwrap_or(0);
        if len == 0 {
            break;
        }
        total += contribute(host, &items);
        if len < 100 || total >= cap {
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
    // Resolve a group namespace path → namespace_id.
    if let Some(namespace) = flex_str(&input, "namespace") {
        let groups = gl_get(
            host,
            &format!("/groups?search={}&per_page=20", enc(&namespace)),
        )?;
        let id = groups.as_array().and_then(|arr| {
            arr.iter().find_map(|g| {
                let full = g.get("full_path").and_then(|v| v.as_str()).unwrap_or("");
                let path = g.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if full.eq_ignore_ascii_case(&namespace) || path.eq_ignore_ascii_case(&namespace) {
                    g.get("id").cloned()
                } else {
                    None
                }
            })
        });
        match id {
            Some(id) => {
                body.insert("namespace_id".into(), id);
            }
            None => return Err(format!("group {namespace:?} not found")),
        }
    }
    gl_post(host, "/projects", &Value::Object(body))
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 100);
    let pairs = [
        ("per_page", limit.to_string()),
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
    gl_get(
        host,
        &format!(
            "/projects/{}/repository/files/{}?ref={}",
            enc(&project),
            enc(&path),
            enc(&git_ref)
        ),
    )
}

fn repo_tree(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 200, 2000);
    let recursive = input
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let pairs = [
        ("per_page", limit.to_string()),
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 200);
    let pairs = [
        ("per_page", limit.to_string()),
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 200);
    let pairs = [
        ("per_page", limit.to_string()),
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
    gl_delete(
        host,
        &format!("/projects/{}/repository/tags/{}", enc(&project), enc(&tag)),
    )?;
    Ok(json!({ "project": project, "tag_name": tag, "message": "tag deleted" }))
}

/// A tag name from `tag_name`/`tag`/`name` aliases.
fn tag_name(input: &Value) -> Result<String, String> {
    flex_str(input, "tag_name")
        .or_else(|| flex_str(input, "tag"))
        .or_else(|| flex_str(input, "name"))
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
    gl_delete(host, &format!("/snippets/{id}"))?;
    Ok(json!({ "snippet_id": id, "message": "snippet deleted" }))
}

// ---------------------------------------------------------------------------
// Search.
// ---------------------------------------------------------------------------

fn search_blobs(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = flex_str(&input, "query").ok_or("`query` (string) required")?;
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 100);
    let project = flex_str(&input, "project");
    let group = flex_str(&input, "group");
    let git_ref = flex_str(&input, "ref").unwrap_or_default();
    let scope = format!("?scope=blobs&search={}&per_page={limit}", enc(&query));
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
    gl_get(host, &path)
}

// ---------------------------------------------------------------------------
// Review: changes / diff lines / compare / discussions.
// ---------------------------------------------------------------------------

fn mr_changes(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let max_files = clamp(flex_i64(&input, &["max_files"]).unwrap_or(0), 50, 200);
    let max_diff_bytes = clamp(
        flex_i64(&input, &["max_diff_bytes"]).unwrap_or(0),
        16384,
        262144,
    ) as usize;
    // Diffs (unique `/diffs` substring) before the MR detail (for diff_refs).
    let diffs = gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/diffs?per_page={max_files}",
            enc(&project)
        ),
    )?;
    let detail = gl_get(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
    )?;
    let diff_refs = detail.get("diff_refs").cloned().unwrap_or(Value::Null);
    let file_filter = flex_str(&input, "file");
    let mut files = Vec::new();
    if let Some(arr) = diffs.as_array() {
        for f in arr {
            if let Some(ff) = &file_filter {
                let np = f.get("new_path").and_then(|v| v.as_str()).unwrap_or("");
                let op = f.get("old_path").and_then(|v| v.as_str()).unwrap_or("");
                if np != ff && op != ff {
                    continue;
                }
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
    }
    let count = files.len();
    Ok(
        json!({ "project": project, "iid": iid, "diff_refs": diff_refs, "files": files, "count": count }),
    )
}

fn mr_diff_lines(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let file = flex_str(&input, "file").ok_or("`file` (string) required")?;
    let diffs = gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/diffs?per_page=200",
            enc(&project)
        ),
    )?;
    let fd = find_file_diff(&diffs, &file)
        .ok_or_else(|| format!("file {file:?} is not part of this merge request"))?;
    let parsed = parse_unified_diff(fd.get("diff").and_then(|v| v.as_str()).unwrap_or(""));
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 200, 2000) as usize;
    let mut lines = Vec::new();
    let mut truncated = false;
    if let Some(target) = flex_i64(&input, &["line"]) {
        let ctx = flex_i64(&input, &["context"]).unwrap_or(3).max(0) as usize;
        match parsed
            .iter()
            .position(|l| l.new_line == target && l.kind != "deleted")
        {
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
                return Ok(json!({
                    "project": project, "iid": iid, "file": file, "lines": [], "count": 0,
                    "hint": format!("new-file line {target} is not part of this file's diff")
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
    let commits = result.get("commits").cloned().unwrap_or(json!([]));
    let commit_count = commits.as_array().map(|a| a.len()).unwrap_or(0);
    let mut files = Vec::new();
    let mut truncated = false;
    if let Some(arr) = result.get("diffs").and_then(|v| v.as_array()) {
        for f in arr {
            if files.len() >= max_files {
                truncated = true;
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
    }
    let file_count = files.len();
    Ok(json!({
        "project": project, "from": from, "to": to,
        "web_url": result.get("web_url"),
        "commits": commits, "commit_count": commit_count,
        "files": files, "file_count": file_count, "truncated": truncated
    }))
}

fn mr_discussion_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 50, 200);
    gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions?per_page={limit}",
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
        let diffs = gl_get(
            host,
            &format!(
                "/projects/{}/merge_requests/{iid}/diffs?per_page=200",
                enc(&project)
            ),
        )?;
        let fd = find_file_diff(&diffs, &path)
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 50, 200);
    let mut path = format!(
        "/projects/{}/pipelines/{id}/jobs?per_page={limit}",
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 200);
    let pairs = [
        ("per_page", limit.to_string()),
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 200);
    let pairs = [
        ("per_page", limit.to_string()),
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
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 200);
    gl_get(
        host,
        &format!("/projects/{}/releases?per_page={limit}", enc(&project)),
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
    let tag = tag_name(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
    )
}

fn release_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
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
    let tag = tag_name(&input)?;
    gl_delete(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
    )?;
    Ok(json!({ "project": project, "tag_name": tag, "message": "release deleted" }))
}

fn release_link_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 20, 200);
    gl_get(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links?per_page={limit}",
            enc(&project),
            enc(&tag)
        ),
    )
}

fn release_link_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
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
    let tag = tag_name(&input)?;
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
    let tag = tag_name(&input)?;
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
fn find_file_diff<'a>(diffs: &'a Value, file: &str) -> Option<&'a Value> {
    diffs.as_array()?.iter().find(|f| {
        f.get("new_path").and_then(|v| v.as_str()) == Some(file)
            || f.get("old_path").and_then(|v| v.as_str()) == Some(file)
    })
}

/// Truncate `s` to at most `max` bytes on a char boundary, appending a marker; `None` if it fits.
fn cap_bytes(s: &str, max: usize) -> Option<String> {
    if max == 0 || s.len() <= max {
        return None;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}\n[diff truncated]", &s[..end]))
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> MockHost {
        MockHost::default()
            .with_endpoint("gitlab.endpoint", "https://gl.example.com")
            .with_secret("personal_token", "tok")
    }

    fn run(op: &str, input: Value, host: &mut MockHost) -> Value {
        manifest_builder().build().call(op, input, host).unwrap()
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
            json!({ "project": "group/app", "state": "opened" }),
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
        let mut host = MockHost::default()
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
            json!({ "project": "group/app" }),
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
        // A missing key is rejected before any HTTP call.
        let bad_key = manifest_builder()
            .build()
            .call(
                "gitlab.pipeline.create",
                json!({ "project": "group/app", "ref": "main", "variables": [{ "value": "v" }] }),
                &mut host,
            )
            .unwrap_err();
        assert!(bad_key.contains("key is required"), "got: {bad_key}");
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
            bad_type.contains("invalid variable_type"),
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
        let mut host = base().with_http("/repository/archive.tar.gz", json!("ARCHIVE-BYTES"));
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
        assert_eq!(m.operations.len(), 64);
        assert_eq!(m.auth[0].purpose, "personal_token");
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
}
