//! Plugin manifest and operation-to-handler catalog.

use super::*;

/// Mark an op's secret-like fields for host-side masking (GL-031 / D-93): their values are redacted
/// wherever flux echoes this op's input or result — the `flux plugin call` dry-run input preview,
/// the live result echo, and the stringified tool result the model sees. Used for the CI/pipeline
/// variable `value` fields, which are matched by name at any depth (so a pipeline's
/// `variables[].value` array is masked element-wise too).
pub(super) fn redacting(mut op: OperationSpec, fields: &[&str]) -> OperationSpec {
    op.redact_fields = fields.iter().map(|s| s.to_string()).collect();
    op
}

pub(super) fn manifest_builder() -> PluginBuilder {
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
        .operation_flexible(
            read_op_typed::<ProjectListInput>(
                "gitlab.project.list",
                "List/search projects you are a MEMBER of by default (membership=true); pass membership=false to widen to every project the token can see.",
            ),
            project_list,
        )
        .operation_flexible(
            read_op_typed::<ProjectShowInput>(
                "gitlab.project.show",
                "Show one project by id or path.",
            ),
            project_show,
        )
        .operation_flexible(
            read_op_typed::<MrListInput>(
                "gitlab.mr.list",
                "List a project's merge requests (state: opened|closed|locked|merged|all). Defaults to state=opened — pass state=all to include closed/merged (index.build indexes all states).",
            ),
            mr_list,
        )
        .operation_flexible(
            read_op_typed::<MrShowInput>(
                "gitlab.mr.show",
                "Show one merge request by ref (PROJECT!IID) or project + iid.",
            ),
            mr_show,
        )
        .operation_flexible(
            read_op_typed::<IssueListInput>(
                "gitlab.issue.list",
                "List a project's issues (state: opened|closed|all). Defaults to state=opened — pass state=all to include closed issues (index.build indexes all states).",
            ),
            issue_list,
        )
        .operation_flexible(
            read_op_typed::<PipelineListInput>(
                "gitlab.pipeline.list",
                "List a project's recent CI pipelines.",
            ),
            pipeline_list,
        )
        // ---- auth test + index ----
        .operation_flexible(
            read_op_typed::<TestInput>(
                "gitlab.test",
                "Test GitLab authentication by fetching the current user.",
            ),
            auth_test,
        )
        .operation_flexible(
            read_op_typed::<IndexBuildInput>(
                "gitlab.index.build",
                "Build GitLab index records across projects, merge requests, and issues.",
            ),
            index_build,
        )
        // ---- project create / delete ----
        .operation_flexible(
            write_op_typed::<ProjectCreateInput>(
                "gitlab.project.create",
                "Create a project, optionally inside a group namespace (resolved by path).",
            ),
            project_create,
        )
        .operation_flexible(
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
        .operation_flexible(
            write_op_typed::<MrCreateInput>(
                "gitlab.mr.create",
                "Create a GitLab merge request.",
            ),
            mr_create,
        )
        .operation_flexible(
            write_op_typed::<MrUpdateInput>(
                "gitlab.mr.update",
                "Update merge request fields (title, description, target branch, labels) or close/reopen via state_event.",
            ),
            mr_update,
        )
        .operation_flexible(
            write_op_typed::<MrApproveInput>(
                "gitlab.mr.approve",
                "Approve a GitLab merge request.",
            ),
            mr_approve,
        )
        .operation_flexible(
            write_op_typed::<MrMergeInput>(
                "gitlab.mr.merge",
                "Merge a GitLab merge request.",
            ),
            mr_merge,
        )
        // ---- issues ----
        .operation_flexible(
            read_op_typed::<IssueShowInput>(
                "gitlab.issue.show",
                "Show one GitLab issue, including its Markdown description.",
            ),
            issue_show,
        )
        .operation_flexible(
            write_op_typed::<IssueCreateInput>(
                "gitlab.issue.create",
                "Create a GitLab issue. Description is GitLab-flavored Markdown.",
            ),
            issue_create,
        )
        .operation_flexible(
            write_op_typed::<IssueUpdateInput>(
                "gitlab.issue.update",
                "Update a GitLab issue (title/description/labels/assignees) or transition it via state_event.",
            ),
            issue_update,
        )
        .operation_flexible(
            read_op_typed::<IssueNoteListInput>(
                "gitlab.issue.note.list",
                "List comments (notes) on a GitLab issue. Bodies are Markdown.",
            ),
            issue_note_list,
        )
        .operation_flexible(
            write_op_typed::<IssueNoteCreateInput>(
                "gitlab.issue.note.create",
                "Add a comment (note) to a GitLab issue. Body is Markdown.",
            ),
            issue_note_create,
        )
        // ---- branches ----
        .operation_flexible(
            write_op_typed::<BranchCreateInput>(
                "gitlab.branch.create",
                "Create a GitLab repository branch.",
            ),
            branch_create,
        )
        .operation_flexible(
            risked(
                write_op_typed::<BranchDeleteInput>(
                    "gitlab.branch.delete",
                    "Delete a GitLab repository branch. Pass confirm_branch equal to the branch to guard against mistakes.",
                ),
                Risk::High,
            ),
            branch_delete,
        )
        .operation_flexible(
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
        .operation_flexible(
            write_op_typed::<RepositoryFileCreateInput>(
                "gitlab.repository.file.create",
                "Create a file in a GitLab repository.",
            ),
            repo_file_create,
        )
        .operation_flexible(
            write_op_typed::<RepositoryFileUpdateInput>(
                "gitlab.repository.file.update",
                "Update a file in a GitLab repository.",
            ),
            repo_file_update,
        )
        .operation_flexible(
            risked(
                write_op_typed::<RepositoryFileDeleteInput>(
                    "gitlab.repository.file.delete",
                    "Delete a file from a GitLab repository (destructive). Pass confirm_file_path equal to file_path to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            repo_file_delete,
        )
        .operation_flexible(
            read_op_typed::<RepositoryFileShowInput>(
                "gitlab.repository.file.show",
                "Read a file's content at a ref (default branch when omitted).",
            ),
            repo_file_show,
        )
        .operation_flexible(
            read_op_typed::<RepositoryTreeInput>(
                "gitlab.repository.tree",
                "List a repository tree at a ref (optionally recursive).",
            ),
            repo_tree,
        )
        // ---- commits ----
        .operation_flexible(
            write_op_typed::<RepositoryCommitCreateInput>(
                "gitlab.repository.commit.create",
                "Create a GitLab commit with one or more file actions.",
            ),
            commit_create,
        )
        .operation_flexible(
            read_op_typed::<RepositoryCommitListInput>(
                "gitlab.repository.commit.list",
                "List a ref's commit history, newest first; filter by path, author, or a since/until window.",
            ),
            commit_list,
        )
        // ---- tags ----
        .operation_flexible(
            write_op_typed::<RepositoryTagCreateInput>(
                "gitlab.repository.tag.create",
                "Create a GitLab repository tag.",
            ),
            tag_create,
        )
        .operation_flexible(
            read_op_typed::<RepositoryTagListInput>(
                "gitlab.repository.tag.list",
                "List a project's git tags with their target commits, newest first.",
            ),
            tag_list,
        )
        .operation_flexible(
            read_op_typed::<RepositoryTagShowInput>(
                "gitlab.repository.tag.show",
                "Show one git tag with its target commit and any annotation message.",
            ),
            tag_show,
        )
        .operation_flexible(
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
        .operation_flexible(
            write_op_typed::<SnippetCreateInput>(
                "gitlab.snippet.create",
                "Create a personal GitLab snippet.",
            ),
            snippet_create,
        )
        .operation_flexible(
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
        .operation_flexible(
            read_op_typed::<SearchBlobsInput>(
                "gitlab.search.blobs",
                "Search file contents (GitLab scope=blobs) in ONE scope: a project (supports ref), a group (no ref), or — with neither — the whole instance, which requires GitLab advanced/exact code search (Elasticsearch/Zoekt) and fails on instances without it.",
            ),
            search_blobs,
        )
        // ---- review / diff ----
        .operation_flexible(
            read_op_typed::<MrChangesInput>(
                "gitlab.mr.changes",
                "List a merge request's changed files with bounded unified diffs, plus the base/start/head diff refs.",
            ),
            mr_changes,
        )
        .operation_flexible(
            read_op_typed::<MrDiffLinesInput>(
                "gitlab.mr.diff.lines",
                "Parse one changed file's diff into typed lines (added/deleted/context with old/new line numbers).",
            ),
            mr_diff_lines,
        )
        .operation_flexible(
            read_op_typed::<CompareInput>(
                "gitlab.compare",
                "Compare two refs (branches, tags, or commits): commits between them and bounded file diffs.",
            ),
            compare,
        )
        .operation_flexible(
            read_op_typed::<MrDiscussionListInput>(
                "gitlab.mr.discussion.list",
                "List a merge request's discussion threads with resolution state and inline line positions.",
            ),
            mr_discussion_list,
        )
        .operation_flexible(
            write_op_typed::<MrNoteCreateInput>(
                "gitlab.mr.note.create",
                "Post a top-level merge request note.",
            ),
            mr_note_create,
        )
        .operation_flexible(
            write_op_typed::<MrDiscussionCreateInput>(
                "gitlab.mr.discussion.create",
                "Open a merge request discussion, optionally anchored to a diff line (path + new_line/old_line). dry_run=true is a SERVER-SIDE preview: it resolves the line anchor via the GitLab API and returns the would-be position without posting (the CLI's --dry-run flag, by contrast, only validates the input locally).",
            ),
            mr_discussion_create,
        )
        .operation_flexible(
            write_op_typed::<MrDiscussionReplyInput>(
                "gitlab.mr.discussion.reply",
                "Reply into an existing merge request discussion thread.",
            ),
            mr_discussion_reply,
        )
        .operation_flexible(
            write_op_typed::<MrDiscussionResolveInput>(
                "gitlab.mr.discussion.resolve",
                "Resolve (or unresolve with resolved=false) a merge request discussion thread.",
            ),
            mr_discussion_resolve,
        )
        // ---- CI/CD ----
        .operation_flexible(
            redacting(
                write_op_typed::<CiVariableCreateInput>(
                    "gitlab.ci.variable.create",
                    "Create a GitLab project CI/CD variable.",
                ),
                &["value"],
            ),
            ci_variable_create,
        )
        .operation_flexible(
            redacting(
                write_op_typed::<CiVariableUpdateInput>(
                    "gitlab.ci.variable.update",
                    "Update a GitLab project CI/CD variable.",
                ),
                &["value"],
            ),
            ci_variable_update,
        )
        .operation_flexible(
            risked(
                write_op_typed::<CiVariableDeleteInput>(
                    "gitlab.ci.variable.delete",
                    "Delete a GitLab project CI/CD variable (destructive). Pass confirm_key equal to key to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            ci_variable_delete,
        )
        .operation_flexible(
            redacting(
                write_op_typed::<PipelineCreateInput>(
                    "gitlab.pipeline.create",
                    "Create a GitLab CI pipeline.",
                ),
                &["value"],
            ),
            pipeline_create,
        )
        .operation_flexible(
            write_op_typed::<PipelineRetryInput>(
                "gitlab.pipeline.retry",
                "Retry a GitLab CI pipeline.",
            ),
            pipeline_retry,
        )
        .operation_flexible(
            write_op_typed::<PipelineCancelInput>(
                "gitlab.pipeline.cancel",
                "Cancel a GitLab CI pipeline.",
            ),
            pipeline_cancel,
        )
        .operation_flexible(
            read_op_typed::<JobListInput>(
                "gitlab.job.list",
                "List one pipeline's jobs with stage, status, duration, and failure_reason.",
            ),
            job_list,
        )
        .operation_flexible(
            read_op_typed::<EnvironmentListInput>(
                "gitlab.environment.list",
                "List a project's environments with state, tier, external URL, and last deployment.",
            ),
            environment_list,
        )
        .operation_flexible(
            read_op_typed::<DeploymentListInput>(
                "gitlab.deployment.list",
                "List a project's deployments, newest first, filterable by environment and status.",
            ),
            deployment_list,
        )
        // ---- releases ----
        .operation_flexible(
            read_op_typed::<ReleaseListInput>(
                "gitlab.release.list",
                "List a project's releases, newest first.",
            ),
            release_list,
        )
        .operation_flexible(
            write_op_typed::<ReleaseCreateInput>(
                "gitlab.release.create",
                "Create a GitLab release for a tag, cutting the tag from ref when it does not yet exist.",
            ),
            release_create,
        )
        .operation_flexible(
            read_op_typed::<ReleaseShowInput>(
                "gitlab.release.show",
                "Show one GitLab release with its description, milestones, and asset links.",
            ),
            release_show,
        )
        .operation_flexible(
            write_op_typed::<ReleaseUpdateInput>(
                "gitlab.release.update",
                "Update a GitLab release's title, notes, milestones, or release date.",
            ),
            release_update,
        )
        .operation_flexible(
            risked(
                write_op_typed::<ReleaseDeleteInput>(
                    "gitlab.release.delete",
                    "Delete a GitLab release (destructive; the underlying git tag is left in place). Pass confirm_tag_name equal to the tag to guard against mistakes.",
                ),
                Risk::Destructive,
            ),
            release_delete,
        )
        .operation_flexible(
            read_op_typed::<ReleaseLinkListInput>(
                "gitlab.release.link.list",
                "List the asset links attached to a release.",
            ),
            release_link_list,
        )
        .operation_flexible(
            write_op_typed::<ReleaseLinkCreateInput>(
                "gitlab.release.link.create",
                "Attach a new asset link (a download or related URL) to a release.",
            ),
            release_link_create,
        )
        .operation_flexible(
            write_op_typed::<ReleaseLinkUpdateInput>(
                "gitlab.release.link.update",
                "Edit an existing release asset link.",
            ),
            release_link_update,
        )
        .operation_flexible(
            write_op_typed::<ReleaseLinkDeleteInput>(
                "gitlab.release.link.delete",
                "Remove an asset link from a release.",
            ),
            release_link_delete,
        )
        // ---- changelog ----
        .operation_flexible(
            read_op_typed::<RepositoryChangelogGenerateInput>(
                "gitlab.repository.changelog.generate",
                "Generate Markdown release notes from the commits between two refs without committing.",
            ),
            changelog_generate,
        )
        .operation_flexible(
            write_op_typed::<RepositoryChangelogAddInput>(
                "gitlab.repository.changelog.add",
                "Generate a changelog section and commit it into the repository's changelog file (default CHANGELOG.md).",
            ),
            changelog_add,
        )
        // ---- archive (blob) ----
        .operation_flexible(
            read_op_typed::<RepositoryArchiveInput>(
                "gitlab.repository.archive",
                "Download a repository archive (tar.gz/zip/tar) at a ref into the host blob store. Refuses archives over max_bytes (default 50 MiB) — raise it explicitly for bigger repos.",
            ),
            repository_archive,
        )
        // ---- CI/CD job-token scope (inbound token access allowlist) ----
        .operation_flexible(
            read_op_typed::<CiJobTokenScopeShowInput>(
                "gitlab.ci.job_token.scope.show",
                "Show a project's CI/CD job-token inbound/outbound access scope settings.",
            ),
            ci_job_token_scope_show,
        )
        .operation_flexible(
            write_op_typed::<CiJobTokenScopeSetInput>(
                "gitlab.ci.job_token.scope.set",
                "Enable/disable a project's inbound CI/CD job-token access enforcement (enabled=true restricts to the allowlist).",
            ),
            ci_job_token_scope_set,
        )
        .operation_flexible(
            read_op_typed::<CiJobTokenAllowlistListInput>(
                "gitlab.ci.job_token.allowlist.list",
                "List the projects allowed to use their CI_JOB_TOKEN to access this project.",
            ),
            ci_job_token_allowlist_list,
        )
        .operation_flexible(
            write_op_typed::<CiJobTokenAllowlistAddInput>(
                "gitlab.ci.job_token.allowlist.add",
                "Add a project (by numeric id) to this project's CI job-token allowlist, letting its CI clone/access this project via CI_JOB_TOKEN.",
            ),
            ci_job_token_allowlist_add,
        )
        .operation_flexible(
            risked(
                write_op_typed::<CiJobTokenAllowlistRemoveInput>(
                    "gitlab.ci.job_token.allowlist.remove",
                    "Remove a project from this project's CI job-token allowlist (may break that project's CI access). Pass confirm_target_project_id to guard against mistakes.",
                ),
                Risk::High,
            ),
            ci_job_token_allowlist_remove,
        )
        .operation_flexible(
            read_op_typed::<CiJobTokenGroupsAllowlistListInput>(
                "gitlab.ci.job_token.groups_allowlist.list",
                "List the groups allowed to use their CI_JOB_TOKEN to access this project.",
            ),
            ci_job_token_groups_allowlist_list,
        )
        .operation_flexible(
            write_op_typed::<CiJobTokenGroupsAllowlistAddInput>(
                "gitlab.ci.job_token.groups_allowlist.add",
                "Add a group (by numeric id) to this project's CI job-token groups allowlist.",
            ),
            ci_job_token_groups_allowlist_add,
        )
        .operation_flexible(
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
        .operation_flexible(
            read_op_typed::<RepositoryProtectedTagListInput>(
                "gitlab.repository.protected_tag.list",
                "List a project's protected tags with their create-access levels.",
            ),
            protected_tag_list,
        )
        .operation_flexible(
            read_op_typed::<RepositoryProtectedTagShowInput>(
                "gitlab.repository.protected_tag.show",
                "Show one protected tag (by name or wildcard pattern).",
            ),
            protected_tag_show,
        )
        .operation_flexible(
            write_op_typed::<RepositoryProtectedTagProtectInput>(
                "gitlab.repository.protected_tag.protect",
                "Protect a tag or wildcard pattern (create_access_level defaults to 40 = maintainer).",
            ),
            protected_tag_protect,
        )
        .operation_flexible(
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
        .operation_flexible(
            read_op_typed::<DeployTokenListInput>(
                "gitlab.deploy_token.list",
                "List a project's deploy tokens (metadata only; the secret is never returned by list).",
            ),
            deploy_token_list,
        )
        .operation_flexible(
            risked(
                write_op_typed::<DeployTokenCreateInput>(
                    "gitlab.deploy_token.create",
                    "Create a project deploy token (scopes e.g. read_repository, read_registry). The response `token` is a secret returned ONCE — store it now.",
                ),
                Risk::High,
            ),
            deploy_token_create,
        )
        .operation_flexible(
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

pub(super) fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}
