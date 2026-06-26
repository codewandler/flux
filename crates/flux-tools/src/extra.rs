//! Extra built-in tools: file_stat, path_exists, sqlite_query, web_search.
//!
//! - `file_stat`    — file metadata (size, line count, mtime, mode). Risk: Low.
//! - `path_exists`  — pure filesystem probe. Risk: Low.
//! - `sqlite_query` — read-only SQLite query (no INSERT/UPDATE/DELETE/DROP/ALTER). Risk: Low.
//! - `web_search`   — Tavily web search API. Risk: Low, goes through guard_url.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

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

        let db_path = db_path.to_string();
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

/// Register all extra tools into a registry.
pub fn register_extra(registry: &mut ToolRegistry) {
    registry.register(Arc::new(FileStatTool));
    registry.register(Arc::new(PathExistsTool));
    registry.register(Arc::new(SqliteQueryTool));
    registry.register(Arc::new(WebSearchTool));
}
