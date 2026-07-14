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

mod client;
mod manifest;
mod operations;
mod schema;

use client::*;
use manifest::manifest_builder;
use operations::*;
use schema::*;

fn main() -> Result<(), String> {
    manifest_builder().try_serve()
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
            problems.iter().any(|p| p.contains("state")
                && (p.contains("must be one of") || p.contains("expected one of"))),
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

    /// C-74: a migrated closed handler rejects unknown fields during typed decoding. Flexible
    /// families remain advisory until their own bounded migration defines every compatibility
    /// alias explicitly.
    #[test]
    fn typed_handlers_reject_unknown_fields() {
        let (valid, problems, warnings) = validate(
            "gitlab.issue.list",
            json!({ "project": "g/a", "stat": "opened" }),
        );
        assert!(!valid);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("stat") && problem.contains("unknown field")),
            "{problems:?}"
        );
        assert!(
            warnings.is_empty(),
            "typed decode is an error, not a warning"
        );

        let (valid, problems, warnings) = validate(
            "gitlab.pipeline.list",
            json!({ "project": "g/a", "stat": "success" }),
        );
        assert!(valid, "{problems:?}");
        assert!(warnings.iter().any(|warning| warning.contains("`stat`")));
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

    #[test]
    fn typed_read_results_preserve_vendor_extensions_and_nulls() {
        let cases = [
            (
                "gitlab.project.list",
                json!({}),
                "/projects?membership=true",
                json!([{ "id": 1, "description": null, "vendor_project": { "tier": 7 } }]),
            ),
            (
                "gitlab.project.show",
                json!({ "project": "group/app" }),
                "/projects/group%2Fapp",
                json!({ "id": 1, "description": null, "vendor_project": [1, 2] }),
            ),
            (
                "gitlab.mr.list",
                json!({ "project": "group/app" }),
                "/projects/group%2Fapp/merge_requests",
                json!([{ "iid": 7, "merged_at": null, "vendor_mr": { "mergeability": "checking" } }]),
            ),
            (
                "gitlab.mr.show",
                json!({ "ref": "group/app!7" }),
                "/projects/group%2Fapp/merge_requests/7",
                json!({ "iid": 7, "merged_at": null, "vendor_mr": ["new-field"] }),
            ),
            (
                "gitlab.issue.list",
                json!({ "project": "group/app" }),
                "/projects/group%2Fapp/issues",
                json!([{ "iid": 3, "closed_at": null, "vendor_issue": { "severity": "S2" } }]),
            ),
            (
                "gitlab.issue.show",
                json!({ "ref": "group/app#3" }),
                "/projects/group%2Fapp/issues/3",
                json!({ "iid": 3, "closed_at": null, "vendor_issue": [true] }),
            ),
        ];

        for (operation, input, request, vendor_result) in cases {
            let mut host = base().with_http(request, vendor_result.clone());
            let output = run(operation, input, &mut host);
            assert_eq!(output, vendor_result, "{operation} changed GitLab's result");
        }
    }

    #[test]
    fn typed_read_aliases_normalize_before_preflight_and_execution() {
        let mut host = base().with_http(
            "/projects/group%2Fapp/merge_requests/7",
            json!({ "iid": 7 }),
        );
        let output = run("gitlab.mr.show", json!({ "id": "group/app!7" }), &mut host);
        assert_eq!(output["iid"], 7);

        let mut host = base().with_http("/projects/group%2Fapp/issues/3", json!({ "iid": 3 }));
        let output = run(
            "gitlab.issue.show",
            json!({ "path": "group/app", "issue_iid": 3 }),
            &mut host,
        );
        assert_eq!(output["iid"], 3);
    }

    #[test]
    fn typed_read_rejects_a_vendor_result_with_the_wrong_top_level_shape() {
        let mut host = base().with_http(
            "/projects?membership=true",
            json!({ "projects": [{ "id": 1 }] }),
        );
        let error = manifest_builder()
            .build()
            .call("gitlab.project.list", json!({}), &mut host)
            .unwrap_err();
        assert!(error.contains("gitlab.project.list"), "{error}");
        assert!(error.contains("expected a sequence"), "{error}");
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

    /// C-74 keystone contract: these bounded read families publish successful-result schemas.
    /// Before their executable typed migration every operation used `operation_flexible`, leaving
    /// `output_schema` absent even though flux relies on stable fields in each vendor object.
    #[test]
    fn bounded_read_families_publish_output_schemas() {
        let manifest = manifest_builder().build().manifest();
        for name in [
            "gitlab.project.list",
            "gitlab.project.show",
            "gitlab.mr.list",
            "gitlab.mr.show",
            "gitlab.issue.list",
            "gitlab.issue.show",
        ] {
            let operation = manifest
                .operations
                .iter()
                .find(|operation| operation.name == name)
                .unwrap_or_else(|| panic!("missing operation {name}"));
            assert!(
                operation.output_schema.is_some(),
                "{name} must derive an output schema from its executable type"
            );
        }

        let output = |name: &str| {
            manifest
                .operations
                .iter()
                .find(|operation| operation.name == name)
                .and_then(|operation| operation.output_schema.as_ref())
                .unwrap_or_else(|| panic!("missing output schema for {name}"))
        };
        let project_list = output("gitlab.project.list");
        assert_eq!(project_list["type"], "array");
        let project_item = &project_list["$defs"]["GitLabProjectSchema"];
        assert_eq!(project_item["type"], "object");
        assert!(project_item["properties"]["path_with_namespace"].is_object());
        assert_eq!(project_item["additionalProperties"], true);

        let mr_show = output("gitlab.mr.show");
        let mr = &mr_show["$defs"]["GitLabMergeRequestSchema"];
        assert_eq!(mr["type"], "object");
        assert!(mr["properties"]["iid"].is_object());
        assert!(mr["properties"]["source_branch"].is_object());
        assert_eq!(mr["additionalProperties"], true);

        let issue_show = output("gitlab.issue.show");
        let issue = &issue_show["$defs"]["GitLabIssueSchema"];
        assert_eq!(issue["type"], "object");
        assert!(issue["properties"]["title"].is_object());
        assert_eq!(issue["additionalProperties"], true);
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
