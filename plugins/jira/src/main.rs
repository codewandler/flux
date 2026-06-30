//! `jira` — a flux integration plugin for the Atlassian Jira Cloud REST API (v3): full issue lifecycle
//! (create/edit/delete/search/show + create/edit metadata), transitions, comments, attachments, issue
//! links, user search, an auth self-test, and an index builder. The path prefix is `/rest/api/3`.
//!
//! ## Auth — two modes, selected at request time (ported from fluxplane `client.go`)
//!
//! The plugin never builds an `Authorization` header itself — the host injects it per the declared
//! [`AuthScheme`]. Two auth methods are declared and the *mode is chosen per request* from the
//! configured env (see [`api_call`]):
//!
//! - **Primary (reference): Bearer + cloud_id gateway.** When a `cloud_id` is configured
//!   (`ATLASSIAN_CLOUD_ID` / `JIRA_CLOUD_ID`), the base URL is the Atlassian OAuth gateway
//!   `https://api.atlassian.com/ex/jira/{cloud_id}` and requests use the `api_token` purpose →
//!   `Authorization: Bearer <api_token>` ([`AuthMethod::bearer`]). This matches fluxplane, whose
//!   `Kind: bearer_token` always sends Bearer and switches base URL on `cloud_id`.
//! - **Fallback: Basic (email:token) against the site URL.** For setups without a cloud_id/OAuth
//!   gateway: when no cloud_id is configured but an `email` IS (`JIRA_EMAIL` / `ATLASSIAN_EMAIL`),
//!   the base is the configured site URL (`jira.endpoint`) and requests use the `basic` purpose →
//!   `Authorization: Basic base64(email:token)` ([`AuthMethod::basic`]). This is flux's original
//!   direct-Basic path, kept (user-confirmed) for installs that never connected via OAuth.
//! - **Else:** Bearer against the configured site URL (`api_token` purpose, no cloud_id) — fluxplane's
//!   endpoint-ref Bearer path.
//!
//! `site_url` is used only for human browse links (not currently emitted). There is NO hand-rolled
//! base64 anywhere — the host injects both Bearer and Basic.
//!
//! `jira.issue.search`, `jira.user.search`, and `jira.index.build` contribute datasource records
//! (`jira.issue` / `jira.user`) so the agent can search them. Attachments move bytes through the host's
//! content-addressed blob store using the byte-exact `http_bytes` path so binary files round-trip
//! exactly (no `from_utf8_lossy`). Markdown bodies are converted to faithful Atlassian Document Format.

use host_kit::*;
use serde_json::{json, Map, Value};

