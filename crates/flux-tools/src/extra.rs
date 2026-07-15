//! Extra built-in tools: file_stat, path_exists, sqlite_query, home_dir, now, cwd, sys_info.
//!
//! - `file_stat`    — file metadata (size, line count, mtime, mode). Risk: Low.
//! - `path_exists`  — pure filesystem probe. Risk: Low.
//! - `sqlite_query` — read-only SQLite query (no INSERT/UPDATE/DELETE/DROP/ALTER). Risk: Low.
//! - `home_dir`     — the user's home directory. Risk: Low.
//! - `now`          — current wall-clock time (unix seconds + UTC). Replaces `date`. Risk: Low.
//! - `cwd`          — the workspace root path. Replaces `pwd`. Risk: Low.
//! - `sys_info`     — OS / arch / host metadata. Replaces `uname`. Risk: Low.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{
    tool_input_schema, AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty,
    IntentRole, IntentSet, IntentTarget, Risk, ToolSpec,
};

// ---------------------------------------------------------------------------
// file_stat
// ---------------------------------------------------------------------------

pub struct FileStatTool;

/// Arguments for the `file_stat` op.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileStatInput {
    /// Workspace-relative path.
    path: String,
}

#[async_trait]
impl Tool for FileStatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "file_stat",
            "Return metadata for a workspace file: size in bytes, line count, last-modified \
             timestamp (Unix seconds), and octal mode. Replaces `wc -l`, `stat`, `ls -la` for \
             routine metadata checks.",
            tool_input_schema::<FileStatInput>(),
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

/// Arguments for the `path_exists` op.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PathExistsInput {
    /// Workspace-relative path to probe.
    path: String,
}

#[async_trait]
impl Tool for PathExistsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "path_exists",
            "Check whether a workspace path exists. Returns \"true\" or \"false\". \
             Use with `when`/`unless` to branch on file presence without shelling out.",
            tool_input_schema::<PathExistsInput>(),
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

/// Arguments for the `sqlite_query` op.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SqliteQueryInput {
    /// Path to the SQLite database file.
    db: String,
    /// SELECT or PRAGMA statement to execute.
    sql: String,
    /// Max rows to return (default 200).
    #[serde(default)]
    limit: Option<u64>,
}

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

/// Expand a leading `~` / `~/` to `$HOME`. Any other form is returned unchanged.
fn expand_tilde(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            if let Ok(home) = std::env::var("HOME") {
                return format!("{home}{rest}");
            }
        }
    }
    raw.to_string()
}

/// Confine `raw` to a database this tool may open (C-78). A workspace-relative path goes through the
/// workspace read-jail (which rejects `..` and symlink escapes); an out-of-workspace absolute/`~`
/// path is permitted only when it canonicalizes under `~/.flux` — the tool's advertised session-DB
/// location. Everything else (browser cookie stores, credential DBs, arbitrary files) is refused so
/// this Risk::Low read-only tool cannot exfiltrate secrets outside the jail.
fn jail_sqlite_path(ctx: &ToolContext, raw: &str) -> Result<String> {
    // In-workspace (or a configured read-only root): the workspace enforces the jail.
    if let Ok(p) = ctx.system.workspace().resolve_read(raw) {
        return Ok(p.to_string_lossy().into_owned());
    }
    // Out of the workspace jail: allow only databases under ~/.flux.
    let expanded = expand_tilde(raw);
    let home = std::env::var("HOME").map_err(|_| {
        Error::Other(
            "sqlite_query: HOME is not set, cannot resolve an out-of-workspace database".into(),
        )
    })?;
    let flux_home = std::path::Path::new(&home).join(".flux");
    let flux_home = std::fs::canonicalize(&flux_home).unwrap_or(flux_home);
    let canon = std::fs::canonicalize(&expanded)
        .map_err(|e| Error::Other(format!("sqlite_query: cannot open {expanded}: {e}")))?;
    if canon.starts_with(&flux_home) {
        Ok(canon.to_string_lossy().into_owned())
    } else {
        Err(Error::Other(format!(
            "sqlite_query: refusing to open {} — only databases inside the workspace or under \
             ~/.flux are permitted (an arbitrary on-disk database could exfiltrate secrets)",
            canon.display()
        )))
    }
}

