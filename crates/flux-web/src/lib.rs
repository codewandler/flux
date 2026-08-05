//! flux-web — native web capabilities under one scoped egress policy.
//!
//! Working with the web is **three fundamentally different capabilities**, distinguished by what
//! the model sees and what can go wrong. They are three deliberately separate surfaces, not one op
//! with modes (design: `docs/designs/web-capabilities.md`):
//!
//! | Tier | Op(s) | The model sees |
//! |------|-------|----------------|
//! | 1 — request | [`http`]`::http.request` | the record `{status, headers, body}` (C-304) |
//! | 2 — read    | `web.fetch` (D-120) | readable content as condensed markdown |
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

use flux_core::Result;
use flux_runtime::{OperationPlacement, Tool, ToolRegistry};
use flux_system::net::PrivateNetAllow;

pub mod browser;
pub mod cdp;
pub mod condense;
pub mod crawl;
pub mod digest;
mod egress;
pub mod exchange;
pub mod fetch;
pub mod http;

pub use browser::{browser_group, chromium_present};

/// Sink for datasource records contributed by web ops. Fetched HTML pages become `web.page` records
/// (title/url/content) so read content is groundable later — the `websearch` → `web.result` pattern.
/// The surface adapts its datasource backend to this seam; `None` disables contribution (e.g. the
/// catalog-only registry).
///
/// **Contract (C-58): a configured sink is a durable datasource write, not ephemeral evidence.** Its
/// purpose is populating a searchable index that outlives the turn, so a tool constructed WITH a sink
/// declares that persistence honestly instead of masquerading as a bare network read: it reports the
/// [`WRITE_DB_EFFECT_TAG`] semantic effect and names [`WEB_PAGE_RECORD_SUBJECT`] as a permission
/// subject, so policy and the approval preview see the write. A tool constructed WITHOUT a sink stays
/// network-only. The disclosure is computed per-instance from whether `self.records` is set.
pub trait RecordSink: Send + Sync {
    fn contribute(&self, records: &[flux_datasource::Record]);
}

/// The semantic-effect tag (`flux_lang::ast::FlowEffect::WriteDb`, D-138) `web.fetch` / `web.crawl`
/// declare when configured WITH a [`RecordSink`]: contributing `web.page` records is a durable
/// datasource write. The flow layer lowers it to `Effect::Network` + the `flow.write_db` policy
/// action, so the persistence is disclosed in plan-risk previews and gateable by policy — WITHOUT
/// mis-declaring a filesystem `workspace.write`. A plain string per the [`flux_runtime::Tool::semantic_effects`]
/// contract (that seam stays free of a `flux-lang` dependency).
pub(crate) const WRITE_DB_EFFECT_TAG: &str = "write_db";

/// The permission subject naming the durable `web.page` datasource record target that `web.fetch` /
/// `web.crawl` write when configured WITH a [`RecordSink`]. Reported by `permission_subjects` so plan
/// approval and the audit trail disclose the persistence side effect alongside the fetched URL —
/// never an empty subject for the write (see the `permission_subjects` safety invariant in AGENTS.md).
pub(crate) const WEB_PAGE_RECORD_SUBJECT: &str = "datasource:web.page";

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
    /// Sink for `web.page` records contributed by `web.fetch`. `None` disables it.
    pub records: Option<Arc<dyn RecordSink>>,
    /// Optional configured path to a Chromium binary for the browser ops (else `FLUX_BROWSER_BIN` /
    /// `PATH` discovery).
    pub browser_bin: Option<String>,
    /// Allowlist of environment-variable names that `http.request` may resolve via a
    /// `{"$secret": "NAME"}` header or query reference. This is a security boundary: without it a
    /// prompt-injected model could exfiltrate *any* process env var (`AWS_SECRET_ACCESS_KEY`,
    /// `GITHUB_TOKEN`, …) to an arbitrary host in one unapproved call. `None` (the default) means
    /// "fall back to the `FLUX_WEB_SECRET_ALLOW` env var" (comma/whitespace-separated entries);
    /// `Some(vec![])` is an explicit **deny-all**. A name absent from the resolved list is refused
    /// before its value is ever read. See story C-76.
    ///
    /// Since C-459 an entry may also carry the **scope** the secret is granted under —
    /// `NAME;to=<host>;by=<principal>;in=header|query`, parsed by
    /// [`flux_system::secret_scope::SecretAllowlist`]. Each declared axis is default-deny and the
    /// destination is matched against the address the egress guard vetted, never the hostname the
    /// caller typed. A bare `NAME` stays valid and unscoped, so an existing allowlist keeps its
    /// exact meaning.
    pub allowed_secrets: Option<Vec<String>>,
}

/// Register the native web ops on `registry`:
/// - tier 1: `http.request` (arbitrary HTTP);
/// - tier 2: `web.fetch` (readable-markdown fetch) + the pure `html_to_markdown` transform +
///   `web.crawl` (bounded, same-host breadth-first crawl over the same egress envelope);
/// - tier 3: `browser.open`/`goto`/`snapshot`/`act`/`close` (evidence-gated behind the `browser`
///   group — surfaced only when a Chromium binary is discoverable; see [`browser_group`]).
pub fn register_web(registry: &mut ToolRegistry, opts: &WebOptions) {
    try_register_web(registry, opts).expect("flux-web operation pack registration failed");
}

/// Fallibly register the native web pack with a source label retained in collision diagnostics.
pub fn try_register_web(registry: &mut ToolRegistry, opts: &WebOptions) -> Result<()> {
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
    let mut assembled = registry.clone();
    assembled.try_register_all_from_with_placement(
        "flux-web native capability pack",
        vec![
            Arc::new(http::HttpRequestTool::new(opts)) as Arc<dyn Tool>,
            Arc::new(fetch::WebFetchTool::new(opts)),
            Arc::new(crawl::WebCrawlTool::new(opts)),
            Arc::new(browser::BrowserOpenTool {
                registry: registry_ref.clone(),
                config,
            }),
            Arc::new(browser::BrowserGotoTool {
                registry: registry_ref.clone(),
            }),
            Arc::new(browser::BrowserSnapshotTool {
                registry: registry_ref.clone(),
            }),
            Arc::new(browser::BrowserActTool {
                registry: registry_ref.clone(),
            }),
            Arc::new(browser::BrowserCloseTool {
                registry: registry_ref,
            }),
        ],
        OperationPlacement::NativeSystemOnly,
    )?;
    assembled.try_register_all_from_with_placement(
        "flux-web native capability pack",
        vec![Arc::new(fetch::HtmlToMarkdownTool) as Arc<dyn Tool>],
        OperationPlacement::LocalControlPlane,
    )?;
    *registry = assembled;
    Ok(())
}
