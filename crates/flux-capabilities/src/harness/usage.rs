//! Token-shaped harness acquisition: the one metadata-only usage extraction (C-519).
//!
//! [`message`](super::message) answers *what was said*; this answers *what was spent*. Both walk
//! the same [`scan`](super::scan) primitives, so there is exactly one place that knows where a
//! harness keeps its state and how to read it under budget — and exactly one parser per harness,
//! shared by `flux usage` and the observatory timeline instead of copied into either.
//!
//! Two properties are contracts rather than implementation details:
//!
//! - **Metadata only.** A [`UsageFact`] and a [`HarnessSession`] have no field a prompt, an answer,
//!   a tool argument or a transcript body could occupy. Text that passes under the parser is
//!   inspected for its `type`/`usage`/`tokens` shape and dropped; it is never carried out.
//! - **Absence stays absent.** A harness that reports only token history yields a partial fact:
//!   unknown provider, no causal receipt, and no invented CPU/network ownership.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use flux_core::{Error, PricingTable, Result, Usage};
use flux_events::{EventStore, StoredEvent};
use serde_json::Value;

use super::message::{file_stem, json_epoch_ms, json_epoch_ms_at, normalize_epoch_ms};
use super::scan::sqlite_err;
use super::{
    jsonl_files, open_jsonl, open_sqlite_read_only, sqlite_column_exists, sqlite_table_exists,
    HarnessKind, JsonlLine, ScanBudget,
};
use crate::usage_observatory::{flux_facts, ProviderAttribution, UsageFact, UsageRange};

/// One session's own metadata: identity, span, workspace and message *count* — never content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessSession {
    pub harness: HarnessKind,
    pub session_id: String,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub cwd: Option<String>,
    pub messages: u64,
}

/// The result of one metadata-only usage scan.
///
/// `scanned`/`skipped` are the read-only discovery limits made visible: input the budget refused,
/// or that could not be parsed, is counted rather than silently dropped.
#[derive(Clone, Debug, Default)]
pub struct UsageScan {
    pub facts: Vec<UsageFact>,
    pub sessions: Vec<HarnessSession>,
    pub scanned: usize,
    pub skipped: usize,
}

/// A bounded read window applied while facts are built.
///
/// The default is unbounded, which is what `flux usage` asks for before applying its own window.
/// A bounded window is how the observatory reads a range without materializing a whole history —
/// and it never widens what a scan touches: nothing but usage metadata is read either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageWindow {
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
}

impl UsageWindow {
    /// Everything the harness has.
    pub const UNBOUNDED: Self = Self {
        since_ms: None,
        until_ms: None,
    };

    /// Whether a record spanning `started_at_ms..ended_at_ms` belongs to this window.
    ///
    /// A record with no timestamp at all is kept only by an unbounded window: it cannot be proven
    /// to fall inside a bounded one, and assuming it does would silently inflate a range.
    pub fn accepts(&self, started_at_ms: Option<i64>, ended_at_ms: Option<i64>) -> bool {
        let Some(start) = started_at_ms.or(ended_at_ms) else {
            return self.since_ms.is_none() && self.until_ms.is_none();
        };
        let end = ended_at_ms.or(started_at_ms).unwrap_or(start);
        self.since_ms.is_none_or(|since| end >= since)
            && self.until_ms.is_none_or(|until| start < until)
    }
}

impl From<UsageRange> for UsageWindow {
    fn from(range: UsageRange) -> Self {
        Self {
            since_ms: Some(range.start_ms),
            until_ms: Some(range.end_ms),
        }
    }
}

/// Progress for a slow scan, as a callback rather than a rendering decision.
///
/// The surface owns the wording and whether anything is drawn at all; acquisition only reports how
/// far it has got.
pub trait ScanObserver {
    fn begin(&mut self, _harness: HarnessKind, _total: usize) {}
    fn tick(&mut self, _harness: HarnessKind, _current: usize, _total: usize, _skipped: usize) {}
    fn finish(&mut self, _harness: HarnessKind, _current: usize, _total: usize, _skipped: usize) {}
}

/// The observer for callers that render nothing.
pub struct NoProgress;

impl ScanObserver for NoProgress {}