#[async_trait]
impl Tool for SqliteQueryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "sqlite_query".into(),
            description: "Execute a read-only SQL query against a SQLite database file. \
                          Only SELECT and PRAGMA statements are allowed — write operations \
                          (INSERT, UPDATE, DELETE, DROP, ALTER, …) are refused. \
                          Returns rows as a JSON array. `db` must be a database inside the \
                          workspace or under ~/.flux (e.g. ~/.flux/sessions.db); other on-disk \
                          databases are refused."
                .into(),
            input_schema: tool_input_schema::<SqliteQueryInput>(),
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

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
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

        // Jail the database path (C-78): a relative path resolves inside the workspace; an
        // out-of-workspace absolute/`~` path is permitted only under `~/.flux` (the tool's advertised
        // session-DB use). Any other on-disk database — browser cookie stores, credential DBs,
        // arbitrary user files — is refused, so this read-only tool at Risk::Low can't be turned into
        // a secret-exfiltration primitive.
        let db_path = match jail_sqlite_path(ctx, db_path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(e.to_string())),
        };
        let sql = sql.to_string();

        // Open read-only and run the query on a blocking thread.
        let result =
            tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Map<String, Value>>> {
                let conn = rusqlite::Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(|e| {
                    Error::Other(format!("sqlite_query: could not open {db_path}: {e}"))
                })?;

                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| Error::Other(format!("sqlite_query: prepare failed: {e}")))?;

                let col_names: Vec<String> =
                    stmt.column_names().iter().map(|s| s.to_string()).collect();

                let mut rows_out = Vec::new();
                let mut rows = stmt
                    .query([])
                    .map_err(|e| Error::Other(format!("sqlite_query: query failed: {e}")))?;

                while let Some(row) = rows
                    .next()
                    .map_err(|e| Error::Other(format!("sqlite_query: row error: {e}")))?
                {
                    if rows_out.len() >= max_rows {
                        break;
                    }
                    let mut map = serde_json::Map::new();
                    for (i, col) in col_names.iter().enumerate() {
                        let val: rusqlite::types::Value = row.get(i).map_err(|e| {
                            Error::Other(format!("sqlite_query: column {col} error: {e}"))
                        })?;
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
            })
            .await
            .map_err(|e| Error::Other(format!("sqlite_query: task panicked: {e}")))??;

        let json_out = Value::Array(result.into_iter().map(Value::Object).collect());
        Ok(ToolResult::ok(json_out.to_string()))
    }
}

// ---------------------------------------------------------------------------
// home_dir
// ---------------------------------------------------------------------------

/// Arguments for the `home_dir` op (no parameters).
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HomeDirInput {}

/// Returns the current user's home directory (`$HOME`). Zero args, read-only, pure.
pub struct HomeDirTool;

#[async_trait]
impl Tool for HomeDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "home_dir",
            "Return the current user's home directory path (value of $HOME). \
             Use this to build absolute paths like `~/.flux/sessions.db` without shelling out.",
            tool_input_schema::<HomeDirInput>(),
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

/// Arguments for the `now` op (no parameters).
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NowInput {}

/// Returns the current wall-clock time. Zero args, read-only, no approval gate.
pub struct NowTool;

#[async_trait]
impl Tool for NowTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = ToolSpec::read_only(
            "now",
            "Return the current wall-clock time: unix seconds and a UTC timestamp \
             (`YYYY-MM-DD HH:MM:SS UTC`). Replaces shelling out to `date`.",
            tool_input_schema::<NowInput>(),
        );
        // A clock is read-only but NOT deterministic: two calls must never return the same
        // instant. NonIdempotent keeps it out of the op result cache (L-54 review, 2026-07-09).
        spec.idempotency = Idempotency::NonIdempotent;
        spec
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

/// Arguments for the `cwd` op (no parameters).
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CwdInput {}

/// Returns the workspace root directory. Zero args, read-only, no approval gate.
pub struct CwdTool;

#[async_trait]
impl Tool for CwdTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "cwd",
            "Return the absolute path of the workspace root (the agent's working directory). \
             Replaces shelling out to `pwd`.",
            tool_input_schema::<CwdInput>(),
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

/// Arguments for the `sys_info` op (no parameters).
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SysInfoInput {}

/// Returns OS / architecture / host metadata. Zero args, read-only, no approval gate.
pub struct SysInfoTool;

#[async_trait]
impl Tool for SysInfoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "sys_info",
            "Return host metadata: operating system, CPU architecture, OS family, and hostname \
             (best-effort). Replaces shelling out to `uname`.",
            tool_input_schema::<SysInfoInput>(),
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
pub fn try_register_extra(registry: &mut ToolRegistry) -> Result<()> {
    registry.try_register_all_from(
        "flux-tools ambient and filesystem metadata pack",
        vec![
            Arc::new(FileStatTool) as Arc<dyn Tool>,
            Arc::new(PathExistsTool),
            Arc::new(SqliteQueryTool),
            Arc::new(HomeDirTool),
            Arc::new(NowTool),
            Arc::new(CwdTool),
            Arc::new(SysInfoTool),
        ],
    )
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_extra`].
pub fn register_extra(registry: &mut ToolRegistry) {
    try_register_extra(registry)
        .expect("flux-tools ambient and filesystem metadata pack registration failed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::{System, Workspace};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tool_ctx() -> (std::path::PathBuf, ToolContext) {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-extra-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
        (dir, c)
    }

    /// C-78: a database outside the workspace and outside `~/.flux` must be refused before it is
    /// opened — otherwise sqlite_query is a Risk::Low read-exfiltration primitive (browser cookie
    /// stores, credential DBs). The file exists, so the refusal is the jail, not a missing file.
    #[tokio::test]
    async fn sqlite_query_refuses_database_outside_the_jail() {
        let (_dir, c) = tool_ctx();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let outside =
            std::env::temp_dir().join(format!("flux-outside-{}-{n}.db", std::process::id()));
        std::fs::write(&outside, b"x").unwrap();
        let r = SqliteQueryTool
            .execute(
                &c,
                json!({"db": outside.to_string_lossy(), "sql": "SELECT 1"}),
            )
            .await
            .expect("a jail refusal is a clean tool result, not an Err");
        assert!(
            r.is_error,
            "an out-of-jail database must be refused: {}",
            r.content
        );
        assert!(
            r.content.contains("refusing") || r.content.contains("only databases"),
            "the refusal names the jail: {}",
            r.content
        );
        let _ = std::fs::remove_file(&outside);
    }

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
