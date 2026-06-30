//! `confluence` — a flux integration plugin for the Atlassian Confluence Cloud REST API
//! (v1, `/wiki/rest/api`): page CRUD + search/list/show, page comments, attachments (via the host blob
//! store), user search, an auth `test`, and an `index.build` that contributes page + user datasource
//! records.
//!
//! ## Auth (faithful to the fluxplane reference, with a Basic fallback)
//!
//! fluxplane declares a `bearer_token` auth method (purpose `api_token`) and an optional `cloud_id`,
//! and routes every request through the host as `Authorization: Bearer <api_token>` against the
//! Atlassian gateway `https://api.atlassian.com/ex/confluence/{cloud_id}` when a cloud id is set, else
//! against the configured `endpoint_ref` (see `client.go:45-56`, `do`/`getBytes`, `manifest.go:86-95`).
//!
//! This plugin re-ports that and keeps HTTP Basic (`email:api_token`) as a fallback (user-confirmed),
//! selecting at request time — the plugin never builds an `Authorization` header itself:
//!   * `cloud_id` resolvable (`ATLASSIAN_CLOUD_ID`/`CONFLUENCE_CLOUD_ID`) → Bearer (`api_token`) against
//!     the gateway base `https://api.atlassian.com/ex/confluence/{cloud_id}`.
//!   * else an email is configured (`CONFLUENCE_EMAIL`/`ATLASSIAN_EMAIL`) → Basic (`basic`) against the
//!     `confluence.endpoint` site URL.
//!   * else → Bearer (`api_token`) against the `confluence.endpoint` site URL.
//!
//! Three auth methods back this: `api_token` (Bearer secret), `basic` (Basic, email user_env + token
//! secret), and `cloud_id` (Bearer, used only to *resolve* the cloud id at request time — never injected).

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("confluence", "0.1.0")
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["api.atlassian.com".into()],
            private_hosts: vec!["*".into()],
            blob: true,
            // The api-token + email + cloud-id env keys must be granted secrets so the host can resolve
            // them by purpose. The email is *also* a Basic `user_env` (config) — it is granted here only
            // so the request-time Basic-vs-Bearer selection can probe whether it is set.
            secrets: vec![
                "CONFLUENCE_API_TOKEN".into(),
                "ATLASSIAN_API_TOKEN".into(),
                "CONFLUENCE_EMAIL".into(),
                "ATLASSIAN_EMAIL".into(),
                "ATLASSIAN_CLOUD_ID".into(),
                "CONFLUENCE_CLOUD_ID".into(),
            ],
            ..Default::default()
        })
        // Primary (reference): Bearer <api_token>.
        .auth(AuthMethod::bearer(
            "api_token",
            vec!["CONFLUENCE_API_TOKEN".into(), "ATLASSIAN_API_TOKEN".into()],
        ))
        // Fallback: Basic base64(email:api_token) against the site URL.
        .auth(AuthMethod::basic(
            "basic",
            vec!["CONFLUENCE_EMAIL".into(), "ATLASSIAN_EMAIL".into()],
            vec!["CONFLUENCE_API_TOKEN".into(), "ATLASSIAN_API_TOKEN".into()],
        ))
        // Atlassian Cloud id (resolved to select the gateway base; never injected as auth).
        .auth(AuthMethod::bearer(
            "cloud_id",
            vec!["ATLASSIAN_CLOUD_ID".into(), "CONFLUENCE_CLOUD_ID".into()],
        ))
        // The Basic email, exposed as a resolvable purpose so the request-time selector can probe
        // whether an email is configured (the Basic injection itself reads it from the `basic` method's
        // `user_env`). Never injected as auth.
        .auth(AuthMethod::bearer(
            "basic_email",
            vec!["CONFLUENCE_EMAIL".into(), "ATLASSIAN_EMAIL".into()],
        ))
        .endpoint(EndpointSpec {
            name: "confluence.endpoint".into(),
            env: vec![
                "CONFLUENCE_URL".into(),
                "ATLASSIAN_URL".into(),
                "ATLASSIAN_SITE_URL".into(),
            ],
            http_hosts: Vec::new(),
            description: "Confluence Cloud base URL (e.g. https://site.atlassian.net)".into(),
        })
        .datasource(ds("confluence.pages", "confluence.page", "Confluence pages."))
        .datasource(ds("confluence.users", "confluence.user", "Confluence users."))
        .operation(
            read_op(
                "confluence.test",
                "Test Confluence authentication by fetching the current user.",
                json!({"type": "object", "properties": {}}),
            ),
            test,
        )
        .operation(
            read_op(
                "confluence.index.build",
                "Build Confluence page and user index records (contributes to the datasource index).",
                json!({"type": "object", "properties": {
                    "page_limit": {"type": "integer", "description": "page fetch size (default 100, max 100)"},
                    "page_query": {"type": "string", "description": "page text query"},
                    "page_cql": {"type": "string", "description": "page CQL query"},
                    "title": {"type": "string", "description": "exact page title filter"},
                    "space_key": {"type": "string", "description": "space key filter (e.g. OPS)"},
                    "user_limit": {"type": "integer", "description": "user fetch size (default 100, max 100)"},
                    "user_query": {"type": "string", "description": "user search query"},
                    "user_cql": {"type": "string", "description": "user CQL query"}
                }}),
            ),
            index_build,
        )
        .operation(
            write_op(
                "confluence.page.attachment.add",
                "Upload an attachment to a Confluence page from a host blob ref.",
                json!({"type": "object", "properties": {
                    "page_id": {"type": "string", "description": "Confluence page id"},
                    "id": {"type": "string", "description": "alias for page_id"},
                    "blob_ref": {"type": "string", "description": "host blob ref holding the file bytes"},
                    "filename": {"type": "string", "description": "filename shown in Confluence (defaults to the blob name)"},
                    "content_type": {"type": "string", "description": "attachment MIME type"}
                }, "required": ["blob_ref"]}),
            ),
            attachment_add,
        )
        .operation(
            read_op(
                "confluence.page.attachment.list",
                "List the attachments on a Confluence page.",
                json!({"type": "object", "properties": {
                    "page_id": {"type": "string", "description": "Confluence page id"},
                    "id": {"type": "string", "description": "alias for page_id"}
                }, "required": ["page_id"]}),
            ),
            attachment_list,
        )
        .operation(
            read_op(
                "confluence.attachment.get",
                "Download a Confluence attachment into a host blob (or return its metadata).",
                json!({"type": "object", "properties": {
                    "attachment_id": {"type": "string", "description": "Confluence attachment id"},
                    "page_id": {"type": "string", "description": "page id for the download endpoint"},
                    "download": {"type": "boolean", "description": "download bytes into a blob (default true)"}
                }, "required": ["attachment_id"]}),
            ),
            attachment_get,
        )
        .operation(
            write_op(
                "confluence.attachment.delete",
                "Delete a Confluence attachment.",
                json!({"type": "object", "properties": {
                    "attachment_id": {"type": "string", "description": "Confluence attachment id"}
                }, "required": ["attachment_id"]}),
            ),
            attachment_delete,
        )
        .operation(
            write_op(
                "confluence.page.create",
                "Create a Confluence page from Markdown (body_markdown) or storage XHTML (body_storage).",
                json!({"type": "object", "properties": {
                    "space_key": {"type": "string", "description": "Confluence space key"},
                    "title": {"type": "string", "description": "page title"},
                    "body_markdown": {"type": "string", "description": "page body as Markdown (preferred)"},
                    "body_storage": {"type": "string", "description": "page body as storage-format XHTML"},
                    "parent_id": {"type": "string", "description": "optional parent page id"}
                }, "required": ["space_key", "title"]}),
            ),
            page_create,
        )
        .operation(
            write_op(
                "confluence.page.update",
                "Update a page's title and/or body (replaces the whole body), bumping the version.",
                json!({"type": "object", "properties": {
                    "id": {"type": "string", "description": "page id"},
                    "page_id": {"type": "string", "description": "alias for id"},
                    "title": {"type": "string", "description": "new title (empty keeps the current one)"},
                    "body_markdown": {"type": "string", "description": "new body as Markdown (preferred)"},
                    "body_storage": {"type": "string", "description": "new body as storage-format XHTML"},
                    "body_format": {"type": "string", "description": "returned body format: markdown (default), storage, or both", "enum": ["markdown", "storage", "both"]}
                }, "required": ["id"]}),
            ),
            page_update,
        )
        .operation(
            write_op(
                "confluence.page.delete",
                "Delete a Confluence page.",
                json!({"type": "object", "properties": {
                    "id": {"type": "string", "description": "page id"},
                    "page_id": {"type": "string", "description": "alias for id"}
                }, "required": ["id"]}),
            ),
            page_delete,
        )
        .operation(
            read_op(
                "confluence.page.search",
                "Search Confluence pages with CQL (or a text/title query).",
                json!({"type": "object", "properties": {
                    "cql": {"type": "string", "description": "raw CQL query"},
                    "query": {"type": "string", "description": "free-text query (text ~ ...)"},
                    "title": {"type": "string", "description": "title filter (title ~ ...)"},
                    "limit": {"type": "integer", "description": "max results (default 25, max 100)"}
                }}),
            ),
            page_search,
        )
        .operation(
            read_op(
                "confluence.page.list",
                "List Confluence pages, filterable by space and title.",
                json!({"type": "object", "properties": {
                    "space_key": {"type": "string", "description": "space key filter"},
                    "title": {"type": "string", "description": "exact title filter"},
                    "status": {"type": "string", "description": "page status (default current)"},
                    "limit": {"type": "integer", "description": "max results (default 25, max 100)"},
                    "start": {"type": "string", "description": "pagination offset"}
                }}),
            ),
            page_list,
        )
        .operation(
            read_op(
                "confluence.page.show",
                "Show one page by id with its body as Markdown (body_format selects markdown/storage/both).",
                json!({"type": "object", "properties": {
                    "id": {"type": "string", "description": "page id"},
                    "page_id": {"type": "string", "description": "alias for id"},
                    "body_format": {"type": "string", "description": "body format: markdown (default), storage, or both", "enum": ["markdown", "storage", "both"]}
                }, "required": ["id"]}),
            ),
            page_show,
        )
        .operation(
            read_op(
                "confluence.page.comment.list",
                "List the comments on a Confluence page as Markdown (body_format selects markdown/storage/both).",
                json!({"type": "object", "properties": {
                    "page_id": {"type": "string", "description": "Confluence page id"},
                    "id": {"type": "string", "description": "alias for page_id"},
                    "limit": {"type": "integer", "description": "max comments (default 25, max 100)"},
                    "start": {"type": "string", "description": "pagination offset"},
                    "body_format": {"type": "string", "description": "body format: markdown (default), storage, or both", "enum": ["markdown", "storage", "both"]}
                }, "required": ["page_id"]}),
            ),
            comment_list,
        )
        .operation(
            write_op(
                "confluence.page.comment.add",
                "Add a comment to a Confluence page (Markdown or storage XHTML).",
                json!({"type": "object", "properties": {
                    "page_id": {"type": "string", "description": "Confluence page id"},
                    "id": {"type": "string", "description": "alias for page_id"},
                    "body_markdown": {"type": "string", "description": "comment body as Markdown (preferred)"},
                    "body": {"type": "string", "description": "alias for body_markdown"},
                    "body_storage": {"type": "string", "description": "comment body as storage-format XHTML"}
                }, "required": ["page_id"]}),
            ),
            comment_add,
        )
        .operation(
            read_op(
                "confluence.user.search",
                "Search Confluence users (returns the current user when no query is given).",
                json!({"type": "object", "properties": {
                    "query": {"type": "string", "description": "user full-name query"},
                    "cql": {"type": "string", "description": "raw user CQL query"},
                    "limit": {"type": "integer", "description": "max results (default 25, max 100)"}
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

// ---------------------------------------------------------------------------
// Auth + base-URL selection (faithful to fluxplane's client.go `do`/NewLiveClient)
// ---------------------------------------------------------------------------

/// Where a request is routed. The cloud_id path builds a CONSTRUCTED gateway URL from a config value
/// (not a declared endpoint), so it can only be expressed as an absolute URL; the site path is the
/// declared manifest endpoint, addressed by its named reference so the host (not the plugin) holds the
/// URL.
enum Base {
    /// A constructed absolute base URL (no trailing slash) — the Atlassian gateway. JSON IO joins
    /// `path` onto it; byte IO (`http_bytes`) uses it directly.
    Url(String),
    /// A named manifest endpoint reference (`confluence.endpoint`) the host resolves to the site base.
    Ref(&'static str),
}

/// The auth purpose + routing base for one request. fluxplane chooses the gateway base when a cloud id
/// is resolvable and otherwise the endpoint_ref; flux additionally falls back to Basic against the site
/// URL when an email is configured but no cloud id is.
struct AuthCtx {
    /// How the request is routed (constructed gateway URL vs. named site endpoint ref).
    base: Base,
    /// The auth-method purpose the host injects (`api_token` Bearer or `basic`).
    purpose: &'static str,
}

/// Resolve the per-request auth + routing base, mirroring fluxplane's `NewLiveClient` + `do` selection.
fn auth_ctx(host: &mut Host) -> Result<AuthCtx, String> {
    // cloud_id → Atlassian gateway + Bearer (the reference's primary path). The gateway base is built
    // from the cloud-id config value, so it stays an absolute URL (not a declared endpoint ref).
    if let Ok(cloud) = host.secret("cloud_id") {
        let cloud = cloud.trim();
        if !cloud.is_empty() {
            return Ok(AuthCtx {
                base: Base::Url(format!(
                    "https://api.atlassian.com/ex/confluence/{}",
                    urlencode(cloud)
                )),
                purpose: "api_token",
            });
        }
    }
    // No cloud id: requests target the configured site URL by named endpoint reference — the host
    // resolves the ref to a base and joins the path, so the plugin never holds the URL.
    // An email being configured selects Basic (fallback); otherwise Bearer against the site URL.
    let purpose = if email_is_set(host) {
        "basic"
    } else {
        "api_token"
    };
    Ok(AuthCtx {
        base: Base::Ref("confluence.endpoint"),
        purpose,
    })
}

/// Whether a Basic-auth email is configured. Probed via the `basic_email` purpose (the same email env
/// the `basic` method uses as its `user_env`); the actual Basic injection still resolves the email from
/// that `user_env` host-side.
fn email_is_set(host: &mut Host) -> bool {
    host.secret("basic_email")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Input + URL helpers
// ---------------------------------------------------------------------------

/// First non-empty string among `keys` in `input`.
fn first_str<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| {
        input
            .get(*k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    })
}

/// Like [`first_str`] but errors (naming the primary key) when none is present.
fn req_first<'a>(input: &'a Value, keys: &[&str]) -> Result<&'a str, String> {
    first_str(input, keys).ok_or_else(|| format!("`{}` (string) required", keys[0]))
}

/// A `limit`-style integer clamped to `1..=100`, defaulting to `default`.
fn clamp_limit(input: &Value, key: &str, default: i64) -> i64 {
    input
        .get(key)
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
        .clamp(1, 100)
}

/// Percent-encode a path/query component: unreserved chars (`alnum -_.~`) pass through, all else `%XX`.
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

/// Resolve a [`Base`] + relative `path` into an absolute URL for the **byte-exact** HTTP path
/// ([`Host::http_bytes`]), which the JSON ref helpers can't cover: `http_bytes` has no ref-based
/// variant, so a byte upload/download needs a concrete URL. The gateway base is already absolute; the
/// site path materializes its base URL via the host's endpoint resolver. This is the one residual
/// `host.endpoint` use — confined to attachment byte IO — until host-kit grows an `http_bytes_ref`.
fn byte_io_url(host: &mut Host, base: &Base, path: &str) -> Result<String, String> {
    match base {
        Base::Url(b) => Ok(format!("{b}{path}")),
        Base::Ref(r) => {
            let site = host.endpoint(r)?;
            Ok(format!("{}{path}", site.trim_end_matches('/')))
        }
    }
}

/// GET `path` with the request-time auth (cloud_id→Bearer/gateway, else Basic/Bearer/site); returns
/// the parsed JSON. Site requests address the endpoint by reference (host-resolved URL); gateway
/// requests join the path onto the constructed gateway URL.
fn cf_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let ctx = auth_ctx(host)?;
    match ctx.base {
        Base::Ref(r) => host.get_json_ref(r, path, Some(ctx.purpose)),
        Base::Url(b) => host.get_json(&format!("{b}{path}"), Some(ctx.purpose)),
    }
}

/// Send a JSON body to `path` with the request-time auth; returns the parsed JSON.
fn cf_send(host: &mut Host, method: &str, path: &str, body: &Value) -> Result<Value, String> {
    let ctx = auth_ctx(host)?;
    match ctx.base {
        Base::Ref(r) => host.send_json_ref(r, method, path, Some(ctx.purpose), body),
        Base::Url(b) => host.send_json(method, &format!("{b}{path}"), Some(ctx.purpose), body),
    }
}

/// DELETE `path` (Confluence returns 204 / an empty body, so we don't parse it).
fn cf_delete(host: &mut Host, path: &str) -> Result<(), String> {
    let ctx = auth_ctx(host)?;
    let resp = match ctx.base {
        Base::Ref(r) => host.http_ref(r, "DELETE", path, Some(ctx.purpose), None)?,
        Base::Url(b) => host.http(
            "DELETE",
            &format!("{b}{path}"),
            Some(ctx.purpose),
            &[],
            None,
        )?,
    };
    if !resp.is_success() {
        return Err(format!(
            "confluence DELETE {path} → {} {}",
            resp.status, resp.body
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Body + CQL helpers
// ---------------------------------------------------------------------------

/// XML-escape text content for inclusion in Confluence storage XHTML.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// XML-escape a value destined for a double-quoted attribute (also escapes `"`).
fn xml_attr_escape(s: &str) -> String {
    xml_escape(s).replace('"', "&quot;")
}

/// Markdown → Confluence storage-format XHTML, mirroring fluxplane's `MarkdownToStorage`
/// (goldmark + GFM table/strikethrough, XHTML output, fenced code → the Confluence `code` macro).
///
/// This walks `pulldown-cmark` events and emits storage XHTML directly so we can match the
/// reference's construct coverage and output shape: headings, paragraphs, bold/italic/
/// strikethrough/inline-code, links, images, ordered/unordered (incl. nested) lists, blockquotes,
/// fenced code blocks (language → `<ac:structured-macro ac:name="code">`), GFM tables, and
/// horizontal rules. Raw inline/block HTML in the source is dropped (callers wanting hand-authored
/// macros pass `body_storage` directly), matching goldmark's default.
fn md_to_storage(md: &str) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(md, opts);

    let mut out = String::with_capacity(md.len() + md.len() / 4);
    // State for fenced code blocks: language + buffered (un-escaped) code text.
    let mut code_lang: Option<String> = None;
    let mut code_buf = String::new();
    // GFM table state: a header cell renders as <th>, a body cell as <td>.
    let mut in_table_head = false;

    let heading_level = |l: HeadingLevel| -> u8 {
        match l {
            HeadingLevel::H1 => 1,
            HeadingLevel::H2 => 2,
            HeadingLevel::H3 => 3,
            HeadingLevel::H4 => 4,
            HeadingLevel::H5 => 5,
            HeadingLevel::H6 => 6,
        }
    };

    for ev in parser {
        match ev {
            Event::Start(Tag::Paragraph) => out.push_str("<p>"),
            Event::End(TagEnd::Paragraph) => out.push_str("</p>"),
            Event::Start(Tag::Heading { level, .. }) => {
                out.push_str(&format!("<h{}>", heading_level(level)))
            }
            Event::End(TagEnd::Heading(level)) => {
                out.push_str(&format!("</h{}>", heading_level(level)))
            }
            Event::Start(Tag::BlockQuote(_)) => out.push_str("<blockquote>"),
            Event::End(TagEnd::BlockQuote(_)) => out.push_str("</blockquote>"),
            Event::Start(Tag::List(Some(_))) => out.push_str("<ol>"),
            Event::Start(Tag::List(None)) => out.push_str("<ul>"),
            Event::End(TagEnd::List(true)) => out.push_str("</ol>"),
            Event::End(TagEnd::List(false)) => out.push_str("</ul>"),
            Event::Start(Tag::Item) => out.push_str("<li>"),
            Event::End(TagEnd::Item) => out.push_str("</li>"),
            Event::Start(Tag::Emphasis) => out.push_str("<em>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</em>"),
            Event::Start(Tag::Strong) => out.push_str("<strong>"),
            Event::End(TagEnd::Strong) => out.push_str("</strong>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),
            Event::Start(Tag::Link { dest_url, .. }) => {
                out.push_str(&format!("<a href=\"{}\">", xml_attr_escape(&dest_url)))
            }
            Event::End(TagEnd::Link) => out.push_str("</a>"),
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                // Storage images point at a remote URL via <ri:url>; alt text is captured by the
                // surrounding text events, so close the tag immediately and skip the alt content.
                out.push_str(&format!(
                    "<ac:image{}><ri:url ri:value=\"{}\" /></ac:image>",
                    if title.is_empty() {
                        String::new()
                    } else {
                        format!(" ac:title=\"{}\"", xml_attr_escape(&title))
                    },
                    xml_attr_escape(&dest_url)
                ));
            }
            Event::End(TagEnd::Image) => {}
            Event::Start(Tag::CodeBlock(kind)) => {
                code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let lang = lang.trim();
                        if lang.is_empty() {
                            Some(String::new())
                        } else {
                            // goldmark/Confluence key off only the first token of the info string.
                            Some(lang.split_whitespace().next().unwrap_or("").to_string())
                        }
                    }
                    CodeBlockKind::Indented => Some(String::new()),
                };
                code_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                let lang = code_lang.take().unwrap_or_default();
                out.push_str(&storage_code_macro(&lang, &code_buf));
                code_buf.clear();
            }
            Event::Start(Tag::Table(_aligns)) => out.push_str("<table><tbody>"),
            Event::End(TagEnd::Table) => out.push_str("</tbody></table>"),
            Event::Start(Tag::TableHead) => {
                in_table_head = true;
                out.push_str("<tr>");
            }
            Event::End(TagEnd::TableHead) => {
                in_table_head = false;
                out.push_str("</tr>");
            }
            Event::Start(Tag::TableRow) => out.push_str("<tr>"),
            Event::End(TagEnd::TableRow) => out.push_str("</tr>"),
            Event::Start(Tag::TableCell) => {
                out.push_str(if in_table_head { "<th>" } else { "<td>" })
            }
            Event::End(TagEnd::TableCell) => {
                out.push_str(if in_table_head { "</th>" } else { "</td>" })
            }
            Event::Text(t) => {
                if code_lang.is_some() {
                    code_buf.push_str(&t);
                } else {
                    out.push_str(&xml_escape(&t));
                }
            }
            Event::Code(t) => {
                out.push_str("<code>");
                out.push_str(&xml_escape(&t));
                out.push_str("</code>");
            }
            Event::SoftBreak => {
                if code_lang.is_some() {
                    code_buf.push('\n');
                } else {
                    out.push('\n');
                }
            }
            Event::HardBreak => out.push_str("<br />"),
            Event::Rule => out.push_str("<hr />"),
            // Raw HTML and other inline events: drop, matching goldmark's default (no raw HTML
            // passthrough). Footnotes/math/tasklist markers are not enabled.
            _ => {}
        }
    }
    out
}

/// Render a fenced code block as a Confluence `code` structured macro, splitting any `]]>`
/// sequence so it can live inside CDATA (mirrors fluxplane's `storageCodeMacro`).
fn storage_code_macro(language: &str, code: &str) -> String {
    let code = code
        .trim_end_matches('\n')
        .replace("]]>", "]]]]><![CDATA[>");
    let mut b = String::new();
    b.push_str("<ac:structured-macro ac:name=\"code\">");
    let language = language.trim();
    if !language.is_empty() {
        b.push_str(&format!(
            "<ac:parameter ac:name=\"language\">{}</ac:parameter>",
            xml_escape(language)
        ));
    }
    b.push_str(&format!(
        "<ac:plain-text-body><![CDATA[{code}]]></ac:plain-text-body></ac:structured-macro>"
    ));
    b
}

/// Resolve the storage-format body from the caller's choice, rejecting ambiguous input
/// (fluxplane `resolveBodyStorage`).
fn resolve_body(markdown: Option<&str>, storage: Option<&str>) -> Result<String, String> {
    match (markdown, storage) {
        (Some(_), Some(_)) => Err("provide only one of body_markdown or body_storage".into()),
        (Some(m), None) => Ok(md_to_storage(m)),
        (None, Some(s)) => Ok(s.to_string()),
        (None, None) => Ok(String::new()),
    }
}

/// The rich-text body format a caller asked for (fluxplane `bodyFormat`). The default keeps callers
/// away from raw storage XHTML by rendering Markdown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyFormat {
    Markdown,
    Storage,
    Both,
}

fn parse_body_format(input: &Value) -> BodyFormat {
    match first_str(input, &["body_format"]).map(|s| s.to_ascii_lowercase()) {
        Some(s) if s == "storage" => BodyFormat::Storage,
        Some(s) if s == "both" => BodyFormat::Both,
        _ => BodyFormat::Markdown,
    }
}

/// Render the storage XHTML body of a page object into the representation(s) the caller asked for,
/// mirroring fluxplane's `Page.renderBody`: it lifts `body.storage.value` into `body_markdown`
/// (default) and/or `body_storage`, drops the raw `body`, and folds the nested space into flat fields.
fn render_page_body(page: &mut Value, format: BodyFormat) {
    if !page.is_object() {
        return;
    }
    // Fold the nested v1 space object into flat fields.
    if let Some(space) = page.get("space").cloned() {
        if let Some(key) = space.get("key").and_then(|v| v.as_str()) {
            if page.get("spaceKey").and_then(|v| v.as_str()).is_none() {
                page["spaceKey"] = json!(key);
            }
        }
        if let Some(id) = space.get("id") {
            if page.get("spaceId").and_then(|v| v.as_str()).is_none() {
                page["spaceId"] = id.clone();
            }
        }
    }
    let storage = page
        .get("body")
        .and_then(|b| b.get("storage"))
        .and_then(|s| s.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(obj) = page.as_object_mut() {
        obj.remove("body");
    }
    if storage.trim().is_empty() {
        return;
    }
    if matches!(format, BodyFormat::Markdown | BodyFormat::Both) {
        page["body_markdown"] = json!(storage_to_markdown(&storage));
    }
    if matches!(format, BodyFormat::Storage | BodyFormat::Both) {
        page["body_storage"] = json!(storage);
    }
}

/// Render a normalized comment object's storage body into Markdown / storage, mirroring fluxplane's
/// `Comment.renderBody`: `body_markdown` is populated by default and `body_storage` dropped unless the
/// caller asked for storage/both.
fn render_comment_body(comment: &mut Value, format: BodyFormat) {
    if !comment.is_object() {
        return;
    }
    let storage = comment
        .get("body_storage")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if matches!(format, BodyFormat::Markdown | BodyFormat::Both) {
        comment["body_markdown"] = json!(storage_to_markdown(&storage));
    }
    if format == BodyFormat::Markdown {
        if let Some(obj) = comment.as_object_mut() {
            obj.remove("body_storage");
        }
    }
}

/// Normalize a v1 `/child/comment` API comment into the flat comment shape fluxplane returns
/// (`commentFromAPI`): id/status/title, the raw storage body, author + timestamps.
fn comment_from_api(raw: &Value) -> Value {
    let body_storage = raw
        .get("body")
        .and_then(|b| b.get("storage"))
        .and_then(|s| s.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let version = raw.get("version");
    let history = raw.get("history");
    let version_when = version
        .and_then(|v| v.get("when"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let version_number = version
        .and_then(|v| v.get("number"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let author_id = history
        .and_then(|h| h.get("createdBy"))
        .and_then(|u| u.get("accountId"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            version
                .and_then(|v| v.get("by"))
                .and_then(|u| u.get("accountId"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    let author_name = history
        .and_then(|h| h.get("createdBy"))
        .and_then(|u| u.get("displayName"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            version
                .and_then(|v| v.get("by"))
                .and_then(|u| u.get("displayName"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    let created_at = history
        .and_then(|h| h.get("createdDate"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(version_when);
    let updated_at = if version_number > 1 { version_when } else { "" };
    json!({
        "id": raw.get("id").cloned().unwrap_or(Value::Null),
        "status": raw.get("status").cloned().unwrap_or(Value::Null),
        "title": raw.get("title").cloned().unwrap_or(Value::Null),
        "body_storage": body_storage,
        "author_id": author_id,
        "author_name": author_name,
        "created_at": created_at,
        "updated_at": updated_at,
        "location": raw
            .get("extensions")
            .and_then(|e| e.get("location"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    })
}

/// One node of a parsed storage fragment: an element (name + attrs + kids) or, when `name` is
/// empty, a text node (`text` set). Mirrors fluxplane's `storageNode`.
#[derive(Default)]
struct StorageNode {
    name: String,
    text: String,
    attrs: Vec<(String, String)>,
    kids: Vec<StorageNode>,
}

impl StorageNode {
    fn attr(&self, key: &str) -> &str {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim())
            .unwrap_or("")
    }

    /// First direct-or-nested child element with the given name.
    fn find(&self, name: &str) -> Option<&StorageNode> {
        for kid in &self.kids {
            if kid.name == name {
                return Some(kid);
            }
            if let Some(found) = kid.find(name) {
                return Some(found);
            }
        }
        None
    }

    /// All text content under this node (text nodes only), concatenated.
    fn collect_text(&self) -> String {
        let mut out = String::new();
        if self.name.is_empty() {
            out.push_str(&self.text);
        }
        for kid in &self.kids {
            out.push_str(&kid.collect_text());
        }
        out
    }

    /// The value of `<ac:parameter ac:name="key">` within a structured macro.
    fn macro_parameter(&self, key: &str) -> String {
        for kid in &self.kids {
            if kid.name == "ac:parameter" && kid.attr("ac:name") == key {
                return kid.collect_text().trim().to_string();
            }
        }
        String::new()
    }
}

/// Parse a storage-format fragment into a node forest. Confluence fragments use undeclared
/// `ac:`/`ri:` prefixes and HTML entities, so we drive quick-xml in a forgiving mode (no
/// end-name checking) and expand the common HTML entities ourselves (quick-xml only knows the
/// five XML predefined ones). Mirrors fluxplane's `parseStorage`.
fn parse_storage(storage: &str) -> Result<Vec<StorageNode>, ()> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(storage);
    let cfg = reader.config_mut();
    cfg.check_end_names = false;
    cfg.expand_empty_elements = false;
    cfg.trim_text(false);

    // Tracking element indices into their parents is awkward in Rust; instead keep a stack of
    // owned nodes (a synthetic root at the bottom) and re-attach each to its parent on close.
    let mut stack: Vec<StorageNode> = vec![StorageNode::default()];
    let mut buf = Vec::new();

    let push_text = |stack: &mut Vec<StorageNode>, text: String| {
        if let Some(parent) = stack.last_mut() {
            parent.kids.push(StorageNode {
                text,
                ..Default::default()
            });
        }
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let node = element_node(&e);
                stack.push(node);
            }
            Ok(Event::Empty(e)) => {
                let node = element_node(&e);
                if let Some(parent) = stack.last_mut() {
                    parent.kids.push(node);
                }
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let node = stack.pop().unwrap();
                    if let Some(parent) = stack.last_mut() {
                        parent.kids.push(node);
                    }
                }
            }
            Ok(Event::Text(t)) => {
                let raw = t.into_inner();
                push_text(&mut stack, decode_storage_text(&raw));
            }
            Ok(Event::CData(c)) => {
                let raw = c.into_inner();
                push_text(&mut stack, String::from_utf8_lossy(&raw).into_owned());
            }
            Ok(Event::GeneralRef(r)) => {
                // quick-xml 0.41 emits entity/character references (`&nbsp;`, `&amp;`, `&#x2014;`,
                // …) as their own event with the name/number between `&` and `;`. Decode it into a
                // text node so it joins the surrounding inline text.
                let name = String::from_utf8_lossy(r.as_ref()).into_owned();
                let decoded = decode_entity(&name).unwrap_or_else(|| format!("&{name};"));
                push_text(&mut stack, decoded);
            }
            Ok(_) => {}
            Err(_) => return Err(()),
        }
        buf.clear();
    }
    // Collapse the stack back into the root (tolerating unclosed tags).
    while stack.len() > 1 {
        let node = stack.pop().unwrap();
        if let Some(parent) = stack.last_mut() {
            parent.kids.push(node);
        }
    }
    Ok(stack.pop().map(|r| r.kids).unwrap_or_default())
}

/// Build a [`StorageNode`] element (name + decoded attributes) from a quick-xml start/empty tag.
fn element_node(e: &quick_xml::events::BytesStart<'_>) -> StorageNode {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for attr in e.attributes().with_checks(false).flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        // Attribute values are XML-escaped; decode the common entities.
        let val = decode_storage_text(&attr.value);
        attrs.push((key, val));
    }
    StorageNode {
        name,
        attrs,
        ..Default::default()
    }
}

/// Decode storage text: XML predefined entities plus the handful of HTML entities Confluence
/// emits (notably `&nbsp;`) and numeric character references. quick-xml only knows the five XML
/// predefined entities, so we expand the rest before/around it.
fn decode_storage_text(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = s[i..].find(';') {
                let entity = &s[i + 1..i + semi];
                if let Some(rep) = decode_entity(entity) {
                    out.push_str(&rep);
                    i += semi + 1;
                    continue;
                }
            }
        }
        // Copy this UTF-8 char whole.
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_char_len(first: u8) -> usize {
    match first {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        b if b >> 3 == 0b11110 => 4,
        _ => 1,
    }
}

/// Resolve a single entity name (without `&`/`;`) to its replacement text, covering the XML
/// predefined entities, the common named HTML entities Confluence emits, and numeric references.
fn decode_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => Some("&".into()),
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        "nbsp" => Some("\u{00a0}".into()),
        "mdash" => Some("\u{2014}".into()),
        "ndash" => Some("\u{2013}".into()),
        "hellip" => Some("\u{2026}".into()),
        "copy" => Some("\u{00a9}".into()),
        "reg" => Some("\u{00ae}".into()),
        "trade" => Some("\u{2122}".into()),
        "deg" => Some("\u{00b0}".into()),
        "laquo" => Some("\u{00ab}".into()),
        "raquo" => Some("\u{00bb}".into()),
        "lsquo" => Some("\u{2018}".into()),
        "rsquo" => Some("\u{2019}".into()),
        "ldquo" => Some("\u{201c}".into()),
        "rdquo" => Some("\u{201d}".into()),
        _ => {
            // Numeric character references: &#NNN; (decimal) or &#xHH; (hex).
            let num = entity.strip_prefix('#')?;
            let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            char::from_u32(code).map(|c| c.to_string())
        }
    }
}

/// Block-level storage elements (everything else is grouped into inline runs / paragraphs).
fn is_storage_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "table"
            | "blockquote"
            | "pre"
            | "hr"
            | "div"
            | "section"
            | "ac:structured-macro"
            | "ac:task-list"
            | "ac:layout"
            | "ac:layout-section"
            | "ac:layout-cell"
            | "ac:rich-text-body"
    )
}

/// An `<ac:link>` rendered as a block-appearance card (its own line) rather than inline.
fn is_storage_block_card(n: &StorageNode) -> bool {
    n.name == "ac:link" && n.attr("ac:card-appearance") == "block"
}

/// Render a sequence of sibling nodes, grouping consecutive inline content into paragraphs and
/// joining blocks with blank lines (fluxplane's `renderStorageBlocks`).
fn render_storage_blocks(nodes: &[StorageNode]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut inline_run: Vec<&StorageNode> = Vec::new();
    let flush = |parts: &mut Vec<String>, inline_run: &mut Vec<&StorageNode>| {
        if inline_run.is_empty() {
            return;
        }
        let text = render_storage_inline(inline_run).trim().to_string();
        if !text.is_empty() {
            parts.push(text);
        }
        inline_run.clear();
    };
    for node in nodes {
        if node.name.is_empty() && node.text.trim().is_empty() {
            continue;
        }
        if is_storage_block(&node.name) || is_storage_block_card(node) {
            flush(&mut parts, &mut inline_run);
            let rendered = render_storage_block(node);
            if !rendered.trim().is_empty() {
                parts.push(rendered);
            }
            continue;
        }
        inline_run.push(node);
    }
    flush(&mut parts, &mut inline_run);
    parts.join("\n\n")
}

fn render_storage_block(n: &StorageNode) -> String {
    match n.name.as_str() {
        "p" => render_storage_inline_nodes(&n.kids).trim().to_string(),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level: usize = n.name[1..].parse().unwrap_or(1);
            format!(
                "{} {}",
                "#".repeat(level),
                render_storage_inline_nodes(&n.kids).trim()
            )
        }
        "ul" => render_storage_list(n, false),
        "ol" => render_storage_list(n, true),
        "blockquote" => prefix_lines(&render_storage_blocks(&n.kids), "> "),
        "pre" => format!("```\n{}\n```", n.collect_text().trim_end_matches('\n')),
        "hr" => "---".to_string(),
        "table" => render_storage_table(n),
        "ac:structured-macro" => render_storage_macro(n),
        "ac:task-list" => render_storage_task_list(n),
        "ac:link" => render_storage_link(n),
        "div" | "section" | "ac:layout" | "ac:layout-section" | "ac:layout-cell"
        | "ac:rich-text-body" => render_storage_blocks(&n.kids),
        _ => render_storage_blocks(&n.kids),
    }
}

fn render_storage_macro(n: &StorageNode) -> String {
    let name = n.attr("ac:name").to_ascii_lowercase();
    match name.as_str() {
        "code" => {
            let body = n
                .find("ac:plain-text-body")
                .map(|p| p.collect_text())
                .unwrap_or_default();
            format!(
                "```{}\n{}\n```",
                n.macro_parameter("language"),
                body.trim_end_matches('\n')
            )
        }
        "info" | "note" | "warning" | "tip" | "panel" | "expand" => {
            let inner = n
                .find("ac:rich-text-body")
                .map(|rich| render_storage_blocks(&rich.kids))
                .unwrap_or_default();
            let mut label = name.to_ascii_uppercase();
            let title = n.macro_parameter("title");
            if !title.is_empty() {
                label.push_str(": ");
                label.push_str(&title);
            }
            prefix_lines(&format!("**{label}**\n\n{inner}"), "> ")
        }
        "status" => {
            let title = n.macro_parameter("title");
            if title.is_empty() {
                String::new()
            } else {
                format!("[{title}]")
            }
        }
        "toc" | "children" | "anchor" => String::new(),
        _ => {
            if let Some(rich) = n.find("ac:rich-text-body") {
                render_storage_blocks(&rich.kids)
            } else if let Some(plain) = n.find("ac:plain-text-body") {
                plain.collect_text().trim().to_string()
            } else {
                String::new()
            }
        }
    }
}

fn render_storage_task_list(n: &StorageNode) -> String {
    let mut lines = Vec::new();
    for task in &n.kids {
        if task.name != "ac:task" {
            continue;
        }
        let complete = task
            .find("ac:task-status")
            .map(|s| s.collect_text().trim() == "complete")
            .unwrap_or(false);
        let marker = if complete { "- [x] " } else { "- [ ] " };
        let body = task
            .find("ac:task-body")
            .map(|b| render_storage_inline_nodes(&b.kids).trim().to_string())
            .unwrap_or_default();
        lines.push(format!("{marker}{body}"));
    }
    lines.join("\n")
}

fn render_storage_list(n: &StorageNode, ordered: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut number = 0;
    for item in &n.kids {
        if item.name != "li" {
            continue;
        }
        number += 1;
        let marker = if ordered {
            format!("{number}. ")
        } else {
            "- ".to_string()
        };
        let indent = " ".repeat(marker.len());
        let content = render_storage_list_item(item);
        for (j, line) in content.split('\n').enumerate() {
            if j == 0 {
                lines.push(format!("{marker}{line}"));
            } else if line.is_empty() {
                lines.push(String::new());
            } else {
                lines.push(format!("{indent}{line}"));
            }
        }
    }
    lines.join("\n")
}

/// Join a list item's blocks tightly (single newline) so nested lists sit directly under their
/// parent line (fluxplane's `renderStorageListItem`).
fn render_storage_list_item(item: &StorageNode) -> String {
    render_storage_blocks(&item.kids).replace("\n\n", "\n")
}

fn render_storage_table(n: &StorageNode) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    fn walk_rows(nodes: &[StorageNode], rows: &mut Vec<Vec<String>>) {
        for node in nodes {
            match node.name.as_str() {
                "tr" => {
                    let mut cells = Vec::new();
                    for cell in &node.kids {
                        if cell.name != "td" && cell.name != "th" {
                            continue;
                        }
                        let text = render_storage_blocks(&cell.kids)
                            .replace('\n', " ")
                            .trim()
                            .replace('|', "\\|");
                        cells.push(text);
                    }
                    rows.push(cells);
                }
                "thead" | "tbody" | "tfoot" => walk_rows(&node.kids, rows),
                _ => {}
            }
        }
    }
    walk_rows(&n.kids, &mut rows);
    if rows.is_empty() {
        return String::new();
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let pad = |row: &[String]| -> Vec<String> {
        let mut r = row.to_vec();
        while r.len() < width {
            r.push(String::new());
        }
        r
    };
    let mut b = String::new();
    b.push_str(&format!("| {} |\n", pad(&rows[0]).join(" | ")));
    let separators: Vec<&str> = vec!["---"; width];
    b.push_str(&format!("| {} |", separators.join(" | ")));
    for row in &rows[1..] {
        b.push_str(&format!("\n| {} |", pad(row).join(" | ")));
    }
    b
}

/// Render inline children (a slice of owned nodes), borrowing them for [`render_storage_inline`].
fn render_storage_inline_nodes(nodes: &[StorageNode]) -> String {
    let refs: Vec<&StorageNode> = nodes.iter().collect();
    render_storage_inline(&refs)
}

fn render_storage_inline(nodes: &[&StorageNode]) -> String {
    let mut b = String::new();
    for n in nodes {
        match n.name.as_str() {
            "" => b.push_str(&collapse_storage_whitespace(&n.text)),
            "strong" | "b" => b.push_str(&format!(
                "**{}**",
                render_storage_inline_nodes(&n.kids).trim()
            )),
            "em" | "i" => b.push_str(&format!(
                "*{}*",
                render_storage_inline_nodes(&n.kids).trim()
            )),
            "s" | "del" | "strike" => b.push_str(&format!(
                "~~{}~~",
                render_storage_inline_nodes(&n.kids).trim()
            )),
            "code" => b.push_str(&format!("`{}`", n.collect_text())),
            "a" => {
                let text = render_storage_inline_nodes(&n.kids).trim().to_string();
                let href = n.attr("href");
                if href.is_empty() {
                    b.push_str(&text);
                } else if text.is_empty() || text == href {
                    b.push_str(href);
                } else {
                    b.push_str(&format!("[{text}]({href})"));
                }
            }
            "br" => b.push('\n'),
            "time" => b.push_str(n.attr("datetime")),
            "ac:link" => b.push_str(&render_storage_link(n)),
            "ac:image" => b.push_str(&render_storage_image(n)),
            "ac:emoticon" => {
                let fallback = n.attr("ac:emoji-fallback");
                if !fallback.is_empty() {
                    b.push_str(fallback);
                } else {
                    let name = n.attr("ac:name");
                    if !name.is_empty() {
                        b.push_str(&format!(":{name}:"));
                    }
                }
            }
            "ac:placeholder" => {} // editor-only hint, not content
            "span" | "u" | "sub" | "sup" | "ac:inline-comment-marker" => {
                b.push_str(&render_storage_inline_nodes(&n.kids))
            }
            _ => {
                if !n.kids.is_empty() {
                    b.push_str(&render_storage_inline_nodes(&n.kids));
                }
            }
        }
    }
    b
}

/// Render an `<ac:link>`: page links by title, user links as `@account-id`, attachment links by
/// filename — preferring an explicit link body (fluxplane's `renderStorageLink`).
fn render_storage_link(n: &StorageNode) -> String {
    let body = if let Some(plain) = n.find("ac:plain-text-link-body") {
        plain.collect_text().trim().to_string()
    } else if let Some(rich) = n.find("ac:link-body") {
        render_storage_inline_nodes(&rich.kids).trim().to_string()
    } else {
        String::new()
    };
    if let Some(page) = n.find("ri:page") {
        if !body.is_empty() {
            return body;
        }
        return page.attr("ri:content-title").to_string();
    }
    if let Some(user) = n.find("ri:user") {
        if !body.is_empty() {
            return body;
        }
        let id = first_non_empty_storage(&[user.attr("ri:account-id"), user.attr("ri:userkey")]);
        if !id.is_empty() {
            return format!("@{id}");
        }
        return "@unknown".to_string();
    }
    if let Some(att) = n.find("ri:attachment") {
        if !body.is_empty() {
            return body;
        }
        return att.attr("ri:filename").to_string();
    }
    body
}

fn render_storage_image(n: &StorageNode) -> String {
    let alt = first_non_empty_storage(&[n.attr("ac:alt"), n.attr("ac:title")]);
    if let Some(att) = n.find("ri:attachment") {
        return format!("![{alt}]({})", att.attr("ri:filename"));
    }
    if let Some(remote) = n.find("ri:url") {
        return format!("![{alt}]({})", remote.attr("ri:value"));
    }
    String::new()
}

/// Fold the insignificant newlines + indentation that pretty-printed storage XML carries inside
/// text nodes, collapsing each run of whitespace-around-a-newline to a single space.
fn collapse_storage_whitespace(text: &str) -> String {
    // Only ASCII space/tab/newline are insignificant layout whitespace (matching the reference's
    // `[ \t]*\n[ \t]*` regex); `\u{00a0}` (`&nbsp;`) and friends are real content and must survive.
    let is_layout_ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r');
    if text.chars().all(is_layout_ws) {
        return if text.is_empty() {
            String::new()
        } else {
            " ".to_string()
        };
    }
    // Collapse `[ \t]*\n[ \t]*` runs to a single space.
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
            // Look ahead: does this whitespace run contain a newline?
            let mut has_newline = ch == '\n' || ch == '\r';
            while let Some(&next) = chars.peek() {
                if next == ' ' || next == '\t' || next == '\n' || next == '\r' {
                    if next == '\n' || next == '\r' {
                        has_newline = true;
                    }
                    chars.next();
                } else {
                    break;
                }
            }
            if has_newline {
                out.push(' ');
            } else {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Prefix every line of `s` with `prefix`; blank lines get the right-trimmed prefix (so blockquote
/// gaps render as `>` not `> `). Mirrors fluxplane's `prefixLines`.
fn prefix_lines(s: &str, prefix: &str) -> String {
    s.split('\n')
        .map(|line| {
            if line.is_empty() {
                prefix.trim_end().to_string()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_non_empty_storage(values: &[&str]) -> String {
    values
        .iter()
        .map(|v| v.trim())
        .find(|v| !v.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Render Confluence storage-format XHTML into readable Markdown, porting fluxplane's
/// `atlassian.StorageToMarkdown`: a forgiving XML walk over the common block/inline constructs
/// (paragraphs, headings, lists incl. nested, tables, blockquotes, rules, links, text effects,
/// code/info/note/warning/tip/panel/expand macros, task lists, page/user/attachment links, images,
/// emoticons, layouts), degrading to text content on unknown elements. Callers wanting the exact
/// XHTML use `body_format: storage`.
fn storage_to_markdown(storage: &str) -> String {
    let s = storage.trim();
    if s.is_empty() {
        return String::new();
    }
    match parse_storage(s) {
        Ok(nodes) => render_storage_blocks(&nodes).trim().to_string(),
        // Forgiving fallback: hand back the raw storage (matches the reference on parse failure).
        Err(()) => s.to_string(),
    }
}

/// Quote + escape a value for a CQL string literal.
fn cql_str(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Build a page CQL query: raw `cql` wins, else `type = page` plus optional text/title clauses.
fn page_cql(cql: Option<&str>, query: Option<&str>, title: Option<&str>) -> String {
    if let Some(c) = cql {
        return c.to_string();
    }
    let mut parts = vec!["type = page".to_string()];
    if let Some(q) = query {
        parts.push(format!("text ~ {}", cql_str(q)));
    }
    if let Some(t) = title {
        parts.push(format!("title ~ {}", cql_str(t)));
    }
    parts.join(" and ")
}

/// Build a user CQL query: raw `cql` wins, else match on full name.
fn user_cql(cql: Option<&str>, query: &str) -> String {
    cql.map(String::from)
        .unwrap_or_else(|| format!("user.fullname ~ {}", cql_str(query)))
}

// ---------------------------------------------------------------------------
// Record extraction + contribution
// ---------------------------------------------------------------------------

/// Normalize a page object from either a `/content` list item (flat) or a `/search` result
/// (nested under `.content`).
fn page_obj(item: &Value) -> Option<Value> {
    let p = item.get("content").unwrap_or(item);
    let id = p
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let space_key = p
        .get("space")
        .and_then(|s| s.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("");
    Some(json!({ "page_id": id, "title": title, "space_key": space_key, "status": status }))
}

/// Collect normalized page objects from a `/content` or `/search` response.
fn collect_pages(result: &Value) -> Vec<Value> {
    result
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(page_obj).collect())
        .unwrap_or_default()
}

/// Normalize a user object from a `/search` result (nested under `.user`) or a bare user object.
fn user_obj(item: &Value) -> Option<Value> {
    let u = item.get("user").unwrap_or(item);
    let id = u
        .get("accountId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let name = first_str(u, &["displayName", "publicName"]).unwrap_or("");
    let email = first_str(u, &["email", "emailAddress"]).unwrap_or("");
    Some(json!({ "account_id": id, "display_name": name, "email": email }))
}

/// Collect normalized user objects from a `/search` response or a single-user (`user/current`) body.
fn collect_users(result: &Value) -> Vec<Value> {
    if let Some(arr) = result.get("results").and_then(|v| v.as_array()) {
        arr.iter().filter_map(user_obj).collect()
    } else {
        user_obj(result).into_iter().collect()
    }
}

/// Contribute `confluence.page` records from normalized page objects; returns the count indexed.
fn contribute_page_values(host: &mut Host, pages: &[Value]) -> usize {
    let records: Vec<Record> = pages
        .iter()
        .filter_map(|p| {
            let id = p.get("page_id").and_then(|v| v.as_str())?;
            let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let key = p.get("space_key").and_then(|v| v.as_str()).unwrap_or("");
            let body = if key.is_empty() {
                title.to_string()
            } else {
                format!("{title} (space {key})")
            };
            Some(Record::new(
                Source::new("confluence"),
                "confluence.page",
                id,
                title,
                body,
            ))
        })
        .collect();
    if records.is_empty() {
        return 0;
    }
    host.contribute(&records).unwrap_or(records.len())
}

/// Contribute `confluence.user` records from normalized user objects; returns the count indexed.
fn contribute_user_values(host: &mut Host, users: &[Value]) -> usize {
    let records: Vec<Record> = users
        .iter()
        .filter_map(|u| {
            let id = u.get("account_id").and_then(|v| v.as_str())?;
            let name = u.get("display_name").and_then(|v| v.as_str()).unwrap_or("");
            let email = u.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let body = if email.is_empty() {
                name.to_string()
            } else {
                format!("{name} <{email}>")
            };
            Some(Record::new(
                Source::new("confluence"),
                "confluence.user",
                id,
                name,
                body,
            ))
        })
        .collect();
    if records.is_empty() {
        return 0;
    }
    host.contribute(&records).unwrap_or(records.len())
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

fn test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let user = cf_get(host, "/wiki/rest/api/user/current")?;
    Ok(json!({ "text": "Confluence auth OK", "status": "ok", "user": user }))
}

fn index_build(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut indexed = 0usize;

    // Pages: a CQL search when a query is given, else an exhaustive (within limit) content list.
    let page_limit = clamp_limit(&input, "page_limit", 100);
    let page_cql_in = first_str(&input, &["page_cql", "cql"]);
    let page_query = first_str(&input, &["page_query", "query"]);
    let title = first_str(&input, &["title"]);
    let page_result = if page_cql_in.is_some() || page_query.is_some() {
        let cql = page_cql(page_cql_in, page_query, title);
        cf_get(
            host,
            &format!(
                "/wiki/rest/api/search?cql={}&limit={page_limit}",
                urlencode(&cql)
            ),
        )?
    } else {
        let mut path = format!(
            "/wiki/rest/api/content?type=page&status=current&limit={page_limit}&expand=version,space"
        );
        if let Some(sk) = first_str(&input, &["space_key"]) {
            path.push_str(&format!("&spaceKey={}", urlencode(sk)));
        }
        if let Some(t) = title {
            path.push_str(&format!("&title={}", urlencode(t)));
        }
        cf_get(host, &path)?
    };
    indexed += contribute_page_values(host, &collect_pages(&page_result));

    // Users: a CQL search when a query is given, else just the current user.
    let user_limit = clamp_limit(&input, "user_limit", 100);
    let user_cql_in = first_str(&input, &["user_cql", "cql"]);
    let user_query = first_str(&input, &["user_query", "query"]);
    let user_result = if user_cql_in.is_some() || user_query.is_some() {
        let cql = user_cql(user_cql_in, user_query.unwrap_or(""));
        cf_get(
            host,
            &format!(
                "/wiki/rest/api/search?cql={}&limit={user_limit}",
                urlencode(&cql)
            ),
        )?
    } else {
        cf_get(host, "/wiki/rest/api/user/current")?
    };
    indexed += contribute_user_values(host, &collect_users(&user_result));

    Ok(json!({ "indexed": indexed }))
}

fn attachment_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let page_id = req_first(&input, &["page_id", "id"])?.to_string();
    let blob_ref = req_first(&input, &["blob_ref"])?.to_string();
    let data = host.blob_get(&blob_ref)?;
    let filename = match first_str(&input, &["filename"]) {
        Some(f) => f.to_string(),
        None => host
            .blob_info(&blob_ref)
            .map(|i| i.name)
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "attachment".to_string()),
    };
    let content_type = first_str(&input, &["content_type"]).unwrap_or("application/octet-stream");

    // Byte-exact multipart upload via the binary HTTP path (`http_bytes` with a base64 body), so
    // non-UTF-8 attachment bytes survive — mirroring fluxplane's multipart `UploadPageAttachment`.
    let boundary = "fluxconfluenceboundary7M2A9";
    let body = multipart_file(boundary, &filename, content_type, &data);
    let ct = format!("multipart/form-data; boundary={boundary}");
    let ctx = auth_ctx(host)?;
    let path = format!(
        "/wiki/rest/api/content/{}/child/attachment",
        urlencode(&page_id)
    );
    // Byte uploads need an absolute URL: `http_bytes` has no ref-based variant. The gateway base is a
    // constructed absolute URL, so it is used directly; the site path must materialize the base URL
    // (the host resolves the named endpoint) since byte IO cannot ride the ref path — this is the one
    // residual `host.endpoint` use, scoped to byte attachment IO, until host-kit gains `http_bytes_ref`.
    let url = byte_io_url(host, &ctx.base, &path)?;
    let resp = host.http_bytes(
        "POST",
        &url,
        Some(ctx.purpose),
        &[
            ("Content-Type", ct.as_str()),
            ("X-Atlassian-Token", "no-check"),
            ("Accept", "application/json"),
        ],
        Some(&body),
        false,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "confluence attachment upload → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }
    let result: Value = serde_json::from_slice(&resp.bytes)
        .map_err(|e| format!("attachment upload response not JSON: {e}"))?;
    let attachments = result.get("results").cloned().unwrap_or_else(|| json!([]));
    Ok(json!({ "ok": true, "page_id": page_id, "attachments": attachments }))
}

/// Build a single-file `multipart/form-data` body as raw bytes (sent verbatim via `http_bytes`), so
/// binary attachment content is byte-exact.
fn multipart_file(boundary: &str, filename: &str, content_type: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn attachment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let page_id = req_first(&input, &["page_id", "id"])?.to_string();
    let result = cf_get(
        host,
        &format!(
            "/wiki/rest/api/content/{}/child/attachment?limit=100",
            urlencode(&page_id)
        ),
    )?;
    let attachments = result.get("results").cloned().unwrap_or_else(|| json!([]));
    let count = attachments.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(json!({ "page_id": page_id, "count": count, "attachments": attachments }))
}

fn attachment_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_first(&input, &["attachment_id"])?.to_string();
    let meta = cf_get(host, &format!("/wiki/rest/api/content/{}", urlencode(&id)))?;
    let filename = first_str(&meta, &["filename", "title"])
        .unwrap_or(&id)
        .to_string();
    let mime = meta
        .get("mediaType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let want_download = input
        .get("download")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !want_download {
        return Ok(
            json!({ "id": id, "filename": filename, "mime_type": mime, "attachment": meta }),
        );
    }

    let dl_path = if let Some(pid) = first_str(&input, &["page_id"]) {
        format!(
            "/wiki/rest/api/content/{}/child/attachment/{}/download",
            urlencode(pid),
            urlencode(&id)
        )
    } else {
        let d = meta
            .get("_links")
            .and_then(|l| l.get("download"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if d.is_empty() {
            return Err("attachment has no download link (pass page_id)".into());
        }
        if d.starts_with("/wiki") {
            d.to_string()
        } else {
            format!("/wiki{d}")
        }
    };

    // Byte-exact download via the binary HTTP path → host blob (mirrors fluxplane's `getBytes`).
    // Same constraint as the upload path: `http_bytes` needs an absolute URL and has no ref variant,
    // so `byte_io_url` materializes the base (gateway directly, site via the host endpoint resolver).
    let ctx = auth_ctx(host)?;
    let url = byte_io_url(host, &ctx.base, &dl_path)?;
    let resp = host.http_bytes("GET", &url, Some(ctx.purpose), &[], None, true)?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "confluence attachment download → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }
    let bytes = resp.bytes;
    let blob_ref = host.blob_put(&filename, &bytes)?;
    Ok(json!({
        "id": id,
        "filename": filename,
        "mime_type": mime,
        "size": bytes.len(),
        "blob_ref": blob_ref,
        "attachment": meta
    }))
}

fn attachment_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_first(&input, &["attachment_id"])?.to_string();
    cf_delete(host, &format!("/wiki/rest/api/content/{}", urlencode(&id)))?;
    Ok(json!({ "ok": true, "attachment_id": id }))
}

fn page_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let space_key = req_first(&input, &["space_key"])?;
    let title = req_first(&input, &["title"])?;
    let mut body = resolve_body(
        first_str(&input, &["body_markdown"]),
        first_str(&input, &["body_storage"]),
    )?;
    if body.is_empty() {
        body = "<p>Created by flux.</p>".to_string();
    }
    let mut payload = json!({
        "type": "page",
        "title": title,
        "space": { "key": space_key },
        "body": { "storage": { "value": body, "representation": "storage" } }
    });
    if let Some(parent) = first_str(&input, &["parent_id"]) {
        payload["ancestors"] = json!([{ "id": parent }]);
    }
    let created = cf_send(host, "POST", "/wiki/rest/api/content", &payload)?;
    let id = created
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // fluxplane re-reads the created page and renders its body as Markdown.
    let mut page = if id.is_empty() {
        created
    } else {
        cf_get(
            host,
            &format!(
                "/wiki/rest/api/content/{}?expand=body.storage,version,space,ancestors",
                urlencode(&id)
            ),
        )
        .unwrap_or(created)
    };
    render_page_body(&mut page, BodyFormat::Markdown);
    Ok(json!({ "ok": true, "id": id, "page": page }))
}

fn page_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_first(&input, &["id", "page_id"])?.to_string();
    let new_body = resolve_body(
        first_str(&input, &["body_markdown"]),
        first_str(&input, &["body_storage"]),
    )?;
    let new_title = first_str(&input, &["title"]);
    if new_title.is_none() && new_body.is_empty() {
        return Err("nothing to update: provide title, body_markdown, or body_storage".into());
    }

    // Confluence v1 replaces the whole content on PUT and requires the next version number, so read
    // the current page first to learn the version (and to preserve the title/body left unset).
    let current = cf_get(
        host,
        &format!(
            "/wiki/rest/api/content/{}?expand=body.storage,version",
            urlencode(&id)
        ),
    )?;
    let version = current
        .get("version")
        .and_then(|v| v.get("number"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        + 1;
    let title = new_title
        .or_else(|| current.get("title").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let body = if new_body.is_empty() {
        current
            .get("body")
            .and_then(|b| b.get("storage"))
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        new_body
    };
    let payload = json!({
        "id": id,
        "type": "page",
        "title": title,
        "version": { "number": version },
        "body": { "storage": { "value": body, "representation": "storage" } }
    });
    cf_send(
        host,
        "PUT",
        &format!("/wiki/rest/api/content/{}", urlencode(&id)),
        &payload,
    )?;
    // fluxplane re-reads the page and renders the body in the caller's requested format.
    let mut page = cf_get(
        host,
        &format!(
            "/wiki/rest/api/content/{}?expand=body.storage,version,space,ancestors",
            urlencode(&id)
        ),
    )?;
    render_page_body(&mut page, parse_body_format(&input));
    Ok(json!({ "ok": true, "id": id, "page": page }))
}

fn page_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_first(&input, &["id", "page_id"])?.to_string();
    cf_delete(host, &format!("/wiki/rest/api/content/{}", urlencode(&id)))?;
    Ok(json!({ "ok": true, "id": id }))
}

fn page_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let cql = page_cql(
        first_str(&input, &["cql"]),
        first_str(&input, &["query", "search"]),
        first_str(&input, &["title"]),
    );
    let limit = clamp_limit(&input, "limit", 25);
    let result = cf_get(
        host,
        &format!(
            "/wiki/rest/api/search?cql={}&limit={limit}",
            urlencode(&cql)
        ),
    )?;
    let pages = collect_pages(&result);
    contribute_page_values(host, &pages);
    Ok(json!({ "pages": pages, "count": pages.len() }))
}

fn page_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let limit = clamp_limit(&input, "limit", 25);
    let status = first_str(&input, &["status"]).unwrap_or("current");
    let mut path = format!(
        "/wiki/rest/api/content?type=page&status={}&limit={limit}&expand=version,space",
        urlencode(status)
    );
    if let Some(sk) = first_str(&input, &["space_key"]) {
        path.push_str(&format!("&spaceKey={}", urlencode(sk)));
    }
    if let Some(t) = first_str(&input, &["title"]) {
        path.push_str(&format!("&title={}", urlencode(t)));
    }
    if let Some(s) = first_str(&input, &["start"]) {
        path.push_str(&format!("&start={}", urlencode(s)));
    }
    let result = cf_get(host, &path)?;
    let pages = collect_pages(&result);
    contribute_page_values(host, &pages);
    Ok(json!({ "pages": pages, "count": pages.len() }))
}

fn page_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_first(&input, &["id", "page_id"])?;
    let mut page = cf_get(
        host,
        &format!(
            "/wiki/rest/api/content/{}?expand=body.storage,version,space,ancestors",
            urlencode(id)
        ),
    )?;
    render_page_body(&mut page, parse_body_format(&input));
    // fluxplane also folds the page's attachments into the show result.
    if let Ok(att) = cf_get(
        host,
        &format!(
            "/wiki/rest/api/content/{}/child/attachment?limit=100",
            urlencode(id)
        ),
    ) {
        if let Some(results) = att.get("results").cloned() {
            page["attachments"] = results;
        }
    }
    Ok(page)
}

fn comment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let page_id = req_first(&input, &["page_id", "id"])?.to_string();
    let limit = clamp_limit(&input, "limit", 25);
    let mut path = format!(
        "/wiki/rest/api/content/{}/child/comment?depth=all&limit={limit}&expand=body.storage,version,history,extensions.location",
        urlencode(&page_id)
    );
    if let Some(s) = first_str(&input, &["start"]) {
        path.push_str(&format!("&start={}", urlencode(s)));
    }
    let result = cf_get(host, &path)?;
    let format = parse_body_format(&input);
    let comments: Vec<Value> = result
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|raw| {
                    let mut c = comment_from_api(raw);
                    render_comment_body(&mut c, format);
                    c
                })
                .collect()
        })
        .unwrap_or_default();
    let count = comments.len();
    Ok(json!({ "page_id": page_id, "count": count, "comments": comments }))
}

fn comment_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let page_id = req_first(&input, &["page_id", "id"])?;
    let body = resolve_body(
        first_str(&input, &["body_markdown", "body"]),
        first_str(&input, &["body_storage"]),
    )?;
    if body.is_empty() {
        return Err("body_markdown (or body_storage) is required".into());
    }
    let payload = json!({
        "type": "comment",
        "container": { "id": page_id, "type": "page" },
        "body": { "storage": { "value": body, "representation": "storage" } }
    });
    let created = cf_send(host, "POST", "/wiki/rest/api/content", &payload)?;
    // Normalize + render the created comment to Markdown, like fluxplane's CreateComment → renderBody.
    let mut comment = comment_from_api(&created);
    render_comment_body(&mut comment, BodyFormat::Markdown);
    Ok(comment)
}

fn user_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = first_str(&input, &["query", "search"]);
    let cql = first_str(&input, &["cql"]);
    if query.is_none() && cql.is_none() {
        let user = cf_get(host, "/wiki/rest/api/user/current")?;
        let users = collect_users(&user);
        contribute_user_values(host, &users);
        return Ok(json!({ "users": users, "count": users.len() }));
    }
    let limit = clamp_limit(&input, "limit", 25);
    let cql = user_cql(cql, query.unwrap_or(""));
    let result = cf_get(
        host,
        &format!(
            "/wiki/rest/api/search?cql={}&limit={limit}",
            urlencode(&cql)
        ),
    )?;
    let users = collect_users(&result);
    contribute_user_values(host, &users);
    Ok(json!({ "users": users, "count": users.len() }))
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
            .with_endpoint_ref("confluence.endpoint", "https://x.atlassian.net")
            .with_endpoint("confluence.endpoint", "https://x.atlassian.net")
    }

    #[test]
    fn test_op_fetches_current_user() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/user/current",
            json!({ "accountId": "me", "displayName": "Me" }),
        );
        let out = plugin
            .call("confluence.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["status"], "ok");
        assert_eq!(out["user"]["accountId"], "me");
    }

    #[test]
    fn cloud_id_routes_through_the_atlassian_gateway() {
        // With a cloud_id secret resolvable, requests go to the gateway base (Bearer), not the site URL.
        let plugin = manifest_builder().build();
        let mut host = host().with_secret("cloud_id", "abc-123").with_http(
            "api.atlassian.com/ex/confluence/abc-123/wiki/rest/api/user/current",
            json!({ "accountId": "me" }),
        );
        let out = plugin
            .call("confluence.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["user"]["accountId"], "me");
    }

    #[test]
    fn index_build_contributes_pages_and_users() {
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_http(
                "/wiki/rest/api/content",
                json!({"results": [{"id": "p1", "title": "Runbook", "space": {"key": "OPS"}}]}),
            )
            .with_http(
                "/wiki/rest/api/user/current",
                json!({"accountId": "u1", "displayName": "Alice"}),
            );
        let out = plugin
            .call(
                "confluence.index.build",
                json!({ "page_limit": 50 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["indexed"], 2);
        let recs = host.contributed.borrow();
        assert!(recs
            .iter()
            .any(|r| r.entity == "confluence.page" && r.id == "p1"));
        assert!(recs
            .iter()
            .any(|r| r.entity == "confluence.user" && r.id == "u1"));
    }

    #[test]
    fn attachment_add_uploads_from_a_blob() {
        // Byte-exact uploads (no ref-based HTTP variant) materialize the site base via the host
        // endpoint resolver — same site path as the JSON ops, exercised with no cloud_id.
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/child/attachment",
            json!({"results": [{"id": "att5", "title": "diagram.png"}]}),
        );
        host.blobs.borrow_mut().insert(
            "blobref1".into(),
            ("diagram.png".into(), b"hello-bytes".to_vec()),
        );
        let out = plugin
            .call(
                "confluence.page.attachment.add",
                json!({ "page_id": "123", "blob_ref": "blobref1" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["page_id"], "123");
        assert_eq!(out["attachments"][0]["id"], "att5");
    }

    #[test]
    fn attachment_add_preserves_binary_bytes() {
        // The binary upload path must carry non-UTF-8 bytes verbatim through the multipart body.
        let plugin = manifest_builder().build();
        let mut host = host().with_http("/child/attachment", json!({"results": [{"id": "b1"}]}));
        let raw: Vec<u8> = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
        host.blobs
            .borrow_mut()
            .insert("blobbin".into(), ("logo.bin".into(), raw.clone()));
        let out = plugin
            .call(
                "confluence.page.attachment.add",
                json!({ "page_id": "123", "blob_ref": "blobbin" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["attachments"][0]["id"], "b1");
    }

    #[test]
    fn attachment_list_returns_page_attachments() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/child/attachment?limit=100",
            json!({"results": [{"id": "att5", "title": "diagram.png"}]}),
        );
        let out = plugin
            .call(
                "confluence.page.attachment.list",
                json!({ "page_id": "123" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["attachments"][0]["id"], "att5");
    }

    #[test]
    fn attachment_get_downloads_into_a_blob() {
        // Byte-exact downloads materialize the site base via the host endpoint resolver (no ref variant).
        let plugin = manifest_builder().build();
        let mut host = host()
            .with_http(
                "/content/att5",
                json!({"id": "att5", "title": "diagram.png", "mediaType": "image/png",
                       "_links": {"download": "/download/attachments/123/diagram.png"}}),
            )
            // The download endpoint uses the binary HTTP path (response_binary).
            .with_http_bytes("/download", b"PNGBYTES".to_vec());
        let out = plugin
            .call(
                "confluence.attachment.get",
                json!({ "attachment_id": "att5", "page_id": "123" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["filename"], "diagram.png");
        assert_eq!(out["mime_type"], "image/png");
        assert!(out["size"].as_u64().unwrap() > 0);
        assert!(out["blob_ref"].as_str().unwrap().starts_with("mockblob-"));
    }

    #[test]
    fn attachment_get_round_trips_binary_bytes() {
        // Non-UTF-8 download bytes survive into the blob store byte-for-byte.
        let plugin = manifest_builder().build();
        let raw: Vec<u8> = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
        let mut host = host()
            .with_http(
                "/content/attbin",
                json!({"id": "attbin", "title": "raw.bin", "mediaType": "application/octet-stream",
                       "_links": {"download": "/download/attachments/9/raw.bin"}}),
            )
            .with_http_bytes("/download", raw.clone());
        let out = plugin
            .call(
                "confluence.attachment.get",
                json!({ "attachment_id": "attbin", "page_id": "9" }),
                &mut host,
            )
            .unwrap();
        let blob_ref = out["blob_ref"].as_str().unwrap();
        let stored = host.blobs.borrow().get(blob_ref).map(|(_, b)| b.clone());
        assert_eq!(stored, Some(raw));
    }

    #[test]
    fn attachment_delete_removes_the_attachment() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http("/content/att5", json!({}));
        let out = plugin
            .call(
                "confluence.attachment.delete",
                json!({ "attachment_id": "att5" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["attachment_id"], "att5");
    }

    #[test]
    fn page_create_posts_a_new_page() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content",
            json!({"id": "999", "title": "Release notes",
                   "body": {"storage": {"value": "<p>hi</p>"}}}),
        );
        let out = plugin
            .call(
                "confluence.page.create",
                json!({ "space_key": "DEV", "title": "Release notes", "body_markdown": "## Summary" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["id"], "999");
        // The returned page body is rendered to Markdown, not raw storage XHTML.
        assert_eq!(out["page"]["body_markdown"], "hi");
        assert!(out["page"].get("body").is_none());
    }

    #[test]
    fn page_update_bumps_version_via_get_then_put() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content/123",
            json!({"id": "123", "title": "Old", "version": {"number": 3},
                   "body": {"storage": {"value": "<p>old</p>"}}}),
        );
        let out = plugin
            .call(
                "confluence.page.update",
                json!({ "id": "123", "body_markdown": "New body" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["id"], "123");
        assert_eq!(out["page"]["id"], "123");
        assert_eq!(out["page"]["body_markdown"], "old");
    }

    #[test]
    fn page_update_body_format_storage_keeps_xhtml() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content/123",
            json!({"id": "123", "title": "Old", "version": {"number": 1},
                   "body": {"storage": {"value": "<p>raw</p>"}}}),
        );
        let out = plugin
            .call(
                "confluence.page.update",
                json!({ "id": "123", "title": "New", "body_format": "storage" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["page"]["body_storage"], "<p>raw</p>");
        assert!(out["page"].get("body_markdown").is_none());
    }

    #[test]
    fn page_delete_removes_the_page() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http("/wiki/rest/api/content/123", json!({}));
        let out = plugin
            .call("confluence.page.delete", json!({ "id": "123" }), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["id"], "123");
    }

    #[test]
    fn page_search_runs_cql_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/search",
            json!({"results": [
                {"content": {"id": "123", "title": "Warm transfer runbook", "space": {"key": "OPS"}}}
            ]}),
        );
        let out = plugin
            .call(
                "confluence.page.search",
                json!({ "query": "runbook" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["pages"][0]["page_id"], "123");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "confluence.page");
        assert_eq!(recs[0].id, "123");
        assert_eq!(recs[0].body, "Warm transfer runbook (space OPS)");
    }

    #[test]
    fn page_list_filters_by_space_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content?type=page",
            json!({"results": [{"id": "p1", "title": "Runbook", "space": {"key": "OPS"}}]}),
        );
        let out = plugin
            .call(
                "confluence.page.list",
                json!({ "space_key": "OPS" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["pages"][0]["page_id"], "p1");
        assert_eq!(host.contributed.borrow()[0].id, "p1");
    }

    #[test]
    fn page_show_fetches_by_id_and_renders_markdown() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content/123?expand",
            json!({"id": "123", "title": "Warm transfer runbook",
                   "body": {"storage": {"value": "<h2>Heading</h2><p>A <strong>bold</strong> line.</p>"}}}),
        );
        let out = plugin
            .call("confluence.page.show", json!({ "id": "123" }), &mut host)
            .unwrap();
        assert_eq!(out["title"], "Warm transfer runbook");
        let md = out["body_markdown"].as_str().unwrap();
        assert!(md.contains("## Heading"), "md = {md:?}");
        assert!(md.contains("**bold**"), "md = {md:?}");
        assert!(out.get("body").is_none());
    }

    #[test]
    fn page_show_body_format_both_returns_markdown_and_storage() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content/123?expand",
            json!({"id": "123", "title": "T", "body": {"storage": {"value": "<p>hello</p>"}}}),
        );
        let out = plugin
            .call(
                "confluence.page.show",
                json!({ "id": "123", "body_format": "both" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["body_markdown"], "hello");
        assert_eq!(out["body_storage"], "<p>hello</p>");
    }

    #[test]
    fn comment_list_returns_page_comments_as_markdown() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/child/comment",
            json!({"results": [
                {"id": "c1", "title": "Re: runbook",
                 "body": {"storage": {"value": "<p>Looks <em>good</em></p>"}},
                 "history": {"createdBy": {"accountId": "u1", "displayName": "Alice"}, "createdDate": "2026-01-01"}}
            ]}),
        );
        let out = plugin
            .call(
                "confluence.page.comment.list",
                json!({ "page_id": "123" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["comments"][0]["id"], "c1");
        assert_eq!(out["comments"][0]["author_name"], "Alice");
        assert_eq!(out["comments"][0]["body_markdown"], "Looks *good*");
        // Markdown is the default, so raw storage is dropped.
        assert!(out["comments"][0].get("body_storage").is_none());
    }

    #[test]
    fn comment_add_posts_a_comment() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/content",
            json!({"id": "c9", "type": "comment", "body": {"storage": {"value": "<p>Reviewed.</p>"}}}),
        );
        let out = plugin
            .call(
                "confluence.page.comment.add",
                json!({ "page_id": "123", "body_markdown": "Reviewed." }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["id"], "c9");
        assert_eq!(out["body_markdown"], "Reviewed.");
    }

    #[test]
    fn user_search_runs_cql_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/search",
            json!({"results": [
                {"user": {"accountId": "u1", "displayName": "Alice", "email": "a@b.c"}}
            ]}),
        );
        let out = plugin
            .call(
                "confluence.user.search",
                json!({ "query": "Alice" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["users"][0]["account_id"], "u1");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "confluence.user");
        assert_eq!(recs[0].id, "u1");
    }

    #[test]
    fn user_search_without_query_returns_current_user() {
        let plugin = manifest_builder().build();
        let mut host = host().with_http(
            "/wiki/rest/api/user/current",
            json!({"accountId": "me", "displayName": "Me"}),
        );
        let out = plugin
            .call("confluence.user.search", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["users"][0]["account_id"], "me");
    }

    #[test]
    fn storage_to_markdown_renders_common_constructs() {
        let md = storage_to_markdown(
            "<h1>Title</h1><p>See <a href=\"https://x/y\">link</a> and <code>code</code>.</p><ul><li>one</li><li>two</li></ul>",
        );
        assert!(md.contains("# Title"), "{md}");
        assert!(md.contains("[link](https://x/y)"), "{md}");
        assert!(md.contains("`code`"), "{md}");
        assert!(md.contains("- one"), "{md}");
    }

    #[test]
    fn storage_to_markdown_renders_code_macro() {
        let md = storage_to_markdown(
            "<ac:structured-macro ac:name=\"code\"><ac:parameter ac:name=\"language\">rust</ac:parameter><ac:plain-text-body><![CDATA[fn main() {}]]></ac:plain-text-body></ac:structured-macro>",
        );
        assert!(md.contains("```rust"), "{md}");
        assert!(md.contains("fn main() {}"), "{md}");
    }

    #[test]
    fn storage_to_markdown_renders_full_block_set() {
        // Mirrors fluxplane's storage_test.go `TestStorageToMarkdownRendersCommonBlocks` shape.
        let storage = concat!(
            "<h2>Deploy</h2>",
            "<p>Use <strong>caution</strong> with <em>prod</em> and <code>kubectl</code>. ",
            "See <a href=\"https://example.com/docs\">docs</a>.</p>",
            "<ul><li>one</li><li>two<ul><li>nested</li></ul></li></ul>",
            "<ol><li>first</li><li>second</li></ol>",
            "<ac:structured-macro ac:name=\"code\"><ac:parameter ac:name=\"language\">go</ac:parameter>",
            "<ac:plain-text-body><![CDATA[fmt.Println(\"hi\")]]></ac:plain-text-body></ac:structured-macro>",
            "<ac:structured-macro ac:name=\"info\"><ac:rich-text-body><p>Heads up</p></ac:rich-text-body></ac:structured-macro>",
            "<table><tbody><tr><th>Name</th><th>Value</th></tr><tr><td>a</td><td>1</td></tr></tbody></table>",
            "<hr/>",
        );
        let want = concat!(
            "## Deploy\n\n",
            "Use **caution** with *prod* and `kubectl`. See [docs](https://example.com/docs).\n\n",
            "- one\n- two\n  - nested\n\n",
            "1. first\n2. second\n\n",
            "```go\nfmt.Println(\"hi\")\n```\n\n",
            "> **INFO**\n>\n> Heads up\n\n",
            "| Name | Value |\n| --- | --- |\n| a | 1 |\n\n",
            "---",
        );
        assert_eq!(storage_to_markdown(storage), want);
    }

    #[test]
    fn storage_to_markdown_renders_confluence_specifics() {
        // Page/user links, &nbsp; entity, emoticon fallback, task lists, and attachment images.
        let storage = concat!(
            "<p>Ping <ac:link><ri:user ri:account-id=\"acct-1\"/></ac:link> about ",
            "<ac:link><ri:page ri:content-title=\"Runbook\"/></ac:link>&nbsp;today ",
            "<ac:emoticon ac:name=\"smile\" ac:emoji-fallback=\"\u{1f642}\"/></p>",
            "<ac:task-list><ac:task><ac:task-status>complete</ac:task-status><ac:task-body>done thing</ac:task-body></ac:task>",
            "<ac:task><ac:task-status>incomplete</ac:task-status><ac:task-body>open thing</ac:task-body></ac:task></ac:task-list>",
            "<p><ac:image ac:alt=\"chart\"><ri:attachment ri:filename=\"chart.png\"/></ac:image></p>",
        );
        let md = storage_to_markdown(storage);
        assert!(
            md.contains("Ping @acct-1 about Runbook\u{a0}today \u{1f642}"),
            "{md}"
        );
        assert!(md.contains("- [x] done thing"), "{md}");
        assert!(md.contains("- [ ] open thing"), "{md}");
        assert!(md.contains("![chart](chart.png)"), "{md}");
    }

    #[test]
    fn storage_to_markdown_degrades_on_plain_and_empty() {
        assert_eq!(storage_to_markdown(""), "");
        assert_eq!(storage_to_markdown("plain text only"), "plain text only");
    }

    #[test]
    fn md_to_storage_renders_full_construct_set() {
        // Headings, emphasis/code/strikethrough, links, lists (incl. nested), fenced code (with a
        // language → code macro), a GFM table, and a horizontal rule.
        let md = concat!(
            "## Deploy\n\n",
            "Use **caution** with `kubectl` — see [docs](https://example.com/docs). ~~old~~\n\n",
            "- one\n- two\n  - nested\n\n",
            "1. first\n2. second\n\n",
            "```go\nfmt.Println(\"hi\")\n```\n\n",
            "| Name | Value |\n| --- | --- |\n| a | 1 |\n\n",
            "---",
        );
        let storage = md_to_storage(md);
        for needle in [
            "<h2>Deploy</h2>",
            "<strong>caution</strong>",
            "<code>kubectl</code>",
            "<a href=\"https://example.com/docs\">docs</a>",
            "<s>old</s>",
            "<ul>",
            "<li>one</li>",
            "<ol>",
            "<li>first</li>",
            "<ac:structured-macro ac:name=\"code\"><ac:parameter ac:name=\"language\">go</ac:parameter><ac:plain-text-body><![CDATA[fmt.Println(\"hi\")]]></ac:plain-text-body></ac:structured-macro>",
            "<table><tbody>",
            "<th>Name</th>",
            "<td>a</td>",
            "<hr />",
        ] {
            assert!(storage.contains(needle), "missing {needle:?} in:\n{storage}");
        }
        // Raw <pre>/<del> must not leak through (code → macro, strikethrough → <s>).
        assert!(!storage.contains("<pre>"), "{storage}");
        assert!(!storage.contains("<del>"), "{storage}");
    }

    #[test]
    fn md_to_storage_escapes_and_macros_code() {
        // `]]>` inside a code block must be split so it survives the CDATA section.
        let storage = md_to_storage("```\ndata]]>more\n```");
        assert!(storage.contains("]]]]><![CDATA[>"), "{storage}");
        // XML metacharacters in text are escaped.
        let storage = md_to_storage("a < b & c > d");
        assert!(storage.contains("a &lt; b &amp; c &gt; d"), "{storage}");
    }

    #[test]
    fn conversion_round_trips_common_constructs() {
        let md = "# Title\n\nSome **bold** and *italic* text.\n\n- a\n- b";
        let storage = md_to_storage(md);
        assert_eq!(storage_to_markdown(&storage), md);
    }

    #[test]
    fn resolve_body_renders_markdown_to_storage() {
        // `page_create`/`page_update`/`comment_add` all route bodies through `resolve_body`; confirm
        // a markdown body is rendered to storage XHTML (and that dual bodies are rejected).
        let storage = resolve_body(Some("## H\n\n- one\n- two"), None).unwrap();
        assert!(storage.contains("<h2>H</h2>"), "{storage}");
        assert!(storage.contains("<li>one</li>"), "{storage}");
        assert_eq!(
            resolve_body(None, Some("<p>raw</p>")).unwrap(),
            "<p>raw</p>"
        );
        assert!(resolve_body(Some("a"), Some("<p>b</p>")).is_err());
    }

    #[test]
    fn manifest_declares_dual_auth_ops_datasources_and_blob() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 15);
        // Primary Bearer api_token + Basic fallback + cloud_id selector.
        assert!(m
            .auth
            .iter()
            .any(|a| a.purpose == "api_token" && a.scheme == AuthScheme::Bearer));
        assert!(m
            .auth
            .iter()
            .any(|a| a.purpose == "basic" && a.scheme == AuthScheme::Basic));
        assert!(m.auth.iter().any(|a| a.purpose == "cloud_id"));
        assert!(m.capabilities.http);
        assert!(m.capabilities.blob);
        assert!(m.datasources.iter().any(|d| d.entity == "confluence.page"));
        assert!(m.datasources.iter().any(|d| d.entity == "confluence.user"));
        assert!(m
            .datasources
            .iter()
            .all(|d| d.capabilities.iter().any(|c| c == "index")));
    }
}
