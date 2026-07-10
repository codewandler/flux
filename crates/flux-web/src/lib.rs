//! flux-web — native web capabilities under one scoped egress policy.
//!
//! Working with the web is **three fundamentally different capabilities**, distinguished by what
//! the model sees and what can go wrong. They are three deliberately separate surfaces, not one op
//! with modes (design: `docs/designs/web-capabilities.md`):
//!
//! | Tier | Op(s) | The model sees |
//! |------|-------|----------------|
//! | 1 — request | [`http`]`::http.request` | status + headers + capped body (bytes) |
//! | 2 — read    | `web_fetch` (D-120) | readable content as condensed markdown |
//! | 3 — browse  | `browser.*` (D-121…D-124) | an interface digest + deltas after actions |
//!
//! All three are **native** (no plugin/install step) and answer to one family-wide egress policy:
//! the `[private_net] web` scope. Public internet is reachable by default; private/loopback ranges
//! are refused unless the `web` scope grants them (or `--allow-private-net` widens it for the run),
//! and every admit to a private host emits a `PrivateNetAdmit` audit event.
//!
//! Registration follows the flux-eval precedent: the surface calls [`register_web`] on its
//! `ToolRegistry` (plus the egress options the guarded ops need, which `register_eval_ops` doesn't
//! carry because eval ops do no egress).

use std::sync::Arc;

use flux_runtime::ToolRegistry;
use flux_system::net::PrivateNetAllow;

pub mod browser;
pub mod cdp;
pub mod condense;
pub mod digest;
mod egress;
pub mod fetch;
pub mod http;

pub use browser::{browser_group, chromium_present};

/// Sink for datasource records contributed by web ops. Fetched HTML pages become `web.page` records
/// (title/url/content) so read content is groundable later — the `websearch` → `web.result` pattern.
/// The surface adapts its datasource backend to this seam; `None` disables contribution (e.g. the
/// catalog-only registry).
pub trait RecordSink: Send + Sync {
    fn contribute(&self, records: &[flux_datasource::Record]);
}

/// The egress + audit wiring the surface hands the web ops at registration.
///
/// Unlike `flux_eval::register_eval_ops`, the web ops do guarded network IO, so [`register_web`]
/// needs the resolved private-net scope, an audit sink, and the grant-source label to record.
/// [`Default`] is **public-only, no audit, no record sink** — correct for catalog-only registries
/// (the ops skill renderer) that never actually fetch.
#[derive(Clone, Default)]
pub struct WebOptions {
    /// The family-wide `[private_net] web` scope, resolved once. `--allow-private-net` widens it to
    /// [`PrivateNetAllow::Any`] before it reaches here.
    pub private_net: PrivateNetAllow,
    /// Sink for the `PrivateNetAdmit` audit event emitted when a request reaches a private/internal
    /// host under a grant. `None` disables the audit (no event store wired — e.g. the skill catalog).
    pub audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    /// The `grant_source` label recorded on an admit — e.g. `"config:web"` or
    /// `"cli:--allow-private-net"`. Defaults to `"config:web"` when unset.
    pub grant_source: Option<String>,
    /// Sink for `web.page` records contributed by `web_fetch`. `None` disables it.
    pub records: Option<Arc<dyn RecordSink>>,
    /// Optional configured path to a Chromium binary for the browser ops (else `FLUX_BROWSER_BIN` /
    /// `PATH` discovery).
    pub browser_bin: Option<String>,
}

/// Register the native web ops on `registry`:
/// - tier 1: `http.request` (arbitrary HTTP);
/// - tier 2: `web_fetch` (readable-markdown fetch) + the pure `html_to_markdown` transform;
/// - tier 3: `browser.open`/`goto`/`snapshot`/`act`/`close` (evidence-gated behind the `browser`
///   group — surfaced only when a Chromium binary is discoverable; see [`browser_group`]).
pub fn register_web(registry: &mut ToolRegistry, opts: &WebOptions) {
    registry.register(Arc::new(http::HttpRequestTool::new(opts)));
    registry.register(Arc::new(fetch::WebFetchTool::new(opts)));
    registry.register(Arc::new(fetch::HtmlToMarkdownTool));

    // Tier 3: a shared session registry + config for the browser ops. They always register (so the
    // `browser` group can list them); the evidence gate hides them from the catalog when no Chromium
    // is discoverable.
    let registry_ref = Arc::new(browser::SessionRegistry::new());
    let config = browser::BrowserConfig {
        bin: opts.browser_bin.clone(),
        private_net: opts.private_net.clone(),
        audit: opts.audit.clone(),
        grant_source: opts
            .grant_source
            .clone()
            .unwrap_or_else(|| "config:web".to_string()),
    };
    registry.register(Arc::new(browser::BrowserOpenTool {
        registry: registry_ref.clone(),
        config,
    }));
    registry.register(Arc::new(browser::BrowserGotoTool {
        registry: registry_ref.clone(),
    }));
    registry.register(Arc::new(browser::BrowserSnapshotTool {
        registry: registry_ref.clone(),
    }));
    registry.register(Arc::new(browser::BrowserActTool {
        registry: registry_ref.clone(),
    }));
    registry.register(Arc::new(browser::BrowserCloseTool {
        registry: registry_ref,
    }));
}
