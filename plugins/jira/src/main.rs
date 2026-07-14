//! `jira` — a flux integration plugin for the Atlassian Jira Cloud REST API (v3): full issue lifecycle
//! (create/edit/delete/search/show + create/edit metadata), transitions, comments, attachments, issue
//! links, user search, an auth self-test, and an index builder. The path prefix is `/rest/api/3`.
//!
//! ## Auth — two modes, selected at request time (ported from fluxplane `client.go`)
//!
//! The plugin never builds an `Authorization` header itself — the host injects it per the declared
//! [`AuthScheme`]. Two auth methods are declared and the *mode is chosen per request* from gated
//! non-secret config reads ([`Host::config`], see [`auth_mode`]); every request addresses its base
//! by **named endpoint reference** — the plugin never holds a URL (D-32):
//!
//! - **Primary (reference): Bearer + cloud_id gateway.** When a `cloud_id` is configured
//!   (`ATLASSIAN_CLOUD_ID` / `JIRA_CLOUD_ID`), requests address the `jira.gateway` endpoint — a
//!   host-composed template `https://api.atlassian.com/ex/jira/{cloud_id}` — with the `api_token`
//!   purpose → `Authorization: Bearer <api_token>` ([`AuthMethod::bearer`]). This matches
//!   fluxplane, whose `Kind: bearer_token` always sends Bearer and switches base URL on `cloud_id`.
//! - **Fallback: Basic (email:token) against the site URL.** For setups without a cloud_id/OAuth
//!   gateway: when no cloud_id is configured but an `email` IS (`JIRA_EMAIL` / `ATLASSIAN_EMAIL`),
//!   requests address the site endpoint ref (`jira.endpoint`) with the `basic` purpose →
//!   `Authorization: Basic base64(email:token)` ([`AuthMethod::basic`]). This is flux's original
//!   direct-Basic path, kept (user-confirmed) for installs that never connected via OAuth.
//! - **Else:** Bearer against the site endpoint ref (`api_token` purpose, no cloud_id) —
//!   fluxplane's endpoint-ref Bearer path.
//!
//! `site_url` is used only for human browse links (not currently emitted). There is NO hand-rolled
//! base64 anywhere — the host injects both Bearer and Basic.
//!
//! `jira.issue.search`, `jira.user.search`, and `jira.index.build` contribute datasource records
//! (`jira.issue` / `jira.user`) so the agent can search them. Attachments move bytes through the host's
//! content-addressed blob store using the byte-exact `http_bytes_ref` path so binary files round-trip
//! exactly (no `from_utf8_lossy`). Markdown bodies are converted to faithful Atlassian Document Format.

use base64::Engine as _;
use host_kit::*;
use serde_json::{json, Map, Value};

