//! Tier 2 — `web.fetch`: read a page as a **document**, not markup.
//!
//! The everyday "read this page" capability. `text/html` responses come back as condensed markdown
//! (boilerplate stripped, the budget spent on content, capped *after* condensation); non-HTML bodies
//! stay raw; `raw: true` forces the raw body. Fetched HTML pages are contributed as `web.page`
//! datasource records so read content is groundable later. Governed by the same family-wide `web`
//! egress scope as `http.request`.
//!
//! Also hosts the pure `html_to_markdown` op ([`HtmlToMarkdownTool`]) — no egress — so a
//! `http.request → html_to_markdown` pipeline covers any exotic fetch-then-read shape.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_datasource::{Record, Source};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::net::PrivateNetAllow;

use crate::{
    condense, egress, RecordSink, WebOptions, WEB_PAGE_RECORD_SUBJECT, WRITE_DB_EFFECT_TAG,
};

/// Cap on the returned document (bytes, char-boundary safe) — applied *after* condensation so the
/// budget buys content, not tags. Mirrors the historical `web.fetch` `MAX_BYTES`.
const MAX_BYTES: usize = 256 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;

/// `web.fetch`: fetch a URL and return its readable content as condensed markdown (HTML) or the raw
/// body (everything else / `raw: true`).
pub struct WebFetchTool {
    http: reqwest::Client,
    private_net: PrivateNetAllow,
    audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    grant_source: String,
    records: Option<Arc<dyn RecordSink>>,
}

impl WebFetchTool {
    pub fn new(opts: &WebOptions) -> Self {
        Self {
            http: egress::redirect_disabled_client(),
            private_net: opts.private_net.clone(),
            audit: opts.audit.clone(),
            grant_source: opts
                .grant_source
                .clone()
                .unwrap_or_else(|| "config:web".to_string()),
            records: opts.records.clone(),
        }
    }

