//! Local usage dashboard backing `flux usage`.
//!
//! The flux-native data already lives in `flux-events`; the other harnesses keep local state in
//! JSONL/SQLite shapes. This module keeps those adapters read-only and folds them into one compact
//! report model so the renderer does not care where a row came from.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use flux_core::{CostSource, Money, PricingTable, Usage};
use flux_events::{EventStore, ModelCost};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
struct HarnessReport {
    kind: HarnessKind,
    source: Option<PathBuf>,
    note: Option<String>,
    sections: Vec<UsageSection>,
    scanned: usize,
    skipped: usize,
}

impl HarnessReport {
    fn missing(kind: HarnessKind, source: PathBuf) -> Self {
        Self {
            kind,
            source: Some(source),
            note: Some("not found".to_string()),
            sections: Vec::new(),
            scanned: 0,
            skipped: 0,
        }
    }

    fn warning(kind: HarnessKind, source: Option<PathBuf>, warning: impl Into<String>) -> Self {
        Self {
            kind,
            source,
            note: Some(warning.into()),
            sections: Vec::new(),
            scanned: 0,
            skipped: 0,
        }
    }

    fn has_rows(&self) -> bool {
        self.sections.iter().any(|s| !s.rows.is_empty())
    }
}

#[derive(Clone, Debug)]
struct UsageSection {
    title: String,
    rows: Vec<UsageRow>,
    efficiency: Option<String>,
    include_in_combined: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct UsageRow {
    model: String,
    calls: u64,
    usage: Usage,
    cost: Option<CostCell>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CostCell {
    usd: f64,
    subscription: bool,
    source: CostSourceCell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CostSourceCell {
    Reported,
    Estimated,
}

impl CostCell {
    fn from_money(money: Money) -> Self {
        Self {
            usd: money.usd,
            subscription: money.subscription,
            source: match money.source {
                CostSource::Reported => CostSourceCell::Reported,
                CostSource::Estimated => CostSourceCell::Estimated,
            },
        }
    }
}

struct RowFold {
    usage: Usage,
    calls: u64,
    cost_usd: f64,
    priced_calls: u64,
    subscription: bool,
    all_reported: bool,
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
        }
    }
}

impl RowFold {
    fn record_usage(&mut self, usage: &Usage, model: &str, pricing: &PricingTable) {
        self.calls += 1;
        sum_usage(&mut self.usage, usage);
        if let Some(cost) = pricing.cost(usage, model) {
            self.record_cost(cost);
        }
    }

    fn record_row(&mut self, row: &UsageRow) {
        self.calls += row.calls;
        sum_usage(&mut self.usage, &row.usage);
        if let Some(cost) = row.cost {
            self.cost_usd += cost.usd;
            self.priced_calls += row.calls.max(1);
            self.subscription = self.subscription || cost.subscription;
            self.all_reported = self.all_reported && cost.source == CostSourceCell::Reported;
        }
    }

    fn record_cost(&mut self, cost: Money) {
        self.cost_usd += cost.usd;
        self.priced_calls += 1;
        self.subscription = self.subscription || cost.subscription;
        self.all_reported = self.all_reported && cost.source == CostSource::Reported;
    }

    fn into_row(self, model: String) -> UsageRow {
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
            }),
        }
    }
}

/// Run the real `flux usage` command.
pub fn run_usage(args: UsageArgs, pricing: &PricingTable) -> Result<()> {
    let requested = requested_harnesses(&args);
    let explicit = args.no_external || !args.harness.is_empty();
    let mut reports = Vec::new();
    for kind in requested {
        let report = collect_harness(kind, pricing);
        if report.has_rows() || report.note.is_some() || explicit {
            reports.push(report);
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report_json(&reports))?);
    } else {
        print!("{}", render_human(&reports));
    }
    Ok(())
}

