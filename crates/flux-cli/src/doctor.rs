//! `flux doctor` — environment & install diagnostics (C-128).
//!
//! One command that runs a fixed suite of checks over a flux install and reports pass/warn/fail
//! per check with a one-line fix-it hint on every non-pass, so an external user can self-serve
//! instead of filing an "it doesn't work" report. Checks cover: provider credentials (incl. OAuth
//! expiry), plugin-pack signature/hash drift (reusing the D-48 verification), the OS sandbox
//! backend probe, `events.db` integrity + WAL size, private-network egress config sanity, version
//! skew vs the latest release, and `[tools] disable` resolution (C-162).
//!
//! # Architecture
//!
//! Every check is split into two halves:
//!   - a `judge_*` function: pure, takes already-collected facts, returns a [`CheckOutcome`]. This
//!     is what the unit tests below drive directly — no IO, no network, no live credentials.
//!   - a `check_*` function: the thin IO-collecting wrapper a [`CheckDef`] points `run` at. It
//!     gathers facts (reading `~/.flux/*`, probing the sandbox, etc.) and hands them to the judge.
//!
//! [`CHECKS`] is a plain data table (name + `run` fn pointer), so adding a check is one entry.
//! [`run_checks_over`] wraps every call in [`std::panic::catch_unwind`] — one probe panicking
//! (a bug in a check, not the install being diagnosed) turns into a `FAIL` row for that check
//! alone, never aborts the whole report.
//!
//! Exit code: non-zero iff at least one check's status is [`CheckStatus::Fail`]. A `WARN` never
//! affects the exit code (acceptance: "exit code non-zero iff any check fails").

use super::*;

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Report model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        }
    }
}

/// A judge function's verdict — everything about a check except its name (the surrounding
/// [`CheckDef`] supplies that).
#[derive(Debug, Clone)]
struct CheckOutcome {
    status: CheckStatus,
    detail: String,
    /// One-line fix-it hint. `Some` on every non-pass outcome (enforced structurally: only
    /// [`CheckOutcome::warn`]/[`CheckOutcome::fail`] can build one, and both require it).
    hint: Option<String>,
}

