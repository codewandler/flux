//! Local usage dashboard backing `flux usage`.
//!
//! flux-native data already lives in `flux-events`; other agent harnesses keep local state in
//! JSONL/SQLite shapes. The important boundary in this module is: adapters emit normalized usage
//! records, and every table/metric/JSON view is derived from that one intermediate model.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Days, Duration, Local, NaiveDate, TimeZone};
use clap::{Args, ValueEnum};
use flux_core::{CostSource, PricingTable, Usage};
use flux_events::{EventKind, EventStore, StoredEvent};
use rusqlite::OpenFlags;
use serde::Serialize;
use serde_json::{json, Value};

use crate::style;

const MAX_JSONL_FILES: usize = 20_000;
const MAX_JSONL_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// Flags for `flux usage`.
#[derive(Args, Clone, Debug, Default)]
pub struct UsageArgs {
    /// Restrict the dashboard to one or more harnesses: flux,codex,claude,opencode.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub harness: Vec<UsageHarnessFilter>,

    /// Keep the old flux-only scope and do not scan external harness state.
    #[arg(long, conflicts_with = "harness")]
    pub no_external: bool,

    /// Include records at or after this bound: YYYY-MM-DD, RFC3339, or a duration like 24h/7d/2w.
    #[arg(long, conflicts_with = "last")]
    pub since: Option<String>,

    /// Include records before this bound: YYYY-MM-DD or RFC3339. Date-only values mean next midnight.
    #[arg(long)]
    pub until: Option<String>,

    /// Shorthand for --since now-duration, using h/d/w units.
    #[arg(long)]
    pub last: Option<String>,

    /// Show scan progress on stderr: auto, always, or never (--json output always suppresses progress).
    #[arg(long, value_enum, default_value_t)]
    pub progress: ProgressMode,

    /// Emit normalized JSON instead of the human dashboard.
    #[arg(long)]
    pub json: bool,
}

/// Harness selector used by `--harness`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum UsageHarnessFilter {
    Flux,
    Codex,
    Claude,
    Opencode,
}

/// Progress rendering policy for slow external scans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ProgressMode {
    /// Show progress only for interactive stderr and human output.
    #[default]
    Auto,
    /// Force progress for human output.
    Always,
    /// Never show progress.
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum HarnessKind {
    Flux,
    Codex,
    Claude,
    Opencode,
}

impl HarnessKind {
    fn id(self) -> &'static str {
        match self {
            HarnessKind::Flux => "flux",
            HarnessKind::Codex => "codex",
            HarnessKind::Claude => "claude-code",
            HarnessKind::Opencode => "opencode",
        }
    }

    fn label(self) -> &'static str {
        match self {
            HarnessKind::Flux => "flux",
            HarnessKind::Codex => "Codex",
            HarnessKind::Claude => "Claude Code",
            HarnessKind::Opencode => "opencode",
        }
    }
}

impl From<UsageHarnessFilter> for HarnessKind {
    fn from(value: UsageHarnessFilter) -> Self {
        match value {
            UsageHarnessFilter::Flux => HarnessKind::Flux,
            UsageHarnessFilter::Codex => HarnessKind::Codex,
            UsageHarnessFilter::Claude => HarnessKind::Claude,
            UsageHarnessFilter::Opencode => HarnessKind::Opencode,
        }
    }
}

#[derive(Clone, Debug)]
struct HarnessDataset {
    kind: HarnessKind,
    source: Option<PathBuf>,
    note: Option<String>,
    latest_session: Option<String>,
    records: Vec<UsageRecord>,
    sessions: Vec<SessionRecord>,
    // Preformatted efficiency lines (flux only; other harnesses expose no turn projection). Held on
    // the dataset because they need the `EventStore`, which is not available at render time.
    latest_efficiency: Option<String>,
    all_efficiency: Option<String>,
    scanned: usize,
    skipped: usize,
}

impl HarnessDataset {
    fn missing(kind: HarnessKind, source: PathBuf) -> Self {
        Self {
            kind,
            source: Some(source),
            note: Some("not found".to_string()),
            latest_session: None,
            records: Vec::new(),
            sessions: Vec::new(),
            latest_efficiency: None,
            all_efficiency: None,
            scanned: 0,
            skipped: 0,
        }
    }

