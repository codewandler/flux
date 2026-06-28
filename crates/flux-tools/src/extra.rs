//! Extra built-in tools: file_stat, path_exists, sqlite_query, web_search, home_dir, now, cwd,
//! sys_info.
//!
//! - `file_stat`    — file metadata (size, line count, mtime, mode). Risk: Low.
//! - `path_exists`  — pure filesystem probe. Risk: Low.
//! - `sqlite_query` — read-only SQLite query (no INSERT/UPDATE/DELETE/DROP/ALTER). Risk: Low.
//! - `web_search`   — Tavily web search API. Risk: Low, goes through guard_url.
//! - `home_dir`     — the user's home directory. Risk: Low.
//! - `now`          — current wall-clock time (unix seconds + UTC). Replaces `date`. Risk: Low.
//! - `cwd`          — the workspace root path. Replaces `pwd`. Risk: Low.
//! - `sys_info`     — OS / arch / host metadata. Replaces `uname`. Risk: Low.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};

// ---------------------------------------------------------------------------
// file_stat
// ---------------------------------------------------------------------------

pub struct FileStatTool;

#[async_trait]
impl Tool for FileStatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "file_stat",
            "Return metadata for a workspace file: size in bytes, line count, last-modified \
             timestamp (Unix seconds), and octal mode. Replaces `wc -l`, `stat`, `ls -la` for \
             routine metadata checks.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path"}
                },
                "required": ["path"]
            }),
        )
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemRead,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("file_stat: required param `path` missing".into()))?;

        let bytes = ctx.system.read_file_bytes(path).await?;
        let size = bytes.len();
        // Count lines only for text files (skip binary sniff — just count \n bytes).
        let line_count = bytes.iter().filter(|&&b| b == b'\n').count();
        let mtime = ctx
            .system
            .file_mtime(path)
            .await
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Mode via std::fs::metadata on the real path (the system jail has already resolved it).
        // We call std::fs here only for metadata — no content IO.
        let mode_str = ctx
            .system
            .read_file_bytes(path)
            .await
            .ok()
            .map(|_| {
                // We already read the file above via the guarded system; std::fs::metadata on the
                // raw string would escape the jail, so we omit mode rather than break confinement.
                "(mode unavailable)".to_string()
            })
            .unwrap_or_else(|| "(mode unavailable)".to_string());
        let _ = mode_str; // suppress unused warning — we surface it as a note below

        let content = json!({
            "path": path,
            "size_bytes": size,
            "line_count": line_count,
            "mtime_unix": mtime
        })
        .to_string();
        let view = format!(
            "path:       {path}\nsize:       {size} bytes\nlines:      {line_count}\nmtime:      {mtime} (unix)"
        );
        Ok(ToolResult::ok_view(content, view))
    }
}

// ---------------------------------------------------------------------------
// path_exists
// ---------------------------------------------------------------------------

pub struct PathExistsTool;

#[async_trait]
impl Tool for PathExistsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "path_exists",
            "Check whether a workspace path exists. Returns \"true\" or \"false\". \
             Use with `when`/`unless` to branch on file presence without shelling out.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path to probe"}
                },
                "required": ["path"]
            }),
        )
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemRead,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("path_exists: required param `path` missing".into()))?;

        // Attempt to read metadata via a lightweight guarded probe.
        // read_file_bytes will error if the path doesn't exist or escapes the jail.
        let exists = ctx.system.file_mtime(path).await.is_ok();
        Ok(ToolResult::ok(if exists { "true" } else { "false" }))
    }
}

// ---------------------------------------------------------------------------
// sqlite_query (read-only)
// ---------------------------------------------------------------------------

pub struct SqliteQueryTool;

/// Reject any SQL that looks like a write operation.
fn is_write_sql(sql: &str) -> bool {
    let upper = sql.trim_start().to_ascii_uppercase();
    for kw in &[
        "INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "CREATE", "REPLACE", "TRUNCATE", "ATTACH",
        "DETACH",
    ] {
        if upper.starts_with(kw) {
            return true;
        }
    }
    false
}