/// Testable flux-only body retained for the existing `flux usage` unit test seam.
#[cfg(test)]
pub fn run_usage_with(store: &EventStore, pricing: &PricingTable) -> Result<()> {
    let report = flux_report_from_store(store, pricing, None)?;
    print!("{}", render_human(&[report]));
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

fn collect_harness(kind: HarnessKind, pricing: &PricingTable) -> HarnessReport {
    match kind {
        HarnessKind::Flux => collect_flux(pricing),
        HarnessKind::Codex => collect_codex(pricing),
        HarnessKind::Claude => collect_claude(pricing),
        HarnessKind::Opencode => collect_opencode(pricing),
    }
}

fn collect_flux(pricing: &PricingTable) -> HarnessReport {
    let Some(path) = flux_events_path() else {
        return HarnessReport::warning(HarnessKind::Flux, None, "HOME is not set");
    };
    if !path.exists() {
        return HarnessReport::missing(HarnessKind::Flux, path);
    }
    match EventStore::open(&path)
        .with_context(|| format!("open {}", path.display()))
        .and_then(|store| flux_report_from_store(&store, pricing, Some(path.clone())))
    {
        Ok(report) => report,
        Err(e) => HarnessReport::warning(HarnessKind::Flux, Some(path), e.to_string()),
    }
}

fn flux_report_from_store(
    store: &EventStore,
    pricing: &PricingTable,
    source: Option<PathBuf>,
) -> Result<HarnessReport> {
    let mut sections = Vec::new();
    if let Some(session_id) = store.latest_session()? {
        let rows = model_cost_rows(store.cost_summary(&session_id, pricing)?);
        sections.push(UsageSection {
            title: format!("latest session {session_id}"),
            rows,
            efficiency: store
                .efficiency(&session_id)?
                .as_ref()
                .map(format_efficiency),
            include_in_combined: false,
        });
    }
    sections.push(UsageSection {
        title: "all sessions".to_string(),
        rows: model_cost_rows(store.cost_summary_all(pricing)?),
        efficiency: store.efficiency_all()?.as_ref().map(format_efficiency),
        include_in_combined: true,
    });

    Ok(HarnessReport {
        kind: HarnessKind::Flux,
        source,
        note: None,
        sections,
        scanned: 0,
        skipped: 0,
    })
}

fn model_cost_rows(rows: Vec<ModelCost>) -> Vec<UsageRow> {
    let mut rows: Vec<UsageRow> = rows
        .into_iter()
        .map(|row| UsageRow {
            model: row.model,
            calls: row.calls,
            usage: row.usage,
            cost: row.cost.map(CostCell::from_money),
        })
        .collect();
    sort_rows(&mut rows);
    rows
}

fn collect_codex(pricing: &PricingTable) -> HarnessReport {
    let Some(root) = harness_root("CODEX_HOME", ".codex") else {
        return HarnessReport::warning(HarnessKind::Codex, None, "HOME is not set");
    };
    let sessions = root.join("sessions");
    if !sessions.exists() {
        return HarnessReport::missing(HarnessKind::Codex, sessions);
    }
    match parse_codex_sessions(&sessions, pricing) {
        Ok((rows, scanned, skipped)) => {
            external_report(HarnessKind::Codex, sessions, rows, scanned, skipped)
        }
        Err(e) => HarnessReport::warning(HarnessKind::Codex, Some(sessions), e.to_string()),
    }
}

fn collect_claude(pricing: &PricingTable) -> HarnessReport {
    let Some(root) = env_path("CLAUDE_CONFIG_DIR").or_else(|| harness_root("", ".claude")) else {
        return HarnessReport::warning(HarnessKind::Claude, None, "HOME is not set");
    };
    let projects = root.join("projects");
    if !projects.exists() {
        return HarnessReport::missing(HarnessKind::Claude, projects);
    }
    match parse_claude_projects(&projects, pricing) {
        Ok((rows, scanned, skipped)) => {
            external_report(HarnessKind::Claude, projects, rows, scanned, skipped)
        }
        Err(e) => HarnessReport::warning(HarnessKind::Claude, Some(projects), e.to_string()),
    }
}

fn collect_opencode(pricing: &PricingTable) -> HarnessReport {
    let Some(root) = env_path("OPENCODE_DATA_DIR")
        .or_else(|| home_dir().map(|h| h.join(".local").join("share").join("opencode")))
    else {
        return HarnessReport::warning(HarnessKind::Opencode, None, "HOME is not set");
    };
    let db = root.join("opencode.db");
    if !db.exists() {
        return HarnessReport::missing(HarnessKind::Opencode, db);
    }
    match parse_opencode_db(&db, pricing) {
        Ok(rows) => external_report(HarnessKind::Opencode, db, rows, 1, 0),
        Err(e) => HarnessReport::warning(HarnessKind::Opencode, Some(db), e.to_string()),
    }
}

fn external_report(
    kind: HarnessKind,
    source: PathBuf,
    rows: Vec<UsageRow>,
    scanned: usize,
    skipped: usize,
) -> HarnessReport {
    HarnessReport {
        kind,
        source: Some(source),
        note: None,
        sections: vec![UsageSection {
            title: "all sessions".to_string(),
            rows,
            efficiency: None,
            include_in_combined: true,
        }],
        scanned,
        skipped,
    }
}

fn parse_claude_projects(
    projects: &Path,
    pricing: &PricingTable,
) -> Result<(Vec<UsageRow>, usize, usize)> {
    let files = jsonl_files(projects)?;
    let mut seen = HashSet::new();
    let mut rows = BTreeMap::<String, RowFold>::new();
    let mut scanned = 0;
    let mut skipped = 0;

    for file in files {
        if too_large(&file) {
            skipped += 1;
            continue;
        }
        scanned += 1;
        let reader = BufReader::new(File::open(&file)?);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if !line.contains("\"assistant\"") || !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                skipped += 1;
                continue;
            };
            if v.get("type").and_then(Value::as_str) != Some("assistant") {
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
                let sid = v
                    .get("sessionId")
                    .or_else(|| v.get("session_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !seen.insert(format!("{sid}:{key}")) {
                    continue;
                }
            }
            let model = format!("claude/{model}");
            let usage = usage_from_anthropic(usage_value);
            if usage_is_empty(&usage) {
                continue;
            }
            rows.entry(model.clone())
                .or_default()
                .record_usage(&usage, &model, pricing);
        }
    }
    Ok((finish_rows(rows), scanned, skipped))
}

