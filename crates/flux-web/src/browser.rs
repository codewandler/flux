//! Tier 3 — non-visual browser sessions (D-121 · D-122 · D-123 · D-124).
//!
//! A [`BrowserSession`] drives headless Chromium through the [`crate::cdp`] client: it opens a page,
//! observes it as a byte-budgeted [digest](crate::digest) (what a screen reader sees + a resolved
//! action space of stable `e<N>` refs), acts on refs and returns **deltas**, and routes **every**
//! subrequest through the family-wide `web` egress guard via CDP `Fetch` interception. Chrome is
//! spawned as a direct child through the guarded [`flux_system::System::spawn_debug_pipe`] seam
//! (argv-only, env-cleared, fd-3/4 CDP pipe). The `browser.*` ops are evidence-gated behind a
//! Chromium-discoverable signal so they never mislead the planner on a machine without a browser.
//!
//! Everything below the [`ops`] boundary is transport-agnostic — [`BrowserSession::from_client`]
//! takes a [`CdpClient`], so tests drive a scripted fake over an in-memory duplex (no Chrome in CI).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use flux_core::{Error, Result};
use flux_runtime::{AuthorityRequirement, Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::net::{guard_url_scoped, host_resolves_private, PrivateNetAllow};

use crate::cdp::{CdpClient, CdpEvent};
use crate::digest::{build_digest, DigestCaps, RefMap, View};

/// The evidence-gated group the browser ops belong to.
pub const BROWSER_GROUP: &str = "browser";
/// The signal that surfaces [`BROWSER_GROUP`]: a Chromium binary is discoverable.
pub const BROWSER_SIGNAL: &str = "browser";
/// The op names in the browser group (the group owns its membership).
pub const BROWSER_OPS: [&str; 5] = [
    "browser.open",
    "browser.goto",
    "browser.snapshot",
    "browser.close",
    "browser.act",
];

/// The evidence-gated `browser` [`flux_evidence::ToolGroup`]: its ops are advertised only when the
/// `browser` signal is present (a Chromium binary is discoverable — see the `detect_signals` probe).
pub fn browser_group() -> flux_evidence::ToolGroup {
    flux_evidence::ToolGroup {
        name: BROWSER_GROUP.into(),
        description:
            "Non-visual browser automation (open/goto/snapshot/act/close) — surfaced only \
                      when a Chromium binary is discoverable."
                .into(),
        tools: BROWSER_OPS.iter().map(|s| s.to_string()).collect(),
        surface_when: vec![flux_evidence::SignalMatch {
            kind: flux_evidence::KIND_SIGNAL.into(),
            signal: Some(BROWSER_SIGNAL.into()),
        }],
    }
}

/// Chrome binaries to look for on `PATH`, in order.
const CHROME_CANDIDATES: [&str; 6] = [
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "chrome",
    "google-chrome-unstable",
];

/// Discover a Chromium binary: `FLUX_BROWSER_BIN` → configured path → `PATH` candidates. **No
/// auto-download** (supply-chain stance) — a missing browser is an actionable error naming the order.
pub fn discover_chrome(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("FLUX_BROWSER_BIN") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Some(c) = configured.filter(|c| !c.is_empty()) {
        return Ok(PathBuf::from(c));
    }
    if let Some(p) = CHROME_CANDIDATES.iter().find_map(|c| which_on_path(c)) {
        return Ok(p);
    }
    Err(Error::Other(format!(
        "no Chromium found. Set FLUX_BROWSER_BIN, configure a path, or install one of: {}",
        CHROME_CANDIDATES.join(", ")
    )))
}

/// Whether a Chromium binary is discoverable (the `browser`-group evidence signal).
pub fn chromium_present(configured: Option<&str>) -> bool {
    discover_chrome(configured).is_ok()
}

/// First match for `name` on `PATH` (executable check best-effort).
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Observable state accumulated between observations (drives deltas + the interception audit).
#[derive(Default)]
struct ObserveState {
    loaded: bool,
    /// A navigation is in flight (a frame started loading and hasn't fired `load` yet).
    navigating: bool,
    url: String,
    title: String,
    /// Console errors since the last observe.
    console_errors: Vec<String>,
    /// JS dialogs auto-surfaced (and auto-dismissed to avoid hangs) since the last observe.
    dialogs: Vec<String>,
    /// Subrequests the egress guard refused since the last observe (the model sees *why* the page
    /// is broken).
    egress_refusals: Vec<String>,
}

struct SessionInner {
    client: Arc<CdpClient>,
    /// The attached page's CDP `sessionId` (flattened mode).
    page_session: String,
    refs: Mutex<RefMap>,
    state: Mutex<ObserveState>,
    load_notify: Notify,
    private_net: PrivateNetAllow,
    audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    grant_source: String,
    /// The Chrome child (present for a real launch; `None` for a client-injected test session). Only
    /// the process handle is kept — the CDP pipe was split into `client` at launch.
    child: Mutex<Option<tokio::process::Child>>,
    /// The ephemeral profile dir to remove on close.
    profile_dir: Mutex<Option<PathBuf>>,
    last_used: Mutex<Instant>,
    pump: Mutex<Option<JoinHandle<()>>>,
}

/// A live browser page session.
#[derive(Clone)]
pub struct BrowserSession(Arc<SessionInner>);

impl BrowserSession {
    /// Build a session around an already-connected [`CdpClient`] + its event stream, enabling the
    /// page domains + `Fetch` interception and starting the event pump. Used by the ops-layer
    /// `launch_session` and, directly, by tests (no Chrome).
    pub async fn from_client(
        client: Arc<CdpClient>,
        events: tokio::sync::mpsc::Receiver<CdpEvent>,
        page_session: String,
        private_net: PrivateNetAllow,
        audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
        grant_source: String,
        child: Option<tokio::process::Child>,
    ) -> Result<Self> {
        // Enable the domains we drive. Fetch.enable with no patterns intercepts every request at the
        // Request stage — the D-124 egress chokepoint (no off switch: this *is* the policy).
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("DOM.enable", json!({})),
            ("Accessibility.enable", json!({})),
            ("Log.enable", json!({})),
            ("Fetch.enable", json!({})),
        ] {
            client
                .call_on(method, params, Some(&page_session))
                .await
                .map_err(|e| Error::Other(format!("browser: enable {method}: {e}")))?;
        }

        let inner = Arc::new(SessionInner {
            client,
            page_session,
            refs: Mutex::new(RefMap::new()),
            state: Mutex::new(ObserveState::default()),
            load_notify: Notify::new(),
            private_net,
            audit,
            grant_source,
            child: Mutex::new(child),
            profile_dir: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
            pump: Mutex::new(None),
        });
        let pump = tokio::spawn(pump_loop(inner.clone(), events));
        *inner.pump.lock().unwrap() = Some(pump);
        Ok(BrowserSession(inner))
    }

    /// Navigate to `url` and wait (bounded) for load. The nav URL is guarded up front; every
    /// subrequest is guarded by the interception pump (D-124).
    pub async fn goto(&self, url: &str) -> Result<()> {
        guard_url_scoped(url, &self.0.private_net)?;
        {
            let mut st = self.0.state.lock().unwrap();
            st.loaded = false;
            st.console_errors.clear();
            st.dialogs.clear();
            st.egress_refusals.clear();
        }
        self.0
            .client
            .call_on(
                "Page.navigate",
                json!({ "url": url }),
                Some(&self.0.page_session),
            )
            .await
            .map_err(|e| Error::Other(format!("browser.goto: {e}")))?;
        self.await_load(Duration::from_secs(20)).await;
        self.touch();
        Ok(())
    }

    /// Wait until the pump reports load (or the deadline passes — a slow page still yields a digest).
    async fn await_load(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.0.load_notify.notified();
            if self.0.state.lock().unwrap().loaded {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep(remaining) => return,
            }
        }
    }

    /// Observe the page as a digest, updating the ref map.
    pub async fn snapshot(&self, view: View, caps: DigestCaps) -> Result<String> {
        self.touch();
        let ax = self
            .0
            .client
            .call_on(
                "Accessibility.getFullAXTree",
                json!({}),
                Some(&self.0.page_session),
            )
            .await
            .map_err(|e| Error::Other(format!("browser.snapshot: getFullAXTree: {e}")))?;
        let (url, title) = self.page_url_title().await;
        {
            let mut st = self.0.state.lock().unwrap();
            st.url = url.clone();
            st.title = title.clone();
        }
        let mut refs = self.0.refs.lock().unwrap();
        Ok(build_digest(&url, &title, &ax, &mut refs, view, caps))
    }

    /// Fetch the current document url + title (best-effort).
    async fn page_url_title(&self) -> (String, String) {
        let url = self
            .0
            .client
            .call_on(
                "Runtime.evaluate",
                json!({ "expression": "location.href", "returnByValue": true }),
                Some(&self.0.page_session),
            )
            .await
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let title = self
            .0
            .client
            .call_on(
                "Runtime.evaluate",
                json!({ "expression": "document.title", "returnByValue": true }),
                Some(&self.0.page_session),
            )
            .await
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        (url, title)
    }

    /// Perform an action against a digest ref (or a bare nav) and return a **delta digest** — what
    /// changed — not the whole page (`full` forces a full digest).
    pub async fn act(
        &self,
        action: &str,
        reef: Option<u32>,
        value: Option<&str>,
        full: bool,
    ) -> Result<String> {
        self.touch();
        // Snapshot the pre-action ref set (for the added/removed diff) + reset the observe buffers.
        let before: std::collections::HashSet<u32> = {
            let refs = self.0.refs.lock().unwrap();
            (1..=refs_high(&refs))
                .filter(|&n| refs.is_alive(n))
                .collect()
        };
        {
            let mut st = self.0.state.lock().unwrap();
            st.console_errors.clear();
            st.dialogs.clear();
            st.egress_refusals.clear();
        }
        let before_url = self.0.state.lock().unwrap().url.clone();

        self.dispatch_action(action, reef, value).await?;
        // Bounded auto-wait: a brief probe to see whether the act kicked off a navigation, and only
        // then wait (hard-capped) for load — so a non-navigating click settles fast.
        tokio::time::sleep(Duration::from_millis(250)).await;
        if self.0.state.lock().unwrap().navigating {
            self.await_load(Duration::from_secs(10)).await;
        }

        // Re-observe.
        let after_digest = self.snapshot(View::Full, DigestCaps::default()).await?;
        if full {
            return Ok(after_digest);
        }
        let (added, removed) = {
            let refs = self.0.refs.lock().unwrap();
            let after: std::collections::HashSet<u32> = (1..=refs_high(&refs))
                .filter(|&n| refs.is_alive(n))
                .collect();
            let added: Vec<u32> = after.difference(&before).copied().collect();
            let removed: Vec<u32> = before.difference(&after).copied().collect();
            (sorted(added), sorted(removed))
        };
        Ok(self.render_delta(&before_url, &added, &removed))
    }

    /// Render a compact delta from the observe buffers + the ref diff.
    fn render_delta(&self, before_url: &str, added: &[u32], removed: &[u32]) -> String {
        let st = self.0.state.lock().unwrap();
        let mut out = String::new();
        out.push_str("## delta\n");
        if st.url != before_url {
            out.push_str(&format!("navigated: {} · {:?}\n", st.url, st.title));
        }
        if !added.is_empty() {
            out.push_str(&format!(
                "added: {}\n",
                added
                    .iter()
                    .map(|n| format!("e{n}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if !removed.is_empty() {
            out.push_str(&format!(
                "removed: {}\n",
                removed
                    .iter()
                    .map(|n| format!("e{n}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        for d in &st.dialogs {
            out.push_str(&format!("dialog: {d}\n"));
        }
        for e in &st.console_errors {
            out.push_str(&format!("console-error: {e}\n"));
        }
        for r in &st.egress_refusals {
            out.push_str(&format!("blocked (egress policy): {r}\n"));
        }
        if out.trim() == "## delta" {
            out.push_str("(no observable change)\n");
        }
        out.trim_end().to_string()
    }

    /// Dispatch a single action via CDP.
    async fn dispatch_action(
        &self,
        action: &str,
        reef: Option<u32>,
        value: Option<&str>,
    ) -> Result<()> {
        let sid = self.0.page_session.clone();
        match action {
            "goto" => {
                let url = value.ok_or_else(|| {
                    Error::Other("browser.act goto: `value` (url) required".into())
                })?;
                self.goto(url).await
            }
            "back" => {
                self.eval("history.back()").await?;
                Ok(())
            }
            "scroll" => {
                let dy = value.and_then(|v| v.parse::<i64>().ok()).unwrap_or(600);
                self.eval(&format!("window.scrollBy(0, {dy})")).await?;
                Ok(())
            }
            "press" => {
                let key = value.ok_or_else(|| {
                    Error::Other("browser.act press: `value` (key) required".into())
                })?;
                for t in ["keyDown", "keyUp"] {
                    self.0
                        .client
                        .call_on(
                            "Input.dispatchKeyEvent",
                            json!({ "type": t, "key": key }),
                            Some(&sid),
                        )
                        .await
                        .map_err(|e| Error::Other(format!("browser.act press: {e}")))?;
                }
                Ok(())
            }
            "click" | "type" | "fill" | "select" => {
                let reef = reef
                    .ok_or_else(|| Error::Other(format!("browser.act {action}: `ref` required")))?;
                let object_id = self.resolve_ref(reef).await?;
                match action {
                    "click" => {
                        self.call_on_node(&object_id, "function(){ this.click(); }", &[])
                            .await
                    }
                    "type" | "fill" | "select" => {
                        let v = value.unwrap_or("");
                        self.call_on_node(
                            &object_id,
                            "function(v){ this.focus && this.focus(); this.value = v; \
                             this.dispatchEvent(new Event('input',{bubbles:true})); \
                             this.dispatchEvent(new Event('change',{bubbles:true})); }",
                            &[json!(v)],
                        )
                        .await
                    }
                    _ => unreachable!(),
                }
            }
            other => Err(Error::Other(format!(
                "browser.act: unknown action {other:?}"
            ))),
        }
    }

    /// Resolve a digest ref to a live JS object id (or a structured dead-ref error).
    async fn resolve_ref(&self, reef: u32) -> Result<String> {
        let backend = {
            let refs = self.0.refs.lock().unwrap();
            if !refs.is_alive(reef) {
                return Err(Error::Other(format!(
                    "browser.act: ref e{reef} is not on the current page (dead or unknown) — snapshot again"
                )));
            }
            refs.backend_of(reef)
                .ok_or_else(|| Error::Other(format!("browser.act: unknown ref e{reef}")))?
        };
        let resolved = self
            .0
            .client
            .call_on(
                "DOM.resolveNode",
                json!({ "backendNodeId": backend }),
                Some(&self.0.page_session),
            )
            .await
            .map_err(|e| Error::Other(format!("browser.act: resolve e{reef}: {e}")))?;
        resolved
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Other(format!("browser.act: e{reef} has no live object")))
    }

    async fn call_on_node(&self, object_id: &str, func: &str, args: &[Value]) -> Result<()> {
        let arguments: Vec<Value> = args.iter().map(|a| json!({ "value": a })).collect();
        self.0
            .client
            .call_on(
                "Runtime.callFunctionOn",
                json!({ "objectId": object_id, "functionDeclaration": func, "arguments": arguments }),
                Some(&self.0.page_session),
            )
            .await
            .map(|_| ())
            .map_err(|e| Error::Other(format!("browser.act: dispatch: {e}")))
    }

    async fn eval(&self, expr: &str) -> Result<()> {
        self.0
            .client
            .call_on(
                "Runtime.evaluate",
                json!({ "expression": expr }),
                Some(&self.0.page_session),
            )
            .await
            .map(|_| ())
            .map_err(|e| Error::Other(format!("browser.act: eval: {e}")))
    }

    /// Close the page + Chrome child and clean up the ephemeral profile.
    pub async fn close(&self) -> Result<()> {
        let _ = self
            .0
            .client
            .call_on("Page.close", json!({}), Some(&self.0.page_session))
            .await;
        if let Some(pump) = self.0.pump.lock().unwrap().take() {
            pump.abort();
        }
        if let Some(mut child) = self.0.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        if let Some(dir) = self.0.profile_dir.lock().unwrap().take() {
            // flux-allow-direct-io: ephemeral browser profile dir (host infra, not a model-directed
            // path) — cleanup on session close.
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    fn touch(&self) {
        *self.0.last_used.lock().unwrap() = Instant::now();
    }

    fn idle(&self) -> Duration {
        self.0.last_used.lock().unwrap().elapsed()
    }

    /// The Chrome child's PID (for the live-smoke no-orphan assertion).
    #[cfg(test)]
    fn test_child_pid(&self) -> Option<u32> {
        self.0.child.lock().unwrap().as_ref().and_then(|c| c.id())
    }
}

/// Highest ref number issued so far (for iterating the ref set).
fn refs_high(refs: &RefMap) -> u32 {
    // RefMap doesn't expose `next`; probe upward until backend_of returns None twice past a gap.
    let mut hi = 0;
    let mut n = 1;
    let mut misses = 0;
    while misses < 4 && n < 100_000 {
        if refs.backend_of(n).is_some() {
            hi = n;
            misses = 0;
        } else {
            misses += 1;
        }
        n += 1;
    }
    hi
}

fn sorted(mut v: Vec<u32>) -> Vec<u32> {
    v.sort_unstable();
    v
}

/// The event pump: routes Fetch interception through the egress guard and tracks load/console/dialogs.
async fn pump_loop(inner: Arc<SessionInner>, mut events: tokio::sync::mpsc::Receiver<CdpEvent>) {
    while let Some(ev) = events.recv().await {
        match ev.method.as_str() {
            "Fetch.requestPaused" => handle_fetch(&inner, &ev).await,
            "Page.frameStartedLoading" | "Page.frameRequestedNavigation" => {
                let mut st = inner.state.lock().unwrap();
                st.navigating = true;
                st.loaded = false;
            }
            "Page.loadEventFired" => {
                let mut st = inner.state.lock().unwrap();
                st.loaded = true;
                st.navigating = false;
                drop(st);
                inner.load_notify.notify_waiters();
            }
            "Page.lifecycleEvent" => {
                if matches!(
                    ev.params.get("name").and_then(Value::as_str),
                    Some("load") | Some("networkIdle") | Some("DOMContentLoaded")
                ) {
                    let mut st = inner.state.lock().unwrap();
                    st.loaded = true;
                    st.navigating = false;
                    drop(st);
                    inner.load_notify.notify_waiters();
                }
            }
            "Runtime.consoleAPICalled" => {
                if ev.params.get("type").and_then(Value::as_str) == Some("error") {
                    let msg = ev
                        .params
                        .get("args")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|a| a.get("value"))
                        .and_then(Value::as_str)
                        .unwrap_or("(error)")
                        .to_string();
                    inner.state.lock().unwrap().console_errors.push(msg);
                }
            }
            "Log.entryAdded" => {
                let entry = ev.params.get("entry");
                if entry.and_then(|e| e.get("level")).and_then(Value::as_str) == Some("error") {
                    let msg = entry
                        .and_then(|e| e.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("(error)")
                        .to_string();
                    inner.state.lock().unwrap().console_errors.push(msg);
                }
            }
            "Page.javascriptDialogOpening" => {
                let text = ev
                    .params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                inner.state.lock().unwrap().dialogs.push(text);
                // Auto-dismiss so navigation never hangs; the delta surfaces the dialog.
                let _ = inner
                    .client
                    .call_on(
                        "Page.handleJavaScriptDialog",
                        json!({ "accept": false }),
                        Some(&inner.page_session),
                    )
                    .await;
            }
            _ => {}
        }
    }
}

/// D-124: guard one intercepted request. Non-http(s) schemes (data:/blob:/about:) pass through; every
/// http(s) request runs `guard_url_scoped` with the session's `web`-scope allow-list; a violation
/// fails the request and is recorded; an admitted private host audits `PrivateNetAdmit`.
async fn handle_fetch(inner: &Arc<SessionInner>, ev: &CdpEvent) {
    let request_id = ev
        .params
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let url = ev
        .params
        .get("request")
        .and_then(|r| r.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let sid = if ev.session_id.is_empty() {
        inner.page_session.clone()
    } else {
        ev.session_id.clone()
    };

    let scheme_ok = url.starts_with("http://") || url.starts_with("https://");
    if !scheme_ok {
        let _ = inner
            .client
            .call_on(
                "Fetch.continueRequest",
                json!({ "requestId": request_id }),
                Some(&sid),
            )
            .await;
        return;
    }

    match guard_url_scoped(url, &inner.private_net) {
        Ok(parsed) => {
            // Admitted. If it reached a private host, it was admitted under the `web` grant — audit it.
            if let Some(host) = parsed.host_str() {
                if host_resolves_private(host) {
                    if let Some(audit) = &inner.audit {
                        audit.record_private_admit("web:browser", host, &inner.grant_source);
                    }
                }
            }
            let _ = inner
                .client
                .call_on(
                    "Fetch.continueRequest",
                    json!({ "requestId": request_id }),
                    Some(&sid),
                )
                .await;
        }
        Err(_) => {
            inner
                .state
                .lock()
                .unwrap()
                .egress_refusals
                .push(url.to_string());
            let _ = inner
                .client
                .call_on(
                    "Fetch.failRequest",
                    json!({ "requestId": request_id, "errorReason": "AccessDenied" }),
                    Some(&sid),
                )
                .await;
        }
    }
}

/// Create an ephemeral, isolated browser profile dir under the system temp dir (host infra, removed
/// on close — not model-directed path IO).
fn ephemeral_profile_dir() -> Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flux-browser-{}-{}", std::process::id(), n));
    // flux-allow-direct-io: ephemeral browser profile dir under the system temp dir (host infra,
    // not a model-directed path) — created for the guarded Chromium child spawned via flux-system.
    std::fs::create_dir_all(&dir).map_err(|e| Error::Other(format!("browser profile dir: {e}")))?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Session registry (Arc<Mutex<…>> stateful-tool shape, idle TTL)
// ---------------------------------------------------------------------------

const SESSION_TTL: Duration = Duration::from_secs(300);

/// In-process registry of live browser sessions, keyed by opaque id, with a lazily-swept idle TTL —
/// the `EndpointBroker`/`SqliteBackend`/`ReadTracker` stateful-native-tool shape.
pub struct SessionRegistry {
    sessions: Mutex<HashMap<String, BrowserSession>>,
    next: std::sync::atomic::AtomicU64,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn insert(&self, session: BrowserSession) -> String {
        self.sweep();
        let id = format!(
            "br-{}",
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.sessions.lock().unwrap().insert(id.clone(), session);
        id
    }

    fn get(&self, id: &str) -> Result<BrowserSession> {
        self.sweep();
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Other(format!("browser: no session {id:?} (expired or closed)")))
    }

    async fn remove(&self, id: &str) -> Result<()> {
        let s = self.sessions.lock().unwrap().remove(id);
        match s {
            Some(session) => session.close().await,
            None => Err(Error::Other(format!("browser: no session {id:?}"))),
        }
    }

    /// Close + drop any session idle past the TTL.
    fn sweep(&self) {
        let expired: Vec<(String, BrowserSession)> = {
            let mut map = self.sessions.lock().unwrap();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, s)| s.idle() > SESSION_TTL)
                .map(|(k, _)| k.clone())
                .collect();
            ids.into_iter()
                .filter_map(|k| map.remove(&k).map(|s| (k, s)))
                .collect()
        };
        for (_, s) in expired {
            // Best-effort teardown; the pump/child are killed on drop anyway (`kill_on_drop`).
            if let Some(pump) = s.0.pump.lock().unwrap().take() {
                pump.abort();
            }
            if let Some(mut child) = s.0.child.lock().unwrap().take() {
                let _ = child.start_kill();
            }
            if let Some(dir) = s.0.profile_dir.lock().unwrap().take() {
                // flux-allow-direct-io: ephemeral browser profile dir (host infra, not a
                // model-directed path) — cleanup on idle sweep.
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ops
// ---------------------------------------------------------------------------

/// Config the browser ops carry: discovery hint + the family-wide `web` egress scope + audit.
#[derive(Clone, Default)]
pub struct BrowserConfig {
    pub bin: Option<String>,
    pub private_net: PrivateNetAllow,
    pub audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    pub grant_source: String,
}

fn browser_open_effects() -> Vec<Effect> {
    vec![Effect::Process, Effect::Network, Effect::Browser]
}

fn browser_navigation_effects() -> Vec<Effect> {
    vec![Effect::Network, Effect::Browser]
}

/// `browser.open {url?}` — spawn headless Chromium, attach a page, optionally navigate, return the
/// session id + the first digest.
pub struct BrowserOpenTool {
    pub registry: Arc<SessionRegistry>,
    pub config: BrowserConfig,
}

#[async_trait]
impl Tool for BrowserOpenTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser.open".into(),
            description: "Open a headless browser session (spawns Chromium) and optionally navigate \
                to `url`. Returns a `session` id and a page digest — condensed content plus an action \
                space of stable `e<N>` refs. Drive it with browser.goto/snapshot/act and browser.close \
                when done. Non-visual: you never receive HTML source or screenshots."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "url": {"type": "string", "description": "Optional URL to navigate to on open."} }
            }),
            output_schema: None,
            effects: browser_open_effects(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::Network, AccessKind::Browser],
            group: Some(BROWSER_GROUP.into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("url")
            .and_then(Value::as_str)
            .map(|url| vec![url.to_string()])
            .unwrap_or_default()
    }

    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        let process = self
            .config
            .bin
            .as_deref()
            .map(str::trim)
            .filter(|bin| !bin.is_empty())
            .unwrap_or("*");
        let destination = params
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or("*");
        Ok(vec![
            AuthorityRequirement::process_exec(process),
            AuthorityRequirement::network_fetch(destination),
            AuthorityRequirement::browser_navigate(destination),
        ])
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(url) = params.get("url").and_then(Value::as_str) {
            set.push(Intent {
                behavior: IntentBehavior::BrowserNavigate,
                target: IntentTarget::Browser {
                    url: url.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let session = launch_session(&ctx.system(), &self.config).await?;
        if let Some(url) = params.get("url").and_then(Value::as_str) {
            session.goto(url).await?;
        }
        let digest = session.snapshot(View::Full, DigestCaps::default()).await?;
        let id = self.registry.insert(session);
        Ok(ToolResult::ok(format!("session: {id}\n{digest}")))
    }
}

/// `browser.goto {session, url}`.
pub struct BrowserGotoTool {
    pub registry: Arc<SessionRegistry>,
}

#[async_trait]
impl Tool for BrowserGotoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser.goto".into(),
            description:
                "Navigate an open browser session to a URL; returns a delta of what changed.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "url": {"type": "string"}
                },
                "required": ["session", "url"]
            }),
            output_schema: None,
            effects: browser_navigation_effects(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Network, AccessKind::Browser],
            group: Some(BROWSER_GROUP.into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("url")
            .and_then(Value::as_str)
            .map(|url| vec![url.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let (session, id) = session_arg(&self.registry, &params)?;
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("browser.goto: `url` required".into()))?;
        session
            .act("goto", None, Some(url), false)
            .await
            .map(|delta| ToolResult::ok(format!("session: {id}\n{delta}")))
    }
}

/// `browser.snapshot {session, view?}`.
pub struct BrowserSnapshotTool {
    pub registry: Arc<SessionRegistry>,
}

#[async_trait]
impl Tool for BrowserSnapshotTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser.snapshot".into(),
            description:
                "Re-observe an open browser session as a digest. `view`: full (default) | \
                actions | content."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "view": {"type": "string", "description": "full | actions | content"}
                },
                "required": ["session"]
            }),
            output_schema: None,
            effects: vec![Effect::Browser],
            risk: Risk::Low,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Browser],
            group: Some(BROWSER_GROUP.into()),
        }
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let (session, _id) = session_arg(&self.registry, &params)?;
        let view = View::parse(params.get("view").and_then(Value::as_str).unwrap_or("full"));
        session
            .snapshot(view, DigestCaps::default())
            .await
            .map(ToolResult::ok)
    }
}