/// Extract one harness's usage from the state at `path` (see [`HarnessKind::locate`]).
///
/// This is the whole cross-harness contract in one call: every [`HarnessKind`] variant resolves to
/// its own adapter, and every adapter returns the same [`UsageScan`].
pub fn harness_usage(
    kind: HarnessKind,
    path: &Path,
    pricing: &PricingTable,
    window: UsageWindow,
    observer: &mut dyn ScanObserver,
) -> Result<UsageScan> {
    match kind {
        HarnessKind::Flux => {
            let store = EventStore::open(path)
                .map_err(|e| Error::Other(format!("open {}: {e}", path.display())))?;
            flux_usage(&store, pricing, window, observer)
        }
        HarnessKind::Codex => codex_usage(path, pricing, window, observer),
        HarnessKind::Claude => claude_usage(path, pricing, window, observer),
        HarnessKind::Opencode => opencode_usage(path, pricing, window, observer),
    }
}

/// flux's own history, from an already-open event store.
///
/// `CallUsage` is canonical per turn and `TurnEnded.usage` is the uncovered-legacy-turn fallback
/// ([`flux_facts`]). A sub-agent stream whose parent is in the same store is skipped: its usage is
/// already inside the parent's, and counting both double-bills the same tokens.
pub fn flux_usage(
    store: &EventStore,
    pricing: &PricingTable,
    window: UsageWindow,
    observer: &mut dyn ScanObserver,
) -> Result<UsageScan> {
    let streams = store.all_streams()?;
    let mut loaded = Vec::new();
    observer.begin(HarnessKind::Flux, streams.len());
    for (idx, stream) in streams.iter().enumerate() {
        let events = store.load_stream(stream, None)?;
        let correlation_id = events
            .first()
            .and_then(|e| e.context.correlation_id.clone());
        loaded.push((stream.clone(), events, correlation_id));
        observer.tick(HarnessKind::Flux, idx + 1, streams.len(), 0);
    }
    observer.finish(HarnessKind::Flux, streams.len(), streams.len(), 0);

    let ids: HashSet<String> = loaded.iter().map(|(id, _, _)| id.clone()).collect();
    let mut scan = UsageScan {
        scanned: streams.len(),
        ..Default::default()
    };
    for (stream, events, correlation_id) in loaded {
        if correlation_id
            .as_ref()
            .is_some_and(|parent| ids.contains(parent))
        {
            continue;
        }
        scan.facts.extend(
            flux_facts(&stream, &events, pricing)
                .into_iter()
                .filter(|fact| window.accepts(fact.started_at_ms, fact.ended_at_ms)),
        );
        scan.sessions.push(flux_session(&stream, &events));
    }
    Ok(scan)
}

fn flux_session(stream: &str, events: &[StoredEvent]) -> HarnessSession {
    let mut build = SessionBuild::default();
    for event in events {
        build.observe(Some(event.ts_ms));
    }
    build.messages = flux_events::turns(events).len() as u64;
    build.into_session(HarnessKind::Flux, stream.to_string())
}