fn parse_codex_sessions(
    sessions: &Path,
    pricing: &PricingTable,
) -> Result<(Vec<UsageRow>, usize, usize)> {
    let files = jsonl_files(sessions)?;
    let mut rows = BTreeMap::<String, RowFold>::new();
    let mut scanned = 0;
    let mut skipped = 0;

    for file in files {
        if too_large(&file) {
            skipped += 1;
            continue;
        }
        scanned += 1;
        let reader = BufReader::new(File::open(&file)?);
        let mut model = "codex/gpt-5.5".to_string();
        let mut token_count_rows = Vec::<Usage>::new();
        let mut fallback_rows = Vec::<(String, Usage)>::new();
        let mut seen_fallback = HashSet::new();

        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let interesting = line.contains("\"turn_context\"")
                || line.contains("\"token_count\"")
                || (line.contains("\"response_item\"") && line.contains("\"usage\""));
            if !interesting {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                skipped += 1;
                continue;
            };
            if v.get("type").and_then(Value::as_str) == Some("turn_context") {
                if let Some(m) = v
                    .pointer("/payload/model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                {
                    model = codex_model(m);
                }
                continue;
            }
            if v.get("type").and_then(Value::as_str) == Some("event_msg")
                && v.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
            {
                if let Some(info) = v.pointer("/payload/info/last_token_usage") {
                    let usage = usage_from_codex_token_count(info);
                    if !usage_is_empty(&usage) {
                        token_count_rows.push(usage);
                    }
                }
                continue;
            }

            if v.get("type").and_then(Value::as_str) == Some("response_item") {
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
                    .map(codex_model)
                    .unwrap_or_else(|| model.clone());
                let usage = usage_from_anthropic(usage_value);
                if !usage_is_empty(&usage) {
                    fallback_rows.push((fallback_model, usage));
                }
            }
        }

        if token_count_rows.is_empty() {
            for (model, usage) in fallback_rows {
                rows.entry(model.clone())
                    .or_default()
                    .record_usage(&usage, &model, pricing);
            }
        } else {
            for usage in token_count_rows {
                rows.entry(model.clone())
                    .or_default()
                    .record_usage(&usage, &model, pricing);
            }
        }
    }
    Ok((finish_rows(rows), scanned, skipped))
}