/// `browser.act {session, action, ref?, value?, full?}`.
pub struct BrowserActTool {
    pub registry: Arc<SessionRegistry>,
}

#[async_trait]
impl Tool for BrowserActTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser.act".into(),
            description: "Act on an open browser session and return a delta of what changed. \
                `action`: click | type | fill | select | press | scroll | goto | back. `ref` is an \
                `e<N>` number from the digest (for click/type/fill/select); `value` is the text/url/key \
                (for type/fill/select/press/goto/scroll). Set `full: true` for a full digest instead \
                of a delta."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "action": {"type": "string"},
                    "ref": {"type": ["integer", "string"], "description": "The e<N> ref number (or \"eN\")."},
                    "value": {"type": "string"},
                    "full": {"type": "boolean"}
                },
                "required": ["session", "action"]
            }),
            output_schema: None,
            // Acting can submit forms and mutate remote state — honest Network + Browser + Medium risk.
            effects: browser_navigation_effects(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Network, AccessKind::Browser],
            group: Some(BROWSER_GROUP.into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        if params.get("action").and_then(Value::as_str) != Some("goto") {
            return Vec::new();
        }
        params
            .get("value")
            .and_then(Value::as_str)
            .map(|url| vec![url.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        let action = params.get("action").and_then(Value::as_str).unwrap_or("");
        set.push(Intent {
            behavior: IntentBehavior::NetworkFetch,
            target: IntentTarget::Browser {
                url: format!("browser.act:{action}"),
            },
            role: if matches!(action, "click" | "goto" | "back") {
                IntentRole::ReadTarget
            } else {
                IntentRole::WriteTarget
            },
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let (session, _id) = session_arg(&self.registry, &params)?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("browser.act: `action` required".into()))?;
        let reef = parse_ref(params.get("ref"));
        let value = params.get("value").and_then(Value::as_str);
        let full = params.get("full").and_then(Value::as_bool).unwrap_or(false);
        session
            .act(action, reef, value, full)
            .await
            .map(ToolResult::ok)
    }
}

/// `browser.close {session}`.
pub struct BrowserCloseTool {
    pub registry: Arc<SessionRegistry>,
}

#[async_trait]
impl Tool for BrowserCloseTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser.close".into(),
            description: "Close a browser session and its Chromium child.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "session": {"type": "string"} },
                "required": ["session"]
            }),
            output_schema: None,
            effects: vec![Effect::Process],
            risk: Risk::Low,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: Some(BROWSER_GROUP.into()),
        }
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = params
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("browser.close: `session` required".into()))?;
        self.registry.remove(id).await?;
        Ok(ToolResult::ok(format!("closed {id}")))
    }
}