/// The issue fields requested on issue reads (so status/links/attachments/etc. are present).
const FIELDS: &str = "summary,description,status,assignee,reporter,creator,updated,created,project,issuetype,priority,labels,parent,issuelinks,attachment";

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("jira", "0.1.0")
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
            http_hosts: Vec::new(),
            description: "Jira Cloud site URL (e.g. https://site.atlassian.net)".into(),
        })
        // The cloud_id (config) selects the OAuth gateway base + Bearer. Absent → site-URL modes.
        .endpoint(EndpointSpec {
            name: "jira.cloud_id".into(),
            env: vec!["ATLASSIAN_CLOUD_ID".into(), "JIRA_CLOUD_ID".into()],
            http_hosts: Vec::new(),
            description: "Atlassian Cloud ID; when set, calls go through the OAuth gateway".into(),
        })
        // The email (config) selects the Basic fallback when no cloud_id is set.
        .endpoint(EndpointSpec {
            name: "jira.email".into(),
            env: vec!["JIRA_EMAIL".into(), "ATLASSIAN_EMAIL".into()],
            http_hosts: Vec::new(),
            description: "Atlassian account email; enables the Basic auth fallback".into(),
        })
        .datasource(ds("jira.issues", "jira.issue", "Jira issues."))
        .datasource(ds("jira.users", "jira.user", "Jira users."))
        // --- auth + index -------------------------------------------------------------------------
        .operation(
            read_op(
                "jira.test",
                "Test Jira authentication by fetching the current user.",
                json!({"type": "object", "properties": {}}),
            ),
            auth_test,
        )
        .operation(
            read_op(
                "jira.index.build",
                "Build Jira issue and user index records for reverse lookup.",
                json!({"type": "object", "properties": {
                    "issue_jql": {"type": "string", "description": "issue JQL query"},
                    "issue_query": {"type": "string", "description": "issue text query"},
                    "issue_limit": {"type": "integer", "description": "issue page size (max 100)"},
                    "project": {"type": "string", "description": "issue project key filter"},
                    "status": {"type": "string", "description": "issue status filter"},
                    "user_query": {"type": "string", "description": "user search query"},
                    "user_limit": {"type": "integer", "description": "user page size (max 100)"}
                }}),
            ),
            index_build,
        )
        // --- issue CRUD ---------------------------------------------------------------------------
        .operation(
            write_op(
                "jira.issue.create",
                "Create a Jira issue from structured fields and Markdown.",
                json!({"type": "object", "properties": {
                    "project_key": {"type": "string", "description": "project key such as DEV"},
                    "project": {"type": "string", "description": "alias for project_key"},
                    "issue_type": {"type": "string", "description": "issue type name such as Task or Bug"},
                    "summary": {"type": "string", "description": "issue summary"},
                    "description_markdown": {"type": "string", "description": "description as Markdown (converted to Jira ADF)"},
                    "labels": {"type": "array", "items": {"type": "string"}, "description": "labels to set"},
                    "assignee_account_id": {"type": "string", "description": "assignee Atlassian account ID"},
                    "reporter_account_id": {"type": "string", "description": "reporter Atlassian account ID"},
                    "priority": {"type": "string", "description": "priority name"},
                    "parent_key": {"type": "string", "description": "parent issue key for subtasks"}
                }, "required": ["project_key", "issue_type", "summary"]}),
            ),
            issue_create,
        )
        .operation(
            write_op(
                "jira.issue.edit",
                "Edit a Jira issue's structured fields and Markdown, including reparenting via parent_key.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "summary": {"type": "string"},
                    "description_markdown": {"type": "string", "description": "description as Markdown (converted to Jira ADF)"},
                    "labels": {"type": "array", "items": {"type": "string"}},
                    "assignee_account_id": {"type": "string"},
                    "priority": {"type": "string"},
                    "parent_key": {"type": "string", "description": "parent issue key to reparent under"}
                }, "required": ["key"]}),
            ),
            issue_edit,
        )
        .operation(
            write_op(
                "jira.issue.delete",
                "Delete a Jira issue.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "delete_subtasks": {"type": "boolean", "description": "also delete subtasks"}
                }, "required": ["key"]}),
            ),
            issue_delete,
        )
        .operation(
            read_op(
                "jira.issue.search",
                "Search issues with a JQL query (or project/status/query filters).",
                json!({"type": "object", "properties": {
                    "jql": {"type": "string", "description": "JQL query"},
                    "project": {"type": "string", "description": "project key filter"},
                    "status": {"type": "string", "description": "status filter"},
                    "query": {"type": "string", "description": "free-text filter (JQL `text ~`)"},
                    "order_by": {"type": "string", "description": "JQL order-by expression (default `updated DESC`)"},
                    "max": {"type": "integer", "description": "max results (default 25, cap 100)"}
                }}),
            ),
            issue_search,
        )
        .operation(
            read_op(
                "jira.issue.show",
                "Show one issue by key (e.g. PROJ-123).",
                key_schema(),
            ),
            issue_show,
        )
        .operation(
            read_op(
                "jira.issue.create_meta",
                "Show Jira issue create metadata (settable fields per project/issue type).",
                json!({"type": "object", "properties": {
                    "project_key": {"type": "string", "description": "project key filter"},
                    "issue_type": {"type": "string", "description": "issue type name filter"}
                }}),
            ),
            create_meta,
        )
        .operation(
            read_op(
                "jira.issue.edit_meta",
                "Show Jira issue edit metadata (settable fields for one issue).",
                key_schema(),
            ),
            edit_meta,
        )
        // --- transitions --------------------------------------------------------------------------
        .operation(
            read_op(
                "jira.issue.transition.list",
                "Show a Jira issue's current status and currently available transitions.",
                key_schema(),
            ),
            transition_list,
        )
        .operation(
            write_op(
                "jira.issue.transition.run",
                "Run a Jira issue transition. Provide exactly one of transition_id, transition_name, or \
                 target_status. With auto_transition, walks intermediate transitions until target_status.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "transition_id": {"type": "string"},
                    "transition_name": {"type": "string"},
                    "target_status": {"type": "string", "description": "desired status name or ID"},
                    "auto_transition": {"type": "boolean", "description": "take intermediate transitions to reach target_status"},
                    "max_steps": {"type": "integer", "description": "max transitions for auto_transition (default 5, max 20)"}
                }, "required": ["key"]}),
            ),
            transition_run,
        )
        // --- comments -----------------------------------------------------------------------------
        .operation(
            write_op(
                "jira.issue.comment.add",
                "Add a Markdown comment to a Jira issue.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "body_markdown": {"type": "string", "description": "comment body as Markdown (converted to Jira ADF)"}
                }, "required": ["key", "body_markdown"]}),
            ),
            comment_add,
        )
        .operation(
            write_op(
                "jira.issue.comment.edit",
                "Edit a Jira issue comment with Markdown.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "comment_id": {"type": "string"},
                    "body_markdown": {"type": "string", "description": "comment body as Markdown (converted to Jira ADF)"}
                }, "required": ["key", "comment_id", "body_markdown"]}),
            ),
            comment_edit,
        )
        .operation(
            write_op(
                "jira.issue.comment.delete",
                "Delete a Jira issue comment.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "comment_id": {"type": "string"}
                }, "required": ["key", "comment_id"]}),
            ),
            comment_delete,
        )
        .operation(
            read_op(
                "jira.issue.comment.list",
                "List comments on a Jira issue.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "limit": {"type": "integer", "description": "max comments (default 20, cap 100)"},
                    "start_at": {"type": "integer", "description": "zero-based pagination offset"},
                    "order": {"type": "string", "description": "created (oldest first) or -created (newest first)"}
                }, "required": ["key"]}),
            ),
            comment_list,
        )
        // --- attachments (blob, byte-exact via http_bytes) ----------------------------------------
        .operation(
            write_op(
                "jira.issue.attachment.add",
                "Upload an attachment to a Jira issue from a host blob ref.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"},
                    "blob_ref": {"type": "string", "description": "host blob ref to upload"},
                    "filename": {"type": "string", "description": "filename shown in Jira"},
                    "content_type": {"type": "string", "description": "attachment MIME type"}
                }, "required": ["key", "blob_ref"]}),
            ),
            attachment_add,
        )
        .operation(
            read_op(
                "jira.issue.attachment.get",
                "Download a Jira attachment into the host blob store and return its ref.",
                json!({"type": "object", "properties": {
                    "attachment_id": {"type": "string"},
                    "filename": {"type": "string", "description": "optional filename metadata"},
                    "mime_type": {"type": "string", "description": "optional MIME type metadata"}
                }, "required": ["attachment_id"]}),
            ),
            attachment_get,
        )
        .operation(
            read_op(
                "jira.issue.attachment.list",
                "List a Jira issue's attachments.",
                key_schema(),
            ),
            attachment_list,
        )
        .operation(
            write_op(
                "jira.issue.attachment.delete",
                "Delete a Jira issue attachment.",
                json!({"type": "object", "properties": {
                    "attachment_id": {"type": "string"}
                }, "required": ["attachment_id"]}),
            ),
            attachment_delete,
        )
        // --- links + users ------------------------------------------------------------------------
        .operation(
            write_op(
                "jira.issue.link.add",
                "Link two Jira issues (key <type-verb> to_key, e.g. DEV-1 blocks DEV-2 with type Blocks). \
                 Returns the issue's links read back from Jira so the new link is verified.",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key on the verb side (the blocker in Blocks)"},
                    "to_key": {"type": "string", "description": "issue key the verb points at (the blocked issue)"},
                    "type": {"type": "string", "description": "link type name such as Blocks or Relates"}
                }, "required": ["key", "to_key", "type"]}),
            ),
            issue_link_add,
        )
        .operation(
            read_op(
                "jira.user.search",
                "Search Jira users.",
                json!({"type": "object", "properties": {
                    "query": {"type": "string", "description": "user search query"},
                    "limit": {"type": "integer", "description": "max users (default 20, cap 100)"}
                }}),
            ),
            user_search,
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

fn key_schema() -> Value {
    json!({"type": "object", "properties": {
        "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"}
    }, "required": ["key"]})
}

// ---------------------------------------------------------------------------------------------------
// Auth-mode + base-URL selection (ported from fluxplane `NewLiveClient` / `liveClient.do`).
// ---------------------------------------------------------------------------------------------------