/// Claude Code project transcripts: one usage-bearing `assistant` record per provider response.
pub fn claude_usage(
    projects: &Path,
    pricing: &PricingTable,
    window: UsageWindow,
    observer: &mut dyn ScanObserver,
) -> Result<UsageScan> {
    let scan = jsonl_scan(projects)?;
    let files = scan.files();
    let mut skipped = scan.skipped();
    let mut seen = HashSet::new();
    let mut facts = Vec::new();
    let mut sessions = BTreeMap::<String, SessionBuild>::new();

    observer.begin(HarnessKind::Claude, files.len());
    for (idx, file) in files.iter().enumerate() {
        // An over-budget or unreadable file must not abort the scan: skip it like a bad line and
        // keep the rest.
        let Ok(lines) = open_jsonl(file, ScanBudget::default()) else {
            skipped += 1;
            observer.tick(HarnessKind::Claude, idx + 1, files.len(), skipped);
            continue;
        };
        let fallback_session = file_stem(file);
        for line in lines {
            let JsonlLine::Text(line) = line else {
                skipped += 1;
                continue;
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
            push_fact(
                &mut facts,
                window,
                HarnessKind::Claude,
                sid,
                model,
                ts,
                ts,
                usage,
                pricing,
            );
        }
        observer.tick(HarnessKind::Claude, idx + 1, files.len(), skipped);
    }
    observer.finish(HarnessKind::Claude, files.len(), files.len(), skipped);

    Ok(UsageScan {
        facts,
        sessions: build_sessions(HarnessKind::Claude, sessions),
        scanned: files.len(),
        skipped,
    })
}

/// Codex rollouts: incremental `token_count` rows when present, per-response `usage` otherwise.
pub fn codex_usage(
    sessions_root: &Path,
    pricing: &PricingTable,
    window: UsageWindow,
    observer: &mut dyn ScanObserver,
) -> Result<UsageScan> {
    let scan = jsonl_scan(sessions_root)?;
    let files = scan.files();
    let mut skipped = scan.skipped();
    let mut facts = Vec::new();
    let mut sessions = Vec::new();

    observer.begin(HarnessKind::Codex, files.len());
    for (idx, file) in files.iter().enumerate() {
        // An over-budget or unreadable file must not abort the scan: skip it like a bad line and
        // keep the rest.
        let Ok(lines) = open_jsonl(file, ScanBudget::default()) else {
            skipped += 1;
            observer.tick(HarnessKind::Codex, idx + 1, files.len(), skipped);
            continue;
        };
        let mut session_id = file_stem(file);
        let mut build = SessionBuild::default();
        let mut model = "codex/gpt-5.5".to_string();
        let mut token_count_facts = Vec::<UsageFact>::new();
        let mut fallback_facts = Vec::<UsageFact>::new();
        let mut seen_fallback = HashSet::new();

        for line in lines {
            let JsonlLine::Text(line) = line else {
                skipped += 1;
                continue;
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
                    if let Some(started) = v.pointer("/payload/timestamp").and_then(json_epoch_ms) {
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
                            push_fact(
                                &mut token_count_facts,
                                window,
                                HarnessKind::Codex,
                                session_id.clone(),
                                model.clone(),
                                ts,
                                ts,
                                usage,
                                pricing,
                            );
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
                        push_fact(
                            &mut fallback_facts,
                            window,
                            HarnessKind::Codex,
                            session_id.clone(),
                            fallback_model,
                            ts,
                            ts,
                            usage,
                            pricing,
                        );
                    }
                }
                _ => {}
            }
        }

        if token_count_facts.is_empty() {
            facts.extend(fallback_facts);
        } else {
            facts.extend(token_count_facts);
        }
        sessions.push(build.into_session(HarnessKind::Codex, session_id));
        observer.tick(HarnessKind::Codex, idx + 1, files.len(), skipped);
    }
    observer.finish(HarnessKind::Codex, files.len(), files.len(), skipped);

    Ok(UsageScan {
        facts,
        sessions,
        scanned: files.len(),
        skipped,
    })
}

/// opencode's database: assistant messages carrying a `tokens` object, read **read-only**.
pub fn opencode_usage(
    db: &Path,
    pricing: &PricingTable,
    window: UsageWindow,
    observer: &mut dyn ScanObserver,
) -> Result<UsageScan> {
    // The surface note is this wording verbatim, so the failure keeps naming the file it is about.
    let conn =
        open_sqlite_read_only(db).map_err(|_| Error::Other(format!("open {}", db.display())))?;
    let has_session_table = sqlite_table_exists(&conn, "session")?;
    let message_has_session_id = sqlite_column_exists(&conn, "message", "session_id")?;
    let message_has_time_created = sqlite_column_exists(&conn, "message", "time_created")?;
    let message_has_time_updated = sqlite_column_exists(&conn, "message", "time_updated")?;

    let mut sessions = BTreeMap::<String, SessionBuild>::new();
    if has_session_table {
        let mut stmt = conn
            .prepare(
                "select id, time_created, time_updated, directory from session order by time_created",
            )
            .map_err(sqlite_err)?;
        let mut rows = stmt.query([]).map_err(sqlite_err)?;
        while let Some(row) = rows.next().map_err(sqlite_err)? {
            let id: String = row.get(0).map_err(sqlite_err)?;
            let started: Option<i64> = row
                .get::<_, Option<i64>>(1)
                .map_err(sqlite_err)?
                .map(normalize_epoch_ms);
            let ended: Option<i64> = row
                .get::<_, Option<i64>>(2)
                .map_err(sqlite_err)?
                .map(normalize_epoch_ms);
            let cwd: Option<String> = row.get(3).map_err(sqlite_err)?;
            let build = sessions.entry(id).or_default();
            build.observe_range(started, ended);
            build.cwd = build.cwd.take().or(cwd);
        }
    }

    let total = sqlite_count_assistant_token_messages(&conn).unwrap_or(0);
    observer.begin(HarnessKind::Opencode, total);
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
    let mut stmt = conn.prepare(sql).map_err(sqlite_err)?;
    let mut query = stmt.query([]).map_err(sqlite_err)?;
    let mut facts = Vec::new();
    let mut scanned = 0usize;
    let mut skipped = 0usize;
    while let Some(row) = query.next().map_err(sqlite_err)? {
        scanned += 1;
        let id: String = row.get(0).map_err(sqlite_err)?;
        let row_session: Option<String> = row.get(1).map_err(sqlite_err)?;
        let created: Option<i64> = row
            .get::<_, Option<i64>>(2)
            .map_err(sqlite_err)?
            .map(normalize_epoch_ms);
        let updated: Option<i64> = row
            .get::<_, Option<i64>>(3)
            .map_err(sqlite_err)?
            .map(normalize_epoch_ms);
        let data: String = row.get(4).map_err(sqlite_err)?;
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => {
                skipped += 1;
                observer.tick(HarnessKind::Opencode, scanned, total, skipped);
                continue;
            }
        };
        let provider = v
            .get("providerID")
            .and_then(Value::as_str)
            .unwrap_or("opencode");
        let Some(model_id) = v.get("modelID").and_then(Value::as_str) else {
            skipped += 1;
            observer.tick(HarnessKind::Opencode, scanned, total, skipped);
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
        let started = created.or_else(|| json_epoch_ms_at(&v, &["time", "created"]));
        let ended = updated
            .or_else(|| json_epoch_ms_at(&v, &["time", "completed"]))
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
            observer.tick(HarnessKind::Opencode, scanned, total, skipped);
            continue;
        }
        let build = sessions.entry(session_id.clone()).or_default();
        build.observe_range(started, ended);
        build.messages += 1;
        push_fact(
            &mut facts,
            window,
            HarnessKind::Opencode,
            session_id,
            prefixed_model(provider, model_id),
            started,
            ended,
            usage,
            pricing,
        );
        observer.tick(HarnessKind::Opencode, scanned, total, skipped);
    }
    observer.finish(HarnessKind::Opencode, scanned, total, skipped);

    Ok(UsageScan {
        facts,
        sessions: build_sessions(HarnessKind::Opencode, sessions),
        scanned,
        skipped,
    })
}