fn session_arg(
    registry: &Arc<SessionRegistry>,
    params: &Value,
) -> Result<(BrowserSession, String)> {
    let id = params
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("browser: `session` required".into()))?
        .to_string();
    let session = registry.get(&id)?;
    Ok((session, id))
}

/// Accept a ref as an integer (`3`) or an `"eN"`/`"3"` string.
fn parse_ref(v: Option<&Value>) -> Option<u32> {
    match v {
        Some(Value::Number(n)) => n.as_u64().map(|n| n as u32),
        Some(Value::String(s)) => s.trim_start_matches('e').parse::<u32>().ok(),
        _ => None,
    }
}

/// Non-Unix stub: the CDP transport is a Unix seam (fd-3/4 debug pipe via async-signal-safe
/// `dup2` in `flux_system::spawn_debug_pipe`), so other platforms get a clean runtime error.
/// The browser group is never *surfaced* off-Unix anyway (no Chromium signal), but the code must
/// still compile — the v0.12.0 Windows dist build broke on the unconditional reference
/// (2026-07-09).
#[cfg(not(unix))]
async fn launch_session(
    _system: &flux_system::System,
    _config: &BrowserConfig,
) -> Result<BrowserSession> {
    Err(Error::Other(
        "browser ops are not supported on this platform yet (Unix-only CDP pipe spawn)".into(),
    ))
}

