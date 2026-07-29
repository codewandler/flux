//! `web.crawl` — a bounded, SSRF-guarded breadth-first crawl from a seed URL.
//!
//! The everyday "read this small site/section" capability: from a seed, follow **same-host** links
//! to a bounded depth, page count, and (optionally) total-content byte budget, returning each
//! fetched page as condensed markdown (and
//! contributing `web.page` records, exactly like [`crate::fetch::WebFetchTool`]). It is the
//! multi-page sibling of `web.fetch`, sharing its egress envelope: every hop — seed, each discovered
//! link, and every redirect — passes through [`flux_system::net::guard_url_scoped`] +
//! [`egress::send_guarded`], and a private-host admit emits the same [`flux_plugin::EgressAudit`]
//! event.
//!
//! **v1 non-goals (deliberate, enforced here):** no robots.txt, no sitemaps, no cross-host crawl, no
//! JS rendering (that is the tier-3 `browser.*` path). It stays same-host and strictly bounded — the
//! frontier can never grow without limit.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde_json::{json, Value};
use url::Url;

use flux_core::{Error, Result};
use flux_datasource::{Record, Source};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::net::PrivateNetAllow;

use crate::fetch::{cap_str, looks_like_html};
use crate::{
    condense, egress, RecordSink, WebOptions, WEB_PAGE_RECORD_SUBJECT, WRITE_DB_EFFECT_TAG,
};

/// Per-page cap on the downloaded body (bytes) before condensing — mirrors `web.fetch`'s `MAX_BYTES`.
const MAX_PAGE_BYTES: usize = 256 * 1024;
/// Per-page cap on the condensed markdown appended to the crawl digest, so one page can't spend the
/// whole budget in a many-page crawl.
const MAX_PAGE_RENDER_BYTES: usize = 64 * 1024;
/// Hard ceiling on the whole concatenated crawl result handed to the model.
const MAX_TOTAL_RENDER_BYTES: usize = 512 * 1024;
/// Default and hard-ceiling number of pages fetched by one crawl (total round-trips).
const DEFAULT_MAX_PAGES: usize = 10;
const MAX_PAGES_CEILING: usize = 50;
/// Default and hard-ceiling link-distance (hops) from the seed.
const DEFAULT_MAX_DEPTH: usize = 2;
const MAX_DEPTH_CEILING: usize = 5;
/// Hard ceiling on the queued-but-not-yet-fetched frontier, so a link-dense page cannot make the
/// frontier grow without bound even before the page cap stops the crawl.
const MAX_FRONTIER: usize = 512;
/// Per-page request timeout, and the ceiling on it — mirrors `web.fetch`.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;

/// `web.crawl`: bounded breadth-first, same-host crawl from a seed URL. Holds the same egress wiring
/// as [`crate::fetch::WebFetchTool`] — a redirect-disabled client, the resolved `web` scope, an
/// optional audit sink, and the `web.page` record sink.
pub struct WebCrawlTool {
    http: reqwest::Client,
    private_net: PrivateNetAllow,
    audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    grant_source: String,
    records: Option<Arc<dyn RecordSink>>,
}

impl WebCrawlTool {
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

    /// Emit the `PrivateNetAdmit` audit event when a hop is admitted to a private/internal host under
    /// a grant. Mirrors `web.fetch`'s `audit_admit`, gated on `host_resolves_private`.
    fn audit_admit(&self, host: &str) {
        if let Some(audit) = &self.audit {
            if flux_system::net::host_resolves_private(host) {
                audit.record_private_admit("web:web.crawl", host, &self.grant_source);
            }
        }
    }