/// The base a request resolves against: either a constructed URL the plugin holds (the cloud_id OAuth
/// gateway, which is NOT a declared manifest endpoint and so has no named ref) or a named manifest
/// endpoint reference the host resolves to a base URL (the site URL).
enum Base {
    /// A constructed URL the plugin holds (already trimmed of a trailing slash) — the cloud_id gateway.
    Url(String),
    /// A named manifest endpoint ref the host resolves host-side (the site URL, `"jira.endpoint"`).
    Ref(&'static str),
}

/// The auth purpose + base for the current request, decided from configured env:
/// - cloud_id present → Bearer (`api_token`) against the constructed gateway URL
///   `https://api.atlassian.com/ex/jira/{cloud_id}` (held URL — not a declared endpoint, so no ref);
/// - else email present → Basic (`basic`) against the site URL via the `"jira.endpoint"` ref;
/// - else → Bearer (`api_token`) against the site URL via the `"jira.endpoint"` ref.
struct AuthMode {
    /// The `auth_purpose` to pass to the host (`"api_token"` → Bearer, `"basic"` → Basic).
    purpose: &'static str,
    /// The base the request resolves against.
    base: Base,
}

/// Resolve the request auth mode + base from configured env.
fn auth_mode(host: &mut Host) -> Result<AuthMode, String> {
    // cloud_id (config value, NOT an IO endpoint) → the OAuth gateway base + Bearer. The gateway URL is
    // constructed from the cloud_id, so it is a held URL (`Base::Url`), never a named endpoint ref.
    if let Some(cloud_id) = host
        .endpoint("jira.cloud_id")
        .ok()
        .map(|s| s.trim().to_string())
    {
        if !cloud_id.is_empty() {
            return Ok(AuthMode {
                purpose: "api_token",
                base: Base::Url(format!(
                    "https://api.atlassian.com/ex/jira/{}",
                    urlencode(&cloud_id)
                )),
            });
        }
    }
    // No cloud_id: the site URL is the base, resolved host-side from the named `"jira.endpoint"` ref —
    // the plugin never holds the site URL. email (config value, no cloud_id) → Basic fallback; else
    // Bearer against the site URL.
    let email_set = host
        .endpoint("jira.email")
        .ok()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    Ok(AuthMode {
        purpose: if email_set { "basic" } else { "api_token" },
        base: Base::Ref("jira.endpoint"),
    })
}

// ---------------------------------------------------------------------------------------------------
// HTTP helpers — every call routes through the host with the selected `auth_purpose`, so the host
// injects Bearer or Basic (the plugin never sees the token or builds the header).
// ---------------------------------------------------------------------------------------------------

/// Build the full API path `/rest/api/3{path}` (the `/rest/api/3` prefix the v3 API expects).
fn api_path(path: &str) -> String {
    format!("/rest/api/3{path}")
}

/// Resolve the current auth mode to a full URL + `auth_purpose` — for the **byte-exact** `http_bytes`
/// paths only, which take a URL and have no ref equivalent (`http_bytes_ref` does not exist). The
/// cloud_id gateway is already a held `Base::Url`; the site path materializes its base URL via the
/// host endpoint resolver. This is the one residual `host.endpoint` use, confined to byte attachment
/// IO, until host-kit grows an `http_bytes_ref`.
fn api_url(host: &mut Host, path: &str) -> Result<(String, &'static str), String> {
    let mode = auth_mode(host)?;
    let full = api_path(path);
    match mode.base {
        Base::Url(base) => Ok((format!("{base}{full}"), mode.purpose)),
        Base::Ref(r) => {
            let site = host.endpoint(r)?;
            Ok((
                format!("{}{full}", site.trim_end_matches('/')),
                mode.purpose,
            ))
        }
    }
}

/// GET `/rest/api/3{path}` (against the current base) and parse the JSON body.
fn jget(host: &mut Host, path: &str) -> Result<Value, String> {
    let mode = auth_mode(host)?;
    let full = api_path(path);
    match mode.base {
        Base::Ref(r) => host.get_json_ref(r, &full, Some(mode.purpose)),
        Base::Url(base) => host.get_json(&format!("{base}{full}"), Some(mode.purpose)),
    }
}

/// Send a JSON body with `method` and parse the (non-empty) JSON response.
fn jsend(host: &mut Host, method: &str, path: &str, body: &Value) -> Result<Value, String> {
    let mode = auth_mode(host)?;
    let full = api_path(path);
    match mode.base {
        Base::Ref(r) => host.send_json_ref(r, method, &full, Some(mode.purpose), body),
        Base::Url(base) => {
            host.send_json(method, &format!("{base}{full}"), Some(mode.purpose), body)
        }
    }
}

/// Send a request whose response body is ignored (PUT/DELETE/POST that return 204 No Content).
fn jsend_noresp(
    host: &mut Host,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(), String> {
    let mode = auth_mode(host)?;
    let full = api_path(path);
    match mode.base {
        Base::Ref(r) => {
            // The ref path has no `headers` arg on `http_ref`. For a JSON body, route through
            // `send_json_ref` (it sets content-type and parses a response we ignore — the URL path
            // ignored it too). For no body, use `http_ref` directly and check the status.
            match body {
                Some(b) => {
                    host.send_json_ref(r, method, &full, Some(mode.purpose), b)?;
                }
                None => {
                    let resp = host.http_ref(r, method, &full, Some(mode.purpose), None)?;
                    if !resp.is_success() {
                        return Err(format!(
                            "jira {method} {path} → {} {}",
                            resp.status, resp.body
                        ));
                    }
                }
            }
            Ok(())
        }
        Base::Url(base) => {
            let url = format!("{base}{full}");
            let serialized = match body {
                Some(b) => Some(serde_json::to_string(b).map_err(|e| e.to_string())?),
                None => None,
            };
            let headers: &[(&str, &str)] = if body.is_some() {
                &[("content-type", "application/json")]
            } else {
                &[]
            };
            let resp = host.http(
                method,
                &url,
                Some(mode.purpose),
                headers,
                serialized.as_deref(),
            )?;
            if !resp.is_success() {
                return Err(format!(
                    "jira {method} {path} → {} {}",
                    resp.status, resp.body
                ));
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Small input helpers
// ---------------------------------------------------------------------------------------------------

fn opt_str<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// The issue key from `key` / `id` / `issue_key` (in order), trimmed.
fn issue_key(input: &Value) -> Result<String, String> {
    for k in ["key", "id", "issue_key"] {
        let v = opt_str(input, k).trim();
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }
    Err("`key` (issue key) required".into())
}

/// Pick a positive limit from the first present key, default-then-cap.
fn clamp_limit(input: &Value, keys: &[&str], default: i64, max: i64) -> i64 {
    let mut v = 0;
    for k in keys {
        if let Some(n) = input.get(k).and_then(|x| x.as_i64()) {
            v = n;
            break;
        }
    }
    let v = if v <= 0 { default } else { v };
    v.min(max)
}

/// Build the JQL: explicit `jql` wins, else project/status/query conditions with an order-by tail.
fn build_jql(input: &Value) -> String {
    let jql = opt_str(input, "jql").trim();
    if !jql.is_empty() {
        return jql.to_string();
    }
    let mut conds: Vec<String> = Vec::new();
    let project = opt_str(input, "project").trim();
    if !project.is_empty() {
        conds.push(format!("project = {}", jql_string(project)));
    }
    let status = opt_str(input, "status").trim();
    if !status.is_empty() {
        conds.push(format!("status = {}", jql_string(status)));
    }
    let query = {
        let q = opt_str(input, "query").trim();
        if q.is_empty() {
            opt_str(input, "search").trim()
        } else {
            q
        }
    };
    if !query.is_empty() {
        conds.push(format!("text ~ {}", jql_string(query)));
    }
    let order_by = {
        let o = opt_str(input, "order_by").trim();
        if o.is_empty() {
            "updated DESC"
        } else {
            o
        }
    };
    if conds.is_empty() {
        format!("order by {order_by}")
    } else {
        format!("{} order by {order_by}", conds.join(" and "))
    }
}

/// Quote a JQL string literal, escaping backslashes and double quotes.
fn jql_string(value: &str) -> String {
    let escaped = value.trim().replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Whether a Jira status/transition-target object matches `target` by name or id (case-insensitive).
fn status_matches(status: &Value, target: &str) -> bool {
    let t = target.trim();
    if t.is_empty() {
        return false;
    }
    let name = status.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let id = status.get("id").and_then(|v| v.as_str()).unwrap_or("");
    name.eq_ignore_ascii_case(t) || id.eq_ignore_ascii_case(t)
}

/// Set the typed issue fields shared by create + edit (description/labels/assignee/priority).
fn apply_common(fields: &mut Map<String, Value>, input: &Value) {
    let desc = opt_str(input, "description_markdown").trim();
    if !desc.is_empty() {
        fields.insert("description".into(), markdown_to_adf(desc));
    }
    if let Some(arr) = input.get("labels").and_then(|v| v.as_array()) {
        let labels: Vec<String> = arr
            .iter()
            .filter_map(|l| l.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !labels.is_empty() {
            fields.insert("labels".into(), json!(labels));
        }
    }
    let assignee = opt_str(input, "assignee_account_id").trim();
    if !assignee.is_empty() {
        fields.insert("assignee".into(), json!({"accountId": assignee}));
    }
    let priority = opt_str(input, "priority").trim();
    if !priority.is_empty() {
        fields.insert("priority".into(), json!({"name": priority}));
    }
}

// ---------------------------------------------------------------------------------------------------
// Markdown → Atlassian Document Format (ADF).
//
// Ported by hand from fluxplane's `atlassian.MarkdownToADF`, whose underlying `codewandler/md2adf`
// builds an ADF tree off a goldmark parse and then prunes code-incompatible marks. flux has no
// goldmark, so this is a self-contained block+inline converter covering the constructs Jira renders:
// paragraphs, ATX headings (1-6), bullet/ordered lists, fenced code blocks, blockquotes, thematic
// rules, and inline bold/italic/strikethrough/code/links. As in the reference, the ADF code mark may
// only combine with link, so the inline parser never emits code alongside any other mark.
// ---------------------------------------------------------------------------------------------------

/// Convert a Markdown string into a Jira-ready ADF `doc` node.
fn markdown_to_adf(markdown: &str) -> Value {
    let content = convert_blocks(markdown);
    json!({ "type": "doc", "version": 1, "content": content })
}

/// Split `markdown` into block nodes (paragraphs, headings, lists, code blocks, blockquotes, rules).
fn convert_blocks(markdown: &str) -> Vec<Value> {
    let normalized = markdown.replace('\r', "");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut blocks: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Blank lines separate blocks.
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block (``` or ~~~), optional language on the opening fence.
        if let Some(fence) = code_fence(trimmed) {
            let lang = trimmed[fence.len()..].trim();
            let mut code: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume the closing fence
            }
            let mut node = json!({
                "type": "codeBlock",
                "content": [{"type": "text", "text": code.join("\n")}],
            });
            if !lang.is_empty() {
                node["attrs"] = json!({ "language": lang });
            }
            blocks.push(node);
            continue;
        }

        // Thematic break: ---, ***, ___ (3+).
        if is_thematic_break(trimmed) {
            blocks.push(json!({ "type": "rule" }));
            i += 1;
            continue;
        }

        // ATX heading: 1-6 leading `#` then a space.
        if let Some((level, text)) = atx_heading(trimmed) {
            blocks.push(json!({
                "type": "heading",
                "attrs": { "level": level },
                "content": convert_inline(text),
            }));
            i += 1;
            continue;
        }

        // Blockquote: one or more `>`-prefixed lines; inner text is re-parsed as blocks.
        if trimmed.starts_with('>') {
            let mut inner: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let l = lines[i].trim_start();
                let stripped = l.strip_prefix('>').unwrap_or(l);
                inner.push(stripped.strip_prefix(' ').unwrap_or(stripped).to_string());
                i += 1;
            }
            blocks.push(json!({
                "type": "blockquote",
                "content": convert_blocks(&inner.join("\n")),
            }));
            continue;
        }

        // List (bullet or ordered): a run of contiguous list-marker lines.
        if list_marker(trimmed).is_some() {
            let ordered = matches!(list_marker(trimmed), Some(ListKind::Ordered));
            let mut items: Vec<Value> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim();
                match list_marker(t) {
                    Some(kind) if (kind == ListKind::Ordered) == ordered => {
                        let text = strip_list_marker(t);
                        items.push(json!({
                            "type": "listItem",
                            "content": [{"type": "paragraph", "content": convert_inline(text)}],
                        }));
                        i += 1;
                    }
                    _ => break,
                }
            }
            blocks.push(json!({
                "type": if ordered { "orderedList" } else { "bulletList" },
                "content": items,
            }));
            continue;
        }

        // Otherwise: a paragraph — consecutive non-blank, non-block lines joined as soft breaks.
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len() {
            let t = lines[i].trim();
            if t.is_empty()
                || code_fence(t).is_some()
                || is_thematic_break(t)
                || atx_heading(t).is_some()
                || t.starts_with('>')
                || list_marker(t).is_some()
            {
                break;
            }
            para.push(lines[i].trim());
            i += 1;
        }
        // Soft line breaks become spaces in ADF (matching the reference converter).
        blocks.push(json!({
            "type": "paragraph",
            "content": convert_inline(&para.join(" ")),
        }));
    }
    if blocks.is_empty() {
        blocks.push(json!({ "type": "paragraph", "content": [] }));
    }
    blocks
}

/// The fence marker (```` ``` ```` or `~~~`) if `line` opens a fenced code block.
fn code_fence(line: &str) -> Option<&'static str> {
    if line.starts_with("```") {
        Some("```")
    } else if line.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Whether `line` is a thematic break: 3+ of `-`, `*`, or `_` (ignoring spaces).
fn is_thematic_break(line: &str) -> bool {
    for ch in ['-', '*', '_'] {
        let count = line.chars().filter(|&c| c == ch).count();
        if count >= 3 && line.chars().all(|c| c == ch || c == ' ') {
            return true;
        }
    }
    false
}

/// `(level, text)` if `line` is an ATX heading (`#`..`######` + space).
fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if let Some(text) = rest.strip_prefix(' ') {
            return Some((hashes as u8, text.trim_end_matches('#').trim_end()));
        }
    }
    None
}

#[derive(PartialEq, Clone, Copy)]
enum ListKind {
    Bullet,
    Ordered,
}

/// The list kind if `trimmed` starts with a list marker (`- `/`* `/`+ ` or `N. `/`N) `).
fn list_marker(trimmed: &str) -> Option<ListKind> {
    for m in ["- ", "* ", "+ "] {
        if trimmed.starts_with(m) {
            return Some(ListKind::Bullet);
        }
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some(ListKind::Ordered);
        }
    }
    None
}

/// Strip the leading list marker, returning the item text.
fn strip_list_marker(trimmed: &str) -> &str {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(m) {
            return rest.trim_start();
        }
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &trimmed[digits..];
    rest.strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))
        .unwrap_or(trimmed)
        .trim_start()
}

/// Convert inline Markdown into ADF text nodes. Recognizes `[text](href)` links, `` `code` ``,
/// `**bold**`/`__bold__`, `*em*`/`_em_`, and `~~strike~~`. The ADF code mark only ever combines with
/// link (never bold/em/strike), matching the reference's mark-pruning.
fn convert_inline(text: &str) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut nodes: Vec<Value> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    // Flush the plain-text buffer (with the active marks) into a text node.
    macro_rules! flush {
        ($marks:expr) => {{
            if !buf.is_empty() {
                push_text(&mut nodes, &buf, $marks);
                buf.clear();
            }
        }};
    }