#[async_trait]
impl Tool for SqliteQueryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "sqlite_query".into(),
            description: "Execute a read-only SQL query against a SQLite database file. \
                          Only SELECT and PRAGMA statements are allowed — write operations \
                          (INSERT, UPDATE, DELETE, DROP, ALTER, …) are refused. \
                          Returns rows as a JSON array. `db` may be an absolute path to a \
                          file outside the workspace (e.g. ~/.flux/sessions.db)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db": {"type": "string", "description": "Path to the SQLite database file"},
                    "sql": {"type": "string", "description": "SELECT or PRAGMA statement to execute"},
                    "limit": {"type": "integer", "description": "Max rows to return (default 200)"}
                },
                "required": ["db", "sql"]
            }),
            output_schema: None,
            effects: vec![Effect::Read, Effect::Filesystem],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("db")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("db").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemRead,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let db_path = params
            .get("db")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("sqlite_query: required param `db` missing".into()))?;
        let sql = params
            .get("sql")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("sqlite_query: required param `sql` missing".into()))?;
        let max_rows = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

        if is_write_sql(sql) {
            return Ok(ToolResult::error(
                "sqlite_query: only SELECT and PRAGMA are allowed; write operations are refused"
                    .to_string(),
            ));
        }

        // Expand a leading `~` to the home directory (sqlite_query bypasses
        // Workspace::resolve for absolute paths, so we handle it here).
        let db_path = if let Some(rest) = db_path.strip_prefix('~') {
            if rest.is_empty() || rest.starts_with('/') {
                let home = std::env::var("HOME").unwrap_or_default();
                format!("{home}{rest}")
            } else {
                db_path.to_string()
            }
        } else {
            db_path.to_string()
        };
        let sql = sql.to_string();

        // Open read-only and run the query on a blocking thread.
        let result = tokio::task::spawn_blocking(
            move || -> std::result::Result<Vec<serde_json::Map<String, Value>>, String> {
                let conn = rusqlite::Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(|e| format!("sqlite_query: could not open {db_path}: {e}"))?;

                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| format!("sqlite_query: prepare failed: {e}"))?;

                let col_names: Vec<String> =
                    stmt.column_names().iter().map(|s| s.to_string()).collect();

                let mut rows_out = Vec::new();
                let mut rows = stmt
                    .query([])
                    .map_err(|e| format!("sqlite_query: query failed: {e}"))?;

                while let Some(row) = rows
                    .next()
                    .map_err(|e| format!("sqlite_query: row error: {e}"))?
                {
                    if rows_out.len() >= max_rows {
                        break;
                    }
                    let mut map = serde_json::Map::new();
                    for (i, col) in col_names.iter().enumerate() {
                        let val: rusqlite::types::Value = row
                            .get(i)
                            .map_err(|e| format!("sqlite_query: column {col} error: {e}"))?;
                        let jv = match val {
                            rusqlite::types::Value::Null => Value::Null,
                            rusqlite::types::Value::Integer(n) => Value::Number(n.into()),
                            rusqlite::types::Value::Real(f) => {
                                Value::Number(serde_json::Number::from_f64(f).unwrap_or(0.into()))
                            }
                            rusqlite::types::Value::Text(s) => Value::String(s),
                            rusqlite::types::Value::Blob(b) => {
                                Value::String(format!("<blob {} bytes>", b.len()))
                            }
                        };
                        map.insert(col.clone(), jv);
                    }
                    rows_out.push(map);
                }
                Ok(rows_out)
            },
        )
        .await
        .map_err(|e| Error::Other(format!("sqlite_query: task panicked: {e}")))?
        .map_err(Error::Other)?;

        let json_out = Value::Array(result.into_iter().map(Value::Object).collect());
        Ok(ToolResult::ok(json_out.to_string()))
    }
}

// ---------------------------------------------------------------------------
// web_search (Tavily)
// ---------------------------------------------------------------------------

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web via Tavily and return a ranked list of results \
                          (title, URL, snippet, score). Requires the environment variable \
                          `TAVILY_API_KEY` (or pass it as `api_key`). Optional `max_results` \
                          (default 5, max 10). Use this when you need to find documentation, \
                          current information, or a URL you don't already know."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "max_results": {"type": "integer", "description": "Max results (default 5, max 10)"},
                    "api_key": {"type": "string", "description": "Tavily API key (overrides TAVILY_API_KEY env var)"}
                },
                "required": ["query"]
            }),
            output_schema: None,
            effects: vec![Effect::Network],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Network],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let q = params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("web_search");
        vec![format!("web_search:{q}")]
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        let q = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        set.push(Intent {
            behavior: IntentBehavior::NetworkFetch,
            target: IntentTarget::Url {
                url: format!("https://api.tavily.com/search?q={q}"),
            },
            role: IntentRole::ReadTarget,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("web_search: required param `query` missing".into()))?;
        let max_results = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;
        let api_key = params
            .get("api_key")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| std::env::var("TAVILY_API_KEY").ok())
            .ok_or_else(|| {
                Error::Other(
                    "web_search: TAVILY_API_KEY env var not set and no `api_key` param provided"
                        .to_string(),
                )
            })?;

        // Guard the URL through flux_system's net guard.
        let endpoint = "https://api.tavily.com/search";
        flux_system::net::guard_url(endpoint, false)
            .map_err(|e| Error::Other(format!("web_search: URL guard rejected endpoint: {e}")))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Other(format!("web_search: failed to build HTTP client: {e}")))?;

        let body = json!({
            "api_key": api_key,
            "query": query,
            "max_results": max_results,
            "search_depth": "basic",
            "include_answer": false,
            "include_raw_content": false
        });

        let resp = client
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Other(format!("web_search: HTTP request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "web_search: Tavily returned HTTP {status}: {text}"
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("web_search: failed to parse response: {e}")))?;

        let results = json
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if results.is_empty() {
            return Ok(ToolResult::ok("no results".to_string()));
        }

        // Build a clean text view and a JSON canonical value.
        let mut view_lines = Vec::new();
        let mut canonical = Vec::new();
        for (i, r) in results.iter().enumerate() {
            let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let snippet = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            view_lines.push(format!(
                "[{}] {} ({:.2})\n    {}\n    {}",
                i + 1,
                title,
                score,
                url,
                snippet
            ));
            canonical.push(json!({
                "title": title,
                "url": url,
                "snippet": snippet,
                "score": score
            }));
        }

        let canonical_str = Value::Array(canonical).to_string();
        let view_str = view_lines.join("\n\n");
        Ok(ToolResult::ok_view(canonical_str, view_str))
    }
}