/// Launch a real Chromium session (the guarded fd-3/4 spawn + CDP attach lives here, off the
/// unreachable `BrowserSession::launch` stub, so the transport plumbing stays in one place).
#[cfg(unix)]
async fn launch_session(
    system: &flux_system::System,
    config: &BrowserConfig,
) -> Result<BrowserSession> {
    let bin = discover_chrome(config.bin.as_deref())?;
    let profile = ephemeral_profile_dir()?;
    let argv: Vec<String> = vec![
        bin.to_string_lossy().into_owned(),
        "--headless=new".into(),
        "--remote-debugging-pipe".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-gpu".into(),
        "--disable-extensions".into(),
        "--disable-background-networking".into(),
        "--mute-audio".into(),
        format!("--user-data-dir={}", profile.display()),
        "about:blank".into(),
    ];
    let flux_system::PipeChild { child, pipe } = system.spawn_debug_pipe(&argv, &[])?;
    let (r, w) = tokio::io::split(pipe);
    let (client, events) = CdpClient::connect(r, w);

    // Create + attach a page target (flattened session).
    let target = client
        .call("Target.createTarget", json!({ "url": "about:blank" }))
        .await
        .map_err(|e| Error::Other(format!("browser.open: createTarget: {e}")))?;
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("browser.open: no targetId".into()))?
        .to_string();
    let attached = client
        .call(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        )
        .await
        .map_err(|e| Error::Other(format!("browser.open: attachToTarget: {e}")))?;
    let page_session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("browser.open: no sessionId".into()))?
        .to_string();

    let session = BrowserSession::from_client(
        client,
        events,
        page_session,
        config.private_net.clone(),
        config.audit.clone(),
        config.grant_source.clone(),
        Some(child),
    )
    .await?;
    *session.0.profile_dir.lock().unwrap() = Some(profile);
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex as TokioMutex;

    /// A scripted fake Chrome: auto-responds to CDP calls via `responder(method, params) -> result`,
    /// records every call, and lets the test push events. No Chrome needed.
    struct Scripted {
        emit_w: Arc<TokioMutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>>,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl Scripted {
        async fn emit(&self, method: &str, params: Value, session: &str) {
            let mut ev = json!({ "method": method, "params": params });
            if !session.is_empty() {
                ev["sessionId"] = json!(session);
            }
            let mut bytes = serde_json::to_vec(&ev).unwrap();
            bytes.push(0);
            let mut w = self.emit_w.lock().await;
            w.write_all(&bytes).await.unwrap();
            w.flush().await.unwrap();
        }

        fn called(&self, method: &str) -> bool {
            self.calls.lock().unwrap().iter().any(|(m, _)| m == method)
        }

        fn last_params(&self, method: &str) -> Option<Value> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(m, _)| m == method)
                .map(|(_, p)| p.clone())
        }
    }

    fn scripted<F>(
        responder: F,
    ) -> (
        Arc<CdpClient>,
        tokio::sync::mpsc::Receiver<CdpEvent>,
        Arc<Scripted>,
    )
    where
        F: Fn(&str, &Value) -> Value + Send + 'static,
    {
        let (client_side, chrome_side) = tokio::io::duplex(256 * 1024);
        let (cr, cw) = tokio::io::split(client_side);
        let (mut chrome_r, chrome_w) = tokio::io::split(chrome_side);
        let (client, events) = CdpClient::connect(cr, cw);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let emit_w = Arc::new(TokioMutex::new(chrome_w));

        let calls2 = calls.clone();
        let emit2 = emit_w.clone();
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = match chrome_r.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == 0) {
                    let frame: Vec<u8> = buf.drain(..=pos).collect();
                    let v: Value = match serde_json::from_slice(&frame[..frame.len() - 1]) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let id = v["id"].as_i64().unwrap();
                    let method = v["method"].as_str().unwrap().to_string();
                    let params = v.get("params").cloned().unwrap_or(json!({}));
                    calls2
                        .lock()
                        .unwrap()
                        .push((method.clone(), params.clone()));
                    let result = responder(&method, &params);
                    let resp = json!({ "id": id, "result": result });
                    let mut bytes = serde_json::to_vec(&resp).unwrap();
                    bytes.push(0);
                    let mut w = emit2.lock().await;
                    w.write_all(&bytes).await.unwrap();
                    w.flush().await.unwrap();
                }
            }
        });

        (client, events, Arc::new(Scripted { emit_w, calls }))
    }

    async fn session_with<F>(
        responder: F,
        private_net: PrivateNetAllow,
        audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    ) -> (BrowserSession, Arc<Scripted>)
    where
        F: Fn(&str, &Value) -> Value + Send + 'static,
    {
        let (client, events, fake) = scripted(responder);
        let session = BrowserSession::from_client(
            client,
            events,
            "S1".into(),
            private_net,
            audit,
            "config:web".into(),
            None,
        )
        .await
        .unwrap();
        (session, fake)
    }

    fn ax_tree(nodes: Value) -> Value {
        json!({ "nodes": nodes })
    }

    #[derive(Default)]
    struct RecAudit {
        calls: Mutex<Vec<(String, String, String)>>,
    }
    impl flux_plugin::EgressAudit for RecAudit {
        fn record_private_admit(&self, caller: &str, host: &str, grant: &str) {
            self.calls
                .lock()
                .unwrap()
                .push((caller.into(), host.into(), grant.into()));
        }
    }

    #[test]
    fn browser_goto_declares_its_exact_network_destination() {
        let tool = BrowserGotoTool {
            registry: Arc::new(SessionRegistry::new()),
        };
        let params = json!({
            "session": "browser-1",
            "url": "https://example.com/app",
        });
        let subjects = tool.permission_subjects(&params);

        assert_eq!(subjects, vec!["https://example.com/app"]);
        assert_eq!(
            tool.authority_requirements(&params, &subjects).unwrap(),
            vec![
                AuthorityRequirement::network_fetch("https://example.com/app"),
                AuthorityRequirement::browser_navigate("https://example.com/app"),
            ]
        );
    }

    #[test]
    fn browser_open_separates_its_process_and_navigation_authority() {
        let tool = BrowserOpenTool {
            registry: Arc::new(SessionRegistry::new()),
            config: BrowserConfig {
                bin: Some("/usr/bin/chromium".into()),
                ..Default::default()
            },
        };
        let params = json!({ "url": "https://example.com/start" });
        let subjects = tool.permission_subjects(&params);

        assert_eq!(subjects, vec!["https://example.com/start"]);
        assert_eq!(
            tool.authority_requirements(&params, &subjects).unwrap(),
            vec![
                AuthorityRequirement::process_exec("/usr/bin/chromium"),
                AuthorityRequirement::network_fetch("https://example.com/start"),
                AuthorityRequirement::browser_navigate("https://example.com/start"),
            ]
        );
    }

    #[test]
    fn browser_act_goto_declares_its_exact_network_destination() {
        let tool = BrowserActTool {
            registry: Arc::new(SessionRegistry::new()),
        };
        let params = json!({
            "session": "browser-1",
            "action": "goto",
            "value": "https://example.com/next",
        });
        let subjects = tool.permission_subjects(&params);

        assert_eq!(subjects, vec!["https://example.com/next"]);
        assert_eq!(
            tool.authority_requirements(&params, &subjects).unwrap(),
            vec![
                AuthorityRequirement::network_fetch("https://example.com/next"),
                AuthorityRequirement::browser_navigate("https://example.com/next"),
            ]
        );

        let click = json!({ "session": "browser-1", "action": "click", "ref": "e1" });
        assert!(tool.permission_subjects(&click).is_empty());
        assert_eq!(
            tool.authority_requirements(&click, &[]).unwrap(),
            vec![
                AuthorityRequirement::network_fetch("*"),
                AuthorityRequirement::browser_navigate("*"),
            ],
            "an action whose current remote destination is unknown must stay fail-closed"
        );
    }

    #[tokio::test]
    async fn snapshot_builds_a_digest_from_the_ax_tree() {
        let (session, _fake) = session_with(
            |method, _p| match method {
                "Accessibility.getFullAXTree" => ax_tree(json!([
                    { "role": {"value": "heading"}, "name": {"value": "Hi"}, "backendDOMNodeId": 1 },
                    { "role": {"value": "button"}, "name": {"value": "Go"}, "backendDOMNodeId": 2 },
                ])),
                "Runtime.evaluate" => json!({ "result": { "value": "https://ex/" } }),
                _ => json!({}),
            },
            PrivateNetAllow::Any,
            None,
        )
        .await;
        let d = session
            .snapshot(View::Full, DigestCaps::default())
            .await
            .unwrap();
        assert!(d.contains("# Hi"), "{d}");
        assert!(d.contains(r#"button   "Go""#), "{d}");
    }

    #[tokio::test]
    async fn act_click_dispatches_and_returns_a_delta() {
        // The AX tree mutates after the click (button B appears).
        static PHASE: AtomicU64 = AtomicU64::new(0);
        PHASE.store(0, Ordering::Relaxed);
        let (session, fake) = session_with(
            |method, _p| match method {
                "Accessibility.getFullAXTree" => {
                    let n = PHASE.fetch_add(1, Ordering::Relaxed);
                    if n == 0 {
                        ax_tree(json!([{ "role": {"value": "button"}, "name": {"value": "A"}, "backendDOMNodeId": 1 }]))
                    } else {
                        ax_tree(json!([
                            { "role": {"value": "button"}, "name": {"value": "A"}, "backendDOMNodeId": 1 },
                            { "role": {"value": "button"}, "name": {"value": "B"}, "backendDOMNodeId": 2 },
                        ]))
                    }
                }
                "DOM.resolveNode" => json!({ "object": { "objectId": "obj-1" } }),
                "Runtime.evaluate" => json!({ "result": { "value": "https://ex/" } }),
                _ => json!({}),
            },
            PrivateNetAllow::Any,
            None,
        )
        .await;
        // Prime the ref map so e1 is live.
        session
            .snapshot(View::Actions, DigestCaps::default())
            .await
            .unwrap();
        let delta = session.act("click", Some(1), None, false).await.unwrap();
        assert!(fake.called("Runtime.callFunctionOn"), "click dispatched");
        assert!(delta.contains("## delta"), "{delta}");
        assert!(delta.contains("added: e2"), "new ref reported: {delta}");
    }

    #[tokio::test]
    async fn dead_ref_is_a_structured_error() {
        let (session, _fake) = session_with(
            |method, _p| match method {
                "Accessibility.getFullAXTree" => ax_tree(json!([])),
                "Runtime.evaluate" => json!({ "result": { "value": "u" } }),
                _ => json!({}),
            },
            PrivateNetAllow::Any,
            None,
        )
        .await;
        let err = session
            .act("click", Some(99), None, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("e99"), "names the ref: {err}");
    }

    #[tokio::test]
    async fn interception_blocks_a_private_subrequest_without_a_grant() {
        let (session, fake) = session_with(|_m, _p| json!({}), PrivateNetAllow::None, None).await;
        // A subrequest to a private host is paused; the pump must fail it.
        fake.emit(
            "Fetch.requestPaused",
            json!({ "requestId": "R1", "request": { "url": "http://127.0.0.1:9000/secret" } }),
            "S1",
        )
        .await;
        // Give the pump a moment to react.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            fake.called("Fetch.failRequest"),
            "private subrequest blocked"
        );
        assert!(!fake.called("Fetch.continueRequest"), "not continued");
        let params = fake.last_params("Fetch.failRequest").unwrap();
        assert_eq!(params["errorReason"], "AccessDenied");
        // And it surfaces in the delta as a policy refusal.
        assert!(session
            .0
            .state
            .lock()
            .unwrap()
            .egress_refusals
            .iter()
            .any(|u| u.contains("127.0.0.1")));
    }

    #[tokio::test]
    async fn interception_admits_a_private_subrequest_with_a_grant_and_audits_it() {
        let audit = Arc::new(RecAudit::default());
        let (_session, fake) = session_with(
            |_m, _p| json!({}),
            PrivateNetAllow::Any,
            Some(audit.clone()),
        )
        .await;
        fake.emit(
            "Fetch.requestPaused",
            json!({ "requestId": "R2", "request": { "url": "http://127.0.0.1:9000/ok" } }),
            "S1",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            fake.called("Fetch.continueRequest"),
            "admitted under the web grant"
        );
        assert!(!fake.called("Fetch.failRequest"), "not failed");
        let calls = audit.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "one admit audited");
        assert_eq!(calls[0].0, "web:browser");
        assert_eq!(calls[0].1, "127.0.0.1");
        assert_eq!(calls[0].2, "config:web");
    }

    #[test]
    fn browser_ops_are_gated_by_the_browser_signal() {
        use std::collections::HashSet;
        let groups = vec![browser_group()];
        let spec = BrowserOpenTool {
            registry: Arc::new(SessionRegistry::new()),
            config: BrowserConfig::default(),
        }
        .spec();
        assert_eq!(spec.group.as_deref(), Some("browser"));
        // Every browser op is a member of the group.
        assert!(BROWSER_OPS.contains(&spec.name.as_str()));

        let none: HashSet<String> = HashSet::new();
        assert!(
            !flux_runtime::is_advertised(&spec, &groups, &none),
            "browser ops are absent from the catalog with no Chromium signal"
        );
        let active: HashSet<String> = ["browser".to_string()].into_iter().collect();
        assert!(
            flux_runtime::is_advertised(&spec, &groups, &active),
            "browser ops appear once the browser signal is present"
        );
    }

    /// A persistent loopback fixture server that serves `html` for any request.
    async fn spawn_fixture(html: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{html}",
                        html.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// End-to-end live smoke against a REAL headless Chromium: spawn via the guarded fd-3/4 pipe,
    /// open → goto a local fixture (under a test-scoped `web` grant, since it's loopback) → snapshot
    /// → close, then assert the Chrome child is reaped (no orphan). SKIPS when no Chromium is on PATH.
    #[tokio::test]
    async fn live_smoke_open_goto_snapshot_close_no_orphan() {
        // Opt-in (FLUX_LIVE_BROWSER_SMOKE=1), like every live-external test in this repo: CI
        // runners expose a snap-confined `chromium` shim whose sandboxing drops the inherited
        // fd-3/4 debug pipe ("cdp: connection closed before response", 2026-07-09), so an
        // auto-run keyed only on PATH discovery is nondeterministic across environments.
        if std::env::var("FLUX_LIVE_BROWSER_SMOKE").is_err() {
            eprintln!(
                "SKIP live_smoke: set FLUX_LIVE_BROWSER_SMOKE=1 to run against a real Chromium"
            );
            return;
        }
        if !chromium_present(None) {
            eprintln!("SKIP live_smoke: no Chromium discoverable");
            return;
        }
        let base = spawn_fixture(
            "<html><head><title>Smoke</title></head><body><h1>Live Smoke</h1>\
             <p>Fixture body text.</p><button id=\"b\">Press me</button></body></html>",
        )
        .await;

        let dir = std::env::temp_dir().join(format!("flux-web-smoke-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = flux_system::System::new(flux_system::Workspace::new(&dir).unwrap());

        let session = launch_session(
            &system,
            &BrowserConfig {
                private_net: PrivateNetAllow::Any, // loopback fixture needs a web grant
                grant_source: "config:web".into(),
                ..Default::default()
            },
        )
        .await
        .expect("launch chromium");
        let pid = session.test_child_pid();

        session.goto(&base).await.expect("goto fixture");
        let digest = session
            .snapshot(View::Full, DigestCaps::default())
            .await
            .expect("snapshot");
        assert!(
            digest.contains("Live Smoke"),
            "digest carries page content: {digest}"
        );
        assert!(
            digest.contains("## actions"),
            "action space present: {digest}"
        );
        assert!(
            digest.to_lowercase().contains("press me"),
            "the button is in the action space: {digest}"
        );

        session.close().await.expect("close");

        // No orphan: the child is gone shortly after close (Linux /proc check).
        #[cfg(target_os = "linux")]
        if let Some(pid) = pid {
            let mut gone = false;
            for _ in 0..50 {
                if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    gone = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(gone, "chrome child {pid} reaped after close (no orphan)");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn interception_passes_non_http_schemes_through() {
        let (_session, fake) = session_with(|_m, _p| json!({}), PrivateNetAllow::None, None).await;
        fake.emit(
            "Fetch.requestPaused",
            json!({ "requestId": "R3", "request": { "url": "data:text/html,<b>x</b>" } }),
            "S1",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(fake.called("Fetch.continueRequest"), "data: passes through");
        assert!(!fake.called("Fetch.failRequest"));
    }
}