    let empty: &[&str] = &[];
    while i < chars.len() {
        let c = chars[i];

        // Inline code: `...` — highest precedence, no nested marks (only code, per ADF).
        if c == '`' {
            if let Some(end) = find_char(&chars, i + 1, '`') {
                flush!(empty);
                let code: String = chars[i + 1..end].iter().collect();
                push_text(&mut nodes, &code, &["code"]);
                i = end + 1;
                continue;
            }
        }

        // Link: [text](href).
        if c == '[' {
            if let Some((label, href, next)) = parse_link(&chars, i) {
                flush!(empty);
                let mut inner = convert_inline(&label);
                add_link_mark(&mut inner, &href);
                nodes.append(&mut inner);
                i = next;
                continue;
            }
        }

        // Strong: ** or __.
        if let Some(delim) = strong_delim(&chars, i) {
            if let Some(end) = find_delim(&chars, i + 2, delim) {
                flush!(empty);
                let inner: String = chars[i + 2..end].iter().collect();
                let mut sub = convert_inline_marked(&inner, &["strong"]);
                nodes.append(&mut sub);
                i = end + 2;
                continue;
            }
        }

        // Strikethrough: ~~.
        if c == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            if let Some(end) = find_delim(&chars, i + 2, '~') {
                flush!(empty);
                let inner: String = chars[i + 2..end].iter().collect();
                let mut sub = convert_inline_marked(&inner, &["strike"]);
                nodes.append(&mut sub);
                i = end + 2;
                continue;
            }
        }