    fn warning(kind: HarnessKind, source: Option<PathBuf>, warning: impl Into<String>) -> Self {
        Self {
            kind,
            source,
            note: Some(warning.into()),
            latest_session: None,
            records: Vec::new(),
            sessions: Vec::new(),
            latest_efficiency: None,
            all_efficiency: None,
            scanned: 0,
            skipped: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct HarnessReport {
    kind: HarnessKind,
    source: Option<PathBuf>,
    note: Option<String>,
    sections: Vec<UsageSection>,
    scanned: usize,
    skipped: usize,
}

impl HarnessReport {
    fn has_rows(&self) -> bool {
        self.sections.iter().any(|s| !s.rows.is_empty())
    }
}

#[derive(Clone, Debug)]
struct UsageSection {
    title: String,
    rows: Vec<UsageRow>,
    metrics: UsageMetrics,
    efficiency: Option<String>,
    include_in_combined: bool,
}

#[derive(Clone, Debug)]
struct UsageRecord {
    harness: HarnessKind,
    session_id: String,
    model: String,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    usage: Usage,
    cost: Option<CostCell>,
    cost_status: CostStatus,
}

#[derive(Clone, Debug)]
struct SessionRecord {
    harness: HarnessKind,
    session_id: String,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    cwd: Option<String>,
    messages: u64,
}

#[derive(Default)]
struct SessionBuild {
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    cwd: Option<String>,
    messages: u64,
}

impl SessionBuild {
    fn observe(&mut self, ts_ms: Option<i64>) {
        if let Some(ts) = ts_ms {
            self.started_at_ms = Some(self.started_at_ms.map_or(ts, |old| old.min(ts)));
            self.ended_at_ms = Some(self.ended_at_ms.map_or(ts, |old| old.max(ts)));
        }
    }

    fn observe_range(&mut self, started_at_ms: Option<i64>, ended_at_ms: Option<i64>) {
        self.observe(started_at_ms);
        self.observe(ended_at_ms);
    }

    fn into_record(self, harness: HarnessKind, session_id: String) -> SessionRecord {
        SessionRecord {
            harness,
            session_id,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            cwd: self.cwd,
            messages: self.messages,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct UsageRow {
    model: String,
    calls: u64,
    usage: Usage,
    cost: Option<CostCell>,
    unpriced: BTreeMap<CostStatus, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CostCell {
    usd: f64,
    subscription: bool,
    source: CostSourceCell,
    status: CostStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CostSourceCell {
    Reported,
    Estimated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CostStatus {
    Reported,
    EstimatedTable,
    SubscriptionEquivalent,
    UnpricedUnknownModel,
    UnpricedMissingUsage,
}

impl CostStatus {
    fn as_str(self) -> &'static str {
        match self {
            CostStatus::Reported => "reported",
            CostStatus::EstimatedTable => "estimated_table",
            CostStatus::SubscriptionEquivalent => "subscription_equivalent",
            CostStatus::UnpricedUnknownModel => "unpriced_unknown_model",
            CostStatus::UnpricedMissingUsage => "unpriced_missing_usage",
        }
    }

    fn short_reason(self) -> &'static str {
        match self {
            CostStatus::Reported => "reported",
            CostStatus::EstimatedTable => "table",
            CostStatus::SubscriptionEquivalent => "sub",
            CostStatus::UnpricedUnknownModel => "unknown model",
            CostStatus::UnpricedMissingUsage => "missing usage",
        }
    }

    fn is_unpriced(self) -> bool {
        matches!(
            self,
            CostStatus::UnpricedUnknownModel | CostStatus::UnpricedMissingUsage
        )
    }
}

struct RowFold {
    usage: Usage,
    calls: u64,
    cost_usd: f64,
    priced_calls: u64,
    subscription: bool,
    all_reported: bool,
    status_counts: BTreeMap<CostStatus, u64>,
}

impl Default for RowFold {
    fn default() -> Self {
        Self {
            usage: Usage::default(),
            calls: 0,
            cost_usd: 0.0,
            priced_calls: 0,
            subscription: false,
            all_reported: true,
            status_counts: BTreeMap::new(),
        }
    }
}

impl RowFold {
    fn record_record(&mut self, record: &UsageRecord) {
        self.record_parts(&record.usage, record.cost, record.cost_status);
    }

    fn record_row(&mut self, row: &UsageRow) {
        self.calls += row.calls;
        sum_usage(&mut self.usage, &row.usage);
        for (status, count) in &row.unpriced {
            *self.status_counts.entry(*status).or_insert(0) += *count;
        }
        if let Some(cost) = row.cost {
            self.cost_usd += cost.usd;
            self.priced_calls += row.calls.max(1);
            self.subscription = self.subscription || cost.subscription;
            self.all_reported = self.all_reported && cost.source == CostSourceCell::Reported;
            *self.status_counts.entry(cost.status).or_insert(0) += row.calls.max(1);
        }
    }

    fn record_parts(&mut self, usage: &Usage, cost: Option<CostCell>, status: CostStatus) {
        self.calls += 1;
        sum_usage(&mut self.usage, usage);
        *self.status_counts.entry(status).or_insert(0) += 1;
        if let Some(cost) = cost {
            self.cost_usd += cost.usd;
            self.priced_calls += 1;
            self.subscription = self.subscription || cost.subscription;
            self.all_reported = self.all_reported && cost.source == CostSourceCell::Reported;
        }
    }

    fn into_row(self, model: String) -> UsageRow {
        let unpriced = self
            .status_counts
            .iter()
            .filter_map(|(status, count)| status.is_unpriced().then_some((*status, *count)))
            .collect();
        UsageRow {
            model,
            calls: self.calls,
            usage: self.usage,
            cost: (self.priced_calls > 0).then_some(CostCell {
                usd: self.cost_usd,
                subscription: self.subscription,
                source: if self.all_reported {
                    CostSourceCell::Reported
                } else {
                    CostSourceCell::Estimated
                },
                status: if self.subscription {
                    CostStatus::SubscriptionEquivalent
                } else if self.all_reported {
                    CostStatus::Reported
                } else {
                    CostStatus::EstimatedTable
                },
            }),
            unpriced,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct UsageMetrics {
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    sessions: u64,
    // The distinct local calendar days and workspace paths observed, kept as SETS (not counts) so a
    // cross-harness `merge` unions them: a day or workspace active in two harnesses must be counted
    // once, not twice, in the combined/summary totals. `active_days`/`workspaces` are the set lens,
    // recomputed by `recompute_derived`.
    active_day_keys: BTreeSet<String>,
    workspace_keys: BTreeSet<String>,
    active_days: u64,
    workspaces: u64,
    covered_days: u64,
    sessions_per_day: f64,
    wall_ms: u64,
    calls: u64,
    messages: u64,
    usage: Usage,
    cost_usd: f64,
    unpriced_records: u64,
}

#[derive(Clone, Debug)]
struct SummaryRow {
    label: String,
    metrics: UsageMetrics,
    total: bool,
}

impl UsageMetrics {
    fn merge(&mut self, other: &Self) {
        self.first_ms = merge_min(self.first_ms, other.first_ms);
        self.last_ms = merge_max(self.last_ms, other.last_ms);
        self.sessions += other.sessions;
        self.active_day_keys
            .extend(other.active_day_keys.iter().cloned());
        self.workspace_keys
            .extend(other.workspace_keys.iter().cloned());
        self.wall_ms += other.wall_ms;
        self.calls += other.calls;
        self.messages += other.messages;
        sum_usage(&mut self.usage, &other.usage);
        self.cost_usd += other.cost_usd;
        self.unpriced_records += other.unpriced_records;
        self.recompute_derived();
    }

    fn recompute_derived(&mut self) {
        self.active_days = self.active_day_keys.len() as u64;
        self.workspaces = self.workspace_keys.len() as u64;
        // `covered_days` counts the local calendar days spanned by first..last inclusive — the same
        // day arithmetic `active_days` uses — so covered >= active always holds. An elapsed-time
        // quotient would render "1d covered · 2 active d" for records at 23:59 and 00:01.
        self.covered_days = match (self.first_ms, self.last_ms) {
            (Some(first), Some(last)) => match (local_day(first), local_day(last.max(first))) {
                (Some(first_day), Some(last_day)) => ((last_day - first_day).num_days() + 1) as u64,
                _ => 0,
            },
            _ => 0,
        };
        self.sessions_per_day = if self.active_days == 0 {
            0.0
        } else {
            self.sessions as f64 / self.active_days as f64
        };
    }
}

#[derive(Clone, Debug)]
struct TimeFilter {
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    label: String,
}

impl TimeFilter {
    fn from_args(args: &UsageArgs) -> Result<Self> {
        let now_ms = now_ms();
        let since_ms = if let Some(last) = &args.last {
            let duration = parse_duration_ms(last)?;
            Some(now_ms.saturating_sub(duration))
        } else if let Some(since) = &args.since {
            Some(parse_since_ms(since, now_ms)?)
        } else {
            None
        };
        let until_ms = args.until.as_deref().map(parse_until_ms).transpose()?;
        if let (Some(since), Some(until)) = (since_ms, until_ms) {
            if since >= until {
                bail!("--since must be before --until");
            }
        }
        let label = match (since_ms, until_ms) {
            (None, None) => "all time".to_string(),
            (Some(since), None) => format!("since {}", fmt_ts(since)),
            (None, Some(until)) => format!("until {}", fmt_ts(until)),
            (Some(since), Some(until)) => format!("{}..{}", fmt_ts(since), fmt_ts(until)),
        };
        Ok(Self {
            since_ms,
            until_ms,
            label,
        })
    }

    fn matches(&self, started_at_ms: Option<i64>, ended_at_ms: Option<i64>) -> bool {
        let Some(start) = started_at_ms.or(ended_at_ms) else {
            return self.since_ms.is_none() && self.until_ms.is_none();
        };
        let end = ended_at_ms.or(started_at_ms).unwrap_or(start);
        if let Some(since) = self.since_ms {
            if end < since {
                return false;
            }
        }
        if let Some(until) = self.until_ms {
            if start >= until {
                return false;
            }
        }
        true
    }

    /// True when no `--since`/`--until`/`--last` bound is active (the default `all time` view).
    fn is_unbounded(&self) -> bool {
        self.since_ms.is_none() && self.until_ms.is_none()
    }

    /// True when the whole `[start, end]` span lies inside the active window (half-open on the upper
    /// bound, matching `matches`). Used to gate whole-session aggregates that cannot be sliced to a
    /// sub-window. An unbounded filter contains everything, including spans with unknown timestamps.
    fn fully_contains(&self, started_at_ms: Option<i64>, ended_at_ms: Option<i64>) -> bool {
        let (Some(start), Some(end)) =
            (started_at_ms.or(ended_at_ms), ended_at_ms.or(started_at_ms))
        else {
            return self.since_ms.is_none() && self.until_ms.is_none();
        };
        self.since_ms.is_none_or(|since| start >= since)
            && self.until_ms.is_none_or(|until| end < until)
    }
}

struct ProgressRenderer {
    active: bool,
    last_len: usize,
}

impl ProgressRenderer {
    fn new(mode: ProgressMode, json: bool) -> Self {
        let active = !json
            && match mode {
                ProgressMode::Auto => std::io::stderr().is_terminal(),
                ProgressMode::Always => true,
                ProgressMode::Never => false,
            };
        Self {
            active,
            last_len: 0,
        }
    }

    fn begin(&mut self, label: &str, total: usize) {
        self.draw(label, 0, total, 0);
    }

    fn tick(&mut self, label: &str, current: usize, total: usize, skipped: usize) {
        self.draw(label, current, total, skipped);
    }

    fn finish(&mut self, label: &str, current: usize, total: usize, skipped: usize) {
        if !self.active {
            return;
        }
        self.draw(label, current, total, skipped);
        eprintln!();
        self.last_len = 0;
    }

    fn draw(&mut self, label: &str, current: usize, total: usize, skipped: usize) {
        if !self.active {
            return;
        }
        let bar = progress_bar(current, total);
        let skipped = if skipped > 0 {
            format!(" · {skipped} skipped")
        } else {
            String::new()
        };
        let msg = format!(
            "{} {} {:>5}/{}{}",
            style::dim("scanning"),
            label,
            current,
            total.max(current),
            skipped
        );
        let line = format!("{bar} {msg}");
        let width = self.last_len.max(line.chars().count());
        eprint!("\r{line:<width$}");
        let _ = std::io::stderr().flush();
        self.last_len = width;
    }
}

/// Run the real `flux usage` command.
pub fn run_usage(args: UsageArgs, pricing: &PricingTable) -> Result<()> {
    let filter = TimeFilter::from_args(&args)?;
    let requested = requested_harnesses(&args);
    let explicit = args.no_external || !args.harness.is_empty();
    let mut progress = ProgressRenderer::new(args.progress, args.json);
    let mut reports = Vec::new();
    for kind in requested {
        let dataset = collect_harness(kind, pricing, &mut progress);
        let report = report_from_dataset(dataset, &filter);
        if report.has_rows() || report.note.is_some() || explicit {
            reports.push(report);
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report_json(&reports, &filter))?
        );
    } else {
        print!("{}", render_human(&reports, &filter));
    }
    Ok(())
}

/// Testable flux-only body retained for the existing `flux usage` unit test seam.
#[cfg(test)]
pub fn run_usage_with(store: &EventStore, pricing: &PricingTable) -> Result<()> {
    let filter = TimeFilter {
        since_ms: None,
        until_ms: None,
        label: "all time".to_string(),
    };
    let report = report_from_dataset(flux_dataset_from_store(store, pricing, None)?, &filter);
    print!("{}", render_human(&[report], &filter));
    Ok(())
}

fn requested_harnesses(args: &UsageArgs) -> Vec<HarnessKind> {
    if args.no_external {
        return vec![HarnessKind::Flux];
    }
    if args.harness.is_empty() {
        return vec![
            HarnessKind::Flux,
            HarnessKind::Codex,
            HarnessKind::Claude,
            HarnessKind::Opencode,
        ];
    }
    let mut out = Vec::new();
    for h in &args.harness {
        let kind = HarnessKind::from(*h);
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    out
}

fn collect_harness(
    kind: HarnessKind,
    pricing: &PricingTable,
    progress: &mut ProgressRenderer,
) -> HarnessDataset {
    match kind {
        HarnessKind::Flux => collect_flux(pricing, progress),
        HarnessKind::Codex => collect_codex(pricing, progress),
        HarnessKind::Claude => collect_claude(pricing, progress),
        HarnessKind::Opencode => collect_opencode(pricing, progress),
    }
}

fn collect_flux(pricing: &PricingTable, progress: &mut ProgressRenderer) -> HarnessDataset {
    let Some(path) = flux_events_path() else {
        return HarnessDataset::warning(HarnessKind::Flux, None, "HOME is not set");
    };
    if !path.exists() {
        return HarnessDataset::missing(HarnessKind::Flux, path);
    }
    match EventStore::open(&path)
        .with_context(|| format!("open {}", path.display()))
        .and_then(|store| {
            flux_dataset_from_store_with_progress(&store, pricing, Some(path.clone()), progress)
        }) {
        Ok(report) => report,
        Err(e) => HarnessDataset::warning(HarnessKind::Flux, Some(path), e.to_string()),
    }
}

#[cfg(test)]
fn flux_dataset_from_store(
    store: &EventStore,
    pricing: &PricingTable,
    source: Option<PathBuf>,
) -> Result<HarnessDataset> {
    let mut progress = ProgressRenderer {
        active: false,
        last_len: 0,
    };
    flux_dataset_from_store_with_progress(store, pricing, source, &mut progress)
}

fn flux_dataset_from_store_with_progress(
    store: &EventStore,
    pricing: &PricingTable,
    source: Option<PathBuf>,
    progress: &mut ProgressRenderer,
) -> Result<HarnessDataset> {
    let latest_session = store.latest_session()?;
    let latest_efficiency = match &latest_session {
        Some(session) => store.efficiency(session)?.as_ref().map(format_efficiency),
        None => None,
    };
    let all_efficiency = store.efficiency_all()?.as_ref().map(format_efficiency);
    let streams = store.all_streams()?;
    let mut loaded = Vec::new();
    progress.begin(HarnessKind::Flux.label(), streams.len());
    for (idx, stream) in streams.iter().enumerate() {
        let events = store.load_stream(stream, None)?;
        let correlation_id = events
            .first()
            .and_then(|e| e.context.correlation_id.clone());
        loaded.push((stream.clone(), events, correlation_id));
        progress.tick(HarnessKind::Flux.label(), idx + 1, streams.len(), 0);
    }
    progress.finish(HarnessKind::Flux.label(), streams.len(), streams.len(), 0);

    let ids: HashSet<String> = loaded.iter().map(|(id, _, _)| id.clone()).collect();
    let mut records = Vec::new();
    let mut sessions = Vec::new();
    for (stream, events, correlation_id) in loaded {
        if correlation_id
            .as_ref()
            .is_some_and(|parent| ids.contains(parent))
        {
            continue;
        }
        let mut stream_records = flux_records_from_events(&stream, &events, pricing);
        let session = flux_session_from_events(&stream, &events);
        records.append(&mut stream_records);
        sessions.push(session);
    }

    Ok(HarnessDataset {
        kind: HarnessKind::Flux,
        source,
        note: None,
        latest_session,
        records,
        sessions,
        latest_efficiency,
        all_efficiency,
        scanned: streams.len(),
        skipped: 0,
    })
}

fn flux_session_from_events(stream: &str, events: &[StoredEvent]) -> SessionRecord {
    let mut build = SessionBuild::default();
    for event in events {
        build.observe(Some(event.ts_ms));
    }
    build.messages = flux_events::turns(events).len() as u64;
    build.into_record(HarnessKind::Flux, stream.to_string())
}

fn flux_records_from_events(
    stream: &str,
    events: &[StoredEvent],
    pricing: &PricingTable,
) -> Vec<UsageRecord> {
    let mut records = Vec::new();
    let any_call_usage = events
        .iter()
        .any(|event| matches!(event.kind, EventKind::CallUsage { .. }));
    if any_call_usage {
        for event in events {
            if let EventKind::CallUsage { model, usage } = &event.kind {
                if usage_is_empty(usage) {
                    continue;
                }
                records.push(usage_record(
                    HarnessKind::Flux,
                    stream.to_string(),
                    model.clone(),
                    Some(event.ts_ms),
                    Some(event.ts_ms),
                    usage.clone(),
                    pricing,
                ));
            }
        }
        return records;
    }

    for turn in flux_events::turns(events) {
        if let Some(usage) = turn.usage {
            if usage_is_empty(&usage) {
                continue;
            }
            let ts = turn.ended_at_ms.or(Some(turn.started_at_ms));
            records.push(usage_record(
                HarnessKind::Flux,
                stream.to_string(),
                turn.model,
                ts,
                ts,
                usage,
                pricing,
            ));
        }
    }
    records
}

fn collect_codex(pricing: &PricingTable, progress: &mut ProgressRenderer) -> HarnessDataset {
    let Some(root) = harness_root("CODEX_HOME", ".codex") else {
        return HarnessDataset::warning(HarnessKind::Codex, None, "HOME is not set");
    };
    let sessions = root.join("sessions");
    if !sessions.exists() {
        return HarnessDataset::missing(HarnessKind::Codex, sessions);
    }
    match parse_codex_sessions(&sessions, pricing, progress) {
        Ok((records, session_records, scanned, skipped)) => external_dataset(
            HarnessKind::Codex,
            sessions,
            records,
            session_records,
            scanned,
            skipped,
        ),
        Err(e) => HarnessDataset::warning(HarnessKind::Codex, Some(sessions), e.to_string()),
    }
}

fn collect_claude(pricing: &PricingTable, progress: &mut ProgressRenderer) -> HarnessDataset {
    let Some(root) = env_path("CLAUDE_CONFIG_DIR").or_else(|| harness_root("", ".claude")) else {
        return HarnessDataset::warning(HarnessKind::Claude, None, "HOME is not set");
    };
    let projects = root.join("projects");
    if !projects.exists() {
        return HarnessDataset::missing(HarnessKind::Claude, projects);
    }
    match parse_claude_projects(&projects, pricing, progress) {
        Ok((records, session_records, scanned, skipped)) => external_dataset(
            HarnessKind::Claude,
            projects,
            records,
            session_records,
            scanned,
            skipped,
        ),
        Err(e) => HarnessDataset::warning(HarnessKind::Claude, Some(projects), e.to_string()),
    }
}

fn collect_opencode(pricing: &PricingTable, progress: &mut ProgressRenderer) -> HarnessDataset {
    let Some(root) = env_path("OPENCODE_DATA_DIR")
        .or_else(|| home_dir().map(|h| h.join(".local").join("share").join("opencode")))
    else {
        return HarnessDataset::warning(HarnessKind::Opencode, None, "HOME is not set");
    };
    let db = root.join("opencode.db");
    if !db.exists() {
        return HarnessDataset::missing(HarnessKind::Opencode, db);
    }
    match parse_opencode_db(&db, pricing, progress) {
        Ok((records, session_records, scanned, skipped)) => external_dataset(
            HarnessKind::Opencode,
            db,
            records,
            session_records,
            scanned,
            skipped,
        ),
        Err(e) => HarnessDataset::warning(HarnessKind::Opencode, Some(db), e.to_string()),
    }
}

fn external_dataset(
    kind: HarnessKind,
    source: PathBuf,
    records: Vec<UsageRecord>,
    sessions: Vec<SessionRecord>,
    scanned: usize,
    skipped: usize,
) -> HarnessDataset {
    HarnessDataset {
        kind,
        source: Some(source),
        note: None,
        latest_session: None,
        records,
        sessions,
        latest_efficiency: None,
        all_efficiency: None,
        scanned,
        skipped,
    }
}

fn parse_claude_projects(
    projects: &Path,
    pricing: &PricingTable,
    progress: &mut ProgressRenderer,
) -> Result<(Vec<UsageRecord>, Vec<SessionRecord>, usize, usize)> {
    let (files, mut skipped) = jsonl_files(projects)?;
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let mut sessions = BTreeMap::<String, SessionBuild>::new();

    progress.begin(HarnessKind::Claude.label(), files.len());
    for (idx, file) in files.iter().enumerate() {
        if too_large(file) {
            skipped += 1;
            progress.tick(HarnessKind::Claude.label(), idx + 1, files.len(), skipped);
            continue;
        }
        // One unreadable file must not abort the scan: skip it like a bad line and keep the rest.
        let Ok(open) = File::open(file) else {
            skipped += 1;
            progress.tick(HarnessKind::Claude.label(), idx + 1, files.len(), skipped);
            continue;
        };
        let reader = BufReader::new(open);
        let fallback_session = file_stem(file);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if !line.contains("\"type\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                skipped += 1;
                continue;
            };
            let typ = v.get("type").and_then(Value::as_str);
            let sid = v
                .get("sessionId")
                .or_else(|| v.get("session_id"))
                .and_then(Value::as_str)
                .unwrap_or(&fallback_session)
                .to_string();
            let ts = json_timestamp_ms(&v);
            let build = sessions.entry(sid.clone()).or_default();
            build.observe(ts);
            if matches!(typ, Some("user" | "assistant")) {
                build.messages += 1;
            }
            if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
                build.cwd.get_or_insert_with(|| cwd.to_string());
            }

            if typ != Some("assistant") {
                continue;
            }
            let Some(message) = v.get("message") else {
                continue;
            };
            let Some(usage_value) = message.get("usage") else {
                continue;
            };
            let Some(model) = message.get("model").and_then(Value::as_str) else {
                continue;
            };
            let dedupe_key = message
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| v.get("requestId").and_then(Value::as_str))
                .or_else(|| v.get("uuid").and_then(Value::as_str));
            if let Some(key) = dedupe_key {
                if !seen.insert(format!("{sid}:{key}")) {
                    continue;
                }
            }
            let model = prefixed_model("claude", model);
            let usage = usage_from_anthropic(usage_value);
            if usage_is_empty(&usage) {
                continue;
            }
            records.push(usage_record(
                HarnessKind::Claude,
                sid,
                model,
                ts,
                ts,
                usage,
                pricing,
            ));
        }
        progress.tick(HarnessKind::Claude.label(), idx + 1, files.len(), skipped);
    }
    progress.finish(
        HarnessKind::Claude.label(),
        files.len(),
        files.len(),
        skipped,
    );

    Ok((
        records,
        sessions
            .into_iter()
            .map(|(id, build)| build.into_record(HarnessKind::Claude, id))
            .collect(),
        files.len(),
        skipped,
    ))
}

fn parse_codex_sessions(
    sessions_root: &Path,
    pricing: &PricingTable,
    progress: &mut ProgressRenderer,
) -> Result<(Vec<UsageRecord>, Vec<SessionRecord>, usize, usize)> {
    let (files, mut skipped) = jsonl_files(sessions_root)?;
    let mut records = Vec::new();
    let mut session_records = Vec::new();

    progress.begin(HarnessKind::Codex.label(), files.len());
    for (idx, file) in files.iter().enumerate() {
        if too_large(file) {
            skipped += 1;
            progress.tick(HarnessKind::Codex.label(), idx + 1, files.len(), skipped);
            continue;
        }
        // One unreadable file must not abort the scan: skip it like a bad line and keep the rest.
        let Ok(open) = File::open(file) else {
            skipped += 1;
            progress.tick(HarnessKind::Codex.label(), idx + 1, files.len(), skipped);
            continue;
        };
        let reader = BufReader::new(open);
        let mut session_id = file_stem(file);
        let mut build = SessionBuild::default();
        let mut model = "codex/gpt-5.5".to_string();
        let mut token_count_records = Vec::<UsageRecord>::new();
        let mut fallback_records = Vec::<UsageRecord>::new();
        let mut seen_fallback = HashSet::new();

        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let interesting = line.contains("\"session_meta\"")
                || line.contains("\"turn_context\"")
                || line.contains("\"token_count\"")
                || line.contains("\"user_message\"")
                || line.contains("\"agent_message\"")
                || (line.contains("\"response_item\"") && line.contains("\"usage\""));
            if !interesting {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                skipped += 1;
                continue;
            };
            let ts = json_timestamp_ms(&v);
            build.observe(ts);
            match v.get("type").and_then(Value::as_str) {
                Some("session_meta") => {
                    if let Some(id) = v.pointer("/payload/id").and_then(Value::as_str) {
                        session_id = id.to_string();
                    }
                    if let Some(cwd) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                        build.cwd.get_or_insert_with(|| cwd.to_string());
                    }
                    if let Some(started) = v.pointer("/payload/timestamp").and_then(json_value_ms) {
                        build.observe(Some(started));
                    }
                    continue;
                }
                Some("turn_context") => {
                    if let Some(m) = v
                        .pointer("/payload/model")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                    {
                        model = prefixed_model("codex", m);
                    }
                    continue;
                }
                Some("event_msg")
                    if v.pointer("/payload/type").and_then(Value::as_str)
                        == Some("token_count") =>
                {
                    if let Some(info) = v.pointer("/payload/info/last_token_usage") {
                        let usage = usage_from_codex_token_count(info);
                        if !usage_is_empty(&usage) {
                            token_count_records.push(usage_record(
                                HarnessKind::Codex,
                                session_id.clone(),
                                model.clone(),
                                ts,
                                ts,
                                usage,
                                pricing,
                            ));
                        }
                    }
                    continue;
                }
                Some("event_msg") => {
                    if matches!(
                        v.pointer("/payload/type").and_then(Value::as_str),
                        Some("user_message" | "agent_message")
                    ) {
                        build.messages += 1;
                    }
                }
                Some("response_item") => {
                    let Some(message) = v.pointer("/payload/message") else {
                        continue;
                    };
                    let Some(usage_value) = message.get("usage") else {
                        continue;
                    };
                    let request_key = message
                        .get("id")
                        .and_then(Value::as_str)
                        .or_else(|| v.get("requestId").and_then(Value::as_str));
                    if let Some(key) = request_key {
                        if !seen_fallback.insert(key.to_string()) {
                            continue;
                        }
                    }
                    let fallback_model = message
                        .get("model")
                        .and_then(Value::as_str)
                        .map(|m| prefixed_model("codex", m))
                        .unwrap_or_else(|| model.clone());
                    let usage = usage_from_anthropic(usage_value);
                    if !usage_is_empty(&usage) {
                        fallback_records.push(usage_record(
                            HarnessKind::Codex,
                            session_id.clone(),
                            fallback_model,
                            ts,
                            ts,
                            usage,
                            pricing,
                        ));
                    }
                }
                _ => {}
            }
        }

        if token_count_records.is_empty() {
            records.extend(fallback_records);
        } else {
            records.extend(token_count_records);
        }
        session_records.push(build.into_record(HarnessKind::Codex, session_id));
        progress.tick(HarnessKind::Codex.label(), idx + 1, files.len(), skipped);
    }
    progress.finish(
        HarnessKind::Codex.label(),
        files.len(),
        files.len(),
        skipped,
    );

    Ok((records, session_records, files.len(), skipped))
}

fn parse_opencode_db(
    db: &Path,
    pricing: &PricingTable,
    progress: &mut ProgressRenderer,
) -> Result<(Vec<UsageRecord>, Vec<SessionRecord>, usize, usize)> {
    let conn = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", db.display()))?;
    let has_session_table = sqlite_table_exists(&conn, "session")?;
    let message_has_session_id = sqlite_column_exists(&conn, "message", "session_id")?;
    let message_has_time_created = sqlite_column_exists(&conn, "message", "time_created")?;
    let message_has_time_updated = sqlite_column_exists(&conn, "message", "time_updated")?;

    let mut sessions = BTreeMap::<String, SessionBuild>::new();
    if has_session_table {
        let mut stmt = conn.prepare(
            "select id, time_created, time_updated, directory from session order by time_created",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let started: Option<i64> = row.get::<_, Option<i64>>(1)?.map(normalize_epoch_ms);
            let ended: Option<i64> = row.get::<_, Option<i64>>(2)?.map(normalize_epoch_ms);
            let cwd: Option<String> = row.get(3)?;
            let build = sessions.entry(id).or_default();
            build.observe_range(started, ended);
            build.cwd = build.cwd.take().or(cwd);
        }
    }

    let total = sqlite_count_assistant_token_messages(&conn).unwrap_or(0);
    progress.begin(HarnessKind::Opencode.label(), total);
    let sql = match (
        message_has_session_id,
        message_has_time_created,
        message_has_time_updated,
    ) {
        (true, true, true) => {
            "select id, session_id, time_created, time_updated, data from message \
             where json_extract(data, '$.role') = 'assistant' \
               and json_type(data, '$.tokens') is not null \
             order by time_created, id"
        }
        _ => {
            "select id, null, null, null, data from message \
             where json_extract(data, '$.role') = 'assistant' \
               and json_type(data, '$.tokens') is not null \
             order by id"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mut query = stmt.query([])?;
    let mut records = Vec::new();
    let mut scanned = 0usize;
    let mut skipped = 0usize;
    while let Some(row) = query.next()? {
        scanned += 1;
        let id: String = row.get(0)?;
        let row_session: Option<String> = row.get(1)?;
        let created: Option<i64> = row.get::<_, Option<i64>>(2)?.map(normalize_epoch_ms);
        let updated: Option<i64> = row.get::<_, Option<i64>>(3)?.map(normalize_epoch_ms);
        let data: String = row.get(4)?;
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => {
                skipped += 1;
                progress.tick(HarnessKind::Opencode.label(), scanned, total, skipped);
                continue;
            }
        };
        let provider = v
            .get("providerID")
            .and_then(Value::as_str)
            .unwrap_or("opencode");
        let Some(model_id) = v.get("modelID").and_then(Value::as_str) else {
            skipped += 1;
            progress.tick(HarnessKind::Opencode.label(), scanned, total, skipped);
            continue;
        };
        let session_id = row_session
            .or_else(|| {
                v.get("sessionID")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                v.get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| id.clone());
        let started = created.or_else(|| json_nested_timestamp_ms(&v, &["time", "created"]));
        let ended = updated
            .or_else(|| json_nested_timestamp_ms(&v, &["time", "completed"]))
            .or(started);
        let usage = Usage {
            input_tokens: u64_path(&v, &["tokens", "input"]),
            output_tokens: u64_path(&v, &["tokens", "output"]),
            cache_read_input_tokens: u64_path(&v, &["tokens", "cache", "read"]),
            cache_creation_input_tokens: u64_path(&v, &["tokens", "cache", "write"]),
            reasoning_tokens: u64_path(&v, &["tokens", "reasoning"]),
            reported_cost_usd: f64_path(&v, &["cost"]),
            ..Default::default()
        };
        if usage_is_empty(&usage) {
            skipped += 1;
            progress.tick(HarnessKind::Opencode.label(), scanned, total, skipped);
            continue;
        }
        let build = sessions.entry(session_id.clone()).or_default();
        build.observe_range(started, ended);
        build.messages += 1;
        records.push(usage_record(
            HarnessKind::Opencode,
            session_id,
            prefixed_model(provider, model_id),
            started,
            ended,
            usage,
            pricing,
        ));
        progress.tick(HarnessKind::Opencode.label(), scanned, total, skipped);
    }
    progress.finish(HarnessKind::Opencode.label(), scanned, total, skipped);

    Ok((
        records,
        sessions
            .into_iter()
            .map(|(id, build)| build.into_record(HarnessKind::Opencode, id))
            .collect(),
        scanned,
        skipped,
    ))
}

fn report_from_dataset(dataset: HarnessDataset, filter: &TimeFilter) -> HarnessReport {
    // The efficiency projection is a whole-history aggregate that cannot be sliced to a window, so
    // only surface it on the unbounded `all time` view — never beside window-filtered token metrics.
    let show_efficiency = filter.is_unbounded();
    let mut sections = Vec::new();
    if dataset.kind == HarnessKind::Flux {
        if let Some(latest) = &dataset.latest_session {
            let section = section_from_dataset(&dataset, filter, Some(latest), false);
            if !section.rows.is_empty() || section.metrics.sessions > 0 {
                sections.push(UsageSection {
                    title: format!("latest session {latest}"),
                    efficiency: show_efficiency
                        .then(|| dataset.latest_efficiency.clone())
                        .flatten(),
                    ..section
                });
            }
        }
    }
    let mut all_sessions = section_from_dataset(&dataset, filter, None, true);
    if show_efficiency {
        all_sessions.efficiency = dataset.all_efficiency.clone();
    }
    sections.push(all_sessions);

    HarnessReport {
        kind: dataset.kind,
        source: dataset.source,
        note: dataset.note,
        sections,
        scanned: dataset.scanned,
        skipped: dataset.skipped,
    }
}

fn section_from_dataset(
    dataset: &HarnessDataset,
    filter: &TimeFilter,
    session_id: Option<&str>,
    include_in_combined: bool,
) -> UsageSection {
    let records: Vec<&UsageRecord> = dataset
        .records
        .iter()
        .filter(|r| session_id.is_none_or(|id| r.session_id == id))
        .filter(|r| filter.matches(r.started_at_ms, r.ended_at_ms))
        .collect();
    let sessions: Vec<&SessionRecord> = dataset
        .sessions
        .iter()
        .filter(|s| session_id.is_none_or(|id| s.session_id == id))
        .filter(|s| filter.matches(s.started_at_ms, s.ended_at_ms))
        .collect();
    let title = if session_id.is_some() {
        "latest session".to_string()
    } else {
        "all sessions".to_string()
    };
    UsageSection {
        title,
        rows: rows_from_records(&records),
        metrics: metrics_from_records(&records, &sessions, filter),
        efficiency: None,
        include_in_combined,
    }
}

fn rows_from_records(records: &[&UsageRecord]) -> Vec<UsageRow> {
    let mut rows = BTreeMap::<String, RowFold>::new();
    for record in records {
        rows.entry(record.model.clone())
            .or_default()
            .record_record(record);
    }
    finish_rows(rows)
}

fn metrics_from_records(
    records: &[&UsageRecord],
    sessions: &[&SessionRecord],
    filter: &TimeFilter,
) -> UsageMetrics {
    let mut metrics = UsageMetrics::default();
    let mut session_ids = BTreeSet::new();
    let mut grouped_records = BTreeMap::<String, (Option<i64>, Option<i64>)>::new();

    for session in sessions {
        session_ids.insert((session.harness, session.session_id.clone()));
        let (started_at_ms, ended_at_ms) =
            clamp_range_to_filter(session.started_at_ms, session.ended_at_ms, filter);
        metrics.first_ms = merge_min(metrics.first_ms, started_at_ms);
        metrics.last_ms = merge_max(metrics.last_ms, ended_at_ms.or(started_at_ms));
        if let Some(day) = local_day_key(started_at_ms.or(ended_at_ms)) {
            metrics.active_day_keys.insert(day);
        }
        if let Some(cwd) = &session.cwd {
            metrics.workspace_keys.insert(cwd.clone());
        }
        if let (Some(start), Some(end)) = (started_at_ms, ended_at_ms) {
            metrics.wall_ms += end.saturating_sub(start) as u64;
        }
        // `session.messages` is a whole-session total that cannot be sliced to a sub-window (we keep
        // only the session's min/max timestamp, not per-message times), so attribute it only when the
        // session lies entirely within the active window. With no filter every session qualifies, so
        // this is a no-op on the common `all time` path.
        if filter.fully_contains(session.started_at_ms, session.ended_at_ms) {
            metrics.messages += session.messages;
        }
    }

    for record in records {
        session_ids.insert((record.harness, record.session_id.clone()));
        metrics.first_ms = merge_min(metrics.first_ms, record.started_at_ms);
        metrics.last_ms = merge_max(metrics.last_ms, record.ended_at_ms.or(record.started_at_ms));
        if let Some(day) = local_day_key(record.started_at_ms.or(record.ended_at_ms)) {
            metrics.active_day_keys.insert(day);
        }
        sum_usage(&mut metrics.usage, &record.usage);
        if let Some(cost) = record.cost {
            metrics.cost_usd += cost.usd;
        }
        if record.cost_status.is_unpriced() {
            metrics.unpriced_records += 1;
        }
        grouped_records
            .entry(record.session_id.clone())
            .and_modify(|(start, end)| {
                *start = merge_min(*start, record.started_at_ms);
                *end = merge_max(*end, record.ended_at_ms.or(record.started_at_ms));
            })
            .or_insert((
                record.started_at_ms,
                record.ended_at_ms.or(record.started_at_ms),
            ));
    }

    if metrics.wall_ms == 0 {
        for (start, end) in grouped_records.values() {
            if let (Some(start), Some(end)) = (start, end) {
                metrics.wall_ms += end.saturating_sub(*start) as u64;
            }
        }
    }

    metrics.sessions = session_ids.len() as u64;
    // Calls are counted from the window-filtered records (one record per usage-bearing call), so the
    // call count stays consistent with the token/cost totals under `--since`/`--until`. Using a
    // session's stored whole-session `calls` would over-report against a sub-window. With no filter
    // this equals the sum of session call counts, so the `all time` output is unchanged.
    metrics.calls = records.len() as u64;
    metrics.recompute_derived();
    metrics
}

fn finish_rows(rows: BTreeMap<String, RowFold>) -> Vec<UsageRow> {
    let mut rows: Vec<UsageRow> = rows
        .into_iter()
        .map(|(model, fold)| fold.into_row(model))
        .collect();
    sort_rows(&mut rows);
    rows
}

fn sort_rows(rows: &mut [UsageRow]) {
    rows.sort_by(|a, b| {
        let ac = a.cost.map(|c| c.usd).unwrap_or(0.0);
        let bc = b.cost.map(|c| c.usd).unwrap_or(0.0);
        bc.total_cmp(&ac)
            .then_with(|| b.usage.total().cmp(&a.usage.total()))
            .then_with(|| b.calls.cmp(&a.calls))
            .then_with(|| a.model.cmp(&b.model))
    });
}

fn render_human(reports: &[HarnessReport], filter: &TimeFilter) -> String {
    let mut out = String::new();
    let visible: Vec<&HarnessReport> = reports
        .iter()
        .filter(|r| r.has_rows() || r.note.is_some())
        .collect();
    if visible.is_empty() {
        out.push_str("no usage data found\n");
        return out;
    }

    out.push_str(&format!(
        "{}\n\n",
        style::dim(&format!("period: {}", filter.label))
    ));
    let summary = summary_rows(reports);
    if !summary.is_empty() {
        render_summary(&mut out, &summary);
        out.push('\n');
    }
    for (idx, report) in visible.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        render_harness(&mut out, report);
    }

    let combined = combined_rows(reports);
    if reports.iter().filter(|r| r.has_rows()).count() > 1 && !combined.is_empty() {
        out.push('\n');
        out.push_str(&format!("{}\n", style::bold("combined by model")));
        render_metrics(&mut out, &combined_metrics(reports));
        render_table(&mut out, &combined);
    }
    out
}

fn render_harness(out: &mut String, report: &HarnessReport) {
    let mut header = format!("{} {}", style::cyan("◆"), style::bold(report.kind.label()));
    if let Some(source) = &report.source {
        header.push_str(&format!(" {}", style::dim(&source.display().to_string())));
    }
    out.push_str(&header);
    out.push('\n');

    if let Some(note) = &report.note {
        out.push_str(&format!("  {}\n", style::dim(note)));
    }

    for (idx, section) in report.sections.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!("  {}\n", style::bold(&section.title)));
        render_metrics(out, &section.metrics);
        render_table(out, &section.rows);
        if let Some(efficiency) = &section.efficiency {
            out.push_str(&format!("  {}\n", style::dim(efficiency)));
        }
    }

    if report.scanned > 0 || report.skipped > 0 {
        let mut bits = Vec::new();
        if report.scanned > 0 {
            bits.push(format!(
                "{} item{}",
                report.scanned,
                plural(report.scanned as u64)
            ));
        }
        if report.skipped > 0 {
            bits.push(format!("{} skipped", report.skipped));
        }
        out.push_str(&format!("  {}\n", style::dim(&bits.join(" · "))));
    }
}

fn render_summary(out: &mut String, rows: &[SummaryRow]) {
    out.push_str(&format!("{}\n", style::bold("summary")));
    out.push_str(&style::dim(&format!(
        "  {:<16} {:>9} {:>8} {:>12} {:>7} {:>13} {:>9}\n",
        "scope", "sessions", "calls", "tokens", "cache", "cost", "unpriced"
    )));
    for row in rows {
        out.push_str(&format!(
            "  {:<16} {:>9} {:>8} {:>12} {:>7} {:>13} {:>9}\n",
            truncate_cell(&row.label, 16),
            row.metrics.sessions,
            row.metrics.calls,
            style::fmt_tokens(row.metrics.usage.total()),
            format!("{:.0}%", cache_read_share(&row.metrics.usage) * 100.0),
            format!("${:.4}", row.metrics.cost_usd),
            summary_unpriced(row.metrics.unpriced_records),
        ));
    }
}

fn render_metrics(out: &mut String, metrics: &UsageMetrics) {
    let mut bits = Vec::new();
    if let (Some(first), Some(last)) = (metrics.first_ms, metrics.last_ms) {
        bits.push(format!("{}..{}", fmt_ts_short(first), fmt_ts_short(last)));
        if metrics.covered_days > 0 {
            bits.push(format!("{}d covered", metrics.covered_days));
        }
    }
    if metrics.sessions > 0 {
        bits.push(format!(
            "{} session{}",
            metrics.sessions,
            plural(metrics.sessions)
        ));
    }
    if metrics.active_days > 0 {
        bits.push(format!("{} active d", metrics.active_days));
        bits.push(format!("{:.1}/d", metrics.sessions_per_day));
    }
    if metrics.workspaces > 0 {
        bits.push(format!(
            "{} workspace{}",
            metrics.workspaces,
            plural(metrics.workspaces)
        ));
    }
    if metrics.wall_ms > 0 {
        bits.push(format!("wall {}", fmt_duration_ms(metrics.wall_ms)));
    }
    if metrics.calls > 0 {
        bits.push(format!("{} call{}", metrics.calls, plural(metrics.calls)));
    }
    if metrics.messages > 0 {
        bits.push(format!("{} msg", metrics.messages));
    }
    if metrics.usage.total() > 0 {
        bits.push(format!("{} tok", style::fmt_tokens(metrics.usage.total())));
        bits.push(format!(
            "cache {:.0}%",
            cache_read_share(&metrics.usage) * 100.0
        ));
    }
    if metrics.cost_usd > 0.0 {
        bits.push(format!("cost ${:.4}", metrics.cost_usd));
    }
    if metrics.unpriced_records > 0 {
        bits.push(format!("{} unpriced", metrics.unpriced_records));
    }
    if bits.is_empty() {
        bits.push("no usage in period".to_string());
    }
    out.push_str(&format!("  {}\n", style::dim(&bits.join(" · "))));
}

fn render_table(out: &mut String, rows: &[UsageRow]) {
    if rows.is_empty() {
        out.push_str(&format!("  {}\n", style::dim("(no usage recorded)")));
        return;
    }
    let model_width = rows
        .iter()
        .map(|r| r.model.chars().count())
        .max()
        .unwrap_or(28)
        .clamp(28, 48);
    let header = format!(
        "  {:<model_width$} {:>6} {:>9} {:>9} {:>11} {:>12} {:>10} {:>18}\n",
        "model",
        "calls",
        "ctx",
        "out",
        "cache read",
        "cache write",
        "reason",
        "cost",
        model_width = model_width,
    );
    out.push_str(&style::dim(&header));
    for row in rows {
        out.push_str(&format!(
            "  {:<model_width$} {:>6} {:>9} {:>9} {:>11} {:>12} {:>10} {:>18}\n",
            truncate_cell(&row.model, model_width),
            row.calls,
            style::fmt_tokens(row.usage.context_tokens()),
            style::fmt_tokens(row.usage.output_tokens),
            token_or_dash(row.usage.cache_read_input_tokens),
            token_or_dash(row.usage.cache_creation_input_tokens),
            token_or_dash(row.usage.reasoning_tokens),
            cost_cell(row),
            model_width = model_width,
        ));
    }
}

fn summary_rows(reports: &[HarnessReport]) -> Vec<SummaryRow> {
    let mut rows = Vec::new();
    for report in reports {
        let mut metrics = UsageMetrics::default();
        for section in &report.sections {
            if section.include_in_combined {
                metrics.merge(&section.metrics);
            }
        }
        if metrics.sessions > 0 || metrics.calls > 0 || metrics.usage.total() > 0 {
            rows.push(SummaryRow {
                label: report.kind.label().to_string(),
                metrics,
                total: false,
            });
        }
    }
    if rows.is_empty() {
        return rows;
    }
    rows.push(SummaryRow {
        label: "TOTAL".to_string(),
        metrics: combined_metrics(reports),
        total: true,
    });
    rows
}

fn combined_rows(reports: &[HarnessReport]) -> Vec<UsageRow> {
    let mut rows = BTreeMap::<String, RowFold>::new();
    for report in reports {
        for section in &report.sections {
            if !section.include_in_combined {
                continue;
            }
            for row in &section.rows {
                rows.entry(row.model.clone()).or_default().record_row(row);
            }
        }
    }
    finish_rows(rows)
}

fn combined_metrics(reports: &[HarnessReport]) -> UsageMetrics {
    let mut metrics = UsageMetrics::default();
    for report in reports {
        for section in &report.sections {
            if section.include_in_combined {
                metrics.merge(&section.metrics);
            }
        }
    }
    metrics
}

fn report_json(reports: &[HarnessReport], filter: &TimeFilter) -> Value {
    json!({
        "period": {
            "label": filter.label,
            "since_ms": filter.since_ms,
            "until_ms": filter.until_ms,
        },
        "harnesses": reports.iter().map(harness_json).collect::<Vec<_>>(),
        "summary": summary_json(&summary_rows(reports)),
        "combined": {
            "metrics": metrics_json(&combined_metrics(reports)),
            "rows": combined_rows(reports).iter().map(row_json).collect::<Vec<_>>(),
        },
    })
}

fn harness_json(report: &HarnessReport) -> Value {
    json!({
        "id": report.kind.id(),
        "label": report.kind.label(),
        "source": report.source.as_ref().map(|p| p.display().to_string()),
        "note": report.note,
        "scanned": report.scanned,
        "skipped": report.skipped,
        "sections": report.sections.iter().map(|s| {
            json!({
                "title": s.title,
                "include_in_combined": s.include_in_combined,
                "metrics": metrics_json(&s.metrics),
                "rows": s.rows.iter().map(row_json).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

fn summary_json(rows: &[SummaryRow]) -> Value {
    let mut harnesses = Vec::new();
    let mut total = None;
    for row in rows {
        let value = json!({
            "label": row.label,
            "metrics": metrics_json(&row.metrics),
            "tokens_total": row.metrics.usage.total(),
            "cache_rate": cache_read_share(&row.metrics.usage),
            "cost_usd": row.metrics.cost_usd,
            "sessions": row.metrics.sessions,
            "calls": row.metrics.calls,
            "unpriced_records": row.metrics.unpriced_records,
        });
        if row.total {
            total = Some(value);
        } else {
            harnesses.push(value);
        }
    }
    json!({
        "harnesses": harnesses,
        "total": total,
    })
}

fn metrics_json(metrics: &UsageMetrics) -> Value {
    json!({
        "first_ms": metrics.first_ms,
        "last_ms": metrics.last_ms,
        "sessions": metrics.sessions,
        "active_days": metrics.active_days,
        "workspaces": metrics.workspaces,
        "covered_days": metrics.covered_days,
        "sessions_per_day": metrics.sessions_per_day,
        "wall_ms": metrics.wall_ms,
        "calls": metrics.calls,
        "messages": metrics.messages,
        "usage": UsageJson::from(&metrics.usage),
        "cost_usd": metrics.cost_usd,
        "unpriced_records": metrics.unpriced_records,
    })
}

fn row_json(row: &UsageRow) -> Value {
    json!({
        "model": row.model,
        "calls": row.calls,
        "usage": UsageJson::from(&row.usage),
        "cost": row.cost.map(CostJson::from),
        "unpriced": row.unpriced.iter().map(|(status, count)| {
            json!({ "status": status.as_str(), "calls": count })
        }).collect::<Vec<_>>(),
    })
}

#[derive(Serialize)]
struct UsageJson {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
    reasoning: u64,
    audio_input: u64,
    audio_output: u64,
    context: u64,
    total: u64,
    reported_cost_usd: Option<f64>,
}

impl From<&Usage> for UsageJson {
    fn from(u: &Usage) -> Self {
        Self {
            input: u.input_tokens,
            output: u.output_tokens,
            cache_read: u.cache_read_input_tokens,
            cache_creation: u.cache_creation_input_tokens,
            reasoning: u.reasoning_tokens,
            audio_input: u.audio_input_tokens,
            audio_output: u.audio_output_tokens,
            context: u.context_tokens(),
            total: u.total(),
            reported_cost_usd: u.reported_cost_usd,
        }
    }
}

#[derive(Serialize)]
struct CostJson {
    usd: f64,
    subscription: bool,
    source: &'static str,
    status: &'static str,
}

impl From<CostCell> for CostJson {
    fn from(cost: CostCell) -> Self {
        Self {
            usd: cost.usd,
            subscription: cost.subscription,
            source: match cost.source {
                CostSourceCell::Reported => "reported",
                CostSourceCell::Estimated => "estimated",
            },
            status: cost.status.as_str(),
        }
    }
}

fn usage_record(
    harness: HarnessKind,
    session_id: String,
    model: String,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    usage: Usage,
    pricing: &PricingTable,
) -> UsageRecord {
    let (cost, cost_status) = price_usage(&usage, &model, pricing);
    UsageRecord {
        harness,
        session_id,
        model,
        started_at_ms,
        ended_at_ms,
        usage,
        cost,
        cost_status,
    }
}

fn price_usage(
    usage: &Usage,
    model: &str,
    pricing: &PricingTable,
) -> (Option<CostCell>, CostStatus) {
    if usage_is_empty(usage) {
        return (None, CostStatus::UnpricedMissingUsage);
    }
    match pricing.cost(usage, model) {
        Some(money) => {
            let source = match money.source {
                CostSource::Reported => CostSourceCell::Reported,
                CostSource::Estimated => CostSourceCell::Estimated,
            };
            let status = match money.source {
                CostSource::Reported => CostStatus::Reported,
                CostSource::Estimated if money.subscription => CostStatus::SubscriptionEquivalent,
                CostSource::Estimated => CostStatus::EstimatedTable,
            };
            (
                Some(CostCell {
                    usd: money.usd,
                    subscription: money.subscription,
                    source,
                    status,
                }),
                status,
            )
        }
        None => (None, CostStatus::UnpricedUnknownModel),
    }
}

fn usage_from_anthropic(v: &Value) -> Usage {
    Usage {
        input_tokens: u64_path(v, &["input_tokens"]),
        output_tokens: u64_path(v, &["output_tokens"]),
        cache_creation_input_tokens: u64_path(v, &["cache_creation_input_tokens"]),
        cache_read_input_tokens: u64_path(v, &["cache_read_input_tokens"]),
        reasoning_tokens: u64_path(v, &["reasoning_tokens"]),
        ..Default::default()
    }
}

fn usage_from_codex_token_count(v: &Value) -> Usage {
    let input = u64_path(v, &["input_tokens"]);
    let cached = u64_path(v, &["cached_input_tokens"]);
    Usage {
        input_tokens: input.saturating_sub(cached),
        output_tokens: u64_path(v, &["output_tokens"]),
        cache_read_input_tokens: cached,
        reasoning_tokens: u64_path(v, &["reasoning_output_tokens"]),
        ..Default::default()
    }
}

fn format_efficiency(e: &flux_events::EfficiencySummary) -> String {
    let phases = if e.has_phase_rounds() {
        format!(
            " · gather {:.1}/turn · revise {:.1}/turn",
            e.avg_gather_rounds_per_turn(),
            e.avg_revise_rounds_per_turn(),
        )
    } else {
        String::new()
    };
    format!(
        "efficiency: {} turn{} · {:.1} calls/turn · {:.1} iters/turn · {:.1} plans/turn{} · cache-read {:.0}% · uncached-in {}/turn · out {}/turn",
        e.turns,
        if e.turns == 1 { "" } else { "s" },
        e.avg_calls_per_turn(),
        e.avg_iterations_per_turn(),
        e.avg_plans_per_turn(),
        phases,
        e.cache_read_share() * 100.0,
        style::fmt_tokens(e.uncached_input_per_turn() as u64),
        style::fmt_tokens(e.output_per_turn() as u64),
    )
}

fn sum_usage(acc: &mut Usage, usage: &Usage) {
    acc.input_tokens += usage.input_tokens;
    acc.output_tokens += usage.output_tokens;
    acc.cache_creation_input_tokens += usage.cache_creation_input_tokens;
    acc.cache_read_input_tokens += usage.cache_read_input_tokens;
    acc.reasoning_tokens += usage.reasoning_tokens;
    acc.audio_input_tokens += usage.audio_input_tokens;
    acc.audio_output_tokens += usage.audio_output_tokens;
    if let Some(cost) = usage.reported_cost_usd {
        *acc.reported_cost_usd.get_or_insert(0.0) += cost;
    }
}

fn usage_is_empty(usage: &Usage) -> bool {
    usage.total() == 0 && usage.reasoning_tokens == 0 && usage.reported_cost_usd.is_none()
}

fn u64_path(v: &Value, path: &[&str]) -> u64 {
    let mut cur = v;
    for key in path {
        let Some(next) = cur.get(*key) else {
            return 0;
        };
        cur = next;
    }
    cur.as_u64()
        .or_else(|| cur.as_i64().and_then(|n| u64::try_from(n).ok()))
        .unwrap_or(0)
}

fn f64_path(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_f64()
}

fn json_timestamp_ms(v: &Value) -> Option<i64> {
    v.get("timestamp")
        .and_then(json_value_ms)
        .or_else(|| json_nested_timestamp_ms(v, &["message", "timestamp"]))
}

fn json_nested_timestamp_ms(v: &Value, path: &[&str]) -> Option<i64> {
    let mut cur = v;
    for key in path {
        cur = cur.get(*key)?;
    }
    json_value_ms(cur)
}

fn json_value_ms(v: &Value) -> Option<i64> {
    if let Some(s) = v.as_str() {
        return parse_rfc3339_ms(s).ok();
    }
    v.as_i64().map(normalize_epoch_ms)
}

fn normalize_epoch_ms(n: i64) -> i64 {
    if n.abs() < 10_000_000_000 {
        n.saturating_mul(1000)
    } else {
        n
    }
}

/// `pub(crate)`: reused by `flux sessions --since` (C-164), which accepts the same
/// YYYY-MM-DD/RFC3339/duration forms as `flux usage --since`.
pub(crate) fn parse_since_ms(s: &str, now_ms: i64) -> Result<i64> {
    if let Some(duration) = parse_duration_ms_if_duration(s)? {
        return Ok(now_ms.saturating_sub(duration));
    }
    parse_bound_ms(s, false)
}

/// `pub(crate)`: reused by `flux sessions --until` (C-164).
pub(crate) fn parse_until_ms(s: &str) -> Result<i64> {
    parse_bound_ms(s, true)
}

fn parse_bound_ms(s: &str, until: bool) -> Result<i64> {
    if let Ok(ms) = parse_rfc3339_ms(s) {
        return Ok(ms);
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let date = if until {
            date.checked_add_days(Days::new(1))
                .context("date overflow")?
        } else {
            date
        };
        let local = Local
            .from_local_datetime(
                &date
                    .and_hms_opt(0, 0, 0)
                    .context("invalid local midnight")?,
            )
            .earliest()
            .context("local date does not exist in this timezone")?;
        return Ok(local.timestamp_millis());
    }
    bail!("invalid time bound `{s}`; expected YYYY-MM-DD, RFC3339, or duration for --since/--last")
}

fn parse_duration_ms(s: &str) -> Result<i64> {
    parse_duration_ms_if_duration(s)?.with_context(|| {
        format!("invalid duration `{s}`; expected a number plus h, d, or w (for example 24h)")
    })
}

fn parse_duration_ms_if_duration(s: &str) -> Result<Option<i64>> {
    let Some(unit) = s.chars().last() else {
        return Ok(None);
    };
    let multiplier = match unit {
        'h' => Duration::hours(1),
        'd' => Duration::days(1),
        'w' => Duration::weeks(1),
        _ => return Ok(None),
    };
    let n: i64 = s[..s.len() - unit.len_utf8()]
        .parse()
        .with_context(|| format!("invalid duration `{s}`"))?;
    if n <= 0 {
        bail!("duration must be positive");
    }
    let n = i32::try_from(n).with_context(|| format!("duration `{s}` is too large"))?;
    multiplier
        .checked_mul(n)
        .map(|d| Some(d.num_milliseconds()))
        .with_context(|| format!("duration `{s}` is too large"))
}

fn parse_rfc3339_ms(s: &str) -> Result<i64> {
    Ok(DateTime::parse_from_rfc3339(s)?.timestamp_millis())
}

/// `pub(crate)`: reused by `flux sessions --since/--until` (C-164) to resolve `now` the same way.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn fmt_ts(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn fmt_ts_short(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn local_day(ms: i64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map(|dt| dt.date_naive())
}

fn local_day_key(ms: Option<i64>) -> Option<String> {
    local_day(ms?).map(|day| day.to_string())
}

fn fmt_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => {
            let h = s / 3_600;
            let m = (s % 3_600) / 60;
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h{m}m")
            }
        }
        s => {
            let d = s / 86_400;
            let h = (s % 86_400) / 3_600;
            if h == 0 {
                format!("{d}d")
            } else {
                format!("{d}d{h}h")
            }
        }
    }
}

fn token_or_dash(n: u64) -> String {
    if n == 0 {
        "—".to_string()
    } else {
        style::fmt_tokens(n)
    }
}

fn cost_cell(row: &UsageRow) -> String {
    if let Some(cost) = row.cost {
        let dollars = format!("${:.4}", cost.usd);
        if cost.usd == 0.0 && cost.source == CostSourceCell::Reported {
            return "$0 rpt".to_string();
        }
        if cost.subscription {
            format!("~{dollars} sub")
        } else if cost.source == CostSourceCell::Reported {
            format!("{dollars} rpt")
        } else {
            dollars
        }
    } else if let Some((status, _)) = row.unpriced.iter().next() {
        format!("$? {}", status.short_reason())
    } else if flux_core::is_metered_cloud_spec(&row.model) {
        "$? unknown model".to_string()
    } else {
        "—".to_string()
    }
}

fn cache_read_share(usage: &Usage) -> f64 {
    let prompt =
        usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
    if prompt == 0 {
        0.0
    } else {
        usage.cache_read_input_tokens as f64 / prompt as f64
    }
}

fn summary_unpriced(n: u64) -> String {
    if n == 0 {
        "—".to_string()
    } else {
        n.to_string()
    }
}

fn truncate_cell(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut s: String = text.chars().take(width - 1).collect();
    s.push('…');
    s
}

fn progress_bar(current: usize, total: usize) -> String {
    let width = 18usize;
    if total == 0 {
        return format!("[{}]", "-".repeat(width));
    }
    let filled = ((current.min(total) * width) / total).min(width);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
}

fn prefixed_model(provider: &str, model: &str) -> String {
    if model.starts_with(&format!("{provider}/")) {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn clamp_range_to_filter(
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    filter: &TimeFilter,
) -> (Option<i64>, Option<i64>) {
    let started_at_ms = started_at_ms.map(|ts| match filter.since_ms {
        Some(since) => ts.max(since),
        None => ts,
    });
    let ended_at_ms = ended_at_ms.map(|ts| match filter.until_ms {
        Some(until) => ts.min(until),
        None => ts,
    });
    (started_at_ms, ended_at_ms)
}

fn merge_min(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn merge_max(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Collect `.jsonl` files under `root`, returning the files plus the count of unreadable entries
/// skipped along the way. Only an unreadable root propagates as an error (it becomes the harness
/// note); below the root, unreadable subdirectories and entries get the same per-item tolerance as
/// bad lines and oversized files, so one permission-denied path cannot blank out the whole scan.
fn jsonl_files(root: &Path) -> Result<(Vec<PathBuf>, usize)> {
    let read = fs::read_dir(root).with_context(|| format!("read {}", root.display()))?;
    let mut out = Vec::new();
    let mut skipped = 0usize;
    collect_jsonl_files(read, &mut out, &mut skipped);
    out.sort();
    if out.len() > MAX_JSONL_FILES {
        out.truncate(MAX_JSONL_FILES);
    }
    Ok((out, skipped))
}

fn collect_jsonl_files(read: fs::ReadDir, out: &mut Vec<PathBuf>, skipped: &mut usize) {
    if out.len() >= MAX_JSONL_FILES {
        return;
    }
    let mut entries = Vec::new();
    for entry in read {
        match entry {
            Ok(entry) => entries.push(entry),
            Err(_) => *skipped += 1,
        }
    }
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let Ok(ty) = entry.file_type() else {
            *skipped += 1;
            continue;
        };
        if ty.is_dir() {
            match fs::read_dir(&path) {
                Ok(read) => collect_jsonl_files(read, out, skipped),
                Err(_) => *skipped += 1,
            }
        } else if ty.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
        if out.len() >= MAX_JSONL_FILES {
            break;
        }
    }
}

fn too_large(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.len() > MAX_JSONL_FILE_BYTES)
        .unwrap_or(false)
}

fn sqlite_table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_master where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

fn sqlite_column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_count_assistant_token_messages(conn: &rusqlite::Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "select count(*) from message \
         where json_extract(data, '$.role') = 'assistant' \
           and json_type(data, '$.tokens') is not null",
        [],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

fn flux_events_path() -> Option<PathBuf> {
    env_path("FLUX_HOME")
        .map(|p| p.join("events.db"))
        .or_else(|| home_dir().map(|h| h.join(".flux").join("events.db")))
}

fn harness_root(env_key: &str, home_child: &str) -> Option<PathBuf> {
    if !env_key.is_empty() {
        if let Some(path) = env_path(env_key) {
            return Some(path);
        }
    }
    home_dir().map(|h| h.join(home_child))
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    fn test_path(name: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("flux-usage-{name}-{}-{n}", std::process::id()))
    }

    fn quiet_progress() -> ProgressRenderer {
        ProgressRenderer {
            active: false,
            last_len: 0,
        }
    }

    #[test]
    fn parses_time_bounds_and_duration_windows() {
        let args = UsageArgs {
            since: Some("2026-07-08".to_string()),
            until: Some("2026-07-09".to_string()),
            ..Default::default()
        };
        let filter = TimeFilter::from_args(&args).unwrap();
        assert!(filter.since_ms.unwrap() < filter.until_ms.unwrap());

        assert_eq!(parse_duration_ms("24h").unwrap(), DAY_MS);
        assert_eq!(parse_duration_ms("7d").unwrap(), 7 * DAY_MS);
        assert!(parse_duration_ms("bad").is_err());

        // `parse_since_ms` resolves every accepted form to a concrete bound and rejects the rest.
        let now = 1_772_000_000_000;
        assert_eq!(parse_since_ms("24h", now).unwrap(), now - DAY_MS);
        assert_eq!(
            parse_since_ms("2026-07-08", now).unwrap(),
            parse_bound_ms("2026-07-08", false).unwrap()
        );
        assert!(parse_since_ms("not-a-time", now).is_err());
    }

    #[test]
    fn renderer_outputs_metrics_and_aligned_usage_table() {
        crate::style::init(crate::style::ColorChoice::Never);
        let record = usage_record(
            HarnessKind::Claude,
            "s".to_string(),
            "claude/claude-opus-4-8".to_string(),
            Some(1_772_000_000_000),
            Some(1_772_000_060_000),
            Usage {
                input_tokens: 1000,
                output_tokens: 200,
                cache_read_input_tokens: 3000,
                reasoning_tokens: 40,
                ..Default::default()
            },
            &PricingTable::builtin(),
        );
        let dataset = external_dataset(
            HarnessKind::Claude,
            PathBuf::from("/tmp/claude/projects"),
            vec![record],
            vec![SessionRecord {
                harness: HarnessKind::Claude,
                session_id: "s".to_string(),
                started_at_ms: Some(1_772_000_000_000),
                ended_at_ms: Some(1_772_000_060_000),
                cwd: None,
                messages: 2,
            }],
            1,
            0,
        );
        let filter = TimeFilter {
            since_ms: None,
            until_ms: None,
            label: "all time".to_string(),
        };
        let out = render_human(&[report_from_dataset(dataset, &filter)], &filter);
        assert!(out.contains("period: all time"));
        assert!(out.contains("summary"));
        assert!(out.contains("TOTAL"));
        assert!(out.contains("Claude Code"));
        assert!(out.contains("session"));
        assert!(out.contains("wall 1m"));
        assert!(out.contains("cache read"));
        assert!(out.contains("~$0."));
    }

    #[test]
    fn summary_rows_use_weighted_cache_rate_and_absolute_total() {
        let flux = HarnessReport {
            kind: HarnessKind::Flux,
            source: None,
            note: None,
            scanned: 0,
            skipped: 0,
            sections: vec![UsageSection {
                title: "all sessions".to_string(),
                rows: Vec::new(),
                metrics: UsageMetrics {
                    sessions: 1,
                    calls: 2,
                    usage: Usage {
                        input_tokens: 900,
                        cache_read_input_tokens: 100,
                        output_tokens: 50,
                        ..Default::default()
                    },
                    cost_usd: 1.0,
                    ..Default::default()
                },
                efficiency: None,
                include_in_combined: true,
            }],
        };
        let codex = HarnessReport {
            kind: HarnessKind::Codex,
            source: None,
            note: None,
            scanned: 0,
            skipped: 0,
            sections: vec![UsageSection {
                title: "all sessions".to_string(),
                rows: Vec::new(),
                metrics: UsageMetrics {
                    sessions: 3,
                    calls: 4,
                    usage: Usage {
                        cache_read_input_tokens: 9_000,
                        output_tokens: 10,
                        ..Default::default()
                    },
                    cost_usd: 2.5,
                    unpriced_records: 1,
                    ..Default::default()
                },
                efficiency: None,
                include_in_combined: true,
            }],
        };

        let rows = summary_rows(&[flux, codex]);
        assert_eq!(rows.len(), 3);
        let total = rows.last().unwrap();
        assert!(total.total);
        assert_eq!(total.metrics.sessions, 4);
        assert_eq!(total.metrics.calls, 6);
        assert_eq!(total.metrics.usage.total(), 10_060);
        assert!((cache_read_share(&total.metrics.usage) - 0.91).abs() < 1e-9);
        assert!((total.metrics.cost_usd - 3.5).abs() < 1e-9);
        assert_eq!(total.metrics.unpriced_records, 1);
    }

    #[test]
    fn json_output_includes_summary_harnesses_and_total() {
        let report = HarnessReport {
            kind: HarnessKind::Opencode,
            source: None,
            note: None,
            scanned: 0,
            skipped: 0,
            sections: vec![UsageSection {
                title: "all sessions".to_string(),
                rows: Vec::new(),
                metrics: UsageMetrics {
                    sessions: 2,
                    calls: 5,
                    usage: Usage {
                        input_tokens: 10,
                        cache_read_input_tokens: 30,
                        output_tokens: 7,
                        ..Default::default()
                    },
                    cost_usd: 0.42,
                    unpriced_records: 1,
                    ..Default::default()
                },
                efficiency: None,
                include_in_combined: true,
            }],
        };
        let filter = TimeFilter {
            since_ms: None,
            until_ms: None,
            label: "all time".to_string(),
        };

        let value = report_json(&[report], &filter);
        assert_eq!(value["summary"]["harnesses"][0]["label"], "opencode");
        assert_eq!(value["summary"]["harnesses"][0]["tokens_total"], 47);
        assert_eq!(value["summary"]["total"]["sessions"], 2);
        assert_eq!(value["summary"]["total"]["calls"], 5);
        assert_eq!(value["summary"]["total"]["unpriced_records"], 1);
        assert!((value["summary"]["total"]["cost_usd"].as_f64().unwrap() - 0.42).abs() < 1e-9);
    }

    #[test]
    fn combined_model_table_title_distinguishes_it_from_absolute_summary() {
        crate::style::init(crate::style::ColorChoice::Never);
        let make_report = |kind, model: &str| HarnessReport {
            kind,
            source: None,
            note: None,
            scanned: 0,
            skipped: 0,
            sections: vec![UsageSection {
                title: "all sessions".to_string(),
                rows: vec![UsageRow {
                    model: model.to_string(),
                    calls: 1,
                    usage: Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        ..Default::default()
                    },
                    cost: None,
                    unpriced: BTreeMap::from([(CostStatus::UnpricedUnknownModel, 1)]),
                }],
                metrics: UsageMetrics {
                    sessions: 1,
                    calls: 1,
                    usage: Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        ..Default::default()
                    },
                    unpriced_records: 1,
                    ..Default::default()
                },
                efficiency: None,
                include_in_combined: true,
            }],
        };
        let filter = TimeFilter {
            since_ms: None,
            until_ms: None,
            label: "all time".to_string(),
        };

        let out = render_human(
            &[
                make_report(HarnessKind::Flux, "mock-a"),
                make_report(HarnessKind::Codex, "mock-b"),
            ],
            &filter,
        );
        assert!(out.contains("combined by model"));
        assert!(!out.contains("combined total"));
    }

    #[test]
    fn merge_unions_active_days_and_workspaces_across_harnesses() {
        // A calendar day / workspace active in two harnesses must be counted ONCE in the combined
        // total. Summing the per-harness cardinalities (the old bug) would report 3 days / 3
        // workspaces here instead of the true 2.
        let mut a = UsageMetrics {
            sessions: 1,
            active_day_keys: BTreeSet::from(["2026-07-08".to_string()]),
            workspace_keys: BTreeSet::from(["/w/one".to_string()]),
            ..Default::default()
        };
        a.recompute_derived();
        let mut b = UsageMetrics {
            sessions: 1,
            active_day_keys: BTreeSet::from(["2026-07-08".to_string(), "2026-07-09".to_string()]),
            workspace_keys: BTreeSet::from(["/w/one".to_string(), "/w/two".to_string()]),
            ..Default::default()
        };
        b.recompute_derived();
        a.merge(&b);
        assert_eq!(a.active_days, 2, "the shared day must be counted once");
        assert_eq!(a.workspaces, 2, "the shared workspace must be counted once");
        assert_eq!(a.sessions, 2, "distinct sessions still sum");
    }

    #[test]
    fn covered_days_uses_calendar_days_and_never_undercounts_active_days() {
        // Records at 23:59 and 00:01 the next local day span two calendar days. The old
        // elapsed-time quotient reported 1 covered day beside 2 active days.
        let first = Local
            .with_ymd_and_hms(2026, 7, 8, 23, 59, 0)
            .earliest()
            .unwrap()
            .timestamp_millis();
        let last = Local
            .with_ymd_and_hms(2026, 7, 9, 0, 1, 0)
            .earliest()
            .unwrap()
            .timestamp_millis();
        let mut metrics = UsageMetrics {
            first_ms: Some(first),
            last_ms: Some(last),
            active_day_keys: BTreeSet::from([
                local_day_key(Some(first)).unwrap(),
                local_day_key(Some(last)).unwrap(),
            ]),
            ..Default::default()
        };
        metrics.recompute_derived();
        assert_eq!(metrics.covered_days, 2);
        assert!(metrics.covered_days >= metrics.active_days);

        // A single instant still covers exactly the one calendar day it falls on.
        metrics.last_ms = Some(first);
        metrics.recompute_derived();
        assert_eq!(metrics.covered_days, 1);
    }

    #[test]
    fn flux_efficiency_attaches_only_on_the_unbounded_window() {
        // The efficiency projection is a whole-history aggregate: it is surfaced on `all time` but
        // withheld under a `--since`/`--until` window so it is never shown beside filtered metrics.
        let dataset = || HarnessDataset {
            kind: HarnessKind::Flux,
            source: None,
            note: None,
            latest_session: None,
            records: Vec::new(),
            sessions: Vec::new(),
            latest_efficiency: None,
            all_efficiency: Some("efficiency: 3 turns".to_string()),
            scanned: 0,
            skipped: 0,
        };

        let unbounded = TimeFilter {
            since_ms: None,
            until_ms: None,
            label: "all time".to_string(),
        };
        let report = report_from_dataset(dataset(), &unbounded);
        let all = report
            .sections
            .iter()
            .find(|s| s.include_in_combined)
            .unwrap();
        assert_eq!(all.efficiency.as_deref(), Some("efficiency: 3 turns"));

        let bounded = TimeFilter {
            since_ms: Some(1),
            until_ms: None,
            label: "since 1".to_string(),
        };
        let report = report_from_dataset(dataset(), &bounded);
        let all = report
            .sections
            .iter()
            .find(|s| s.include_in_combined)
            .unwrap();
        assert_eq!(all.efficiency, None, "efficiency is hidden under a filter");
    }

    #[test]
    fn claude_jsonl_dedupes_split_assistant_messages() {
        let root = test_path("claude");
        let project = root.join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let file = project.join("s.jsonl");
        let line = r#"{"type":"assistant","timestamp":"2026-07-08T12:00:00Z","message":{"id":"msg_1","model":"claude-opus-4-8","usage":{"input_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2,"output_tokens":3}},"sessionId":"s"}"#;
        fs::write(&file, format!("{line}\n{line}\n")).unwrap();

        let mut progress = quiet_progress();
        let (records, sessions, scanned, skipped) = parse_claude_projects(
            &root.join("projects"),
            &PricingTable::builtin(),
            &mut progress,
        )
        .unwrap();
        assert_eq!(scanned, 1);
        assert_eq!(skipped, 0);
        assert_eq!(records.len(), 1);
        assert_eq!(sessions.len(), 1);
        assert_eq!(records[0].model, "claude/claude-opus-4-8");
        assert_eq!(records[0].usage.input_tokens, 10);
        assert_eq!(records[0].cost_status, CostStatus::SubscriptionEquivalent);
        assert!(records[0].started_at_ms.is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn claude_scan_skips_unreadable_files_and_dirs_and_keeps_the_rest() {
        use std::os::unix::fs::PermissionsExt;

        // A permission-denied file or subdirectory must be counted as skipped like a bad line, not
        // abort the scan and blank out every record already parsed from readable files.
        let root = test_path("claude-unreadable");
        let project = root.join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let line = r#"{"type":"assistant","timestamp":"2026-07-08T12:00:00Z","message":{"id":"msg_1","model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":3}},"sessionId":"s"}"#;
        fs::write(project.join("good.jsonl"), format!("{line}\n")).unwrap();
        let bad = project.join("locked.jsonl");
        fs::write(&bad, format!("{line}\n")).unwrap();
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();
        let locked_dir = project.join("locked-dir");
        fs::create_dir_all(&locked_dir).unwrap();
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&bad).is_ok() {
            // Running as root: permission bits cannot make paths unreadable, so the scenario is
            // untestable here.
            let _ = fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(root);
            return;
        }

        let mut progress = quiet_progress();
        let (records, sessions, scanned, skipped) = parse_claude_projects(
            &root.join("projects"),
            &PricingTable::builtin(),
            &mut progress,
        )
        .unwrap();
        assert_eq!(scanned, 2, "both jsonl files are listed");
        assert_eq!(skipped, 2, "the unreadable file and directory are skipped");
        assert_eq!(
            records.len(),
            1,
            "the readable file still yields its record"
        );
        assert_eq!(sessions.len(), 1);

        let _ = fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_jsonl_uses_incremental_token_count_rows() {
        let root = test_path("codex");
        let sessions = root.join("sessions").join("2026").join("07").join("08");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("rollout.jsonl"),
            r#"{"timestamp":"2026-07-08T12:00:00Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-08T12:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":12,"reasoning_output_tokens":3}}}}"#
                + "\n",
        )
        .unwrap();

        let mut progress = quiet_progress();
        let (records, sessions, scanned, skipped) = parse_codex_sessions(
            &root.join("sessions"),
            &PricingTable::builtin(),
            &mut progress,
        )
        .unwrap();
        assert_eq!(scanned, 1);
        assert_eq!(skipped, 0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, "codex/gpt-5.5");
        assert_eq!(records[0].usage.input_tokens, 60);
        assert_eq!(records[0].usage.cache_read_input_tokens, 40);
        assert_eq!(records[0].usage.output_tokens, 12);
        assert_eq!(records[0].usage.reasoning_tokens, 3);
        assert!(records[0].started_at_ms.is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn opencode_sqlite_reads_message_tokens_and_reported_cost() {
        let root = test_path("opencode");
        fs::create_dir_all(&root).unwrap();
        let db = root.join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "create table message (id text primary key, data text not null)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into message (id, data) values (?1, ?2)",
            (
                "m1",
                r#"{"role":"assistant","providerID":"openrouter","modelID":"z-ai/glm","tokens":{"input":7,"output":2,"reasoning":1,"cache":{"read":5,"write":3}},"cost":0.0042}"#,
            ),
        )
        .unwrap();
        drop(conn);

        let mut progress = quiet_progress();
        let (records, sessions, scanned, skipped) =
            parse_opencode_db(&db, &PricingTable::builtin(), &mut progress).unwrap();
        assert_eq!(scanned, 1);
        assert_eq!(skipped, 0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, "openrouter/z-ai/glm");
        assert_eq!(records[0].usage.cache_read_input_tokens, 5);
        assert_eq!(records[0].cost.unwrap().source, CostSourceCell::Reported);
        assert_eq!(records[0].cost_status, CostStatus::Reported);
        assert!((records[0].cost.unwrap().usd - 0.0042).abs() < 1e-9);

        let _ = fs::remove_dir_all(root);
    }
}