/// The issue fields requested on issue reads (so status/links/attachments/etc. are present).
const FIELDS: &str = "summary,description,status,assignee,reporter,creator,updated,created,project,issuetype,priority,labels,parent,issuelinks,attachment";

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

    fn host() -> MockHost {
        // ALL IO — JSON and byte-exact alike — resolves the named site ref host-side
        // (`with_endpoint_ref`); the plugin never sees the base URL.
        MockHost::default().with_endpoint_ref("jira.endpoint", "https://x.atlassian.net")
    }

    #[test]
    fn auth_test_fetches_current_user() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/myself",
            json!({"accountId": "acc-1", "displayName": "Bot"}),
        );
        let out = plugin.call("jira.test", json!({}), &mut host).unwrap();
        assert_eq!(out["status"], "ok");
        assert_eq!(out["user"]["accountId"], "acc-1");
    }

    #[test]
    fn cloud_id_routes_through_the_oauth_gateway() {
        // With a cloud_id configured, requests address the `jira.gateway` ref, which the HOST
        // composes from the cloud_id (template) — here mocked as the resolved gateway base.
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_config("cloud_id", "cloud-123")
            .with_endpoint_ref(
                "jira.gateway",
                "https://api.atlassian.com/ex/jira/cloud-123",
            )
            .with_http(
                "https://api.atlassian.com/ex/jira/cloud-123/rest/api/3/myself",
                json!({"accountId": "acc-1"}),
            );
        let out = plugin.call("jira.test", json!({}), &mut host).unwrap();
        assert_eq!(out["user"]["accountId"], "acc-1");
    }

    #[test]
    fn gateway_mode_addresses_the_gateway_by_ref_not_a_held_url() {
        // Gateway mode must put NO URL on the op surface: the plugin passes only the `jira.gateway`
        // ref. Proof in two halves: (1) with the ref unresolvable the call fails naming the ref —
        // a plugin-held gateway URL would have succeeded via url-based IO; (2) resolving the ref to
        // an arbitrary host-side base routes the request there, so the base comes from the host,
        // not from a plugin-composed `https://api.atlassian.com/...` string.
        let plugin = manifest_builder().build();
        let mut unresolvable = MockHost::default().with_config("cloud_id", "cloud-123");
        let err = plugin
            .call("jira.test", json!({}), &mut unresolvable)
            .unwrap_err();
        assert!(
            err.contains("jira.gateway"),
            "error should name the ref: {err}"
        );

        let mut host = MockHost::default()
            .with_config("cloud_id", "cloud-123")
            .with_endpoint_ref(
                "jira.gateway",
                "https://host-composed.example/ex/jira/cloud-123",
            )
            .with_http(
                "https://host-composed.example/ex/jira/cloud-123/rest/api/3/myself",
                json!({"accountId": "acc-1"}),
            );
        let out = plugin.call("jira.test", json!({}), &mut host).unwrap();
        assert_eq!(out["user"]["accountId"], "acc-1");
    }

    #[test]
    fn email_without_cloud_id_selects_basic_against_the_site_ref() {
        // Basic fallback: an email config (and no cloud_id) keeps requests on the `jira.endpoint`
        // site ref — the gateway ref is intentionally NOT resolvable here, so routing through it
        // would fail the call.
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_config("email", "dev@example.com")
            .with_http("/rest/api/3/myself", json!({"accountId": "acc-1"}));
        let out = plugin.call("jira.test", json!({}), &mut host).unwrap();
        assert_eq!(out["user"]["accountId"], "acc-1");
    }

    #[test]
    fn index_build_indexes_issues_and_users() {
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_http(
                "/rest/api/3/search/jql",
                json!({"issues": [{"key": "PROJ-1", "fields": {"summary": "Idx", "status": {"name": "Open"}}}]}),
            )
            .with_http(
                "/rest/api/3/user/search",
                json!([{"accountId": "acc-1", "displayName": "Bot"}]),
            );
        let out = plugin
            .call("jira.index.build", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["indexed"], 2);
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().any(|r| r.entity == "jira.issue"));
        assert!(recs.iter().any(|r| r.entity == "jira.user"));
    }

    #[test]
    fn issue_create_posts_fields() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue",
            json!({"id": "10001", "key": "PROJ-1", "self": "https://x/issue/10001"}),
        );
        let out = plugin
            .call(
                "jira.issue.create",
                json!({"project_key": "DEV", "issue_type": "Task", "summary": "New", "description_markdown": "Hello **world**"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["key"], "PROJ-1");
        assert_eq!(out["id"], "10001");
    }

    #[test]
    fn issue_edit_puts_then_rereads() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1",
            json!({"key": "PROJ-1", "fields": {"summary": "Edited"}}),
        );
        let out = plugin
            .call(
                "jira.issue.edit",
                json!({"key": "PROJ-1", "summary": "Edited"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["issue"]["fields"]["summary"], "Edited");
    }

    #[test]
    fn issue_delete_confirms() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http("/rest/api/3/issue/PROJ-9", json!({}));
        let out = plugin
            .call("jira.issue.delete", json!({"key": "PROJ-9"}), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["key"], "PROJ-9");
    }

    #[test]
    fn issue_search_calls_the_api_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/search/jql",
            json!({"issues": [
                {"key": "PROJ-1", "fields": {"summary": "Warm transfer bug", "status": {"name": "Open"}}}
            ]}),
        );
        let out = plugin
            .call(
                "jira.issue.search",
                json!({ "jql": "project = PROJ", "max": 10 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["issues"][0]["key"], "PROJ-1");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "jira.issue");
        assert_eq!(recs[0].id, "PROJ-1");
        assert_eq!(recs[0].title, "Warm transfer bug");
        assert!(recs[0].body.contains("Open"));
    }

    #[test]
    fn issue_show_fetches_by_key() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1",
            json!({"key": "PROJ-1", "fields": {"summary": "Warm transfer bug"}}),
        );
        let out = plugin
            .call("jira.issue.show", json!({ "key": "PROJ-1" }), &mut host)
            .unwrap();
        assert_eq!(out["key"], "PROJ-1");
    }

    #[test]
    fn attachment_list_and_get_publish_generated_output_contracts() {
        let manifest = manifest_builder().build().manifest();
        let contract = |operation: &str| {
            manifest
                .operations
                .iter()
                .find(|spec| spec.name == operation)
                .unwrap_or_else(|| panic!("missing operation {operation}"))
        };
        let list = contract("jira.issue.attachment.list");
        assert_eq!(list.input_schema, op_input_schema::<AttachmentListInput>());
        assert_eq!(
            list.output_schema.as_ref(),
            Some(&op_output_schema::<AttachmentListOutput>())
        );
        let get = contract("jira.issue.attachment.get");
        assert_eq!(get.input_schema, op_input_schema::<AttachmentGetInput>());
        assert_eq!(
            get.output_schema.as_ref(),
            Some(&op_output_schema::<AttachmentGetOutput>())
        );
    }

    #[test]
    fn create_meta_returns_metadata() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/createmeta",
            json!({"projects": [{"key": "DEV"}]}),
        );
        let out = plugin
            .call(
                "jira.issue.create_meta",
                json!({"project_key": "DEV"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["metadata"]["projects"][0]["key"], "DEV");
    }

    #[test]
    fn edit_meta_returns_metadata() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/editmeta",
            json!({"fields": {"summary": {"required": true}}}),
        );
        let out = plugin
            .call("jira.issue.edit_meta", json!({"key": "PROJ-1"}), &mut host)
            .unwrap();
        assert_eq!(out["metadata"]["fields"]["summary"]["required"], true);
    }

    #[test]
    fn transition_list_returns_status_and_transitions() {
        let plugin = manifest_builder().build();
        // transitions mock FIRST so the `/transitions` URL wins the substring match.
        let mut host = host()
            .with_http(
                "/rest/api/3/issue/PROJ-1/transitions",
                json!({"transitions": [{"id": "11", "name": "Start", "to": {"name": "In Progress"}}]}),
            )
            .with_http(
                "/rest/api/3/issue/PROJ-1",
                json!({"key": "PROJ-1", "fields": {"status": {"name": "To Do"}}}),
            );
        let out = plugin
            .call(
                "jira.issue.transition.list",
                json!({"key": "PROJ-1"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["current_status"]["name"], "To Do");
        assert_eq!(out["transitions"][0]["id"], "11");
    }

    #[test]
    fn transition_run_applies_transition_by_id() {
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_http(
                "/rest/api/3/issue/PROJ-1/transitions",
                json!({"transitions": [{"id": "11", "name": "Start", "to": {"name": "In Progress"}}]}),
            )
            .with_http(
                "/rest/api/3/issue/PROJ-1",
                json!({"key": "PROJ-1", "fields": {"status": {"name": "To Do"}}}),
            );
        let out = plugin
            .call(
                "jira.issue.transition.run",
                json!({"key": "PROJ-1", "transition_id": "11"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["steps"], 1);
        assert_eq!(out["applied_transitions"][0]["id"], "11");
    }

    #[test]
    fn transition_run_target_already_reached_does_not_mutate() {
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_http(
                "/rest/api/3/issue/DONE-1/transitions",
                json!({"transitions": []}),
            )
            .with_http(
                "/rest/api/3/issue/DONE-1",
                json!({"key": "DONE-1", "fields": {"status": {"name": "Done"}}}),
            );
        let out = plugin
            .call(
                "jira.issue.transition.run",
                json!({"key": "DONE-1", "target_status": "Done"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["steps"], 0);
        assert_eq!(out["applied_transitions"].as_array().unwrap().len(), 0);
        assert_eq!(out["current_status"]["name"], "Done");
    }

    #[test]
    fn comment_add_posts_and_echoes_id() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/comment",
            json!({"id": "1001", "body": "x"}),
        );
        let out = plugin
            .call(
                "jira.issue.comment.add",
                json!({"key": "PROJ-1", "body_markdown": "Investigated."}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["comment_id"], "1001");
    }

    #[test]
    fn comment_edit_puts_and_echoes_id() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/comment/1001",
            json!({"id": "1001", "body": "y"}),
        );
        let out = plugin
            .call(
                "jira.issue.comment.edit",
                json!({"key": "PROJ-1", "comment_id": "1001", "body_markdown": "Edited."}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["comment_id"], "1001");
    }

    #[test]
    fn comment_delete_confirms() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http("/rest/api/3/issue/PROJ-1/comment/1001", json!({}));
        let out = plugin
            .call(
                "jira.issue.comment.delete",
                json!({"key": "PROJ-1", "comment_id": "1001"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["comment_id"], "1001");
    }

    #[test]
    fn comment_list_returns_page() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/comment",
            json!({"comments": [{"id": "1001", "body": "x"}], "total": 1, "startAt": 0}),
        );
        let out = plugin
            .call(
                "jira.issue.comment.list",
                json!({"key": "PROJ-1"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["comments"][0]["id"], "1001");
    }

    #[test]
    fn attachment_add_uploads_from_blob_byte_exact() {
        let plugin = manifest_builder().build();
        // Binary (non-UTF-8) bytes must round-trip exactly through the multipart body. Byte-exact
        // upload goes through the ref-based `http_bytes_ref` — the site base resolves host-side
        // from the `jira.endpoint` ref (exercised here with no cloud_id).
        let raw: Vec<u8> = vec![0, 159, 146, 150, 255, b'h', b'i'];
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/attachments",
            json!([{"id": "20001", "filename": "report.bin"}]),
        );
        host.blobs
            .borrow_mut()
            .insert("blob-1".into(), ("report.bin".into(), raw.clone()));
        let out = plugin
            .call(
                "jira.issue.attachment.add",
                json!({"key": "PROJ-1", "blob_ref": "blob-1", "filename": "report.bin"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["attachments"][0]["id"], "20001");
    }

    #[test]
    fn attachment_get_downloads_into_blob_byte_exact() {
        let plugin = manifest_builder().build();
        // Non-UTF-8 download bytes must survive into the blob store unchanged. Byte-exact download
        // goes through the ref-based `http_bytes_ref` with binary_response=true — the site base
        // resolves host-side from the `jira.endpoint` ref (exercised here with no cloud_id).
        let raw: Vec<u8> = vec![0, 159, 146, 150, 255];
        let mut host = host().with_http_bytes("/rest/api/3/attachment/content/20001", raw.clone());
        let out = plugin
            .call(
                "jira.issue.attachment.get",
                json!({"attachment_id": "20001", "filename": "report.bin"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["id"], "20001");
        assert_eq!(out["size"], raw.len());
        let blob_ref = out["blob_ref"].as_str().unwrap();
        assert!(blob_ref.starts_with("mockblob"));
        let blobs = host.blobs.borrow();
        assert_eq!(blobs.get(blob_ref).unwrap().1, raw);
    }

    #[test]
    fn attachment_list_returns_attachments() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1",
            json!({"key": "PROJ-1", "fields": {"attachment": [{"id": "20001", "filename": "r.txt"}]}}),
        );
        let out = plugin
            .call(
                "jira.issue.attachment.list",
                json!({"key": "PROJ-1"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["attachments"][0]["id"], "20001");
    }

    #[test]
    fn attachment_delete_confirms() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http("/rest/api/3/attachment/20001", json!({}));
        let out = plugin
            .call(
                "jira.issue.attachment.delete",
                json!({"attachment_id": "20001"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["attachment_id"], "20001");
    }

    #[test]
    fn issue_link_add_posts_and_reads_back() {
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_http("/rest/api/3/issueLink", json!({}))
            .with_http(
                "/rest/api/3/issue/PROJ-1",
                json!({"key": "PROJ-1", "fields": {"issuelinks": [{"id": "5", "type": {"name": "Blocks"}}]}}),
            );
        let out = plugin
            .call(
                "jira.issue.link.add",
                json!({"key": "PROJ-1", "to_key": "PROJ-2", "type": "Blocks"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["links"][0]["id"], "5");
    }

    #[test]
    fn user_search_calls_the_api_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/user/search",
            json!([{"accountId": "acc-1", "displayName": "Bot", "emailAddress": "b@c.d"}]),
        );
        let out = plugin
            .call("jira.user.search", json!({"query": "Bot"}), &mut host)
            .unwrap();
        assert_eq!(out["users"][0]["accountId"], "acc-1");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "jira.user");
        assert_eq!(recs[0].id, "acc-1");
    }

    #[test]
    fn markdown_to_adf_renders_blocks_and_inline() {
        // Heading + paragraph with bold/italic/code/link + a bullet list + a fenced code block.
        let md = "# Title\n\nSome **bold**, *em*, `code`, and a [link](https://x).\n\n- one\n- two\n\n```rust\nfn main() {}\n```";
        let doc = markdown_to_adf(md);
        assert_eq!(doc["type"], "doc");
        assert_eq!(doc["version"], 1);
        let content = doc["content"].as_array().unwrap();
        // heading
        assert_eq!(content[0]["type"], "heading");
        assert_eq!(content[0]["attrs"]["level"], 1);
        assert_eq!(content[0]["content"][0]["text"], "Title");
        // paragraph with marks
        assert_eq!(content[1]["type"], "paragraph");
        let inline = content[1]["content"].as_array().unwrap();
        assert!(inline
            .iter()
            .any(|n| n["text"] == "bold" && n["marks"][0]["type"] == "strong"));
        assert!(inline
            .iter()
            .any(|n| n["text"] == "em" && n["marks"][0]["type"] == "em"));
        let code_node = inline.iter().find(|n| n["text"] == "code").unwrap();
        assert_eq!(code_node["marks"][0]["type"], "code");
        // code mark stands alone (never combined with bold/em)
        assert_eq!(code_node["marks"].as_array().unwrap().len(), 1);
        let link_node = inline.iter().find(|n| n["text"] == "link").unwrap();
        assert_eq!(link_node["marks"][0]["type"], "link");
        assert_eq!(link_node["marks"][0]["attrs"]["href"], "https://x");
        // bullet list
        assert_eq!(content[2]["type"], "bulletList");
        assert_eq!(content[2]["content"][0]["type"], "listItem");
        assert_eq!(
            content[2]["content"][0]["content"][0]["content"][0]["text"],
            "one"
        );
        // code block with language
        assert_eq!(content[3]["type"], "codeBlock");
        assert_eq!(content[3]["attrs"]["language"], "rust");
        assert_eq!(content[3]["content"][0]["text"], "fn main() {}");
    }

    #[test]
    fn manifest_declares_ops_dual_auth_and_datasources() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.iter().filter(|o| !o.internal).count(), 21);
        // Two auth methods: primary Bearer (api_token) + Basic fallback (basic).
        assert_eq!(m.auth.len(), 2);
        let bearer = m.auth.iter().find(|a| a.purpose == "api_token").unwrap();
        assert_eq!(bearer.scheme, AuthScheme::Bearer);
        let basic = m.auth.iter().find(|a| a.purpose == "basic").unwrap();
        assert_eq!(basic.scheme, AuthScheme::Basic);
        assert!(basic.user_env.contains(&"JIRA_EMAIL".to_string()));
        // the token is a gated secret; the email is config, NOT a gated secret.
        assert!(m
            .capabilities
            .secrets
            .contains(&"JIRA_API_TOKEN".to_string()));
        assert!(!m.capabilities.secrets.contains(&"JIRA_EMAIL".to_string()));
        assert!(m.capabilities.blob);
        assert!(m.datasources.iter().any(|d| d.entity == "jira.issue"));
        assert!(m.datasources.iter().any(|d| d.entity == "jira.user"));
    }

    #[test]
    fn manifest_declares_site_and_gateway_endpoints_plus_configs() {
        let m = manifest_builder().build().manifest();
        // Two endpoints: the env-resolved site URL + the host-composed gateway template. The old
        // `jira.cloud_id` / `jira.email` pseudo-endpoints (config values abusing the endpoint
        // mechanism) are gone.
        assert_eq!(m.endpoints.len(), 2);
        let site = m
            .endpoints
            .iter()
            .find(|e| e.name == "jira.endpoint")
            .unwrap();
        assert!(site.env.contains(&"JIRA_URL".to_string()));
        assert!(site.template.is_none());
        let gateway = m
            .endpoints
            .iter()
            .find(|e| e.name == "jira.gateway")
            .unwrap();
        assert_eq!(
            gateway.template.as_deref(),
            Some("https://api.atlassian.com/ex/jira/{cloud_id}")
        );
        assert!(gateway
            .http_hosts
            .contains(&"api.atlassian.com".to_string()));
        // cloud_id + email are gated NON-SECRET config declarations (D-32), read via host.config.
        assert_eq!(m.config.len(), 2);
        let cloud_id = m.config.iter().find(|c| c.name == "cloud_id").unwrap();
        assert_eq!(cloud_id.env, vec!["ATLASSIAN_CLOUD_ID", "JIRA_CLOUD_ID"]);
        let email = m.config.iter().find(|c| c.name == "email").unwrap();
        assert_eq!(email.env, vec!["JIRA_EMAIL", "ATLASSIAN_EMAIL"]);
    }

    // ---------------------------------------------------------------------------
    // D-36 parity gap tests (failing-first on the pre-port tree)
    // ---------------------------------------------------------------------------

    #[test]
    fn issue_show_renders_description_adf_to_markdown() {
        let plugin = manifest_builder().build();
        let description = markdown_to_adf("Hello **world**");
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1",
            json!({
                "key": "PROJ-1",
                "fields": {
                    "summary": "ADF test",
                    "description": description
                }
            }),
        );
        let out = plugin
            .call("jira.issue.show", json!({"key": "PROJ-1"}), &mut host)
            .unwrap();
        assert_eq!(out["fields"]["description"], "Hello **world**");
    }

    #[test]
    fn issue_search_renders_each_issue_description() {
        let plugin = manifest_builder().build();
        let description = markdown_to_adf("A *list*:\n\n- one\n- two");
        let mut host = host().with_http(
            "/rest/api/3/search/jql",
            json!({
                "issues": [
                    {"key": "PROJ-1", "fields": {"summary": "S", "description": description}}
                ]
            }),
        );
        let out = plugin
            .call(
                "jira.issue.search",
                json!({"jql": "project = PROJ"}),
                &mut host,
            )
            .unwrap();
        let desc = out["issues"][0]["fields"]["description"].as_str().unwrap();
        assert!(desc.contains("*list*"));
        assert!(desc.contains("- one"));
    }

    #[test]
    fn comment_list_renders_comment_bodies() {
        let plugin = manifest_builder().build();
        let body = markdown_to_adf("Check the `logs`");
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/comment",
            json!({
                "comments": [{"id": "1001", "body": body}],
                "total": 1,
                "startAt": 0
            }),
        );
        let out = plugin
            .call(
                "jira.issue.comment.list",
                json!({"key": "PROJ-1"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["comments"][0]["body"], "Check the `logs`");
    }

    #[test]
    fn issue_show_returns_raw_adf_when_requested() {
        let plugin = manifest_builder().build();
        let adf = markdown_to_adf("Hello **world**");
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1",
            json!({
                "key": "PROJ-1",
                "fields": {"summary": "ADF test", "description": adf.clone()}
            }),
        );
        let out = plugin
            .call(
                "jira.issue.show",
                json!({"key": "PROJ-1", "body_format": "adf"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["fields"]["description"]["type"], "doc");
    }

    #[test]
    fn attachment_add_accepts_inline_content_bytes() {
        let plugin = manifest_builder().build();
        let bytes = b"hello from base64";
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/attachments",
            json!([{"id": "20001", "filename": "report.txt"}]),
        );
        let out = plugin
            .call(
                "jira.issue.attachment.add",
                json!({
                    "key": "PROJ-1",
                    "content_bytes": b64,
                    "filename": "report.txt",
                    "content_type": "text/plain"
                }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["attachments"][0]["id"], "20001");
    }

    #[test]
    fn attachment_add_rejects_both_blob_ref_and_content_bytes() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1/attachments",
            json!([{"id": "20001"}]),
        );
        host.blobs
            .borrow_mut()
            .insert("blob-1".into(), ("x.bin".into(), vec![1, 2, 3]));
        let err = plugin
            .call(
                "jira.issue.attachment.add",
                json!({
                    "key": "PROJ-1",
                    "blob_ref": "blob-1",
                    "content_bytes": "aGVsbG8="
                }),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("exactly one of blob_ref or content_bytes"));
    }

    #[test]
    fn issue_edit_accepts_update_only() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue/PROJ-1",
            json!({"key": "PROJ-1", "fields": {"summary": "Updated"}}),
        );
        let out = plugin
            .call(
                "jira.issue.edit",
                json!({
                    "key": "PROJ-1",
                    "update": {"summary": [{"set": "Updated"}]}
                }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["key"], "PROJ-1");
    }

    #[test]
    fn issue_create_accepts_raw_fields_and_update() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/rest/api/3/issue",
            json!({"id": "10001", "key": "PROJ-1", "self": "https://x/issue/10001"}),
        );
        let out = plugin
            .call(
                "jira.issue.create",
                json!({
                    "project_key": "DEV",
                    "issue_type": "Task",
                    "summary": "New",
                    "fields": {"customfield_10001": "custom value"},
                    "update": {"labels": [{"add": "triaged"}]}
                }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["key"], "PROJ-1");
    }
}

#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        ArrayStr,
        ArrayAny,
        Object,
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
    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        let key_aliases = || vec![p("id", Kind::Str), p("issue_key", Kind::Str)];
        vec![
            ("jira.test", c(vec![], vec![])),
            (
                "jira.index.build",
                c(
                    vec![
                        p("issue_jql", Kind::Str),
                        p("issue_query", Kind::Str),
                        p("issue_limit", Kind::Int),
                        p("project", Kind::Str),
                        p("status", Kind::Str),
                        p("user_query", Kind::Str),
                        p("user_limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "jira.issue.create",
                c(
                    vec![
                        p("project_key", Kind::Str),
                        p("project", Kind::Str),
                        p("issue_type", Kind::Str),
                        p("summary", Kind::Str),
                        p("description_markdown", Kind::Str),
                        p("labels", Kind::ArrayStr),
                        p("assignee_account_id", Kind::Str),
                        p("reporter_account_id", Kind::Str),
                        p("priority", Kind::Str),
                        p("parent_key", Kind::Str),
                        p("fields", Kind::Object),
                        p("update", Kind::Object),
                    ],
                    vec!["project_key", "issue_type", "summary"],
                ),
            ),
            (
                "jira.issue.edit",
                c(
                    {
                        let mut v = key_aliases();
                        v.extend_from_slice(&[
                            p("key", Kind::Str),
                            p("summary", Kind::Str),
                            p("description_markdown", Kind::Str),
                            p("labels", Kind::ArrayStr),
                            p("assignee_account_id", Kind::Str),
                            p("priority", Kind::Str),
                            p("parent_key", Kind::Str),
                            p("fields", Kind::Object),
                            p("update", Kind::Object),
                        ]);
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.delete",
                c(
                    vec![
                        p("key", Kind::Str),
                        p("id", Kind::Str),
                        p("delete_subtasks", Kind::Bool),
                    ],
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.search",
                c(
                    vec![
                        p("jql", Kind::Str),
                        p("project", Kind::Str),
                        p("status", Kind::Str),
                        p("query", Kind::Str),
                        p("order_by", Kind::Str),
                        p("max", Kind::Int),
                        p("fields", Kind::ArrayStr),
                        p(
                            "body_format",
                            Kind::Enum(vec!["markdown".into(), "adf".into(), "both".into()]),
                        ),
                    ],
                    vec![],
                ),
            ),
            (
                "jira.issue.show",
                c(
                    {
                        let mut v = key_aliases();
                        v.push(p("key", Kind::Str));
                        v.push(p(
                            "body_format",
                            Kind::Enum(vec!["markdown".into(), "adf".into(), "both".into()]),
                        ));
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.create_meta",
                c(
                    vec![p("project_key", Kind::Str), p("issue_type", Kind::Str)],
                    vec![],
                ),
            ),
            (
                "jira.issue.edit_meta",
                c(
                    {
                        let mut v = key_aliases();
                        v.push(p("key", Kind::Str));
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.transition.list",
                c(
                    {
                        let mut v = key_aliases();
                        v.push(p("key", Kind::Str));
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.transition.run",
                c(
                    {
                        let mut v = key_aliases();
                        v.extend_from_slice(&[
                            p("key", Kind::Str),
                            p("transition_id", Kind::Str),
                            p("transition_name", Kind::Str),
                            p("target_status", Kind::Str),
                            p("auto_transition", Kind::Bool),
                            p("max_steps", Kind::Int),
                        ]);
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.comment.add",
                c(
                    {
                        let mut v = key_aliases();
                        v.push(p("key", Kind::Str));
                        v.push(p("body_markdown", Kind::Str));
                        v
                    },
                    vec!["key", "body_markdown"],
                ),
            ),
            (
                "jira.issue.comment.edit",
                c(
                    {
                        let mut v = key_aliases();
                        v.extend_from_slice(&[
                            p("key", Kind::Str),
                            p("comment_id", Kind::Str),
                            p("body_markdown", Kind::Str),
                        ]);
                        v
                    },
                    vec!["key", "comment_id", "body_markdown"],
                ),
            ),
            (
                "jira.issue.comment.delete",
                c(
                    {
                        let mut v = key_aliases();
                        v.extend_from_slice(&[p("key", Kind::Str), p("comment_id", Kind::Str)]);
                        v
                    },
                    vec!["key", "comment_id"],
                ),
            ),
            (
                "jira.issue.comment.list",
                c(
                    {
                        let mut v = key_aliases();
                        v.extend_from_slice(&[
                            p("key", Kind::Str),
                            p("limit", Kind::Int),
                            p("start_at", Kind::Int),
                            p("order", Kind::Str),
                            p(
                                "body_format",
                                Kind::Enum(vec!["markdown".into(), "adf".into(), "both".into()]),
                            ),
                        ]);
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.attachment.add",
                c(
                    {
                        let mut v = key_aliases();
                        v.extend_from_slice(&[
                            p("key", Kind::Str),
                            p("blob_ref", Kind::Str),
                            p("content_bytes", Kind::Str),
                            p("filename", Kind::Str),
                            p("content_type", Kind::Str),
                        ]);
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.attachment.list",
                c(
                    {
                        let mut v = key_aliases();
                        v.push(p("key", Kind::Str));
                        v
                    },
                    vec!["key"],
                ),
            ),
            (
                "jira.issue.attachment.get",
                c(
                    vec![
                        p("attachment_id", Kind::Str),
                        p("filename", Kind::Str),
                        p("mime_type", Kind::Str),
                        p("blob_ref", Kind::Str),
                    ],
                    vec!["attachment_id"],
                ),
            ),
            (
                "jira.issue.attachment.delete",
                c(vec![p("attachment_id", Kind::Str)], vec!["attachment_id"]),
            ),
            (
                "jira.issue.link.add",
                c(
                    vec![
                        p("key", Kind::Str),
                        p("to_key", Kind::Str),
                        p("type", Kind::Str),
                    ],
                    vec!["key", "to_key", "type"],
                ),
            ),
            (
                "jira.user.search",
                c(vec![p("query", Kind::Str), p("limit", Kind::Int)], vec![]),
            ),
        ]
    }

    fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
        if let Some(obj) = node.as_object() {
            if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
                if let Some(name) = r
                    .strip_prefix("#/definitions/")
                    .or_else(|| r.strip_prefix("#/$defs/"))
                {
                    return defs.get(name).unwrap_or(node);
                }
            }
            if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
                for m in any {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
        }
        node
    }

    fn kind_of(node: &Value) -> Kind {
        if let Some(one) = node.get("oneOf").and_then(|v| v.as_array()) {
            // A doc-commented enum: schemars 0.8 emitted per-variant `{"type":"string","enum":
            // ["x"],…}`, 1.x emits `{"const":"x",…}` — accept both, and sort (0.8 ordered
            // alphabetically, 1.x by declaration; the contract is a set).
            let mut vals: Vec<String> = one
                .iter()
                .filter_map(|v| {
                    v.get("const")
                        .and_then(|c| c.as_str())
                        .map(String::from)
                        .or_else(|| {
                            v.get("enum")
                                .and_then(|e| e.as_array())
                                .and_then(|arr| arr.iter().next())
                                .and_then(|x| x.as_str())
                                .map(String::from)
                        })
                })
                .collect();
            vals.sort();
            if !vals.is_empty() {
                return Kind::Enum(vals);
            }
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

    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "array" => {
                let items = node.get("items").cloned().unwrap_or(Value::Null);
                if items.get("type").and_then(|v| v.as_str()) == Some("string") {
                    Kind::ArrayStr
                } else {
                    Kind::ArrayAny
                }
            }
            "string" => {
                if let Some(e) = node.get("enum").and_then(|v| v.as_array()) {
                    let mut vals: Vec<String> = e
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    vals.sort();
                    return Kind::Enum(vals);
                }
                Kind::Str
            }
            "object" | "" => Kind::Object,
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn normalize_enum(kind: &Kind) -> Kind {
        match kind {
            Kind::Enum(vals) => {
                let mut v = vals.clone();
                v.sort();
                Kind::Enum(v)
            }
            _ => kind.clone(),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        assert_eq!(schema["type"], "object", "{op_name}: root type");
        let defs = schema
            .get("definitions")
            .or_else(|| schema.get("$defs"))
            .cloned()
            .unwrap_or(json!({}));
        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                got.insert(k.as_str(), kind_of(resolve(v, &defs)));
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
            assert_eq!(
                normalize_enum(got_kind),
                normalize_enum(kind),
                "{op_name}: property `{name}` kind"
            );
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
    fn derived_schemas_match_contract() {
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