        // Emphasis: single * or _.
        if (c == '*' || c == '_') && strong_delim(&chars, i).is_none() {
            if let Some(end) = find_char(&chars, i + 1, c) {
                if end > i + 1 {
                    flush!(empty);
                    let inner: String = chars[i + 1..end].iter().collect();
                    let mut sub = convert_inline_marked(&inner, &["em"]);
                    nodes.append(&mut sub);
                    i = end + 1;
                    continue;
                }
            }
        }

        buf.push(c);
        i += 1;
    }
    flush!(empty);
    nodes
}

/// Convert inline Markdown, adding `extra` marks to every produced text node (used for the inside of
/// a bold/em/strike span). The code mark is never extended with `extra` — code stands alone.
fn convert_inline_marked(text: &str, extra: &[&str]) -> Vec<Value> {
    let mut nodes = convert_inline(text);
    for node in &mut nodes {
        // Never add formatting marks to a code-marked node (ADF forbids code + bold/em/strike).
        if has_mark(node, "code") {
            continue;
        }
        for m in extra {
            add_mark(node, m);
        }
    }
    nodes
}

/// Push a text node carrying `marks` onto `nodes`.
fn push_text(nodes: &mut Vec<Value>, text: &str, marks: &[&str]) {
    let mut node = json!({ "type": "text", "text": text });
    for m in marks {
        add_mark(&mut node, m);
    }
    nodes.push(node);
}

/// Add a simple (attr-less) mark to a text node if not already present.
fn add_mark(node: &mut Value, mark: &str) {
    if has_mark(node, mark) {
        return;
    }
    let marks = node
        .as_object_mut()
        .unwrap()
        .entry("marks")
        .or_insert_with(|| json!([]));
    if let Some(arr) = marks.as_array_mut() {
        arr.push(json!({ "type": mark }));
    }
}

/// Add a `link` mark with `href` to every text node in `nodes` (links may combine with code).
fn add_link_mark(nodes: &mut [Value], href: &str) {
    for node in nodes.iter_mut() {
        let marks = node
            .as_object_mut()
            .unwrap()
            .entry("marks")
            .or_insert_with(|| json!([]));
        if let Some(arr) = marks.as_array_mut() {
            arr.push(json!({ "type": "link", "attrs": { "href": href } }));
        }
    }
}

/// Whether a text node already carries a mark of `kind`.
fn has_mark(node: &Value, kind: &str) -> bool {
    node.get("marks")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .any(|m| m.get("type").and_then(|t| t.as_str()) == Some(kind))
        })
        .unwrap_or(false)
}

/// The next index of `target` at or after `from`, if any.
fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// The next index where a doubled `delim` (`**`, `__`, `~~`) begins, at or after `from`. A single
/// trailing delimiter cannot close the span, so the run only matches a doubled occurrence.
fn find_delim(chars: &[char], from: usize, delim: char) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == delim && chars[j + 1] == delim {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// The strong delimiter char at `i` (`*` for `**`, `_` for `__`), if a doubled run starts there.
fn strong_delim(chars: &[char], i: usize) -> Option<char> {
    if i + 1 < chars.len() && (chars[i] == '*' || chars[i] == '_') && chars[i + 1] == chars[i] {
        Some(chars[i])
    } else {
        None
    }
}

/// Parse a `[label](href)` link starting at `chars[i] == '['`; returns `(label, href, next_index)`.
fn parse_link(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let close = find_char(chars, i + 1, ']')?;
    if close + 1 >= chars.len() || chars[close + 1] != '(' {
        return None;
    }
    let href_end = find_char(chars, close + 2, ')')?;
    let label: String = chars[i + 1..close].iter().collect();
    let href: String = chars[close + 2..href_end].iter().collect();
    Some((label, href, href_end + 1))
}

// ---------------------------------------------------------------------------------------------------
// Datasource contribution
// ---------------------------------------------------------------------------------------------------

/// Contribute one `jira.issue` record per issue in `result.issues[]`. Returns the record count.
fn contribute_issues(host: &mut Host, result: &Value) -> usize {
    let Some(arr) = result.get("issues").and_then(|v| v.as_array()) else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let key = it.get("key").and_then(|v| v.as_str())?;
            let fields = it.get("fields");
            let summary = fields
                .and_then(|f| f.get("summary"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let status = fields
                .and_then(|f| f.get("status"))
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str());
            let body = match status {
                Some(s) => format!("{summary} [{s}]"),
                None => summary.to_string(),
            };
            Some(Record::new(
                Source::new("jira"),
                "jira.issue",
                key,
                summary,
                body,
            ))
        })
        .collect();
    let n = records.len();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
    n
}

/// Contribute one `jira.user` record per user in the `users[]` array. Returns the record count.
fn contribute_users(host: &mut Host, users: &Value) -> usize {
    let Some(arr) = users.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|u| {
            let id = u.get("accountId").and_then(|v| v.as_str())?;
            let name = u.get("displayName").and_then(|v| v.as_str()).unwrap_or(id);
            let email = u.get("emailAddress").and_then(|v| v.as_str()).unwrap_or("");
            Some(Record::new(
                Source::new("jira"),
                "jira.user",
                id,
                name,
                email,
            ))
        })
        .collect();
    let n = records.len();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
    n
}

// ---------------------------------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------------------------------

fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let user = jget(host, "/myself")?;
    Ok(json!({"text": "Jira auth OK", "status": "ok", "user": user}))
}

fn index_build(input: Value, host: &mut Host) -> Result<Value, String> {
    let issue_selector = json!({
        "jql": opt_str(&input, "issue_jql"),
        "project": opt_str(&input, "project"),
        "status": opt_str(&input, "status"),
        "query": opt_str(&input, "issue_query"),
    });
    let jql = build_jql(&issue_selector);
    let issue_limit = clamp_limit(&input, &["issue_limit"], 100, 100);
    let issues = jget(
        host,
        &format!(
            "/search/jql?jql={}&maxResults={issue_limit}&fields={}",
            urlencode(&jql),
            urlencode(FIELDS)
        ),
    )?;
    let n_issues = contribute_issues(host, &issues);

    let user_query = opt_str(&input, "user_query").trim();
    let user_limit = clamp_limit(&input, &["user_limit"], 100, 100);
    let users = jget(
        host,
        &format!(
            "/user/search?query={}&maxResults={user_limit}&startAt=0",
            urlencode(user_query)
        ),
    )?;
    let n_users = contribute_users(host, &users);

    Ok(json!({"indexed": n_issues + n_users}))
}