fn parse_opencode_db(db: &Path, pricing: &PricingTable) -> Result<Vec<UsageRow>> {
    let conn = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", db.display()))?;
    let mut stmt = conn.prepare(
        "select data from message \
         where json_extract(data, '$.role') = 'assistant' \
           and json_type(data, '$.tokens') is not null",
    )?;
    let mut query = stmt.query([])?;
    let mut rows = BTreeMap::<String, RowFold>::new();
    while let Some(row) = query.next()? {
        let data: String = row.get(0)?;
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let provider = v
            .get("providerID")
            .and_then(Value::as_str)
            .unwrap_or("opencode");
        let Some(model_id) = v.get("modelID").and_then(Value::as_str) else {
            continue;
        };
        let model = format!("{provider}/{model_id}");
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
            continue;
        }
        rows.entry(model.clone())
            .or_default()
            .record_usage(&usage, &model, pricing);
    }
    Ok(finish_rows(rows))
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

fn codex_model(model: &str) -> String {
    if model.starts_with("codex/") {
        model.to_string()
    } else {
        format!("codex/{model}")
    }
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

fn render_human(reports: &[HarnessReport]) -> String {
    let mut out = String::new();
    let visible: Vec<&HarnessReport> = reports
        .iter()
        .filter(|r| r.has_rows() || r.note.is_some())
        .collect();
    if visible.is_empty() {
        out.push_str("no usage data found\n");
        return out;
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
        out.push_str(&format!("{}\n", style::bold("combined total")));
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
        render_table(out, &section.rows);
        if let Some(efficiency) = &section.efficiency {
            out.push_str(&format!("  {}\n", style::dim(efficiency)));
        }
    }

    if report.scanned > 0 || report.skipped > 0 {
        let mut bits = Vec::new();
        if report.scanned > 0 {
            bits.push(format!(
                "{} file{}",
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
        "  {:<model_width$} {:>6} {:>9} {:>9} {:>11} {:>12} {:>10} {:>13}\n",
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
            "  {:<model_width$} {:>6} {:>9} {:>9} {:>11} {:>12} {:>10} {:>13}\n",
            truncate_model(&row.model, model_width),
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

fn report_json(reports: &[HarnessReport]) -> Value {
    json!({
        "harnesses": reports.iter().map(harness_json).collect::<Vec<_>>(),
        "combined": combined_rows(reports).iter().map(row_json).collect::<Vec<_>>(),
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
                "rows": s.rows.iter().map(row_json).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

fn row_json(row: &UsageRow) -> Value {
    json!({
        "model": row.model,
        "calls": row.calls,
        "usage": UsageJson::from(&row.usage),
        "cost": row.cost.map(CostJson::from),
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
        }
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

fn token_or_dash(n: u64) -> String {
    if n == 0 {
        "—".to_string()
    } else {
        style::fmt_tokens(n)
    }
}

fn cost_cell(row: &UsageRow) -> String {
    match row.cost {
        Some(cost) if cost.usd > 0.0 => {
            let dollars = format!("${:.4}", cost.usd);
            if cost.subscription {
                format!("~{dollars} sub")
            } else if cost.source == CostSourceCell::Reported {
                format!("{dollars} rpt")
            } else {
                dollars
            }
        }
        None if flux_core::is_metered_cloud_spec(&row.model) => "$?".to_string(),
        _ => "—".to_string(),
    }
}

fn truncate_model(model: &str, width: usize) -> String {
    let len = model.chars().count();
    if len <= width {
        return model.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut s: String = model.chars().take(width - 1).collect();
    s.push('…');
    s
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn jsonl_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_jsonl_files(root, &mut out)?;
    out.sort();
    if out.len() > MAX_JSONL_FILES {
        out.truncate(MAX_JSONL_FILES);
    }
    Ok(out)
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if out.len() >= MAX_JSONL_FILES {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_jsonl_files(&path, out)?;
        } else if ty.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
        if out.len() >= MAX_JSONL_FILES {
            break;
        }
    }
    Ok(())
}

fn too_large(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.len() > MAX_JSONL_FILE_BYTES)
        .unwrap_or(false)
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

    fn test_path(name: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("flux-usage-{name}-{}-{n}", std::process::id()))
    }

    #[test]
    fn renderer_outputs_aligned_usage_table() {
        crate::style::init(crate::style::ColorChoice::Never);
        let report = HarnessReport {
            kind: HarnessKind::Claude,
            source: Some(PathBuf::from("/tmp/claude/projects")),
            note: None,
            sections: vec![UsageSection {
                title: "all sessions".to_string(),
                rows: vec![UsageRow {
                    model: "claude/claude-opus-4-8".to_string(),
                    calls: 2,
                    usage: Usage {
                        input_tokens: 1000,
                        output_tokens: 200,
                        cache_read_input_tokens: 3000,
                        reasoning_tokens: 40,
                        ..Default::default()
                    },
                    cost: Some(CostCell {
                        usd: 0.1234,
                        subscription: true,
                        source: CostSourceCell::Estimated,
                    }),
                }],
                efficiency: None,
                include_in_combined: true,
            }],
            scanned: 1,
            skipped: 0,
        };
        let out = render_human(&[report]);
        assert!(out.contains("◆"));
        assert!(out.contains("Claude Code"));
        assert!(out.contains("cache read"));
        assert!(out.contains("~$0.1234 sub"));
    }

    #[test]
    fn claude_jsonl_dedupes_split_assistant_messages() {
        let root = test_path("claude");
        let project = root.join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let file = project.join("s.jsonl");
        let line = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-opus-4-8","usage":{"input_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2,"output_tokens":3}},"sessionId":"s"}"#;
        fs::write(&file, format!("{line}\n{line}\n")).unwrap();

        let (rows, scanned, skipped) =
            parse_claude_projects(&root.join("projects"), &PricingTable::builtin()).unwrap();
        assert_eq!(scanned, 1);
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude/claude-opus-4-8");
        assert_eq!(rows[0].calls, 1);
        assert_eq!(rows[0].usage.input_tokens, 10);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_jsonl_uses_incremental_token_count_rows() {
        let root = test_path("codex");
        let sessions = root.join("sessions").join("2026").join("07").join("08");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("rollout.jsonl"),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#.to_string()
                + "\n"
                + r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":12,"reasoning_output_tokens":3}}}}"#
                + "\n",
        )
        .unwrap();

        let (rows, scanned, skipped) =
            parse_codex_sessions(&root.join("sessions"), &PricingTable::builtin()).unwrap();
        assert_eq!(scanned, 1);
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "codex/gpt-5.5");
        assert_eq!(rows[0].usage.input_tokens, 60);
        assert_eq!(rows[0].usage.cache_read_input_tokens, 40);
        assert_eq!(rows[0].usage.output_tokens, 12);
        assert_eq!(rows[0].usage.reasoning_tokens, 3);

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

        let rows = parse_opencode_db(&db, &PricingTable::builtin()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "openrouter/z-ai/glm");
        assert_eq!(rows[0].usage.cache_read_input_tokens, 5);
        assert_eq!(rows[0].cost.unwrap().source, CostSourceCell::Reported);
        assert!((rows[0].cost.unwrap().usd - 0.0042).abs() < 1e-9);

        let _ = fs::remove_dir_all(root);
    }
}