    /// Fetch one already-guarded page, following redirects under the guard. Returns the final status,
    /// content-type, and capped body bytes. A network/read error is `Err` so the caller can skip the
    /// page without failing the whole crawl.
    async fn fetch_page(&self, url: Url) -> Result<(reqwest::StatusCode, String, Vec<u8>)> {
        // Re-vet and capture the addresses to pin the connection to (C-77). This resolve is the one
        // the connection uses, so there is no rebinding gap between vetting and connect.
        let (url, pinned) =
            flux_system::net::guard_url_scoped_pinned(url.as_str(), &self.private_net)?;
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
            "web.crawl",
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
        let capped = egress::read_body_capped(response, MAX_PAGE_BYTES, "web.crawl").await?;
        Ok((status, content_type, capped.bytes))
    }
}

#[async_trait]
impl Tool for WebCrawlTool {
    fn spec(&self) -> ToolSpec {
        let mut description = String::from(
            "Crawl a small site or section: from a seed URL, follow same-host links breadth-first to \
             a bounded depth and page count, returning each fetched page as condensed markdown. Use \
             this instead of many one-URL `web.fetch` calls when you need to read several linked \
             pages of one site. Bounded and same-host by design — it does NOT follow cross-host \
             links, obey/read robots.txt or sitemaps, or render JavaScript (use the `browser.*` ops \
             for JS-rendered pages). Loopback/private addresses are blocked unless the `web` egress \
             scope grants them.",
        );
        if self.records.is_some() {
            // Disclose the durable side effect in the model-facing spec too (C-58): crawled HTML
            // pages are persisted into the searchable knowledge datasource.
            description.push_str(
                " Crawled HTML pages are also persisted as searchable `web.page` datasource records \
                 (a durable datasource write).",
            );
        }
        let mut spec = ToolSpec::read_only(
            "web.crawl",
            description,
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The absolute http(s) seed URL to crawl from."},
                    "max_pages": {
                        "type": "integer",
                        "description": "Maximum total pages to fetch (default 10, hard cap 50)."
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum link distance from the seed to follow (default 2, hard cap 5)."
                    },
                    "max_total_bytes": {
                        "type": "integer",
                        "description": "Optional caller budget: stop the crawl as soon as the total \
                                        condensed-markdown bytes gathered reach this many. An extra \
                                        upper bound alongside max_pages/max_depth (hard cap 512 KiB); \
                                        the pages already fetched are returned (partial crawl)."
                    }
                },
                "required": ["url"]
            }),
        )
        // `Read` alongside the carrier (C-208): a crawl is a bounded read of a site, and `Network`
        // alone declares an unread egress. Same reasoning as `web.fetch`; every hop still passes
        // through the `web` egress scope.
        .with_effects(vec![Effect::Read, Effect::Network])
        .with_access(vec![AccessKind::Network]);
        // `Conditional`, not the `Idempotent` `read_only()` supplies. A crawl fetches up to 50
        // pages and, with a record sink wired, upserts a `web.page` record per HTML response;
        // `Idempotent` is the word that lets the op cache return a stored result *instead of
        // executing*, which would silently skip all of that. Repeatable, never replayable.
        //
        // Adding `Read` above took this spec out of `is_consequence_bearing`, so I3 no longer
        // fires here — the declaration has to stand on its own now that the invariant stopped
        // watching it.
        spec.idempotency = Idempotency::Conditional;
        if self.records.is_some() {
            // C-210, same reasoning as `web.fetch`: a wired sink makes this op self-declare
            // `write_db`, which is consequence-bearing, so `Risk::Low` would violate I1. `Medium`
            // is honest and removes the op from pre-approval gathering without adding a prompt.
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
        // With a record sink configured, each crawled HTML page is persisted as a durable `web.page`
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
        let seed_raw = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("web.crawl: `url` required".into()))?;
        let max_pages = params
            .get("max_pages")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_PAGES)
            .clamp(1, MAX_PAGES_CEILING);
        let max_depth = params
            .get("max_depth")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_DEPTH)
            .min(MAX_DEPTH_CEILING);
        // Optional caller byte budget: an additional upper bound on the running condensed-content
        // total, never a widening of any axis. Absent → the hard ceiling. Clamped to at least 1 so a
        // budget always still yields the seed page (checked *after* each fetch, partial-crawl `Ok`).
        let byte_budget = params
            .get("max_total_bytes")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(MAX_TOTAL_RENDER_BYTES)
            .clamp(1, MAX_TOTAL_RENDER_BYTES);

        // Guard the seed up front so a bad/private seed is a clean error (mirrors `web.fetch`). Every
        // *discovered* hop is guarded again below and skipped (not fatal) if it is refused.
        let seed_url = flux_system::net::guard_url_scoped(seed_raw, &self.private_net)?;
        let seed_host = seed_url.host_str().unwrap_or_default().to_ascii_lowercase();

        let mut visited: HashSet<String> = HashSet::new();
        let mut frontier: VecDeque<(String, usize)> = VecDeque::new();
        visited.insert(normalize_url(&seed_url));
        frontier.push_back((seed_url.to_string(), 0));

        let mut sections: Vec<String> = Vec::new();
        let mut records: Vec<Record> = Vec::new();
        let mut fetched = 0usize;
        let mut total_render = 0usize;

        while let Some((url_str, depth)) = frontier.pop_front() {
            if fetched >= max_pages {
                break;
            }

            // Guard every hop (SSRF). A discovered link that resolves to a private address without a
            // grant is skipped, not fatal — the seed was already guarded above.
            let url = match flux_system::net::guard_url_scoped(&url_str, &self.private_net) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let (status, content_type, bytes) = match self.fetch_page(url.clone()).await {
                Ok(page) => page,
                // A single page failing (timeout, connection reset, oversized) doesn't kill the crawl.
                Err(_) => continue,
            };
            fetched += 1;

            let raw = String::from_utf8_lossy(&bytes);
            let is_html = content_type.contains("text/html") || looks_like_html(&raw);

            if is_html {
                let md = cap_str(condense::html_to_markdown(&raw), MAX_PAGE_RENDER_BYTES);
                let title = condense::page_title(&raw).unwrap_or_else(|| url.to_string());

                // Same-host, bounded frontier expansion — only when there is depth budget left.
                if depth < max_depth {
                    for link in condense::extract_links(&raw, &url) {
                        if frontier.len() >= MAX_FRONTIER {
                            break;
                        }
                        let Ok(link_url) = Url::parse(&link) else {
                            continue;
                        };
                        // Same-host scope by default: only follow links whose host equals the seed's.
                        let same_host = link_url
                            .host_str()
                            .is_some_and(|h| h.eq_ignore_ascii_case(&seed_host));
                        if !same_host {
                            continue;
                        }
                        let norm = normalize_url(&link_url);
                        if visited.insert(norm) {
                            frontier.push_back((link_url.to_string(), depth + 1));
                        }
                    }
                }

                // Collect a groundable record per HTML page (contributed once, after the loop).
                if self.records.is_some() {
                    records.push(Record::new(
                        Source::new("web"),
                        "web.page",
                        url.as_str(),
                        title,
                        md.clone(),
                    ));
                }

                push_section(
                    &mut sections,
                    &mut total_render,
                    format!("## [{status}] {url}\n\n{md}"),
                );
            } else {
                // Non-HTML pages contribute nothing to the frontier and aren't condensed; note them
                // so the digest is honest about what was reached.
                push_section(
                    &mut sections,
                    &mut total_render,
                    format!(
                        "## [{status}] {url}\n\n_(non-HTML content-type: {content_type}; skipped)_"
                    ),
                );
            }

            // Stop once the running condensed-content total reaches the caller budget (or, absent
            // one, the hard ceiling). The pages already gathered are returned below (partial crawl).
            if total_render >= byte_budget {
                break;
            }
        }

        if let Some(sink) = &self.records {
            if !records.is_empty() {
                sink.contribute(&records);
            }
        }

        let header = format!("Crawled {fetched} page(s) from {seed_url}");
        let content = if sections.is_empty() {
            header
        } else {
            format!("{header}\n\n{}", sections.join("\n\n"))
        };
        Ok(ToolResult::ok(content))
    }
}