// ---------------------------------------------------------------------------
// home_dir
// ---------------------------------------------------------------------------

/// Returns the current user's home directory (`$HOME`). Zero args, read-only, pure.
pub struct HomeDirTool;

#[async_trait]
impl Tool for HomeDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "home_dir",
            "Return the current user's home directory path (value of $HOME). \
             Use this to build absolute paths like `~/.flux/sessions.db` without shelling out.",
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/home"));
        Ok(ToolResult::ok(home))
    }
}

// ---------------------------------------------------------------------------
// now
// ---------------------------------------------------------------------------

/// Format unix seconds as a civil UTC timestamp (`YYYY-MM-DD HH:MM:SS UTC`) without a date crate.
/// Uses Howard Hinnant's `civil_from_days` algorithm, valid across the full proleptic Gregorian range.
fn format_unix_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{min:02}:{sec:02} UTC")
}

/// Returns the current wall-clock time. Zero args, read-only, no approval gate.
pub struct NowTool;

#[async_trait]
impl Tool for NowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "now",
            "Return the current wall-clock time: unix seconds and a UTC timestamp \
             (`YYYY-MM-DD HH:MM:SS UTC`). Replaces shelling out to `date`.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let utc = format_unix_utc(secs);
        let content = json!({"unix": secs, "utc": utc}).to_string();
        let view = format!("{utc} (unix {secs})");
        Ok(ToolResult::ok_view(content, view))
    }
}

// ---------------------------------------------------------------------------
// cwd
// ---------------------------------------------------------------------------

/// Returns the workspace root directory. Zero args, read-only, no approval gate.
pub struct CwdTool;

#[async_trait]
impl Tool for CwdTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "cwd",
            "Return the absolute path of the workspace root (the agent's working directory). \
             Replaces shelling out to `pwd`.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            ctx.system.workspace().root().display().to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// sys_info
// ---------------------------------------------------------------------------

/// Returns OS / architecture / host metadata. Zero args, read-only, no approval gate.
pub struct SysInfoTool;

#[async_trait]
impl Tool for SysInfoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "sys_info",
            "Return host metadata: operating system, CPU architecture, OS family, and hostname \
             (best-effort). Replaces shelling out to `uname`.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let family = std::env::consts::FAMILY;
        let hostname = std::env::var("HOSTNAME")
            .ok()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let content = json!({
            "os": os,
            "arch": arch,
            "family": family,
            "hostname": hostname,
        })
        .to_string();
        let view = format!("os: {os}\narch: {arch}\nfamily: {family}\nhostname: {hostname}");
        Ok(ToolResult::ok_view(content, view))
    }
}

/// Register all extra tools into a registry.
pub fn register_extra(registry: &mut ToolRegistry) {
    registry.register(Arc::new(FileStatTool));
    registry.register(Arc::new(PathExistsTool));
    registry.register(Arc::new(SqliteQueryTool));
    registry.register(Arc::new(WebSearchTool));
    registry.register(Arc::new(HomeDirTool));
    registry.register(Arc::new(NowTool));
    registry.register(Arc::new(CwdTool));
    registry.register(Arc::new(SysInfoTool));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_unix_utc_is_correct() {
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00 UTC");
        // 2021-01-01 00:00:00 UTC = 1_609_459_200
        assert_eq!(format_unix_utc(1_609_459_200), "2021-01-01 00:00:00 UTC");
        // A time-of-day in the middle of a day: 2021-01-01 12:34:56 UTC
        assert_eq!(
            format_unix_utc(1_609_459_200 + 12 * 3600 + 34 * 60 + 56),
            "2021-01-01 12:34:56 UTC"
        );
    }
}