    fn audit_admit(&self, host: &str) {
        if let Some(audit) = &self.audit {
            if flux_system::net::host_resolves_private(host) {
                audit.record_private_admit("web:web.fetch", host, &self.grant_source);
            }
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        #[cfg(feature = "pdf")]
        let mut description = String::from(
            "Read a web page as a readable document: HTML is returned as condensed markdown \
             (navigation, scripts, and boilerplate stripped) and PDFs are returned as extracted \
             text; other non-HTML content is returned raw. Pass `raw: true` for the unprocessed \
             body. Use this to read a page; for calling an API prefer `http.request`. \
             Loopback/private addresses are blocked unless the `web` egress scope grants them.",
        );
        #[cfg(not(feature = "pdf"))]
        let mut description = String::from(
            "Read a web page as a readable document: HTML is returned as condensed markdown; \
             PDF bodies are identified but omitted because this build has no PDF extractor; \
             other non-HTML content is returned raw. Pass raw: true for the unprocessed body. \
             Use this to read a page; for calling an API prefer http.request. Loopback/private \
             addresses are blocked unless the web egress scope grants them.",
        );
        if self.records.is_some() {
            // Disclose the durable side effect in the model-facing spec too (C-58): a read here also
            // persists the page into the searchable knowledge datasource.
            description.push_str(
                " Read HTML pages are also persisted as searchable `web.page` datasource records (a \
                 durable datasource write).",
            );
        }
        let mut spec = ToolSpec::read_only(
            "web.fetch",
            description,
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The absolute http(s) URL to read."},
                    "raw": {"type": "boolean", "description": "Return the raw body instead of condensed markdown (default false)."}
                },
                "required": ["url"]
            }),
        )
        // `Read` is not decorative (C-208): a fetch *is* a read, and `Network` alone describes an
        // unread egress — a POST. The omission made this op consequence-bearing under
        // `flux_spec::coherence` while it carried `Risk::Low`. The egress envelope is unchanged:
        // every request still goes through `guard_url_scoped_pinned` below.
        .with_effects(vec![Effect::Read, Effect::Network])
        .with_access(vec![AccessKind::Network]);
        // `read_only()` also supplied `Idempotent`, and that claim is false independently of the
        // effect set: `Idempotent` is what licenses the op cache to serve a stored result *instead
        // of executing*, and with a record sink wired each call upserts a `web.page` record. A
        // replayed fetch would skip a durable contribution the caller asked for, and skip the live
        // page. Repeating is safe, replaying is not — which is exactly `Conditional`.
        //
        // Adding `Read` above moved this spec out of `is_consequence_bearing`, so I3 no longer
        // fires on it. That makes fixing it here load-bearing rather than optional: the invariant
        // stopped watching, so the declaration has to be right on its own.
        spec.idempotency = Idempotency::Conditional;
        if self.records.is_some() {
            // C-210: with a sink wired this op self-declares `write_db`, which now counts as
            // consequence-bearing — so `Risk::Low` would violate I1. `Medium` is the honest tier
            // and is what takes the op out of the pre-approval gather path; it adds no approval
            // prompt (`RiskApprover` gates writes at `High`+, `dispatch` forces only
            // `Destructive`). Conditional on the sink for the same reason the tag is: without one,
            // this really is a pure network read. See `docs/designs/security-assurance.md`.
            spec.risk = Risk::Medium;
        }
        spec
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let mut subjects: Vec<String> = params
            .get("url")
            .and_then(Value::as_str)
            .map(|s| vec![s.to_string()])
            .unwrap_or_default();
        // With a record sink configured, a fetched HTML page is persisted as a durable `web.page`
        // datasource record; name that write target so approval + audit disclose the persistence
        // (C-58). Paired with the `write_db` semantic effect below.
        if self.records.is_some() {
            subjects.push(WEB_PAGE_RECORD_SUBJECT.to_string());
        }
        subjects
    }

    fn semantic_effects(&self) -> Vec<String> {
        // Contributing `web.page` records is a durable datasource write (C-58) — disclosed only when a
        // sink is actually wired, so the catalog-only registry stays a pure network read.
        if self.records.is_some() {
            vec![WRITE_DB_EFFECT_TAG.to_string()]
        } else {
            Vec::new()
        }
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(url) = params.get("url").and_then(Value::as_str) {
            set.push(Intent {
                behavior: IntentBehavior::NetworkFetch,
                target: IntentTarget::Url {
                    url: url.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let raw_url = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("web.fetch: `url` required".into()))?;
        let raw_body = params.get("raw").and_then(Value::as_bool).unwrap_or(false);

        let (url, pinned) = flux_system::net::guard_url_scoped_pinned(raw_url, &self.private_net)?;
        let response = egress::send_guarded(
            &self.http,
            egress::GuardedRequest {
                url,
                pinned,
                method: reqwest::Method::GET,
                headers: HeaderMap::new(),
                body: None,
                timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS.min(MAX_TIMEOUT_SECS)),
            },
            "web.fetch",
            |raw| flux_system::net::guard_url_scoped_pinned(raw, &self.private_net),
            |url| {
                if let Some(host) = url.host_str() {
                    self.audit_admit(host);
                }
            },
        )
        .await?;

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let capped = egress::read_body_capped(response, MAX_BYTES, "web.fetch").await?;

        // Classify from the *raw bytes* + content-type before any lossy UTF-8 decode (which would
        // corrupt a binary PDF). A PDF is either declared (`application/pdf`) or sniffed by its
        // `%PDF` magic bytes, so a mislabeled/absent content-type still routes to text extraction.
        let is_pdf = !raw_body
            && (content_type.contains("application/pdf") || looks_like_pdf(&capped.bytes));
        let body = cap_str(
            String::from_utf8_lossy(&capped.bytes).into_owned(),
            MAX_BYTES,
        );
        let is_html =
            !raw_body && !is_pdf && (content_type.contains("text/html") || looks_like_html(&body));

        let mut rendered = if is_html {
            let md = condense::html_to_markdown(&body);
            cap_str(md, MAX_BYTES)
        } else if is_pdf {
            #[cfg(feature = "pdf")]
            // Extract text, capped exactly like the HTML branch. A malformed/truncated/text-less PDF
            // falls back to the raw pass-through rather than erroring the whole fetch.
            let rendered = match extract_pdf_text(&capped.bytes) {
                Some(text) => cap_str(text, MAX_BYTES),
                None => cap_str(body, MAX_BYTES),
            };
            #[cfg(not(feature = "pdf"))]
            let rendered = String::from(
                "[PDF content omitted: this build does not enable the safe pdf extraction feature]",
            );
            rendered
        } else {
            cap_str(body, MAX_BYTES)
        };
        if capped.truncated && !rendered.ends_with("…[truncated]") {
            rendered.push_str("\n…[truncated]");
        }

        // Contribute HTML as a groundable record after the input and output caps are applied.
        if is_html {
            if let Some(sink) = &self.records {
                let title = condense::page_title(&String::from_utf8_lossy(&capped.bytes))
                    .unwrap_or_else(|| raw_url.to_string());
                let record = Record::new(
                    Source::new("web"),
                    "web.page",
                    raw_url,
                    title,
                    rendered.clone(),
                );
                sink.contribute(&[record]);
            }
        }

        Ok(ToolResult {
            content: format!("[{status}]\n{rendered}"),
            view: None,
            is_error: !status.is_success(),
        })
    }
}

/// The pure `html_to_markdown` op — condense an HTML string to markdown with no egress, so a
/// `http.request → html_to_markdown` pipeline reads any fetched HTML.
pub struct HtmlToMarkdownTool;

#[async_trait]
impl Tool for HtmlToMarkdownTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "html_to_markdown".into(),
            description: "Convert an HTML string to condensed, readable markdown — navigation, \
                scripts, and boilerplate stripped. Pure: no network. Compose with `http.request` to \
                fetch a page and then read it."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "html": {"type": "string", "description": "The HTML source to condense."}
                },
                "required": ["html"]
            }),
            output_schema: None,
            effects: Vec::new(),
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let html = params
            .get("html")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("html_to_markdown: `html` required".into()))?;
        Ok(ToolResult::ok(cap_str(
            condense::html_to_markdown(html),
            MAX_BYTES,
        )))
    }
}