impl CheckOutcome {
    fn pass(detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Pass,
            detail: detail.into(),
            hint: None,
        }
    }
    fn warn(detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Warn,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
    fn fail(detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// One check's rendered result — a [`CheckOutcome`] stamped with the check's name.
#[derive(Debug, Clone)]
pub(super) struct CheckReport {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    pub hint: Option<String>,
}

// ---------------------------------------------------------------------------
// Context — every fact a check might need, collected once up front
// ---------------------------------------------------------------------------

/// Everything the check suite reads. Built once by [`run_doctor`] from the real environment;
/// tests build it by hand (temp dirs, a canned config, a canned `release_tags` result) so every
/// check's IO-collecting half can be exercised without touching the network or a real `HOME`.
pub(super) struct DoctorCtx {
    pub cfg: flux_config::Config,
    /// The three pre-merge config layers, each parsed independently (C-165) — the raw material
    /// `check_config_provenance` feeds to `flux_config::effective_settings` to report which layer
    /// supplied each pinnable key. `cfg` above is the already-merged result; these are not simply
    /// derivable from it, since a key set identically by two layers is indistinguishable in the
    /// merged view.
    pub managed_cfg: flux_config::Config,
    pub user_cfg: flux_config::Config,
    pub project_cfg: flux_config::Config,
    pub own_version: &'static str,
    /// `Ok(tags)` = every release tag the pack channel repo reports; `Err(reason)` = the lookup
    /// failed (offline, rate-limited, …). Pre-fetched (the one check that needs the network) so
    /// every check function itself stays synchronous and IO-free beyond local file reads.
    pub release_tags: Result<Vec<String>, String>,
    /// `~/.flux/plugins` — `None` when `HOME` is unset.
    pub plugins_dir: Option<PathBuf>,
    /// `~/.flux/events.db` — `None` when `HOME` is unset.
    pub events_db_path: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// The check table
// ---------------------------------------------------------------------------

type CheckFn = fn(&DoctorCtx) -> CheckOutcome;

struct CheckDef {
    name: &'static str,
    run: CheckFn,
}

/// The whole suite, as data. Adding a check is one entry here plus its `judge_*`/`check_*` pair.
const CHECKS: &[CheckDef] = &[
    CheckDef {
        name: "credentials",
        run: check_credentials,
    },
    CheckDef {
        name: "plugin pack integrity",
        run: check_plugin_pack,
    },
    CheckDef {
        name: "sandbox backend",
        run: check_sandbox,
    },
    CheckDef {
        name: "events.db integrity",
        run: check_events_db,
    },
    CheckDef {
        name: "egress / private-net config",
        run: check_egress,
    },
    CheckDef {
        name: "version",
        run: check_version_skew,
    },
    CheckDef {
        name: "tools disable (C-162)",
        run: check_tools_disable,
    },
    CheckDef {
        name: "config provenance (C-165)",
        run: check_config_provenance,
    },
];

/// Run `checks` against `ctx`, isolating each one behind [`std::panic::catch_unwind`] so a bug in
/// a single probe becomes a `FAIL` row for that check, not an aborted report. Parameterized over
/// the check slice (rather than reading [`CHECKS`] directly) so tests can drive the isolation
/// behavior with a throwaway check list instead of the production suite.
fn run_checks_over(checks: &[CheckDef], ctx: &DoctorCtx) -> Vec<CheckReport> {
    checks
        .iter()
        .map(|def| {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (def.run)(ctx)))
                .unwrap_or_else(|_| {
                    CheckOutcome::fail(
                        "check panicked (see stderr for the panic message)",
                        "this is a flux bug — please report it, including this check's name",
                    )
                });
            CheckReport {
                name: def.name,
                status: outcome.status,
                detail: outcome.detail,
                hint: outcome.hint,
            }
        })
        .collect()
}

pub(super) fn run_checks(ctx: &DoctorCtx) -> Vec<CheckReport> {
    run_checks_over(CHECKS, ctx)
}

// ---------------------------------------------------------------------------
// credentials
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn judge_credentials(
    rows: &[flux_credentials::ProviderAuth],
    expired_no_refresh: &[String],
) -> CheckOutcome {
    if !expired_no_refresh.is_empty() {
        return CheckOutcome::warn(
            format!(
                "OAuth token(s) expired with no refresh token available: {}",
                expired_no_refresh.join(", ")
            ),
            format!(
                "re-authenticate: {}",
                expired_no_refresh
                    .iter()
                    .map(|p| format!("`flux auth login {p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    let available: Vec<&str> = rows
        .iter()
        .filter(|r| r.available)
        .map(|r| r.provider)
        .collect();
    if available.is_empty() {
        return CheckOutcome::warn(
            "no provider credentials configured (checked env keys and the flux credential store)",
            "set a provider API key (e.g. ANTHROPIC_API_KEY), or run `flux auth login claude|codex`",
        );
    }
    CheckOutcome::pass(format!(
        "{} of {} providers configured: {}",
        available.len(),
        rows.len(),
        available.join(", ")
    ))
}

fn check_credentials(_ctx: &DoctorCtx) -> CheckOutcome {
    let rows = flux_credentials::auth_status();
    let now = now_ms();
    let mut expired_no_refresh = Vec::new();
    for provider in ["claude", "codex"] {
        if let Some(tok) = flux_credentials::load_token(provider) {
            let expired = tok.expires_at_ms.map(|exp| now >= exp).unwrap_or(false);
            if expired && tok.refresh.is_none() {
                expired_no_refresh.push(provider.to_string());
            }
        }
    }
    judge_credentials(&rows, &expired_no_refresh)
}

// ---------------------------------------------------------------------------
// plugin pack integrity (reuses the D-48 verification machinery)
// ---------------------------------------------------------------------------

fn judge_plugin_pack(total: usize, drifted: &[String]) -> CheckOutcome {
    if !drifted.is_empty() {
        return CheckOutcome::fail(
            format!(
                "{} of {total} installed plugin(s) failed hash verification: {}",
                drifted.len(),
                drifted.join("; ")
            ),
            "reinstall the affected plugin(s) (`flux plugin install <name>`), or `flux plugin \
             pin|rollback` to a known-good version — a drifted binary also refuses to spawn (D-48)",
        );
    }
    if total == 0 {
        return CheckOutcome::pass("no plugins installed");
    }
    CheckOutcome::pass(format!("{total} installed plugin(s) verified"))
}

fn check_plugin_pack(ctx: &DoctorCtx) -> CheckOutcome {
    let Some(dir) = &ctx.plugins_dir else {
        return CheckOutcome::pass("HOME is not set — no plugin store to check");
    };
    let discovered = flux_plugin::discover(dir);
    let mut drifted = Vec::new();
    for p in &discovered {
        if let flux_plugin::Verification::HashDrift { expected, actual } =
            flux_plugin::verify_descriptor(&p.descriptor)
        {
            drifted.push(format!(
                "{} (recorded {expected}, on-disk hashes to {actual})",
                p.name
            ));
        }
    }
    judge_plugin_pack(discovered.len(), &drifted)
}

// ---------------------------------------------------------------------------
// sandbox backend probe (bwrap / sandbox-exec)
// ---------------------------------------------------------------------------

fn judge_sandbox(
    active: bool,
    confined: bool,
    required: bool,
    reason: Option<&str>,
) -> CheckOutcome {
    if active {
        return CheckOutcome::pass("a sandbox backend is available and functional");
    }
    if confined {
        return CheckOutcome::pass("already confined by an outer flux sandbox");
    }
    let reason = reason.unwrap_or("no backend detected");
    if required {
        return CheckOutcome::fail(
            format!("[sandbox] require is set but no backend is available: {reason}"),
            "install bubblewrap (Linux) or the Xcode command line tools (macOS: sandbox-exec), \
             or unset `[sandbox] require`",
        );
    }
    CheckOutcome::warn(
        format!("no sandbox backend available: {reason}"),
        "OS sandboxing is opt-in defense-in-depth, not required — install bubblewrap (Linux) or \
         the Xcode command line tools (macOS) to enable it, or ignore this inside an \
         already-isolated environment (containers, CI)",
    )
}

fn check_sandbox(ctx: &DoctorCtx) -> CheckOutcome {
    use flux_system::sandbox::{Sandbox, SandboxMode, SandboxSettings};
    // Force a real probe regardless of the operator's configured posture — doctor reports what's
    // ACTUALLY available, not just an echo of the configured mode.
    let settings = SandboxSettings {
        mode: SandboxMode::On,
        network: true,
        extra_writable: Vec::new(),
    };
    let sandbox = Sandbox::resolve(settings);
    judge_sandbox(
        sandbox.is_active(),
        sandbox.confined_by_parent(),
        ctx.cfg.sandbox.require,
        sandbox.reason(),
    )
}

// ---------------------------------------------------------------------------
// events.db integrity + WAL size
// ---------------------------------------------------------------------------

/// Warn once the WAL file crosses this size — it means nothing has checkpointed it in a while.
const WAL_WARN_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
struct SqliteFacts {
    integrity_ok: bool,
    integrity_detail: String,
    wal_bytes: u64,
}

fn judge_events_db(facts: &SqliteFacts) -> CheckOutcome {
    if !facts.integrity_ok {
        return CheckOutcome::fail(
            format!(
                "PRAGMA integrity_check reported corruption: {}",
                facts.integrity_detail
            ),
            "restore events.db from a backup, or move it aside so flux starts a fresh store — a \
             corrupt row is otherwise silently skipped on every read (see flux-events' decode_all)",
        );
    }
    if facts.wal_bytes > WAL_WARN_BYTES {
        return CheckOutcome::warn(
            format!(
                "the WAL file is {} and has not been checkpointed in a while",
                format_bytes(facts.wal_bytes)
            ),
            "close other flux processes holding the store open, or run `sqlite3 ~/.flux/events.db \
             'PRAGMA wal_checkpoint(TRUNCATE);'`",
        );
    }
    CheckOutcome::pass(format!(
        "integrity check passed; WAL is {}",
        format_bytes(facts.wal_bytes)
    ))
}

fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// Open `path` read-only and read its integrity + WAL-sibling size — the IO half [`check_events_db`]
/// hands to [`judge_events_db`]. A failure to even open the file (locked, not a database, …) is
/// reported as its own `Err`, distinct from a completed-but-failing integrity check.
fn probe_sqlite_file(path: &std::path::Path) -> Result<SqliteFacts, String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    let detail: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let integrity_ok = detail == "ok";
    let wal_path = path.with_extension("db-wal");
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    Ok(SqliteFacts {
        integrity_ok,
        integrity_detail: detail,
        wal_bytes,
    })
}

fn check_events_db(ctx: &DoctorCtx) -> CheckOutcome {
    let Some(path) = &ctx.events_db_path else {
        return CheckOutcome::pass("HOME is not set — no events.db to check");
    };
    if !path.is_file() {
        return CheckOutcome::pass("no events.db yet — the first run will create it");
    }
    match probe_sqlite_file(path) {
        Ok(facts) => judge_events_db(&facts),
        Err(e) => CheckOutcome::fail(
            format!("could not open events.db: {e}"),
            "the file may be locked by another flux process, or corrupted — if the problem \
             persists, back it up and remove it to start fresh",
        ),
    }
}

// ---------------------------------------------------------------------------
// egress / private-net config sanity
// ---------------------------------------------------------------------------

fn is_wildcard_grant(grant: &flux_config::PrivateNetGrant) -> bool {
    match grant {
        flux_config::PrivateNetGrant::Enabled(on) => *on,
        flux_config::PrivateNetGrant::Hosts(hosts) => hosts.iter().any(|h| h == "*"),
    }
}

fn judge_egress(cfg: &flux_config::Config) -> CheckOutcome {
    let mut wide_open: Vec<String> = Vec::new();
    if cfg.allow_private_net {
        wide_open.push("allow_private_net = true (deprecated wildcard)".to_string());
    }
    if is_wildcard_grant(&cfg.private_net.web) {
        wide_open.push("[private_net] web = true".to_string());
    }
    for (plugin, grant) in &cfg.private_net.plugins {
        if is_wildcard_grant(grant) {
            wide_open.push(format!("[private_net.plugins] {plugin} = true"));
        }
    }
    for (key, grant) in &cfg.private_net.endpoints {
        if is_wildcard_grant(grant) {
            wide_open.push(format!("[private_net.endpoints] {key} = true"));
        }
    }
    if !wide_open.is_empty() {
        return CheckOutcome::warn(
            format!(
                "{} private-network egress grant(s) are wide open: {}",
                wide_open.len(),
                wide_open.join(", ")
            ),
            "scope these to explicit host patterns instead of `true` (see `[private_net]` in the \
             config reference)",
        );
    }
    CheckOutcome::pass("no wide-open private-network egress grants")
}

fn check_egress(ctx: &DoctorCtx) -> CheckOutcome {
    judge_egress(&ctx.cfg)
}

// ---------------------------------------------------------------------------
// config provenance (C-165) — "why can't I enable this" gets an answer
// ---------------------------------------------------------------------------

/// Render one pinnable key's provenance as `key=value (layer[, pinned])`.
fn render_setting(setting: &flux_config::EffectiveSetting) -> String {
    let layer = match setting.layer {
        flux_config::ConfigLayer::Managed => "managed",
        flux_config::ConfigLayer::User => "user",
        flux_config::ConfigLayer::Project => "project",
        flux_config::ConfigLayer::BuiltIn => "built-in",
    };
    if setting.pinned {
        format!(
            "{}={} ({layer}, pinned)",
            setting.key.as_str(),
            setting.value
        )
    } else {
        format!("{}={} ({layer})", setting.key.as_str(), setting.value)
    }
}

/// Always a pass: this check is inspection, not judgment — a managed pin refusing a downstream
/// relax is already a hard `load_config` error before `doctor` ever runs (surfaced as the run's own
/// top-level failure, not a per-check row). Its job is answering "why can't I enable this" by
/// naming every pinnable key's effective value and the layer that supplied it.
fn judge_config_provenance(settings: &[flux_config::EffectiveSetting]) -> CheckOutcome {
    let pinned = settings.iter().filter(|s| s.pinned).count();
    let rendered = settings
        .iter()
        .map(render_setting)
        .collect::<Vec<_>>()
        .join(", ");
    if pinned == 0 {
        CheckOutcome::pass(format!("no managed pins in effect: {rendered}"))
    } else {
        CheckOutcome::pass(format!(
            "{pinned} setting(s) pinned by managed config: {rendered}"
        ))
    }
}

fn check_config_provenance(ctx: &DoctorCtx) -> CheckOutcome {
    let settings =
        flux_config::effective_settings(&ctx.managed_cfg, &ctx.user_cfg, &ctx.project_cfg);
    judge_config_provenance(&settings)
}

// ---------------------------------------------------------------------------
// version skew vs the latest release
// ---------------------------------------------------------------------------

fn parse_version(v: &str) -> Option<Vec<u64>> {
    let v = v.trim_start_matches('v');
    v.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

/// The highest `v<semver>` tag among `tags` (excludes `plugins-v*` and anything else non-numeric).
fn latest_flux_version(tags: &[String]) -> Option<String> {
    tags.iter()
        .filter_map(|t| t.strip_prefix('v'))
        .filter_map(|v| parse_version(v).map(|parsed| (parsed, v.to_string())))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, v)| v)
}

fn judge_version_skew(own: &str, tags: &Result<Vec<String>, String>) -> CheckOutcome {
    let tags = match tags {
        Err(reason) => return CheckOutcome::warn(
            format!("could not check for updates: {reason}"),
            "check network connectivity — this does not affect local operation, ignore if offline",
        ),
        Ok(tags) => tags,
    };
    let Some(latest) = latest_flux_version(tags) else {
        return CheckOutcome::warn(
            "no tagged flux releases found on the release channel",
            "nothing to compare against — this is unusual, please report it",
        );
    };
    match (parse_version(own), parse_version(&latest)) {
        (Some(o), Some(l)) if o < l => CheckOutcome::warn(
            format!("running {own}; the latest release is {latest}"),
            format!(
                "upgrade: see https://github.com/{}/releases/tag/v{latest}",
                flux_plugin::pack::DEFAULT_REPO
            ),
        ),
        (Some(_), Some(_)) => {
            CheckOutcome::pass(format!("running {own} (latest release: {latest})"))
        }
        _ => CheckOutcome::warn(
            format!("could not compare versions ({own} vs {latest})"),
            "this is unusual — please report it",
        ),
    }
}

fn check_version_skew(ctx: &DoctorCtx) -> CheckOutcome {
    judge_version_skew(ctx.own_version, &ctx.release_tags)
}

/// Fetch every release tag from the pack channel repo — the same GitHub API call `flux plugin
/// install` already makes (`flux_plugin::pack::GithubFetcher`) — reduced to a fatal-free `Err`.
/// The version-skew check must never fail the whole `doctor` run just because the network (or
/// GitHub) is unavailable.
pub(super) async fn fetch_release_tags() -> Result<Vec<String>, String> {
    use flux_plugin::pack::{Fetcher, GithubFetcher, DEFAULT_REPO};
    GithubFetcher::default()
        .list_release_tags(DEFAULT_REPO)
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// [tools] disable visibility (C-162)
// ---------------------------------------------------------------------------

fn judge_tools_disable(resolved: &flux_runtime::ResolvedDisabledOps) -> CheckOutcome {
    if !resolved.unmatched.is_empty() {
        let mut unmatched = resolved.unmatched.clone();
        unmatched.sort();
        return CheckOutcome::warn(
            format!(
                "[tools] disable has {} entr{} matching no known built-in op: {}",
                unmatched.len(),
                if unmatched.len() == 1 { "y" } else { "ies" },
                unmatched.join(", ")
            ),
            "fix the typo, or remove the stale entry naming a retired op",
        );
    }
    if resolved.disabled.is_empty() {
        return CheckOutcome::pass("no [tools] disable entries configured");
    }
    let mut names: Vec<&str> = resolved.disabled.iter().map(String::as_str).collect();
    names.sort();
    CheckOutcome::pass(format!(
        "{} op(s) disabled via [tools] disable: {}",
        names.len(),
        names.join(", ")
    ))
}

/// Resolves `[tools] disable` against the **built-in** op registry only — a plugin-provided op
/// name is not checked here (loading plugins would make this check slow, side-effecting, and
/// non-hermetic). An unmatched pattern that actually names a plugin op is a rare false positive;
/// the detail line says "built-in" so it doesn't read as a hard claim about plugin ops too.
fn check_tools_disable(ctx: &DoctorCtx) -> CheckOutcome {
    let mut registry = flux_runtime::ToolRegistry::new();
    if let Err(e) = flux_tools::try_register_builtins(&mut registry) {
        return CheckOutcome::fail(
            format!("could not assemble the built-in tool registry: {e}"),
            "this is a flux bug — please report it",
        );
    }
    let resolved = registry.resolve_disabled(&ctx.cfg.tools.disable);
    judge_tools_disable(&resolved)
}

// ---------------------------------------------------------------------------
// Rendering + exit code
// ---------------------------------------------------------------------------

pub(super) fn any_failed(reports: &[CheckReport]) -> bool {
    reports.iter().any(|r| r.status == CheckStatus::Fail)
}

fn counts(reports: &[CheckReport]) -> (usize, usize, usize) {
    let pass = reports
        .iter()
        .filter(|r| r.status == CheckStatus::Pass)
        .count();
    let warn = reports
        .iter()
        .filter(|r| r.status == CheckStatus::Warn)
        .count();
    let fail = reports
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .count();
    (pass, warn, fail)
}

pub(super) fn render_report(reports: &[CheckReport]) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n\n", style::bold("flux doctor")));
    let name_w = reports.iter().map(|r| r.name.len()).max().unwrap_or(0);
    for r in reports {
        let label = match r.status {
            CheckStatus::Pass => style::green(r.status.label()),
            CheckStatus::Warn => style::yellow(r.status.label()),
            CheckStatus::Fail => style::red(r.status.label()),
        };
        out.push_str(&format!("  {label}  {:<name_w$}  {}\n", r.name, r.detail));
        if let Some(hint) = &r.hint {
            out.push_str(&format!(
                "        {}\n",
                style::dim(&format!("\u{2192} {hint}"))
            ));
        }
    }
    let (pass, warn, fail) = counts(reports);
    out.push_str(&format!("\n{pass} passed, {warn} warned, {fail} failed\n"));
    out
}

fn json_report(reports: &[CheckReport]) -> serde_json::Value {
    serde_json::json!({
        "ok": !any_failed(reports),
        "checks": reports.iter().map(|r| serde_json::json!({
            "name": r.name,
            "status": r.status.label(),
            "detail": r.detail,
            "hint": r.hint,
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Command entry point
// ---------------------------------------------------------------------------

pub(super) async fn run_doctor(json: bool) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let (managed_cfg, user_cfg, project_cfg) =
        flux_runtime::metadata::config_layers(&cwd).context("load .flux/config.toml layers")?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plugins_dir = home.as_ref().map(|h| h.join(".flux").join("plugins"));
    let events_db_path = home.as_ref().map(|h| h.join(".flux").join("events.db"));
    let release_tags = fetch_release_tags().await;

    let ctx = DoctorCtx {
        cfg,
        managed_cfg,
        user_cfg,
        project_cfg,
        own_version: env!("CARGO_PKG_VERSION"),
        release_tags,
        plugins_dir,
        events_db_path,
    };
    let reports = run_checks(&ctx);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_report(&reports)).context("serialize report")?
        );
    } else {
        print!("{}", render_report(&reports));
    }

    if any_failed(&reports) {
        std::process::exit(1);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(name: &'static str, available: bool) -> flux_credentials::ProviderAuth {
        flux_credentials::ProviderAuth {
            provider: name,
            available,
            source: if available {
                "env".into()
            } else {
                "not set".into()
            },
            hint: if available {
                None
            } else {
                Some(format!("set ${name}"))
            },
        }
    }

    // -- credentials --------------------------------------------------------------------------

    #[test]
    fn judge_credentials_passes_when_at_least_one_provider_is_configured() {
        let rows = vec![provider("anthropic", true), provider("openai", false)];
        let out = judge_credentials(&rows, &[]);
        assert_eq!(out.status, CheckStatus::Pass);
        assert!(out.detail.contains("anthropic"));
        assert!(out.hint.is_none());
    }

    #[test]
    fn judge_credentials_warns_when_nothing_is_configured() {
        let rows = vec![provider("anthropic", false), provider("openai", false)];
        let out = judge_credentials(&rows, &[]);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.hint.is_some());
    }

    #[test]
    fn judge_credentials_warns_on_expired_oauth_token_with_no_refresh() {
        let rows = vec![provider("claude", true)];
        let out = judge_credentials(&rows, &["claude".to_string()]);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("claude"));
        assert!(out.hint.unwrap().contains("flux auth login claude"));
    }

    // -- plugin pack integrity -----------------------------------------------------------------

    #[test]
    fn judge_plugin_pack_passes_with_no_plugins() {
        let out = judge_plugin_pack(0, &[]);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_plugin_pack_passes_when_all_verify() {
        let out = judge_plugin_pack(3, &[]);
        assert_eq!(out.status, CheckStatus::Pass);
        assert!(out.detail.contains('3'));
    }

    #[test]
    fn judge_plugin_pack_fails_on_hash_drift() {
        let out = judge_plugin_pack(
            2,
            &["gitlab (recorded abc, on-disk hashes to def)".to_string()],
        );
        assert_eq!(out.status, CheckStatus::Fail);
        assert!(out.detail.contains("gitlab"));
        assert!(out.hint.unwrap().contains("reinstall"));
    }

    /// End-to-end through `check_plugin_pack`'s real IO path (a temp `~/.flux/plugins`-shaped
    /// dir), proving the D-48 reuse is wired correctly, not just the pure judge.
    #[test]
    fn check_plugin_pack_detects_drift_against_a_real_descriptor_and_binary() {
        let dir = std::env::temp_dir().join(format!(
            "flux-doctor-plugin-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin_path = dir.join("flux-plugin-drifted");
        std::fs::write(&bin_path, b"original-bytes").unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "drifted",
            &flux_plugin::PluginDescriptor {
                program: bin_path.to_string_lossy().into_owned(),
                sha256: Some(
                    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
                ),
                version: Some("1.0.0".to_string()),
                source: Some("plugins-v1.0.0".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let ctx = DoctorCtx {
            cfg: flux_config::Config::default(),
            managed_cfg: flux_config::Config::default(),
            user_cfg: flux_config::Config::default(),
            project_cfg: flux_config::Config::default(),
            own_version: "0.0.0",
            release_tags: Ok(vec![]),
            plugins_dir: Some(dir.clone()),
            events_db_path: None,
        };
        let out = check_plugin_pack(&ctx);
        assert_eq!(out.status, CheckStatus::Fail);
        assert!(out.detail.contains("drifted"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- sandbox backend ------------------------------------------------------------------------

    #[test]
    fn judge_sandbox_passes_when_active() {
        let out = judge_sandbox(true, false, false, None);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_sandbox_passes_when_confined_by_parent() {
        let out = judge_sandbox(false, true, true, None);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_sandbox_warns_when_unavailable_and_not_required() {
        let out = judge_sandbox(false, false, false, Some("bwrap not found"));
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("bwrap not found"));
    }

    #[test]
    fn judge_sandbox_fails_when_unavailable_and_required() {
        let out = judge_sandbox(false, false, true, Some("bwrap not found"));
        assert_eq!(out.status, CheckStatus::Fail);
        assert!(out.hint.unwrap().contains("require"));
    }

    /// The real probe (`check_sandbox`) must never panic regardless of host support — it always
    /// returns SOME outcome (pass, warn, or fail; whichever is correct on the current machine).
    #[test]
    fn check_sandbox_runs_without_panicking() {
        let ctx = DoctorCtx {
            cfg: flux_config::Config::default(),
            managed_cfg: flux_config::Config::default(),
            user_cfg: flux_config::Config::default(),
            project_cfg: flux_config::Config::default(),
            own_version: "0.0.0",
            release_tags: Ok(vec![]),
            plugins_dir: None,
            events_db_path: None,
        };
        let out = check_sandbox(&ctx);
        assert!(matches!(
            out.status,
            CheckStatus::Pass | CheckStatus::Warn | CheckStatus::Fail
        ));
    }

    // -- events.db integrity --------------------------------------------------------------------

    #[test]
    fn judge_events_db_fails_on_corruption() {
        let facts = SqliteFacts {
            integrity_ok: false,
            integrity_detail: "row 12 missing".to_string(),
            wal_bytes: 0,
        };
        let out = judge_events_db(&facts);
        assert_eq!(out.status, CheckStatus::Fail);
        assert!(out.detail.contains("row 12 missing"));
    }

    #[test]
    fn judge_events_db_warns_on_large_wal() {
        let facts = SqliteFacts {
            integrity_ok: true,
            integrity_detail: "ok".to_string(),
            wal_bytes: WAL_WARN_BYTES + 1,
        };
        let out = judge_events_db(&facts);
        assert_eq!(out.status, CheckStatus::Warn);
    }

    #[test]
    fn judge_events_db_passes_when_clean() {
        let facts = SqliteFacts {
            integrity_ok: true,
            integrity_detail: "ok".to_string(),
            wal_bytes: 4096,
        };
        let out = judge_events_db(&facts);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn check_events_db_passes_when_the_file_does_not_exist_yet() {
        let dir = std::env::temp_dir().join(format!(
            "flux-doctor-eventsdb-missing-{}-{}",
            std::process::id(),
            line!()
        ));
        let ctx = DoctorCtx {
            cfg: flux_config::Config::default(),
            managed_cfg: flux_config::Config::default(),
            user_cfg: flux_config::Config::default(),
            project_cfg: flux_config::Config::default(),
            own_version: "0.0.0",
            release_tags: Ok(vec![]),
            plugins_dir: None,
            events_db_path: Some(dir.join("events.db")),
        };
        let out = check_events_db(&ctx);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn probe_sqlite_file_reports_ok_against_a_real_fresh_database() {
        let dir = std::env::temp_dir().join(format!(
            "flux-doctor-eventsdb-real-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE t (x INTEGER)", []).unwrap();
        }
        let facts = probe_sqlite_file(&path).expect("a real sqlite file opens cleanly");
        assert!(facts.integrity_ok, "{}", facts.integrity_detail);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn probe_sqlite_file_errs_on_a_file_that_is_not_a_database() {
        let dir = std::env::temp_dir().join(format!(
            "flux-doctor-eventsdb-bogus-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.db");
        std::fs::write(&path, b"not a sqlite file").unwrap();
        let err = probe_sqlite_file(&path).expect_err("garbage bytes are not a valid database");
        assert!(!err.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- egress / private-net config sanity ------------------------------------------------------

    #[test]
    fn judge_egress_passes_with_default_config() {
        let out = judge_egress(&flux_config::Config::default());
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_egress_warns_on_deprecated_wildcard_flag() {
        let cfg = flux_config::Config {
            allow_private_net: true,
            ..Default::default()
        };
        let out = judge_egress(&cfg);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("allow_private_net"));
    }

    #[test]
    fn judge_egress_warns_on_wildcard_plugin_grant() {
        let mut cfg = flux_config::Config::default();
        cfg.private_net.plugins.insert(
            "gitlab".to_string(),
            flux_config::PrivateNetGrant::Enabled(true),
        );
        let out = judge_egress(&cfg);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("gitlab"));
    }

    #[test]
    fn judge_egress_warns_on_wildcard_host_pattern() {
        let mut cfg = flux_config::Config::default();
        cfg.private_net.web = flux_config::PrivateNetGrant::Hosts(vec!["*".to_string()]);
        let out = judge_egress(&cfg);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("web"));
    }

    // -- config provenance (C-165) ---------------------------------------------------------------

    #[test]
    fn judge_config_provenance_passes_and_names_every_key_when_nothing_is_managed() {
        let settings = flux_config::effective_settings(
            &flux_config::Config::default(),
            &flux_config::Config::default(),
            &flux_config::Config::default(),
        );
        let out = judge_config_provenance(&settings);
        assert_eq!(out.status, CheckStatus::Pass);
        assert!(out.detail.contains("no managed pins in effect"));
        for (name, _) in flux_config::PinnableKey::ALL {
            assert!(
                out.detail.contains(name),
                "{name} missing from provenance detail: {}",
                out.detail
            );
        }
    }

    #[test]
    fn judge_config_provenance_names_the_pinned_key_and_its_managed_value() {
        let managed = flux_config::Config {
            managed: flux_config::ManagedMeta {
                pins: vec!["private_net.web".to_string()],
            },
            private_net: flux_config::PrivateNetConfig {
                web: flux_config::PrivateNetGrant::Hosts(vec!["reports.internal".to_string()]),
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = flux_config::effective_settings(
            &managed,
            &flux_config::Config::default(),
            &flux_config::Config::default(),
        );
        let out = judge_config_provenance(&settings);
        assert_eq!(out.status, CheckStatus::Pass);
        assert!(out.detail.contains("1 setting(s) pinned by managed config"));
        assert!(out.detail.contains("private_net.web"));
        assert!(out.detail.contains("managed"));
        assert!(out.detail.contains("pinned"));
    }

    /// Wired correctly end-to-end: `check_config_provenance` reads the three raw layers off
    /// `DoctorCtx`, not the already-merged `cfg` — a project-only setting must be attributed to
    /// `project`, not silently folded into an unattributed merged value.
    #[test]
    fn check_config_provenance_attributes_a_project_only_setting_to_the_project_layer() {
        let mut project_cfg = flux_config::Config::default();
        project_cfg.workspace.allow_all = true;
        let ctx = DoctorCtx {
            cfg: project_cfg.clone(),
            managed_cfg: flux_config::Config::default(),
            user_cfg: flux_config::Config::default(),
            project_cfg,
            own_version: "0.0.0",
            release_tags: Ok(vec![]),
            plugins_dir: None,
            events_db_path: None,
        };
        let out = check_config_provenance(&ctx);
        assert_eq!(out.status, CheckStatus::Pass);
        assert!(out.detail.contains("workspace.allow_all=true (project)"));
    }

    // -- version skew -----------------------------------------------------------------------------

    #[test]
    fn judge_version_skew_warns_offline_never_fails() {
        let out = judge_version_skew("0.31.0", &Err("network unreachable".to_string()));
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("network unreachable"));
    }

    #[test]
    fn judge_version_skew_passes_when_current() {
        let tags = Ok(vec!["v0.31.0".to_string(), "plugins-v0.9.0".to_string()]);
        let out = judge_version_skew("0.31.0", &tags);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_version_skew_warns_when_behind() {
        let tags = Ok(vec![
            "v0.30.0".to_string(),
            "v0.32.0".to_string(),
            "plugins-v9.9.9".to_string(),
        ]);
        let out = judge_version_skew("0.31.0", &tags);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("0.32.0"));
        assert!(out.hint.unwrap().contains("v0.32.0"));
    }

    #[test]
    fn judge_version_skew_ignores_plugin_pack_tags() {
        // A pack release far ahead of any flux release must never be mistaken for a flux version.
        let tags = Ok(vec!["v0.31.0".to_string(), "plugins-v99.0.0".to_string()]);
        let out = judge_version_skew("0.31.0", &tags);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_version_skew_warns_when_no_release_tags_found() {
        let out = judge_version_skew("0.31.0", &Ok(vec!["plugins-v1.0.0".to_string()]));
        assert_eq!(out.status, CheckStatus::Warn);
    }

    // -- tools disable (C-162) -----------------------------------------------------------------

    #[test]
    fn judge_tools_disable_passes_when_empty() {
        let resolved = flux_runtime::ResolvedDisabledOps::default();
        let out = judge_tools_disable(&resolved);
        assert_eq!(out.status, CheckStatus::Pass);
    }

    #[test]
    fn judge_tools_disable_passes_and_lists_disabled_ops() {
        let mut resolved = flux_runtime::ResolvedDisabledOps::default();
        resolved.disabled.insert("bash".to_string());
        let out = judge_tools_disable(&resolved);
        assert_eq!(out.status, CheckStatus::Pass);
        assert!(out.detail.contains("bash"));
    }

    #[test]
    fn judge_tools_disable_warns_on_unmatched_pattern() {
        let mut resolved = flux_runtime::ResolvedDisabledOps::default();
        resolved.unmatched.push("nonexistent.*".to_string());
        let out = judge_tools_disable(&resolved);
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("nonexistent.*"));
    }

    #[test]
    fn check_tools_disable_resolves_against_the_real_builtin_registry() {
        let mut cfg = flux_config::Config::default();
        cfg.tools.disable = vec!["bash".to_string(), "definitely-not-a-real-op".to_string()];
        let ctx = DoctorCtx {
            cfg,
            managed_cfg: flux_config::Config::default(),
            user_cfg: flux_config::Config::default(),
            project_cfg: flux_config::Config::default(),
            own_version: "0.0.0",
            release_tags: Ok(vec![]),
            plugins_dir: None,
            events_db_path: None,
        };
        let out = check_tools_disable(&ctx);
        // The made-up name is guaranteed to match no registered op — assert the report surfaces
        // it (whether or not `bash` itself resolves is not this test's concern).
        assert_eq!(out.status, CheckStatus::Warn);
        assert!(out.detail.contains("definitely-not-a-real-op"));
    }

    // -- report assembly / panic isolation ------------------------------------------------------

    fn passing(_ctx: &DoctorCtx) -> CheckOutcome {
        CheckOutcome::pass("ok")
    }

    fn panicking(_ctx: &DoctorCtx) -> CheckOutcome {
        panic!("boom");
    }

    #[test]
    fn a_panicking_check_becomes_a_fail_row_without_aborting_the_rest() {
        let checks = &[
            CheckDef {
                name: "before",
                run: passing,
            },
            CheckDef {
                name: "boom",
                run: panicking,
            },
            CheckDef {
                name: "after",
                run: passing,
            },
        ];
        let ctx = DoctorCtx {
            cfg: flux_config::Config::default(),
            managed_cfg: flux_config::Config::default(),
            user_cfg: flux_config::Config::default(),
            project_cfg: flux_config::Config::default(),
            own_version: "0.0.0",
            release_tags: Ok(vec![]),
            plugins_dir: None,
            events_db_path: None,
        };
        let reports = run_checks_over(checks, &ctx);
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[1].status, CheckStatus::Fail);
        assert!(reports[1].hint.is_some());
        assert_eq!(
            reports[2].status,
            CheckStatus::Pass,
            "the panic did not stop later checks"
        );
    }

    #[test]
    fn every_non_pass_outcome_carries_a_hint() {
        for out in [CheckOutcome::warn("d", "h"), CheckOutcome::fail("d", "h")] {
            assert!(out.hint.is_some());
        }
        assert!(CheckOutcome::pass("d").hint.is_none());
    }

    #[test]
    fn any_failed_is_true_iff_some_check_failed() {
        let reports = vec![
            CheckReport {
                name: "a",
                status: CheckStatus::Pass,
                detail: "".into(),
                hint: None,
            },
            CheckReport {
                name: "b",
                status: CheckStatus::Warn,
                detail: "".into(),
                hint: Some("h".into()),
            },
        ];
        assert!(!any_failed(&reports));
        let mut with_fail = reports.clone();
        with_fail.push(CheckReport {
            name: "c",
            status: CheckStatus::Fail,
            detail: "".into(),
            hint: Some("h".into()),
        });
        assert!(any_failed(&with_fail));
    }

    #[test]
    fn render_report_includes_every_check_name_and_the_summary_line() {
        let reports = vec![
            CheckReport {
                name: "credentials",
                status: CheckStatus::Pass,
                detail: "ok".into(),
                hint: None,
            },
            CheckReport {
                name: "version",
                status: CheckStatus::Warn,
                detail: "behind".into(),
                hint: Some("upgrade".into()),
            },
        ];
        let rendered = render_report(&reports);
        assert!(rendered.contains("credentials"));
        assert!(rendered.contains("version"));
        assert!(rendered.contains("behind"));
        assert!(rendered.contains("upgrade"));
        assert!(rendered.contains("1 passed, 1 warned, 0 failed"));
    }

    #[test]
    fn json_report_shapes_ok_and_every_check() {
        let reports = vec![
            CheckReport {
                name: "a",
                status: CheckStatus::Pass,
                detail: "d".into(),
                hint: None,
            },
            CheckReport {
                name: "b",
                status: CheckStatus::Fail,
                detail: "d2".into(),
                hint: Some("h".into()),
            },
        ];
        let v = json_report(&reports);
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["checks"][0]["name"], serde_json::json!("a"));
        assert_eq!(v["checks"][1]["status"], serde_json::json!("FAIL"));
        assert_eq!(v["checks"][1]["hint"], serde_json::json!("h"));
    }
}
