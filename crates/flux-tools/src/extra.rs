//! Extra built-in tools: file_stat, path_exists, sqlite_query, home_dir, now, cwd, sys_info.
//!
//! - `file_stat`    — file metadata (size, line count, mtime). Risk: Low.
//! - `path_exists`  — pure filesystem probe. Risk: Low.
//! - `sqlite_query` — read-only SQLite query; statement-type allowlist (SELECT/WITH/PRAGMA/EXPLAIN). Risk: Low.
//! - `home_dir`     — the user's home directory. Risk: Low.
//! - `now`          — current wall-clock time (unix seconds + UTC). Replaces `date`. Risk: Low.
//! - `cwd`          — the workspace root path. Replaces `pwd`. Risk: Low.
//! - `sys_info`     — OS / arch / host metadata. Replaces `uname`. Risk: Low.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{OperationPlacement, Tool, ToolContext, ToolRegistry, ToolResult};
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
            "Return metadata for a workspace file: size in bytes, line count, and last-modified \
             timestamp (Unix seconds). Replaces `wc -l`, `stat`, `ls -la` for routine metadata \
             checks.",
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

        let bytes = ctx.execution_system().read_file_bytes(path).await?;
        let size = bytes.len();
        // Count lines only for text files (skip binary sniff — just count \n bytes).
        let line_count = bytes.iter().filter(|&&b| b == b'\n').count();
        let mtime = ctx
            .execution_system()
            .file_mtime(path)
            .await
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // No mode is reported, deliberately (C-275). `std::fs::metadata` on the caller's raw
        // string would escape the jail — the reason the original author declined it — and
        // `System` exposes no guarded mode accessor to replace it with. Reporting nothing is the
        // honest option; the alternative is a field the op cannot fill. Do not "fix" this by
        // re-reading the file: the earlier version awaited a second `read_file_bytes` here and
        // discarded the bytes, paying a full read of an arbitrarily large file for no output.

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
        let exists = ctx.execution_system().file_mtime(path).await.is_ok();
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
    /// SQL to execute. Must begin with SELECT, WITH, PRAGMA, or EXPLAIN (read-only allowlist).
    sql: String,
    /// Max rows to return (default 200).
    #[serde(default)]
    limit: Option<u64>,
}

/// The statement types `sqlite_query` admits — an **allowlist**, not a denylist (C-193). The first
/// meaningful token of the SQL (after leading whitespace and SQL comments are stripped exactly as
/// SQLite's tokenizer skips them, see [`leading_statement_keyword`]) must be one of these, or the
/// statement is refused. Everything else — `VACUUM`, `ATTACH`, `INSERT`, … — is refused *as a
/// consequence of not being on the list*, not as a special-cased keyword. In particular this is how
/// `VACUUM INTO` (C-192) is closed: it can no longer reach the connection to create a file outside
/// guarded IO.
///
/// Why exactly these four:
/// - `SELECT`  — the primary read path.
/// - `WITH`    — common-table-expression reads (`WITH … SELECT`). `WITH` can also front DML
///   (`WITH … DELETE`), but such a write is still blocked by `SQLITE_OPEN_READ_ONLY`, and `WITH`
///   cannot express `VACUUM INTO`, so admitting it does not reopen the escape C-192 closes.
/// - `PRAGMA`  — schema/introspection pragmas (`PRAGMA table_info(…)`). A side-effecting pragma is
///   contained by the read-only connection and cannot write to an arbitrary path.
/// - `EXPLAIN` — `EXPLAIN [QUERY PLAN] <stmt>` returns the compiled program and never *executes* the
///   inner statement, so even `EXPLAIN VACUUM INTO …` is inert.
const ALLOWED_STATEMENT_KEYWORDS: [&str; 4] = ["SELECT", "WITH", "PRAGMA", "EXPLAIN"];

/// Return the leading statement keyword of `sql` — its first meaningful token, uppercased — after
/// stripping leading whitespace and SQL comments (`-- …` line comments and `/* … */` block comments,
/// including an unterminated block comment that runs to end of input) exactly as SQLite's tokenizer
/// skips them. Returns `None` when the input is only whitespace/comments or does not begin with an
/// identifier token.
///
/// This is what makes the allowlist see the statement *as SQLite will parse it*: a leading `/* … */`
/// or `-- …`, arbitrary whitespace, and letter case can no longer separate the admission check from
/// what the connection actually executes.
fn leading_statement_keyword(sql: &str) -> Option<String> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // `-- …` line comment: to end of line, or end of input.
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // `/* … */` block comment. SQLite accepts an unterminated one, running to end of input.
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        break;
    }
    // The leading identifier token: a run of ASCII alphanumerics / underscores.
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == start {
        return None;
    }
    Some(sql[start..i].to_ascii_uppercase())
}