fn issue_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = {
        let p = opt_str(&input, "project_key").trim();
        if p.is_empty() {
            opt_str(&input, "project").trim()
        } else {
            p
        }
    };
    let issue_type = opt_str(&input, "issue_type").trim();
    let summary = opt_str(&input, "summary").trim();
    if project.is_empty() || issue_type.is_empty() || summary.is_empty() {
        return Err("project_key (or project), issue_type, and summary are required".into());
    }
    let mut fields = Map::new();
    fields.insert("project".into(), json!({"key": project}));
    fields.insert("issuetype".into(), json!({"name": issue_type}));
    fields.insert("summary".into(), json!(summary));
    apply_common(&mut fields, &input);
    let reporter = opt_str(&input, "reporter_account_id").trim();
    if !reporter.is_empty() {
        fields.insert("reporter".into(), json!({"accountId": reporter}));
    }
    let parent = opt_str(&input, "parent_key").trim();
    if !parent.is_empty() {
        fields.insert("parent".into(), json!({"key": parent}));
    }
    let resp = jsend(host, "POST", "/issue", &json!({"fields": fields}))?;
    Ok(json!({
        "ok": true,
        "id": resp.get("id").cloned().unwrap_or(Value::Null),
        "key": resp.get("key").cloned().unwrap_or(Value::Null),
        "self": resp.get("self").cloned().unwrap_or(Value::Null),
    }))
}

fn issue_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut fields = Map::new();
    let summary = opt_str(&input, "summary").trim();
    if !summary.is_empty() {
        fields.insert("summary".into(), json!(summary));
    }
    apply_common(&mut fields, &input);
    let parent = opt_str(&input, "parent_key").trim();
    if !parent.is_empty() {
        fields.insert("parent".into(), json!({"key": parent}));
    }
    if fields.is_empty() {
        return Err("at least one field to edit is required".into());
    }
    jsend_noresp(
        host,
        "PUT",
        &format!("/issue/{}", urlencode(&key)),
        Some(&json!({"fields": fields})),
    )?;
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    Ok(json!({"ok": true, "key": key, "issue": issue}))
}

fn issue_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut path = format!("/issue/{}", urlencode(&key));
    if input
        .get("delete_subtasks")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        path.push_str("?deleteSubtasks=true");
    }
    jsend_noresp(host, "DELETE", &path, None)?;
    Ok(json!({"ok": true, "key": key}))
}

fn issue_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let jql = build_jql(&input);
    let max = clamp_limit(&input, &["max", "limit"], 25, 100);
    let result = jget(
        host,
        &format!(
            "/search/jql?jql={}&maxResults={max}&fields={}",
            urlencode(&jql),
            urlencode(FIELDS)
        ),
    )?;
    contribute_issues(host, &result);
    Ok(result)
}

fn issue_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )
}

fn create_meta(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut path = String::from("/issue/createmeta?expand=projects.issuetypes.fields");
    let project = opt_str(&input, "project_key").trim();
    if !project.is_empty() {
        path.push_str(&format!("&projectKeys={}", urlencode(project)));
    }
    let issue_type = opt_str(&input, "issue_type").trim();
    if !issue_type.is_empty() {
        path.push_str(&format!("&issuetypeNames={}", urlencode(issue_type)));
    }
    let metadata = jget(host, &path)?;
    Ok(json!({"metadata": metadata}))
}

fn edit_meta(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let metadata = jget(host, &format!("/issue/{}/editmeta", urlencode(&key)))?;
    Ok(json!({"metadata": metadata}))
}

fn transition_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    let current = issue
        .get("fields")
        .and_then(|f| f.get("status"))
        .cloned()
        .unwrap_or(Value::Null);
    let tl = jget(host, &format!("/issue/{}/transitions", urlencode(&key)))?;
    let transitions = tl.get("transitions").cloned().unwrap_or(json!([]));
    Ok(json!({"issue_key": key, "current_status": current, "transitions": transitions}))
}

/// A stable per-transition key (id + target id + target name), matching the reference `transitionKey`
/// — distinguishes transitions even when ids repeat across statuses, so the walk never loops.
fn transition_key(t: &Value) -> String {
    let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
    let to = t.get("to");
    let to_id = to
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let to_name = to
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    format!("{id}\u{0}{to_id}\u{0}{to_name}")
}

/// Clamp the auto_transition step budget (default 5, max 20) — the reference `boundedTransitionSteps`.
fn bounded_transition_steps(value: i64) -> i64 {
    if value <= 0 {
        5
    } else if value > 20 {
        20
    } else {
        value
    }
}

/// Score an intermediate transition (lower is more progress-y) — the reference
/// `intermediateTransitionScore`. Terminal/blocking transitions are heavily penalized; clear
/// forward-motion transitions score 0; done/resolved 50; everything else 10.
fn intermediate_transition_score(t: &Value) -> i64 {
    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    let to = t
        .get("to")
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let text = format!("{name} {to}").to_lowercase();
    for term in [
        "blocked",
        "block",
        "hold",
        "abandoned",
        "closed",
        "cancel",
        "rejected",
    ] {
        if text.contains(term) {
            return 100;
        }
    }
    for term in [
        "progress",
        "prepare",
        "preparation",
        "selected",
        "todo",
        "to do",
        "review",
        "test",
        "qa",
    ] {
        if text.contains(term) {
            return 0;
        }
    }
    if text.contains("done") || text.contains("resolved") {
        return 50;
    }
    10
}

/// Pick the best untried, non-self transition by score — the reference `bestIntermediateTransition`.
fn best_intermediate_transition(
    transitions: &[Value],
    current: &Value,
    tried: &[String],
) -> Option<Value> {
    let current_name = current.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let current_id = current.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let mut best: Option<Value> = None;
    let mut best_score = 1000;
    for t in transitions {
        if tried.iter().any(|x| x == &transition_key(t)) {
            continue;
        }
        let to = t.get("to").unwrap_or(&Value::Null);
        if status_matches(to, current_name) || status_matches(to, current_id) {
            continue;
        }
        let score = intermediate_transition_score(t);
        if score < best_score {
            best = Some(t.clone());
            best_score = score;
        }
    }
    if best_score < 1000 {
        best
    } else {
        None
    }
}

/// Select a transition by id, name, target-status (matching `to`), or — when intermediate steps are
/// allowed — the best-scoring untried transition. Ported from the reference `selectTransition`.
fn select_transition(
    transitions: &[Value],
    current: &Value,
    id: &str,
    name: &str,
    target: &str,
    allow_intermediate: bool,
    tried: &[String],
) -> Option<Value> {
    let untried = |t: &Value| -> bool {
        let k = transition_key(t);
        !tried.iter().any(|x| x == &k)
    };
    if !id.is_empty() {
        return transitions
            .iter()
            .find(|t| {
                untried(t)
                    && t.get("id")
                        .and_then(|v| v.as_str())
                        .map(|x| x.trim().eq_ignore_ascii_case(id))
                        .unwrap_or(false)
            })
            .cloned();
    }
    if !name.is_empty() {
        return transitions
            .iter()
            .find(|t| {
                untried(t)
                    && t.get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| n.trim().eq_ignore_ascii_case(name))
                        .unwrap_or(false)
            })
            .cloned();
    }
    if !target.is_empty() {
        if let Some(t) = transitions
            .iter()
            .find(|t| untried(t) && status_matches(t.get("to").unwrap_or(&Value::Null), target))
        {
            return Some(t.clone());
        }
        if !allow_intermediate {
            return None;
        }
        return best_intermediate_transition(transitions, current, tried);
    }
    if allow_intermediate && !transitions.is_empty() {
        return best_intermediate_transition(transitions, current, tried);
    }
    None
}