/// A session's span, workspace and message count, accumulated as its records go by.
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

    fn into_session(self, harness: HarnessKind, session_id: String) -> HarnessSession {
        HarnessSession {
            harness,
            session_id,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            cwd: self.cwd,
            messages: self.messages,
        }
    }
}

fn build_sessions(
    harness: HarnessKind,
    sessions: BTreeMap<String, SessionBuild>,
) -> Vec<HarnessSession> {
    sessions
        .into_iter()
        .map(|(id, build)| build.into_session(harness, id))
        .collect()
}

/// Build one priced fact and keep it only when the window admits it.
///
/// The provider is [`ProviderAttribution::Unknown`] for every harness here: none of them records a
/// billing provider independently of the model string, and a prefix is not proof.
#[allow(clippy::too_many_arguments)]
fn push_fact(
    out: &mut Vec<UsageFact>,
    window: UsageWindow,
    harness: HarnessKind,
    session_id: String,
    model: String,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    usage: Usage,
    pricing: &PricingTable,
) {
    if !window.accepts(started_at_ms, ended_at_ms) {
        return;
    }
    out.push(UsageFact::priced_span(
        harness,
        session_id,
        model,
        ProviderAttribution::Unknown,
        started_at_ms,
        ended_at_ms,
        usage,
        pricing,
    ));
}

/// The `.jsonl` files under a harness root at the standard scan budget. Only an unreadable root
/// propagates as an error, and it carries the wording the surface note has always shown.
fn jsonl_scan(root: &Path) -> Result<super::JsonlScan> {
    jsonl_files(root, ScanBudget::default())
        .map_err(|_| Error::Other(format!("read {}", root.display())))
}

fn sqlite_count_assistant_token_messages(conn: &rusqlite::Connection) -> Result<usize> {
    let count: i64 = conn
        .query_row(
            "select count(*) from message \
             where json_extract(data, '$.role') = 'assistant' \
               and json_type(data, '$.tokens') is not null",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_err)?;
    Ok(count.max(0) as usize)
}