/// Sniff a body that lacks a helpful `content-type` for an HTML shape.
pub(crate) fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start();
    // Slice the leading sniff window on a char boundary: attacker-supplied content can place a
    // multibyte UTF-8 char straddling byte 512, and a raw `head[..512]` panics off that boundary
    // (C-84). Floor to the nearest boundary at or below 512.
    let mut end = head.len().min(512);
    while end > 0 && !head.is_char_boundary(end) {
        end -= 1;
    }
    let lower = head[..end].to_ascii_lowercase();
    lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.contains("<head")
        || lower.contains("<body")
}

/// Sniff the `%PDF-` magic signature so a PDF served with a wrong/absent `content-type` still routes
/// to text extraction. Per the PDF spec the header may sit after a short prefix (BOM/whitespace), so
/// scan a small leading window rather than requiring byte 0.
fn looks_like_pdf(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(1024)];
    window.windows(5).any(|w| w == b"%PDF-")
}

/// Extract readable text from PDF bytes. Returns `None` on any failure — an extraction error, a
/// panic from a malformed PDF (`pdf-extract` panics on some inputs), or an empty result — so the
/// caller falls back to the raw pass-through instead of erroring or emptying the whole fetch.
#[cfg(feature = "pdf")]
fn extract_pdf_text(bytes: &[u8]) -> Option<String> {
    let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }));
    match extracted {
        Ok(Ok(text)) if !text.trim().is_empty() => Some(text),
        _ => None,
    }
}