fn transition_summary(transitions: &[Value]) -> String {
    if transitions.is_empty() {
        return "none".into();
    }
    transitions
        .iter()
        .map(|t| {
            let name = {
                let n = t.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
                if n.is_empty() {
                    t.get("id").and_then(|v| v.as_str()).unwrap_or("").trim()
                } else {
                    n
                }
            };
            let to = t
                .get("to")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if to.is_empty() {
                name.to_string()
            } else {
                format!("{name} -> {to}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn transition_run(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let id = opt_str(&input, "transition_id").trim();
    let name = opt_str(&input, "transition_name").trim();
    let target = opt_str(&input, "target_status").trim();
    if id.is_empty() && name.is_empty() && target.is_empty() {
        return Err("transition_id, transition_name, or target_status is required".into());
    }
    let auto = input
        .get("auto_transition")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_steps =
        bounded_transition_steps(input.get("max_steps").and_then(|v| v.as_i64()).unwrap_or(0));

    // Read the initial status + currently available transitions (the reference ListTransitions).
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    let initial_status = issue
        .get("fields")
        .and_then(|f| f.get("status"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut current_status = initial_status.clone();
    let mut transitions: Vec<Value> =
        jget(host, &format!("/issue/{}/transitions", urlencode(&key)))?
            .get("transitions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

    let mut applied: Vec<Value> = Vec::new();
    let mut tried: Vec<String> = Vec::new();
    let mut steps: i64 = 0;

    // Already at the target? Re-read the issue and return without mutating.
    if !target.is_empty() && status_matches(&current_status, target) {
        let final_issue = jget(
            host,
            &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
        )?;
        return Ok(transition_result(
            &key,
            &initial_status,
            &current_status,
            target,
            &applied,
            steps,
            &final_issue,
            &transitions,
        ));
    }

    while steps < max_steps {
        let Some(transition) = select_transition(
            &transitions,
            &current_status,
            id,
            name,
            target,
            steps > 0 || auto,
            &tried,
        ) else {
            if !applied.is_empty() {
                // Already mutated — surface what happened (the reference transitionRunFailure).
                return Err(transition_run_failure(
                    &key,
                    &initial_status,
                    &current_status,
                    &applied,
                    &format!(
                        "no further transition matches the request; available: {}",
                        transition_summary(&transitions)
                    ),
                ));
            }
            return Err(format!(
                "no available transition matches the request; available: {}",
                transition_summary(&transitions)
            ));
        };
        let tkey = transition_key(&transition);
        if tried.iter().any(|x| x == &tkey) {
            return Err(transition_run_failure(
                &key,
                &initial_status,
                &current_status,
                &applied,
                &format!(
                    "transition walk repeated {:?} before reaching target status {target:?}",
                    transition
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
            ));
        }
        tried.push(tkey);
        let tid = transition
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        jsend_noresp(
            host,
            "POST",
            &format!("/issue/{}/transitions", urlencode(&key)),
            Some(&json!({"transition": {"id": tid}})),
        )?;
        applied.push(transition);
        steps += 1;

        if target.is_empty() {
            break; // an explicit single transition by id/name
        }
        // Re-read state (the reference re-calls ListTransitions each loop).
        let issue = jget(
            host,
            &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
        )?;
        current_status = issue
            .get("fields")
            .and_then(|f| f.get("status"))
            .cloned()
            .unwrap_or(Value::Null);
        transitions = jget(host, &format!("/issue/{}/transitions", urlencode(&key)))?
            .get("transitions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if status_matches(&current_status, target) {
            break;
        }
        if !auto {
            break;
        }
    }

    if !target.is_empty() && !status_matches(&current_status, target) && steps >= max_steps {
        return Err(transition_run_failure(
            &key,
            &initial_status,
            &current_status,
            &applied,
            &format!(
                "target status {target:?} was not reached within max_steps={max_steps}; current status is {:?}",
                current_status.get("name").and_then(|v| v.as_str()).unwrap_or("")
            ),
        ));
    }

    let final_issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    current_status = final_issue
        .get("fields")
        .and_then(|f| f.get("status"))
        .cloned()
        .unwrap_or(current_status);
    Ok(transition_result(
        &key,
        &initial_status,
        &current_status,
        target,
        &applied,
        steps,
        &final_issue,
        &transitions,
    ))
}

#[allow(clippy::too_many_arguments)]
fn transition_result(
    key: &str,
    initial_status: &Value,
    current_status: &Value,
    target: &str,
    applied: &[Value],
    steps: i64,
    final_issue: &Value,
    available: &[Value],
) -> Value {
    json!({
        "ok": true,
        "issue_key": key,
        "initial_status": initial_status,
        "current_status": current_status,
        "target_status": target,
        "applied_transitions": applied,
        "available_transitions": available,
        "steps": steps,
        "issue": final_issue,
    })
}

/// Build a walker-failure error that does NOT hide the transitions already applied (the reference
/// `transitionRunFailure`): it names every applied transition and the issue's current status.
fn transition_run_failure(
    key: &str,
    initial_status: &Value,
    current_status: &Value,
    applied: &[Value],
    message: &str,
) -> String {
    if applied.is_empty() {
        return message.to_string();
    }
    let names: Vec<String> = applied
        .iter()
        .map(|t| {
            t.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    let was = initial_status
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let now = current_status
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!(
        "{message}; issue {key} WAS mutated before the failure: applied {} transition(s): {}; status is now {now:?} (was {was:?})",
        names.len(),
        names.join(" → ")
    )
}

fn comment_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let body = opt_str(&input, "body_markdown").trim();
    if body.is_empty() {
        return Err("`body_markdown` (string) required".into());
    }
    let resp = jsend(
        host,
        "POST",
        &format!("/issue/{}/comment", urlencode(&key)),
        &json!({"body": markdown_to_adf(body)}),
    )?;
    let comment_id = resp
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(json!({"ok": true, "issue_key": key, "comment_id": comment_id, "comment": resp}))
}

fn comment_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let comment_id = opt_str(&input, "comment_id").trim();
    if comment_id.is_empty() {
        return Err("`comment_id` (string) required".into());
    }
    let body = opt_str(&input, "body_markdown").trim();
    if body.is_empty() {
        return Err("`body_markdown` (string) required".into());
    }
    let resp = jsend(
        host,
        "PUT",
        &format!(
            "/issue/{}/comment/{}",
            urlencode(&key),
            urlencode(comment_id)
        ),
        &json!({"body": markdown_to_adf(body)}),
    )?;
    let resolved = {
        let from_resp = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if from_resp.is_empty() {
            comment_id.to_string()
        } else {
            from_resp.to_string()
        }
    };
    Ok(json!({"ok": true, "issue_key": key, "comment_id": resolved, "comment": resp}))
}

fn comment_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let comment_id = opt_str(&input, "comment_id").trim();
    if comment_id.is_empty() {
        return Err("`comment_id` (string) required".into());
    }
    jsend_noresp(
        host,
        "DELETE",
        &format!(
            "/issue/{}/comment/{}",
            urlencode(&key),
            urlencode(comment_id)
        ),
        None,
    )?;
    Ok(json!({"ok": true, "issue_key": key, "comment_id": comment_id}))
}

fn comment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut path = format!(
        "/issue/{}/comment?maxResults={}",
        urlencode(&key),
        clamp_limit(&input, &["limit"], 20, 100)
    );
    let start_at = input.get("start_at").and_then(|v| v.as_i64()).unwrap_or(0);
    if start_at > 0 {
        path.push_str(&format!("&startAt={start_at}"));
    }
    let order = opt_str(&input, "order").trim();
    if !order.is_empty() {
        path.push_str(&format!("&orderBy={}", urlencode(order)));
    }
    let page = jget(host, &path)?;
    let comments = page.get("comments").cloned().unwrap_or(json!([]));
    let count = comments.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(json!({
        "issue_key": key,
        "count": count,
        "total": page.get("total").cloned().unwrap_or(json!(count)),
        "start_at": page.get("startAt").cloned().unwrap_or(json!(0)),
        "comments": comments,
    }))
}

fn attachment_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let blob_ref = opt_str(&input, "blob_ref").trim();
    if blob_ref.is_empty() {
        return Err("`blob_ref` (host blob ref) required".into());
    }
    let bytes = host.blob_get(blob_ref)?;
    let filename = {
        let f = opt_str(&input, "filename").trim();
        if f.is_empty() {
            "attachment"
        } else {
            f
        }
    };
    let content_type = {
        let c = opt_str(&input, "content_type").trim();
        if c.is_empty() {
            "application/octet-stream"
        } else {
            c
        }
    };
    // Assemble the multipart/form-data body as RAW BYTES (the reference uses mime/multipart), so binary
    // attachments round-trip byte-exact — no `from_utf8_lossy`. Upload via the byte-exact http_bytes
    // path with a non-binary response (we want the JSON attachment list back as text). The byte-exact
    // path takes a URL (there is no `http_bytes_ref`), so it works on the cloud_id gateway; on the site
    // (ref) path `api_url` errors rather than re-deriving the site URL via a `jira.endpoint` handback.
    let boundary = "----fluxjiraFormBoundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let (url, purpose) = api_url(host, &format!("/issue/{}/attachments", urlencode(&key)))?;
    let content_type_header = format!("multipart/form-data; boundary={boundary}");
    let resp = host.http_bytes(
        "POST",
        &url,
        Some(purpose),
        &[
            ("Accept", "application/json"),
            ("content-type", content_type_header.as_str()),
            ("X-Atlassian-Token", "no-check"),
        ],
        Some(&body),
        false,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "jira attachment upload → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }
    let attachments: Value = serde_json::from_slice(&resp.bytes)
        .map_err(|e| format!("attachment upload response not JSON: {e}"))?;
    Ok(json!({"ok": true, "issue_key": key, "attachments": attachments}))
}

fn attachment_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = opt_str(&input, "attachment_id").trim();
    if id.is_empty() {
        return Err("`attachment_id` (string) required".into());
    }
    let (url, purpose) = api_url(host, &format!("/attachment/content/{}", urlencode(id)))?;
    // Byte-exact download: binary_response=true returns the raw bytes (no UTF-8 corruption).
    let resp = host.http_bytes("GET", &url, Some(purpose), &[], None, true)?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("jira attachment get → {}", resp.status));
    }
    let bytes = resp.bytes;
    let filename = {
        let f = opt_str(&input, "filename").trim();
        if f.is_empty() {
            id
        } else {
            f
        }
    };
    let blob_ref = host.blob_put(filename, &bytes)?;
    Ok(json!({
        "id": id,
        "filename": filename,
        "mime_type": opt_str(&input, "mime_type"),
        "size": bytes.len(),
        "blob_ref": blob_ref,
    }))
}

fn attachment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let issue = jget(
        host,
        &format!("/issue/{}?fields=attachment", urlencode(&key)),
    )?;
    let attachments = issue
        .get("fields")
        .and_then(|f| f.get("attachment"))
        .cloned()
        .unwrap_or(json!([]));
    let count = attachments.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(json!({"issue_key": key, "count": count, "attachments": attachments}))
}

fn attachment_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = opt_str(&input, "attachment_id").trim();
    if id.is_empty() {
        return Err("`attachment_id` (string) required".into());
    }
    jsend_noresp(
        host,
        "DELETE",
        &format!("/attachment/{}", urlencode(id)),
        None,
    )?;
    Ok(json!({"ok": true, "attachment_id": id}))
}

fn issue_link_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let to_key = opt_str(&input, "to_key").trim();
    let link_type = opt_str(&input, "type").trim();
    if to_key.is_empty() || link_type.is_empty() {
        return Err("key, to_key, and type are required".into());
    }
    // Reference `LinkIssues`: "key <verb> to_key" posts the type's name with inwardIssue=key,
    // outwardIssue=to_key.
    jsend_noresp(
        host,
        "POST",
        "/issueLink",
        Some(&json!({
            "type": {"name": link_type},
            "inwardIssue": {"key": key},
            "outwardIssue": {"key": to_key},
        })),
    )?;
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    let links = issue
        .get("fields")
        .and_then(|f| f.get("issuelinks"))
        .cloned()
        .unwrap_or(json!([]));
    Ok(json!({"ok": true, "key": key, "to_key": to_key, "type": link_type, "links": links}))
}

