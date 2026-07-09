//! Tier 2 — `web_fetch`: read a page as a **document**, not markup.
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
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_datasource::{Record, Source};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::net::PrivateNetAllow;

use crate::{condense, RecordSink, WebOptions};

/// Cap on the returned document (bytes, char-boundary safe) — applied *after* condensation so the
/// budget buys content, not tags. Mirrors the historical `web_fetch` `MAX_BYTES`.
const MAX_BYTES: usize = 256 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;

/// `web_fetch`: fetch a URL and return its readable content as condensed markdown (HTML) or the raw
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
            http: reqwest::Client::new(),
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
                audit.record_private_admit("web:web_fetch", host, &self.grant_source);
            }
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "web_fetch",
            "Read a web page as a readable document: HTML is returned as condensed markdown \
             (navigation, scripts, and boilerplate stripped); non-HTML content is returned raw. Pass \
             `raw: true` for the unprocessed body. Use this to read a page; for calling an API prefer \
             `http.request`. Loopback/private addresses are blocked unless the `web` egress scope \
             grants them.",
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The absolute http(s) URL to read."},
                    "raw": {"type": "boolean", "description": "Return the raw body instead of condensed markdown (default false)."}
                },
                "required": ["url"]
            }),
        )
        .with_effects(vec![Effect::Network])
        .with_access(vec![AccessKind::Network])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("url")
            .and_then(Value::as_str)
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
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
            .ok_or_else(|| Error::Other("web_fetch: `url` required".into()))?;
        let raw_body = params.get("raw").and_then(Value::as_bool).unwrap_or(false);

        let url = flux_system::net::guard_url_scoped(raw_url, &self.private_net)?;
        let resp = self
            .http
            .get(url.clone())
            .timeout(Duration::from_secs(
                DEFAULT_TIMEOUT_SECS.min(MAX_TIMEOUT_SECS),
            ))
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if let Some(host) = url.host_str() {
            self.audit_admit(host);
        }

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes().await.map_err(|e| Error::Http(e.to_string()))?;
        let body = String::from_utf8_lossy(&bytes).into_owned();

        let is_html = !raw_body && (content_type.contains("text/html") || looks_like_html(&body));

        let rendered = if is_html {
            let md = condense::html_to_markdown(&body);
            // Contribute the page as a groundable record (title/url/content), before capping.
            if let Some(sink) = &self.records {
                let title = condense::page_title(&body).unwrap_or_else(|| raw_url.to_string());
                let record = Record::new(
                    Source::new("web"),
                    "web.page",
                    raw_url,
                    title,
                    cap_str(md.clone(), MAX_BYTES),
                );
                sink.contribute(&[record]);
            }
            cap_str(md, MAX_BYTES)
        } else {
            cap_str(body, MAX_BYTES)
        };

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
fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start();
    let lower = head[..head.len().min(512)].to_ascii_lowercase();
    lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.contains("<head")
        || lower.contains("<body")
}

/// Cap a string to `max` bytes, cut on a char boundary (an arbitrary body may not split cleanly).
fn cap_str(mut s: String, max: usize) -> String {
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
    use flux_runtime::ToolContext;
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

    #[tokio::test]
    async fn private_refused_without_grant_but_admitted_and_audited_with_one() {
        // Refused without a grant.
        let base = one_shot("200 OK", "text/html", "<html><body>x</body></html>").await;
        let denied = tool(PrivateNetAllow::None, None);
        assert!(denied
            .execute(&ctx(), json!({ "url": base }))
            .await
            .is_err());

        // Admitted with the web grant, and the admit is audited as `web:web_fetch`.
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
        assert_eq!(calls[0].0, "web:web_fetch");
        assert_eq!(calls[0].2, "config:web");
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