/// True when `sql`'s leading statement type is on the read-only allowlist
/// ([`ALLOWED_STATEMENT_KEYWORDS`]).
fn is_allowed_sql(sql: &str) -> bool {
    leading_statement_keyword(sql)
        .is_some_and(|kw| ALLOWED_STATEMENT_KEYWORDS.contains(&kw.as_str()))
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
    if let Ok(p) = ctx.system().workspace().resolve_read(raw) {
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
    // flux-allow-direct-io: sqlite_query ~/.flux jail (C-78) — canonicalize the allowed base so the
    // comparison below is against a real, symlink-resolved path. Path resolution only, no content IO.
    let flux_home = std::fs::canonicalize(&flux_home).unwrap_or(flux_home);
    // flux-allow-direct-io: sqlite_query jail (C-78) — canonicalize the requested DB path to reject
    // symlink/`..` escapes before the read-only open below. Path resolution only, no content IO.
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
                          Admission is an allowlist over the statement type: the statement must \
                          begin with SELECT, WITH, PRAGMA, or EXPLAIN (a leading comment or \
                          whitespace is stripped first, so it cannot cloak the statement); \
                          anything else — VACUUM, ATTACH, INSERT, UPDATE, DELETE, DROP, … — is \
                          refused. Returns rows as a JSON array. `db` must be a database inside \
                          the workspace or under ~/.flux (e.g. ~/.flux/sessions.db); other \
                          on-disk databases are refused."
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

        // Admission is an allowlist over the statement type (C-193), evaluated against the SQL as
        // SQLite will parse it. This is also what closes VACUUM INTO (C-192): VACUUM is simply not on
        // the list, so it never reaches the connection.
        if !is_allowed_sql(sql) {
            return Ok(ToolResult::error(
                "sqlite_query: only SELECT / WITH / PRAGMA / EXPLAIN statements are admitted; the \
                 leading statement type is not on the read-only allowlist (VACUUM, ATTACH, INSERT, \
                 UPDATE, DELETE, DROP, … are refused, and a leading comment cannot cloak them)"
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
        //
        // DEVIATION from `docs/architecture.md`'s "all IO goes through flux-system" invariant: rusqlite
        // opens this file descriptor directly, not through flux-system, so flux-system's confinement,
        // symlink rejection and canonicalization do not cover it. The primitive is instead contained by
        // three guards in this op: `jail_sqlite_path` above (the `db` path stays inside the workspace or
        // under ~/.flux), `SQLITE_OPEN_READ_ONLY` (no write to the source), and the statement-type
        // allowlist (only SELECT/WITH/PRAGMA/EXPLAIN reach `prepare`, which is what stops `VACUUM INTO`
        // from creating a file at an arbitrary path — C-192). C-194 will add the mechanical no-direct-IO
        // lint that flags this call site so the deviation cannot spread silently.
        let result =
            tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Map<String, Value>>> {
                // flux-allow-direct-io: sqlite_query read-only exception (C-192) — contained by
                // jail_sqlite_path + SQLITE_OPEN_READ_ONLY + the SELECT/PRAGMA statement allowlist; see
                // the DEVIATION note above. This is the call site C-194's lint exists to keep visible.
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

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let home = ctx
            .execution_system()
            .env("HOME")
            .unwrap_or_else(|| String::from("/home"));
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
            ctx.execution_system().substrate_identity().workspace,
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
    let mut assembled = registry.clone();
    assembled.try_register_all_from_with_placement(
        "flux-tools ambient and filesystem metadata pack",
        vec![
            Arc::new(FileStatTool) as Arc<dyn Tool>,
            Arc::new(PathExistsTool),
            Arc::new(HomeDirTool),
            Arc::new(CwdTool),
        ],
        OperationPlacement::SelectedExecutionSystem,
    )?;
    assembled.try_register_all_from_with_placement(
        "flux-tools local ambient metadata pack",
        vec![Arc::new(NowTool) as Arc<dyn Tool>],
        OperationPlacement::LocalControlPlane,
    )?;
    assembled.try_register_all_from_with_placement(
        "flux-tools native metadata pack",
        vec![
            Arc::new(SqliteQueryTool) as Arc<dyn Tool>,
            Arc::new(SysInfoTool),
        ],
        OperationPlacement::NativeSystemOnly,
    )?;
    *registry = assembled;
    Ok(())
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
        let c = ToolContext::new(Arc::new(
            System::new(Workspace::new(&dir).unwrap())
                .with_worktree_base(crate::test_worktrees::pinned_worktree_base()),
        ));
        (dir, c)
    }

    /// Create a valid source SQLite database with a seeded table `t`, so that a `VACUUM INTO`
    /// against it has real content to copy and an `INSERT INTO t …` is stopped only by the
    /// read-only connection flag (not by a missing table).
    fn make_source_db(path: &std::path::Path) {
        let conn = rusqlite::Connection::open(path).expect("create source db");
        conn.execute_batch("CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (42);")
            .expect("seed source db");
    }

    /// C-192: `sqlite_query` is declared `Effect::Read` / `Risk::Low` and authorized as a workspace
    /// *read*. `VACUUM INTO '<absolute path>'` is read-only with respect to the *source* database, so
    /// `SQLITE_OPEN_READ_ONLY` does not stop it — it creates a brand-new file at an arbitrary absolute
    /// path entirely outside flux-system's guarded IO. Admission must refuse it; the proof is that the
    /// target file is never created. Red before the allowlist lands (the file appears), green after.
    #[tokio::test]
    async fn sqlite_query_vacuum_into_cannot_escape_the_workspace() {
        let (dir, c) = tool_ctx();
        let src = dir.join("source.db");
        make_source_db(&src);

        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let escape =
            std::env::temp_dir().join(format!("flux-vacuum-escape-{}-{n}.db", std::process::id()));
        let _ = std::fs::remove_file(&escape);

        let r = SqliteQueryTool
            .execute(
                &c,
                json!({
                    "db": "source.db",
                    "sql": format!("VACUUM INTO '{}'", escape.display()),
                }),
            )
            .await
            .expect("an admission refusal is a clean tool result, not an Err");

        let created = escape.exists();
        let _ = std::fs::remove_file(&escape);
        assert!(
            !created,
            "VACUUM INTO created a file outside guarded IO at {}",
            escape.display()
        );
        assert!(r.is_error, "the VACUUM INTO must be refused: {}", r.content);
    }

    /// C-192 regression: even when the `VACUUM INTO` target is *inside* the workspace, it is still a
    /// write performed outside flux-system by an op declaring only `Effect::Read`. The allowlist
    /// refuses it for the same reason (VACUUM is not an admitted statement type), and the internal
    /// target is never created.
    #[tokio::test]
    async fn sqlite_query_vacuum_into_inside_the_workspace_is_refused() {
        let (dir, c) = tool_ctx();
        let src = dir.join("source.db");
        make_source_db(&src);
        let target = dir.join("copy.db");
        let _ = std::fs::remove_file(&target);

        let r = SqliteQueryTool
            .execute(
                &c,
                json!({"db": "source.db", "sql": format!("VACUUM INTO '{}'", target.display())}),
            )
            .await
            .expect("a clean tool result");
        assert!(
            r.is_error,
            "a workspace-internal VACUUM INTO is still a misdeclared write: {}",
            r.content
        );
        assert!(
            !target.exists(),
            "the workspace-internal VACUUM target must not be created"
        );
    }

    /// C-193: a leading `/* … */` comment defeats the old `trim_start()` prefix denylist — the check
    /// saw `/*x*/`, not `INSERT`. The statement must be refused by flux's *own* admission (the
    /// allowlist), not merely bounce off `SQLITE_OPEN_READ_ONLY`, so the refusal must carry flux's
    /// admission message rather than SQLite's "readonly database" error. Red before, green after.
    #[tokio::test]
    async fn sqlite_query_refuses_a_comment_cloaked_write() {
        let (dir, c) = tool_ctx();
        let src = dir.join("t.db");
        make_source_db(&src);
        let r = SqliteQueryTool
            .execute(
                &c,
                json!({"db": "t.db", "sql": "/*x*/ INSERT INTO t VALUES (1)"}),
            )
            .await
            .expect("a clean tool result");
        assert!(
            r.is_error,
            "the comment-cloaked write must be refused: {}",
            r.content
        );
        assert!(
            r.content.contains("allowlist") || r.content.contains("admitted"),
            "the refusal is flux's admission check, not SQLite's read-only flag: {}",
            r.content
        );
    }

    /// C-193: the allowlist is applied to the statement *as SQLite parses it* — a leading block
    /// comment, leading whitespace and lower-case cannot separate the check from execution. A
    /// comment-cloaked, lower-case `select` is admitted and returns its row; a comment-cloaked,
    /// lower-case `vacuum into` is refused and its target never appears.
    #[tokio::test]
    async fn sqlite_query_allowlist_reads_past_comments_and_case() {
        let (dir, c) = tool_ctx();
        let src = dir.join("t.db");
        make_source_db(&src);

        let ok = SqliteQueryTool
            .execute(&c, json!({"db": "t.db", "sql": "  /*hi*/ select 1 as n"}))
            .await
            .expect("a clean tool result");
        assert!(
            !ok.is_error,
            "a comment/lower-case SELECT is a read and must be admitted: {}",
            ok.content
        );
        assert!(
            ok.content.contains("\"n\":1"),
            "the SELECT returned its row: {}",
            ok.content
        );

        let escape = dir.join("cloaked.db");
        let _ = std::fs::remove_file(&escape);
        let bad = SqliteQueryTool
            .execute(
                &c,
                json!({"db": "t.db", "sql": format!("/*c*/ vacuum into '{}'", escape.display())}),
            )
            .await
            .expect("a clean tool result");
        assert!(
            bad.is_error,
            "a comment/lower-case VACUUM must be refused: {}",
            bad.content
        );
        assert!(
            !escape.exists(),
            "the comment-cloaked VACUUM target must not be created"
        );
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

    // -----------------------------------------------------------------------
    // C-275: `file_stat` is metadata-only, and says nothing about mode
    // -----------------------------------------------------------------------

    /// The section banner this file uses between op declarations.
    const SECTION_BANNER: &str =
        "\n// ---------------------------------------------------------------------------";

    /// Every guarded call that pulls a file's **whole contents** into memory. `file_stat` needs
    /// exactly one of these (for `line_count`); a second is by definition read-and-discard.
    const WHOLE_CONTENT_READS: &[&str] = &[
        ".read_file_bytes(",
        ".read_file_bytes_capped(",
        ".read_file(",
        ".read_file_scoped(",
        ".read_optional_text(",
    ];

    /// This file's `file_stat` declaration: `pub struct FileStatTool` up to the next banner.
    fn file_stat_section() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/extra.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let start = src
            .find("\npub struct FileStatTool")
            .expect("`pub struct FileStatTool` is gone — this scan lost its anchor, fix the scan");
        let rest = &src[start + 1..];
        let end = rest.find(SECTION_BANNER).unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// C-275 / envelope-integrity finding 4. `file_stat` used to await `read_file_bytes` a second
    /// time and feed the bytes to `.map(|_| …)`, paying a full read of an arbitrarily large file
    /// for nothing. Confinement was never at risk — the cost was. Behaviour cannot witness a
    /// discarded read (the emitted JSON is identical either way), so the contract is pinned at the
    /// source: **one** whole-content read in the whole declaration. Reintroduce a second guarded
    /// read anywhere in `FileStatTool` — for a binary sniff, a mode probe, anything — and this
    /// fails with the count. It cannot pass vacuously: losing the anchor or the op's own markers
    /// panics rather than scanning an empty string.
    #[test]
    fn file_stat_reads_the_target_exactly_once() {
        let section = file_stat_section();
        assert!(
            section.contains("\"file_stat\"") && section.contains("line_count"),
            "the scanned section is not `file_stat`'s — the scan needs updating, not silencing"
        );

        let counts: Vec<(&str, usize)> = WHOLE_CONTENT_READS
            .iter()
            .map(|form| (*form, section.matches(form).count()))
            .filter(|(_, n)| *n > 0)
            .collect();
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        assert_eq!(
            total, 1,
            "`file_stat` must read the target exactly once (line counting); found {total}: {counts:?}"
        );
    }

    /// C-275. The op reports **no** mode at all — the deliberate choice over reporting one, because
    /// an honest mode needs a guarded accessor on `System` that does not exist, and the only other
    /// route is `std::fs::metadata` on the caller's raw string, which escapes the jail (the reason
    /// the original author declined it, and what `scripts/check-no-direct-io.sh` refuses). Silence
    /// is then the honest contract, and it has to be silent *everywhere*: the model reads the spec
    /// description, which used to advertise "octal mode" that no field ever carried.
    #[tokio::test]
    async fn file_stat_reports_no_mode_anywhere_in_its_contract() {
        let (dir, c) = tool_ctx();
        std::fs::write(dir.join("sample.txt"), "alpha\nbeta\n").unwrap();

        let description = FileStatTool.spec().description.to_lowercase();
        assert!(
            !description.contains("mode"),
            "the spec description promises the model a mode it never returns: {description}"
        );

        let r = FileStatTool
            .execute(&c, json!({"path": "sample.txt"}))
            .await
            .expect("file_stat on a workspace file succeeds");
        let parsed: Value = serde_json::from_str(&r.content).expect("content is JSON");
        let mut keys: Vec<&str> = parsed
            .as_object()
            .expect("content is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["line_count", "mtime_unix", "path", "size_bytes"],
            "file_stat's emitted contract changed"
        );
        assert_eq!(parsed["size_bytes"], 11);
        assert_eq!(parsed["line_count"], 2);

        let view = r.view.unwrap_or_default().to_lowercase();
        assert!(
            !r.content.to_lowercase().contains("mode") && !view.contains("mode"),
            "file_stat mentions mode without reporting one: {} / {view}",
            r.content
        );
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