fn user_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = opt_str(&input, "query").trim();
    let limit = clamp_limit(&input, &["limit"], 20, 100);
    let users = jget(
        host,
        &format!(
            "/user/search?query={}&maxResults={limit}&startAt=0",
            urlencode(query)
        ),
    )?;
    let count = users.as_array().map(|a| a.len()).unwrap_or(0);
    contribute_users(host, &users);
    Ok(json!({"users": users, "count": count}))
}

/// Percent-encode a query/path component: unreserved chars (`alnum -_.~`) pass through, all else `%XX`.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> MockHost {
        // JSON IO resolves the named ref (`with_endpoint_ref`); byte-exact attachment IO has no ref
        // variant and materializes the site base via the host endpoint resolver (`with_endpoint`).
        MockHost::default()
            .with_endpoint_ref("jira.endpoint", "https://x.atlassian.net")
            .with_endpoint("jira.endpoint", "https://x.atlassian.net")
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
        // With a cloud_id configured the base URL becomes the api.atlassian.com gateway.
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_endpoint("jira.cloud_id", "cloud-123")
            .with_http(
                "https://api.atlassian.com/ex/jira/cloud-123/rest/api/3/myself",
                json!({"accountId": "acc-1"}),
            );
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
        // upload uses the URL-based `http_bytes` (no `http_bytes_ref`); on the site path it
        // materializes the base URL via the host endpoint resolver — exercised here with no cloud_id.
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
        // Non-UTF-8 download bytes must survive into the blob store unchanged. Byte-exact download uses
        // the URL-based `http_bytes` (no `http_bytes_ref`); on the site path it materializes the base
        // URL via the host endpoint resolver — exercised here with no cloud_id.
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
        assert_eq!(m.operations.len(), 21);
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
}