fn usage_is_empty(usage: &Usage) -> bool {
    usage.total() == 0 && usage.reasoning_tokens == 0 && usage.reported_cost_usd.is_none()
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

fn json_timestamp_ms(v: &Value) -> Option<i64> {
    v.get("timestamp")
        .and_then(json_epoch_ms)
        .or_else(|| json_epoch_ms_at(v, &["message", "timestamp"]))
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

fn prefixed_model(provider: &str, model: &str) -> String {
    if model.starts_with(&format!("{provider}/")) {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::PathBuf;

    use flux_core::PricingTable;

    use super::*;
    use crate::usage_observatory::{CostSourceCell, CostStatus};

    fn test_path(name: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("flux-usage-{name}-{}-{n}", std::process::id()))
    }

    #[test]
    fn claude_jsonl_dedupes_split_assistant_messages() {
        let root = test_path("claude");
        let project = root.join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let file = project.join("s.jsonl");
        let line = r#"{"type":"assistant","timestamp":"2026-07-08T12:00:00Z","message":{"id":"msg_1","model":"claude-opus-4-8","usage":{"input_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2,"output_tokens":3}},"sessionId":"s"}"#;
        fs::write(&file, format!("{line}\n{line}\n")).unwrap();

        let scan = claude_usage(
            &root.join("projects"),
            &PricingTable::builtin(),
            UsageWindow::UNBOUNDED,
            &mut NoProgress,
        )
        .unwrap();
        assert_eq!(scan.scanned, 1);
        assert_eq!(scan.skipped, 0);
        assert_eq!(scan.facts.len(), 1);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.facts[0].model, "claude/claude-opus-4-8");
        assert_eq!(scan.facts[0].usage.input_tokens, 10);
        assert_eq!(
            scan.facts[0].cost_status,
            CostStatus::SubscriptionEquivalent
        );
        assert!(scan.facts[0].started_at_ms.is_some());

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

        let scan = claude_usage(
            &root.join("projects"),
            &PricingTable::builtin(),
            UsageWindow::UNBOUNDED,
            &mut NoProgress,
        )
        .unwrap();
        assert_eq!(scan.scanned, 2, "both jsonl files are listed");
        assert_eq!(
            scan.skipped, 2,
            "the unreadable file and directory are skipped"
        );
        assert_eq!(
            scan.facts.len(),
            1,
            "the readable file still yields its record"
        );
        assert_eq!(scan.sessions.len(), 1);

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

        let scan = codex_usage(
            &root.join("sessions"),
            &PricingTable::builtin(),
            UsageWindow::UNBOUNDED,
            &mut NoProgress,
        )
        .unwrap();
        assert_eq!(scan.scanned, 1);
        assert_eq!(scan.skipped, 0);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.facts.len(), 1);
        assert_eq!(scan.facts[0].model, "codex/gpt-5.5");
        assert_eq!(scan.facts[0].usage.input_tokens, 60);
        assert_eq!(scan.facts[0].usage.cache_read_input_tokens, 40);
        assert_eq!(scan.facts[0].usage.output_tokens, 12);
        assert_eq!(scan.facts[0].usage.reasoning_tokens, 3);
        assert!(scan.facts[0].started_at_ms.is_some());

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

        let scan = opencode_usage(
            &db,
            &PricingTable::builtin(),
            UsageWindow::UNBOUNDED,
            &mut NoProgress,
        )
        .unwrap();
        assert_eq!(scan.scanned, 1);
        assert_eq!(scan.skipped, 0);
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.facts.len(), 1);
        assert_eq!(scan.facts[0].model, "openrouter/z-ai/glm");
        assert_eq!(scan.facts[0].usage.cache_read_input_tokens, 5);
        assert_eq!(scan.facts[0].cost.unwrap().source, CostSourceCell::Reported);
        assert_eq!(scan.facts[0].cost_status, CostStatus::Reported);
        assert!((scan.facts[0].cost.unwrap().usd - 0.0042).abs() < 1e-9);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_bounded_window_drops_records_outside_it() {
        let root = test_path("claude-window");
        let project = root.join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let record = |ts: &str, id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"{ts}","message":{{"id":"{id}","model":"claude-opus-4-8","usage":{{"input_tokens":10,"output_tokens":3}}}},"sessionId":"s"}}"#
            )
        };
        fs::write(
            project.join("s.jsonl"),
            format!(
                "{}\n{}\n",
                record("2026-07-08T12:00:00Z", "old"),
                record("2026-07-09T12:00:00Z", "new")
            ),
        )
        .unwrap();

        // Between the two records: 2026-07-08T12:00:00Z is 1_783_512_000_000 and the next day's
        // record is 1_783_598_400_000.
        let window = UsageWindow {
            since_ms: Some(1_783_550_000_000),
            until_ms: None,
        };
        let bounded = claude_usage(
            &root.join("projects"),
            &PricingTable::builtin(),
            window,
            &mut NoProgress,
        )
        .unwrap();
        let unbounded = claude_usage(
            &root.join("projects"),
            &PricingTable::builtin(),
            UsageWindow::UNBOUNDED,
            &mut NoProgress,
        )
        .unwrap();
        assert_eq!(unbounded.facts.len(), 2);
        assert_eq!(bounded.facts.len(), 1, "only the record inside the window");

        let _ = fs::remove_dir_all(root);
    }
}