/// Cap a string to `max` bytes, cut on a char boundary (an arbitrary body may not split cleanly).
pub(crate) fn cap_str(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n…[truncated]");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_policy::{
        Action, AuthorizationPolicy, Grant, ResourceKind, ResourceRef, SubjectKind, SubjectRef,
        TrustLevel,
    };
    use flux_runtime::{
        AllowApprover, AuthorityRequirement, Executor, PermissionManager, ToolContext, ToolRegistry,
    };
    use flux_system::{System, Workspace};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!(
            "flux-web-fetch-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[test]
    fn looks_like_html_does_not_panic_on_utf8_boundary() {
        // A multibyte char ('€' = 3 bytes) straddling byte 512 makes a raw `head[..512]` slice off a
        // char boundary → panic. The sniffer must floor to a boundary instead (C-84).
        let mut body = String::from("<html>");
        body.push_str(&"a".repeat(504)); // pushes the euro's bytes across the 512 mark
        body.push('€');
        body.push_str(&"z".repeat(200));
        assert_eq!(body.len(), 6 + 504 + 3 + 200);
        // Would panic before the fix; must return a bool now.
        assert!(looks_like_html(&body), "an <html> prefix is still detected");

        // And a euro exactly at the window that is pure non-HTML must not panic either.
        let mut plain = "x".repeat(511);
        plain.push('€');
        assert!(!looks_like_html(&plain));
    }

    async fn one_shot(
        status_line: &'static str,
        ctype: &'static str,
        body: &'static str,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    /// Like [`one_shot`] but serves an arbitrary **binary** body (headers, then the raw bytes) — the
    /// PDF/binary fixtures aren't valid UTF-8, so they can't go through the `&str` variant.
    async fn one_shot_bytes(ctype: &'static str, body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    /// Build a tiny valid single-page PDF whose content stream shows `text`, via `lopdf` (so the
    /// cross-reference table is correct — a hand-rolled xref is what `pdf-extract` rejects).
    fn make_pdf(text: &str) -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => resources_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    async fn redirect_to_ungranted_loopback() -> String {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = target.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 7\r\n\r\nreached",
                    )
                    .await;
            }
        });

        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_port = source.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = source.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nlocation: http://{target_addr}/private\r\ncontent-length: 0\r\n\r\n"
                );
                let _ = sock.write_all(response.as_bytes()).await;
            }
        });
        format!("http://localhost:{source_port}/start")
    }

    async fn same_origin_redirect() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut first, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = first.read(&mut buf).await;
                let _ = first
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nlocation: /final\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            }
            if let Ok((mut second, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = second.read(&mut buf).await;
                let _ = second
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\nconnection: close\r\n\r\nfinal",
                    )
                    .await;
            }
        });
        format!("http://{addr}/start")
    }

    async fn oversized_body_with_delayed_eof() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut request = [0u8; 2048];
                let _ = sock.read(&mut request).await;
                let declared = MAX_BYTES + 64 * 1024;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {declared}\r\n\r\n"
                );
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.write_all(&vec![b'x'; MAX_BYTES + 4096]).await;
                let _ = sock.flush().await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        format!("http://{addr}/large")
    }

    #[derive(Default)]
    struct RecordingSink {
        records: Mutex<Vec<Record>>,
    }
    impl RecordSink for RecordingSink {
        fn contribute(&self, records: &[Record]) {
            self.records.lock().unwrap().extend(records.iter().cloned());
        }
    }

    #[derive(Default)]
    struct RecordingAudit {
        calls: Mutex<Vec<(String, String, String)>>,
    }
    impl flux_plugin::EgressAudit for RecordingAudit {
        fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
            self.calls.lock().unwrap().push((
                caller.to_string(),
                host.to_string(),
                grant_source.to_string(),
            ));
        }
    }

    fn tool(private_net: PrivateNetAllow, records: Option<Arc<dyn RecordSink>>) -> WebFetchTool {
        WebFetchTool::new(&WebOptions {
            private_net,
            records,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn html_is_returned_as_condensed_markdown_and_recorded() {
        let base = one_shot(
            "200 OK",
            "text/html",
            "<html><head><title>Doc</title></head><body><article><h1>Hi</h1>\
             <p>Body text long enough to beat the readability threshold so the article region is the \
             one selected for condensation on this fixture page.</p></article>\
             <script>x()</script></body></html>",
        )
        .await;
        let sink = Arc::new(RecordingSink::default());
        let t = tool(PrivateNetAllow::Any, Some(sink.clone()));
        let r = t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert!(
            r.content.contains("# Hi"),
            "markdown heading: {}",
            r.content
        );
        assert!(!r.content.contains("x()"), "script stripped: {}", r.content);
        assert!(!r.content.contains("<"), "no markup: {}", r.content);
        let recs = sink.records.lock().unwrap();
        assert_eq!(recs.len(), 1, "one web.page record");
        assert_eq!(recs[0].entity, "web.page");
        assert_eq!(recs[0].title, "Doc");
    }

    #[tokio::test]
    async fn raw_true_returns_unprocessed_body() {
        let base = one_shot(
            "200 OK",
            "text/html",
            "<html><body><p>Raw</p></body></html>",
        )
        .await;
        let t = tool(PrivateNetAllow::Any, None);
        let r = t
            .execute(&ctx(), json!({ "url": base, "raw": true }))
            .await
            .unwrap();
        assert!(
            r.content.contains("<p>Raw</p>"),
            "raw markup kept: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn non_html_stays_raw() {
        let base = one_shot("200 OK", "application/json", "{\"k\": 1}").await;
        let t = tool(PrivateNetAllow::Any, None);
        let r = t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert!(
            r.content.contains("{\"k\": 1}"),
            "json kept raw: {}",
            r.content
        );
    }

    #[cfg(feature = "pdf")]
    #[tokio::test]
    async fn pdf_body_is_returned_as_extracted_text() {
        // A PDF served with the honest content-type comes back as extracted text — not the raw
        // `%PDF-...` byte dump the pre-D-161 else-branch produced.
        let base = one_shot_bytes("application/pdf", make_pdf("Hello Flux PDF")).await;
        let t = tool(PrivateNetAllow::Any, None);
        let r = t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert!(
            r.content.contains("Hello Flux PDF"),
            "extracted text present: {}",
            r.content
        );
        assert!(
            !r.content.contains("%PDF"),
            "raw PDF bytes must not leak — the header proves a raw dump: {}",
            r.content
        );
    }

    #[cfg(feature = "pdf")]
    #[tokio::test]
    async fn pdf_extracted_via_magic_byte_sniff_when_mislabeled() {
        // Same bytes, but served as `application/octet-stream` (a common mislabel / absent type):
        // the `%PDF` magic-byte sniff must still route it to extraction.
        let base = one_shot_bytes("application/octet-stream", make_pdf("Sniffed PDF Text")).await;
        let t = tool(PrivateNetAllow::Any, None);
        let r = t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert!(
            r.content.contains("Sniffed PDF Text"),
            "magic-byte sniff routed to extraction: {}",
            r.content
        );
        assert!(!r.content.contains("%PDF"), "no raw bytes: {}", r.content);
    }

    #[cfg(not(feature = "pdf"))]
    #[tokio::test]
    async fn a_pdf_is_opaque_when_the_safe_extractor_is_not_enabled() {
        let base = one_shot_bytes(
            "application/octet-stream",
            make_pdf("REMOTE PDF TEXT MUST NOT LEAK RAW"),
        )
        .await;
        let t = tool(PrivateNetAllow::Any, None);
        let r = t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert!(r.content.contains("PDF content omitted"), "{}", r.content);
        assert!(!r.content.contains("%PDF"), "no raw header: {}", r.content);
        assert!(
            !r.content.contains("REMOTE PDF TEXT MUST NOT LEAK RAW"),
            "no embedded content: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn non_pdf_binary_stays_raw() {
        // A non-PDF, non-HTML binary blob (a PNG signature here) is unchanged by D-161: it still
        // comes back as the raw lossy pass-through, never routed through PDF extraction.
        let blob = b"\x89PNG\r\n\x1a\nNOT-A-PDF-just-raw-binary".to_vec();
        let base = one_shot_bytes("application/octet-stream", blob).await;
        let t = tool(PrivateNetAllow::Any, None);
        let r = t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert!(
            r.content.contains("NOT-A-PDF-just-raw-binary"),
            "non-PDF binary kept raw: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn every_redirect_hop_is_guarded() {
        let url = redirect_to_ungranted_loopback().await;
        let t = tool(PrivateNetAllow::from_hosts(["localhost".to_string()]), None);
        let err = t
            .execute(&ctx(), json!({ "url": url }))
            .await
            .expect_err("the redirect target is outside the scoped private-net grant");
        assert!(err.to_string().contains("private/loopback"));
    }

    #[tokio::test]
    async fn bounded_same_origin_redirect_is_followed() {
        let url = same_origin_redirect().await;
        let t = tool(PrivateNetAllow::Any, None);
        let result = t.execute(&ctx(), json!({ "url": url })).await.unwrap();
        assert_eq!(result.content, "[200 OK]\nfinal");
    }

    #[tokio::test]
    async fn response_cap_returns_without_waiting_for_the_whole_body() {
        let url = oversized_body_with_delayed_eof().await;
        let t = tool(PrivateNetAllow::Any, None);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            t.execute(&ctx(), json!({ "url": url })),
        )
        .await
        .expect("the reader stops at the byte cap before the server closes")
        .unwrap();
        assert!(result.content.ends_with("…[truncated]"));
        assert!(result.content.len() <= MAX_BYTES + 64);
    }

    #[tokio::test]
    async fn private_refused_without_grant_but_admitted_and_audited_with_one() {
        // Refused without a grant.
        let base = one_shot("200 OK", "text/html", "<html><body>x</body></html>").await;
        let denied = tool(PrivateNetAllow::None, None);
        assert!(denied
            .execute(&ctx(), json!({ "url": base }))
            .await
            .is_err());

        // Admitted with the web grant, and the admit is audited as `web:web.fetch`.
        let base2 = one_shot("200 OK", "text/html", "<html><body>x</body></html>").await;
        let audit = Arc::new(RecordingAudit::default());
        let t = WebFetchTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            audit: Some(audit.clone()),
            grant_source: Some("config:web".into()),
            records: None,
            ..Default::default()
        });
        t.execute(&ctx(), json!({ "url": base2 })).await.unwrap();
        let calls = audit.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "web:web.fetch");
        assert_eq!(calls[0].2, "config:web");
    }

    #[tokio::test]
    async fn with_record_sink_declares_the_datasource_write_it_performs() {
        // Contract (C-58): configured WITH a record sink, `web.fetch` durably persists each fetched
        // HTML page as a searchable `web.page` datasource record. That persistence is disclosed — as
        // the `write_db` semantic effect + a `datasource:web.page` permission subject — so the
        // DECLARED contract matches the record actually contributed, instead of a bare network read
        // silently becoming durable local storage.
        let sink = Arc::new(RecordingSink::default());
        let t = tool(PrivateNetAllow::Any, Some(sink.clone()));

        assert!(
            t.semantic_effects().iter().any(|e| e == "write_db"),
            "sink-configured web.fetch must declare the `write_db` datasource-write effect: {:?}",
            t.semantic_effects()
        );
        let subjects = t.permission_subjects(&json!({ "url": "http://example.com/p" }));
        assert!(
            subjects.iter().any(|s| s == "datasource:web.page"),
            "must name the durable `web.page` record target as a permission subject: {subjects:?}"
        );
        assert!(
            subjects.iter().any(|s| s == "http://example.com/p"),
            "still names the fetched URL: {subjects:?}"
        );
        assert_eq!(
            t.authority_requirements(&json!({ "url": "http://example.com/p" }), &subjects)
                .unwrap(),
            vec![
                AuthorityRequirement::network_fetch("http://example.com/p"),
                AuthorityRequirement::new(
                    "flow.write_db",
                    ResourceRef::named(ResourceKind::Datasource, "web.page"),
                ),
            ],
            "the datasource marker must not be interpreted as a network destination"
        );
        // The host effects stay a network *read*: a `write_db` lowers to Network + the
        // `flow.write_db` policy action, deliberately NOT a filesystem `workspace.write`. Wiring a
        // record sink must not silently promote the host effect set — the durable contribution is
        // carried by the semantic effect and the authority requirement asserted above.
        assert_eq!(t.spec().effects, vec![Effect::Read, Effect::Network]);

        // Observed: the declaration matches a real contribution.
        let base = one_shot(
            "200 OK",
            "text/html",
            "<html><head><title>Doc</title></head><body><article><h1>Hi</h1>\
             <p>Body text long enough to beat the readability threshold so the article region is the \
             one selected for condensation on this fixture page.</p></article></body></html>",
        )
        .await;
        t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        assert_eq!(
            sink.records.lock().unwrap().len(),
            1,
            "declared write_db matches the observed record contribution"
        );
    }

    fn grant(action: &str, resource: ResourceRef) -> Grant {
        Grant {
            subjects: vec![SubjectRef {
                kind: SubjectKind::User,
                id: "*".into(),
            }],
            resources: vec![resource],
            actions: vec![Action::from(action)],
            required_trust: TrustLevel::Untrusted,
            required_scopes: Vec::new(),
            requires_approval: false,
        }
    }

    #[tokio::test]
    async fn sink_backed_fetch_requires_the_datasource_authority_at_dispatch() {
        let build = |policy: AuthorizationPolicy| {
            let fetch = Arc::new(tool(
                PrivateNetAllow::None,
                Some(Arc::new(RecordingSink::default())),
            ));
            let mut registry = ToolRegistry::new();
            registry.register(fetch);
            Executor::new(
                registry,
                PermissionManager::from_rules(&["web.fetch".into()], &[]),
                Arc::new(AllowApprover),
                ctx(),
            )
            .with_policy(policy)
        };
        let network_only = AuthorizationPolicy {
            grants: vec![grant(
                "network.fetch",
                ResourceRef::any(ResourceKind::Network),
            )],
        };
        let denied = build(network_only)
            .dispatch_outcome("web.fetch", json!({"url": "http://127.0.0.1/"}))
            .await;
        assert!(denied.denied);
        assert!(denied.result.content.contains("flow.write_db"));

        let matching = AuthorizationPolicy {
            grants: vec![
                grant("network.fetch", ResourceRef::any(ResourceKind::Network)),
                grant("flow.write_db", ResourceRef::any(ResourceKind::Datasource)),
            ],
        };
        let admitted = build(matching)
            .dispatch_outcome("web.fetch", json!({"url": "http://127.0.0.1/"}))
            .await;
        assert!(
            !admitted.denied,
            "matching datasource authority reaches guarded IO: {}",
            admitted.result.content
        );
    }

    #[tokio::test]
    async fn without_record_sink_stays_network_only() {
        // No sink ⇒ nothing is persisted, so the tool must NOT declare a datasource write: no
        // `write_db` effect and no `datasource:` subject — a pure network read.
        let t = tool(PrivateNetAllow::Any, None);
        assert!(
            t.semantic_effects().is_empty(),
            "no sink ⇒ no datasource-write effect: {:?}",
            t.semantic_effects()
        );
        let subjects = t.permission_subjects(&json!({ "url": "http://example.com/p" }));
        assert_eq!(
            subjects,
            vec!["http://example.com/p".to_string()],
            "no sink ⇒ the URL is the only subject (no datasource target): {subjects:?}"
        );
        assert_eq!(
            t.authority_requirements(&json!({ "url": "http://example.com/p" }), &subjects)
                .unwrap(),
            vec![AuthorityRequirement::network_fetch("http://example.com/p")]
        );
        // `Read` + `Network` — a fetch, not an unread egress (C-208). The pair is what keeps the
        // op coherent at `Risk::Low`; `Network` alone would declare a POST.
        assert_eq!(t.spec().effects, vec![Effect::Read, Effect::Network]);
        assert_eq!(t.spec().access, vec![AccessKind::Network]);
        assert!(flux_spec::metadata_violations(&t.spec(), &t.semantic_effects()).is_empty());
        // Pinned explicitly, and this assertion carries more weight than it looks: declaring
        // `Read` above takes the spec out of `is_consequence_bearing`, so `metadata_violations`
        // no longer checks idempotency here. This is the only thing standing between
        // `read_only()`'s inherited `Idempotent` and an op-cache replay that would skip both the
        // live fetch and the `web.page` record it contributes.
        assert_eq!(t.spec().idempotency, Idempotency::Conditional);
    }

    /// C-210 states the gather posture outright instead of leaving it to emerge from the effect set.
    ///
    /// The `write_db` tag is instance-conditional, so this op sits on both sides of the line: a
    /// catalog-only registration really is a pure network read and stays pre-approval reachable,
    /// while the sink-wired instance `flux-cli` builds self-declares a durable datasource write and
    /// must not run before a human sees the plan. `Risk::Medium` is what carries that — it costs an
    /// extra loop round, not an approval prompt (`RiskApprover` gates writes at `High`+).
    #[tokio::test]
    async fn a_sink_wired_fetch_is_consequence_bearing_and_leaves_the_gather_path() {
        let wired = tool(
            PrivateNetAllow::Any,
            Some(Arc::new(RecordingSink::default())),
        );
        assert!(
            flux_spec::is_consequence_bearing_with_effects(&wired.spec(), &wired.semantic_effects()),
            "a self-declared `write_db` is a consequence even though the effect set is [Read, Network]"
        );
        assert_eq!(wired.spec().risk, Risk::Medium);
        assert!(
            flux_spec::metadata_violations(&wired.spec(), &wired.semantic_effects()).is_empty(),
            "the raised tier is what keeps I1 satisfied: {:?}",
            flux_spec::metadata_violations(&wired.spec(), &wired.semantic_effects())
        );

        let catalog_only = tool(PrivateNetAllow::Any, None);
        assert!(
            !flux_spec::is_consequence_bearing_with_effects(
                &catalog_only.spec(),
                &catalog_only.semantic_effects()
            ),
            "with no sink there is no durable write, so gather-safety is retained"
        );
        assert_eq!(catalog_only.spec().risk, Risk::Low);
    }

    #[tokio::test]
    async fn pure_html_to_markdown_op_condenses_without_egress() {
        let t = HtmlToMarkdownTool;
        let r = t
            .execute(
                &ctx(),
                json!({ "html": "<article><h2>Sec</h2><p>Text body long enough to be selected as the main content region here.</p></article>" }),
            )
            .await
            .unwrap();
        assert!(r.content.contains("## Sec"), "heading: {}", r.content);
        assert!(!r.content.contains("<"), "no markup: {}", r.content);
    }
}