/// Append a rendered page section, tracking the running total against the whole-result cap.
fn push_section(sections: &mut Vec<String>, total: &mut usize, section: String) {
    *total += section.len();
    sections.push(section);
}

/// A URL normalized for the visited-set: fragment stripped (same page), everything else preserved.
fn normalize_url(u: &Url) -> String {
    let mut u = u.clone();
    u.set_fragment(None);
    u.to_string()
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
            "flux-web-crawl-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingSink {
        records: Mutex<Vec<Record>>,
    }
    impl RecordSink for RecordingSink {
        fn contribute(&self, records: &[Record]) {
            self.records.lock().unwrap().extend(records.iter().cloned());
        }
    }

    fn tool(private_net: PrivateNetAllow, records: Option<Arc<dyn RecordSink>>) -> WebCrawlTool {
        WebCrawlTool::new(&WebOptions {
            private_net,
            records,
            ..Default::default()
        })
    }

    /// A persistent loopback site: loops accepting connections, routing each by request path to the
    /// matching HTML (or 404). Stays up for the whole crawl (which fetches pages one at a time).
    async fn site_server(pages: Vec<(&'static str, String)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let raw_path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let path = raw_path.split(['?', '#']).next().unwrap_or("/");
                let resp = match pages.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn page(heading: &str, marker: &str, links: &[&str]) -> String {
        let anchors: String = links
            .iter()
            .map(|l| format!("<a href=\"{l}\">go</a> "))
            .collect();
        format!(
            "<html><head><title>{heading}</title></head><body>\
             <h1>{heading}</h1>\
             <p>{marker} this page has enough descriptive body text to be selected as the main \
             readable content region for condensation on this fixture.</p>\
             <nav>{anchors}</nav></body></html>"
        )
    }

    #[tokio::test]
    async fn bfs_stops_at_depth_and_page_caps() {
        // A chain seed -> /a -> /b. At max_depth=1/max_pages=2 the crawl fetches the seed + /a and
        // stops; /b (reachable only at depth 2) is never fetched.
        let base = site_server(vec![
            ("/", page("Seed", "SEEDMARKER", &["/a"])),
            ("/a", page("Page A", "PAGEAMARKER", &["/b"])),
            ("/b", page("Page B", "PAGEBMARKER", &[])),
        ])
        .await;
        let sink = Arc::new(RecordingSink::default());
        let t = tool(PrivateNetAllow::Any, Some(sink.clone()));
        let r = t
            .execute(
                &ctx(),
                json!({ "url": base, "max_depth": 1, "max_pages": 2 }),
            )
            .await
            .unwrap();
        assert!(
            r.content.contains("SEEDMARKER"),
            "seed fetched: {}",
            r.content
        );
        assert!(
            r.content.contains("PAGEAMARKER"),
            "page A fetched: {}",
            r.content
        );
        assert!(
            !r.content.contains("PAGEBMARKER"),
            "page B must NOT be fetched (beyond depth cap): {}",
            r.content
        );
        let recs = sink.records.lock().unwrap();
        assert_eq!(recs.len(), 2, "exactly seed + one page recorded: {recs:?}");
    }

    #[tokio::test]
    async fn byte_budget_stops_crawl_before_page_cap() {
        // Same seed -> /a -> /b chain, but with generous page/depth caps and a tiny byte budget. The
        // seed's own condensed section already crosses the budget, so the crawl stops after the seed:
        // fewer than max_pages pages, capped on bytes rather than page count.
        let base = site_server(vec![
            ("/", page("Seed", "SEEDMARKER", &["/a"])),
            ("/a", page("Page A", "PAGEAMARKER", &["/b"])),
            ("/b", page("Page B", "PAGEBMARKER", &[])),
        ])
        .await;
        let sink = Arc::new(RecordingSink::default());
        let t = tool(PrivateNetAllow::Any, Some(sink.clone()));
        let r = t
            .execute(
                &ctx(),
                json!({ "url": base, "max_depth": 5, "max_pages": 10, "max_total_bytes": 1 }),
            )
            .await
            .unwrap();
        assert!(
            r.content.contains("SEEDMARKER"),
            "seed fetched (partial crawl still returns Ok): {}",
            r.content
        );
        assert!(
            !r.content.contains("PAGEAMARKER"),
            "page A must NOT be fetched — the byte budget was spent by the seed: {}",
            r.content
        );
        assert!(
            !r.content.contains("PAGEBMARKER"),
            "page B must NOT be fetched: {}",
            r.content
        );
        let recs = sink.records.lock().unwrap();
        assert_eq!(
            recs.len(),
            1,
            "only the seed recorded before the byte budget stopped the crawl: {recs:?}"
        );
    }

    #[tokio::test]
    async fn off_host_links_are_not_followed() {
        // An off-host page with a distinctive marker. The seed links to it by a *different host
        // spelling* (localhost vs 127.0.0.1) plus a same-host relative link.
        let off = site_server(vec![("/off", page("Off", "OFFMARKER", &[]))]).await;
        let off_port = off.rsplit(':').next().unwrap();
        let seed = site_server(vec![
            (
                "/",
                page(
                    "Seed",
                    "SEEDMARKER",
                    &["/on", &format!("http://localhost:{off_port}/off")],
                ),
            ),
            ("/on", page("On", "ONMARKER", &[])),
        ])
        .await;
        let sink = Arc::new(RecordingSink::default());
        let t = tool(PrivateNetAllow::Any, Some(sink.clone()));
        let r = t.execute(&ctx(), json!({ "url": seed })).await.unwrap();
        assert!(r.content.contains("SEEDMARKER"), "seed: {}", r.content);
        assert!(
            r.content.contains("ONMARKER"),
            "same-host link followed: {}",
            r.content
        );
        assert!(
            !r.content.contains("OFFMARKER"),
            "off-host link must NOT be followed: {}",
            r.content
        );
        let recs = sink.records.lock().unwrap();
        assert_eq!(recs.len(), 2, "only seed + same-host page: {recs:?}");
    }

    #[tokio::test]
    async fn with_record_sink_declares_the_datasource_write_it_performs() {
        // Contract (C-58): configured WITH a record sink, `web.crawl` durably persists each fetched
        // HTML page as a searchable `web.page` datasource record. That persistence is disclosed — as
        // the `write_db` semantic effect + a `datasource:web.page` permission subject — so the
        // DECLARED contract matches the records actually contributed, instead of a bare network read
        // silently becoming durable local storage.
        let sink = Arc::new(RecordingSink::default());
        let t = tool(PrivateNetAllow::Any, Some(sink.clone()));

        assert!(
            t.semantic_effects().iter().any(|e| e == "write_db"),
            "sink-configured web.crawl must declare the `write_db` datasource-write effect: {:?}",
            t.semantic_effects()
        );
        let subjects = t.permission_subjects(&json!({ "url": "http://example.com/" }));
        assert!(
            subjects.iter().any(|s| s == "datasource:web.page"),
            "must name the durable `web.page` record target as a permission subject: {subjects:?}"
        );
        assert!(
            subjects.iter().any(|s| s == "http://example.com/"),
            "still names the seed URL: {subjects:?}"
        );
        assert_eq!(
            t.authority_requirements(&json!({ "url": "http://example.com/" }), &subjects)
                .unwrap(),
            vec![
                AuthorityRequirement::network_fetch("http://example.com/"),
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
        let base = site_server(vec![("/", page("Seed", "SEEDMARKER", &[]))]).await;
        t.execute(&ctx(), json!({ "url": base, "max_pages": 1 }))
            .await
            .unwrap();
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
    async fn sink_backed_crawl_requires_the_datasource_authority_at_dispatch() {
        let build = |policy: AuthorizationPolicy| {
            let crawl = Arc::new(tool(
                PrivateNetAllow::None,
                Some(Arc::new(RecordingSink::default())),
            ));
            let mut registry = ToolRegistry::new();
            registry
                .try_register_from("sink-backed web crawl", crawl)
                .unwrap();
            Executor::new(
                registry,
                PermissionManager::from_rules(&["web.crawl".into()], &[]),
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
            .dispatch_outcome("web.crawl", json!({"url": "http://127.0.0.1/"}))
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
            .dispatch_outcome("web.crawl", json!({"url": "http://127.0.0.1/"}))
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
        let subjects = t.permission_subjects(&json!({ "url": "http://example.com/" }));
        assert_eq!(
            subjects,
            vec!["http://example.com/".to_string()],
            "no sink ⇒ the seed URL is the only subject (no datasource target): {subjects:?}"
        );
        assert_eq!(
            t.authority_requirements(&json!({ "url": "http://example.com/" }), &subjects)
                .unwrap(),
            vec![AuthorityRequirement::network_fetch("http://example.com/")]
        );
        // `Read` + `Network` — a bounded read of a site, not an unread egress (C-208).
        assert_eq!(t.spec().effects, vec![Effect::Read, Effect::Network]);
        assert_eq!(t.spec().access, vec![AccessKind::Network]);
        assert!(flux_spec::metadata_violations(&t.spec(), &t.semantic_effects()).is_empty());
        // Pinned explicitly: declaring `Read` above takes the spec out of
        // `is_consequence_bearing`, so `metadata_violations` no longer checks idempotency here.
        // A crawl replayed from the op cache would skip up to 50 live fetches and every
        // `web.page` record they contribute.
        assert_eq!(t.spec().idempotency, Idempotency::Conditional);
    }

    /// C-210, the `web.fetch` posture applied to the crawl — stated rather than left to emerge.
    /// A wired sink means up to 50 durable `web.page` upserts, which is not something to perform
    /// before a human has seen the plan; unwired, the op is a pure bounded read and stays
    /// gather-safe.
    #[tokio::test]
    async fn a_sink_wired_crawl_is_consequence_bearing_and_leaves_the_gather_path() {
        let wired = tool(
            PrivateNetAllow::Any,
            Some(Arc::new(RecordingSink::default())),
        );
        assert!(flux_spec::is_consequence_bearing_with_effects(
            &wired.spec(),
            &wired.semantic_effects()
        ));
        assert_eq!(wired.spec().risk, Risk::Medium);
        assert!(
            flux_spec::metadata_violations(&wired.spec(), &wired.semantic_effects()).is_empty(),
            "the raised tier is what keeps I1 satisfied: {:?}",
            flux_spec::metadata_violations(&wired.spec(), &wired.semantic_effects())
        );

        let catalog_only = tool(PrivateNetAllow::Any, None);
        assert!(!flux_spec::is_consequence_bearing_with_effects(
            &catalog_only.spec(),
            &catalog_only.semantic_effects()
        ));
        assert_eq!(catalog_only.spec().risk, Risk::Low);
    }

    #[tokio::test]
    async fn private_seed_refused_without_grant_but_ok_with_grant() {
        // Loopback seed: refused under the full SSRF guard, admitted with the `web` grant.
        let base = site_server(vec![("/", page("Seed", "SEEDMARKER", &[]))]).await;
        let denied = tool(PrivateNetAllow::None, None);
        assert!(
            denied
                .execute(&ctx(), json!({ "url": base.clone() }))
                .await
                .is_err(),
            "a loopback seed must be refused without a `web` grant"
        );

        let allowed = tool(PrivateNetAllow::Any, None);
        let r = allowed
            .execute(&ctx(), json!({ "url": base }))
            .await
            .unwrap();
        assert!(
            r.content.contains("SEEDMARKER"),
            "admitted with grant: {}",
            r.content
        );
    }
}
