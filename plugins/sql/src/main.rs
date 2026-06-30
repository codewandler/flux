//! `sql` — a flux integration plugin for **read-only** SQL introspection and bounded query, ported
//! from the fluxplane `sql` plugin. It is the hardest plugin in the pack: real async SQL driver crates
//! own their own socket and cannot sit on flux's synchronous, host-proxied byte stream, so this plugin
//! carries a **hand-rolled PostgreSQL wire-protocol client** that runs entirely over the host `conn.*`
//! capability (via [`host_kit::ConnStream`]). The plugin opens no socket and reads no env itself: the
//! host dials TCP, and credentials come back through the gated `secret` capability.
//!
//! ## Dialects
//! - **PostgreSQL** — fully implemented: StartupMessage → Authentication (Ok / cleartext / MD5 /
//!   SASL SCRAM-SHA-256) → the Simple Query protocol. All six read ops run parameter-free, whitelisted
//!   introspection SQL over Simple Query and shape the rows into the same JSON the fluxplane reference
//!   returns.
//! - **MySQL** — *not yet supported*. Routed to a clear error; a minimal handshake-v10 + query client
//!   is the residual (see the module note on [`open_target`]).
//! - **SQLite** — *unsupported by design*. SQLite is a local file and flux plugins have no filesystem
//!   capability (`conn.*` is sockets only); a host file capability would be required.
//!
//! ## Honesty note on interop confidence
//! The PostgreSQL client is exercised by `MockHost` tests that replay **hand-crafted** server frames.
//! Those prove the *frame parser and message assembly* are correct against bytes the test author wrote;
//! they are **not** a live-interop test against a real `postgres` server. The protocol is implemented to
//! the spec (length-prefixed messages, the documented Authentication subtypes, RowDescription/DataRow/
//! CommandComplete/ErrorResponse/ReadyForQuery), and SCRAM is covered by a dedicated unit test against
//! the RFC 5802 / RFC 7677 client-key derivation, but first contact with a real server is unverified.

use host_kit::*;
use serde_json::{json, Map, Value};
use std::io::{Read, Write};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

// ===========================================================================
// Manifest
// ===========================================================================

const PG_DEFAULT_PORT: u16 = 5432;
const MYSQL_DEFAULT_PORT: u16 = 3306;

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("sql", "0.1.0")
        .capabilities(Caps {
            // Sockets only — the conn allow-list covers the two SQL ports. (SSRF-guarded host-side.)
            conn: vec![
                format!("tcp:*:{PG_DEFAULT_PORT}"),
                format!("tcp:*:{MYSQL_DEFAULT_PORT}"),
            ],
            private_hosts: vec!["*".into()],
            secrets: vec![
                "SQL_USERNAME".into(),
                "SQL_PASSWORD".into(),
                "MYSQL_USERNAME".into(),
                "MYSQL_PASSWORD".into(),
            ],
            ..Default::default()
        })
        // Credentials resolved by purpose. The handshake needs the *raw* values (it builds its own
        // SCRAM/MD5 proof on the wire), so it fetches them via `host.secret` rather than the HTTP
        // auth-injection path — there is no HTTP here.
        .auth(AuthMethod {
            purpose: "username".into(),
            env: vec!["SQL_USERNAME".into(), "MYSQL_USERNAME".into()],
            description: "SQL username".into(),
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "password".into(),
            env: vec!["SQL_PASSWORD".into(), "MYSQL_PASSWORD".into()],
            description: "SQL password".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "sql.endpoint".into(),
            env: vec!["SQL_DSN".into(), "SQL_URL".into()],
            http_hosts: Vec::new(),
            description: "SQL connection DSN/URL, e.g. postgres://host:5432/db".into(),
        })
        .datasource(Declaration {
            name: "sql.query_rows".into(),
            entity: "sql.query_result".into(),
            description: Some("SQL query result rows.".into()),
            capabilities: vec!["search".into()],
            entity_schema: None,
        })
        .operation(
            read_op(
                "sql.test",
                "Test SQL endpoint connectivity with a SELECT 1 round trip; reports the server version.",
                so(conn_props(), json!([])),
            ),
            op_test,
        )
        .operation(
            read_op(
                "sql.query",
                "Run a bounded, read-only SQL query (SELECT/SHOW/DESCRIBE/EXPLAIN/WITH only) against the endpoint.",
                so(
                    merge(
                        conn_props(),
                        json!({
                            "query": {"type": "string", "description": "Read-only SQL query."},
                            "max_rows": {"type": "integer", "description": "Max rows (default 100, capped 1000)."},
                        }),
                    ),
                    json!(["query"]),
                ),
            ),
            op_query,
        )
        .operation(
            read_op(
                "sql.database.list",
                "List databases and the connected database's non-system schemas.",
                so(conn_props(), json!([])),
            ),
            op_database_list,
        )
        .operation(
            read_op(
                "sql.table.list",
                "List tables (optionally views) with a cheap row estimate where the engine keeps statistics.",
                so(
                    merge(
                        conn_props(),
                        json!({
                            "schema": {"type": "string"},
                            "include_views": {"type": "boolean"},
                            "max_results": {"type": "integer", "description": "Max tables (default 200, capped 1000)."},
                        }),
                    ),
                    json!([]),
                ),
            ),
            op_table_list,
        )
        .operation(
            read_op(
                "sql.table.show",
                "Describe a table: columns with types and nullability, the primary key, and foreign keys.",
                so(
                    merge(
                        conn_props(),
                        json!({"schema": {"type": "string"}, "table": {"type": "string"}}),
                    ),
                    json!(["table"]),
                ),
            ),
            op_table_show,
        )
        .operation(
            read_op(
                "sql.index.list",
                "List indexes across a schema or for one table, with columns and uniqueness.",
                so(
                    merge(
                        conn_props(),
                        json!({"schema": {"type": "string"}, "table": {"type": "string"}}),
                    ),
                    json!([]),
                ),
            ),
            op_index_list,
        )
}

/// The connection fields every op accepts (the `endpoint_ref`/driver/database trio mirrors the
/// reference). `endpoint_ref` is optional here: flux resolves the single declared `sql.endpoint`.
fn conn_props() -> Value {
    json!({
        "endpoint_ref": {"type": "string", "description": "Registered SQL endpoint name (default sql.endpoint)."},
        "driver": {"type": "string", "description": "Dialect override: postgres|mysql|sqlite.", "enum": ["postgres", "mysql", "sqlite"]},
        "database": {"type": "string", "description": "Database override."}
    })
}

/// `{ "type": "object", "properties": <props>, "required": <required> }`.
fn so(props: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// Shallow-merge two JSON objects (right wins) — used to extend `conn_props()` per op.
fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

fn main() {
    manifest_builder().serve();
}

// ===========================================================================
// Target resolution — DSN/URL → dialect + host/port/database (+ creds from the DSN)
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Dialect {
    Postgres,
    MySql,
    Sqlite,
}

impl Dialect {
    fn label(&self) -> &'static str {
        match self {
            Dialect::Postgres => "postgres",
            Dialect::MySql => "mysql",
            Dialect::Sqlite => "sqlite",
        }
    }
}

/// A resolved connection target: where to dial and which database, plus a redacted URL for output.
#[derive(Debug, Clone)]
struct SqlTarget {
    dialect: Dialect,
    host: String,
    port: u16,
    database: String,
    /// Username parsed from the DSN userinfo (a `host.secret("username")` overrides it when set).
    dsn_user: Option<String>,
    /// Password parsed from the DSN userinfo (a `host.secret("password")` overrides it when set).
    dsn_password: Option<String>,
    /// A password-redacted form of the URL, surfaced as `endpoint_url`.
    safe_url: String,
}

/// Normalize a dialect override / URL scheme to a [`Dialect`] (matching the reference aliases).
fn normalize_dialect(value: &str) -> Option<Dialect> {
    match value.trim().to_ascii_lowercase().as_str() {
        "mysql" | "mariadb" => Some(Dialect::MySql),
        "postgres" | "postgresql" | "pg" | "pgx" => Some(Dialect::Postgres),
        "sqlite" | "sqlite3" | "file" => Some(Dialect::Sqlite),
        "" => None,
        _ => None,
    }
}

/// Resolve the endpoint DSN (from the host) + any input overrides into a [`SqlTarget`].
fn resolve_target(input: &Value, host: &mut Host) -> Result<SqlTarget, String> {
    let endpoint_ref = flex_str(input, "endpoint_ref").unwrap_or_else(|| "sql.endpoint".into());
    let raw_url = host.endpoint(&endpoint_ref)?;
    let raw_url = raw_url.trim();
    if raw_url.is_empty() {
        return Err("endpoint has no url".into());
    }
    target_from_url(
        flex_str(input, "driver").as_deref(),
        raw_url,
        flex_str(input, "database").as_deref(),
    )
}

/// Parse a `scheme://[user[:pass]@]host[:port]/database` DSN into a [`SqlTarget`]. The dialect comes
/// from `driver_override` or the URL scheme. This is a deliberately small URL parser (no external
/// `url` crate) covering the SQL-DSN shape; query strings after `?` are ignored.
fn target_from_url(
    driver_override: Option<&str>,
    raw_url: &str,
    database_override: Option<&str>,
) -> Result<SqlTarget, String> {
    let (scheme, rest) = raw_url
        .split_once("://")
        .ok_or("endpoint URL must be scheme://… (e.g. postgres://host/db)")?;
    let dialect = driver_override
        .and_then(normalize_dialect)
        .or_else(|| normalize_dialect(scheme))
        .ok_or_else(|| format!("unsupported SQL URL scheme {scheme:?}"))?;

    // Strip a trailing `?query` and split userinfo from host/path.
    let rest = rest.split('?').next().unwrap_or(rest);
    let (userinfo, hostpath) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };
    let (dsn_user, dsn_password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((user, pass)) => (
                opt_nonempty(pct_decode(user)),
                opt_nonempty(pct_decode(pass)),
            ),
            None => (opt_nonempty(pct_decode(u)), None),
        },
        None => (None, None),
    };

    let (hostport, path) = match hostpath.split_once('/') {
        Some((hp, p)) => (hp, p),
        None => (hostpath, ""),
    };
    let mut database = path.trim_matches('/').to_string();
    if let Some(db) = database_override {
        if !db.trim().is_empty() {
            database = db.trim().to_string();
        }
    }

    if dialect == Dialect::Sqlite {
        return Err(
            "sqlite unsupported (needs a host file capability): flux plugins have no filesystem \
             access and conn.* is sockets only"
                .into(),
        );
    }

    let (host, port_str) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (hostport, None),
    };
    if host.trim().is_empty() {
        return Err("endpoint URL has no host".into());
    }
    let default_port = match dialect {
        Dialect::Postgres => PG_DEFAULT_PORT,
        Dialect::MySql => MYSQL_DEFAULT_PORT,
        Dialect::Sqlite => unreachable!(),
    };
    let port = match port_str {
        Some(p) => p
            .parse::<u16>()
            .map_err(|_| format!("invalid port {p:?} in endpoint URL"))?,
        None => default_port,
    };

    let safe_user = dsn_user.clone().unwrap_or_default();
    let mut safe_url = format!("{}://", dialect.label());
    if !safe_user.is_empty() {
        safe_url.push_str(&safe_user);
        if dsn_password.is_some() {
            safe_url.push_str(":xxxxx");
        }
        safe_url.push('@');
    }
    safe_url.push_str(host);
    safe_url.push(':');
    safe_url.push_str(&port.to_string());
    if !database.is_empty() {
        safe_url.push('/');
        safe_url.push_str(&database);
    }

    Ok(SqlTarget {
        dialect,
        host: host.to_string(),
        port,
        database,
        dsn_user,
        dsn_password,
        safe_url,
    })
}

/// Resolve the effective `(user, password, database)` for the handshake: host secrets win over the
/// DSN userinfo; the Postgres database defaults to the user when unset (libpq behavior).
fn resolve_credentials(
    target: &SqlTarget,
    host: &mut Host,
) -> Result<(String, String, String), String> {
    let user = host
        .secret("username")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| target.dsn_user.clone())
        .unwrap_or_else(|| match target.dialect {
            Dialect::Postgres => "postgres".into(),
            Dialect::MySql => "root".into(),
            Dialect::Sqlite => String::new(),
        });
    let password = host
        .secret("password")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| target.dsn_password.clone())
        .unwrap_or_default();
    let database = if target.database.trim().is_empty() {
        user.clone()
    } else {
        target.database.clone()
    };
    Ok((user, password, database))
}

// ===========================================================================
// Input helpers (mirroring gitlab's small validators)
// ===========================================================================

fn flex_str(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn flex_i64(input: &Value, key: &str) -> Option<i64> {
    match input.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn flex_bool(input: &Value, key: &str) -> bool {
    input.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn clamp(value: i64, default: i64, max: i64) -> i64 {
    if value <= 0 {
        default
    } else if value > max {
        max
    } else {
        value
    }
}

fn opt_nonempty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Minimal percent-decode for DSN userinfo (e.g. `p%40ss` → `p@ss`).
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ===========================================================================
// Read-only query whitelist (ported from the fluxplane tokenizer)
// ===========================================================================

const READ_ONLY_MSG: &str =
    "SQL query must be read-only; allowed statements are SELECT, SHOW, DESCRIBE, EXPLAIN, and WITH";

#[derive(Debug)]
struct SqlToken {
    text: String,
    /// Whether the word is immediately followed by `(` (a function-call form).
    call: bool,
}

/// Tokenize SQL into lowercased identifier words, skipping string literals (`'…'`, `"…"`, backticks)
/// and comments (`-- …`, `# …`, `/* … */`). Returns the tokens and whether a top-level `;` separator
/// (a second statement) was seen.
fn sql_tokens(query: &str) -> (Vec<SqlToken>, bool) {
    let bytes = query.as_bytes();
    let mut tokens: Vec<SqlToken> = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    let flush = |current: &mut String, tokens: &mut Vec<SqlToken>, next: Option<u8>| {
        if current.is_empty() {
            return;
        }
        tokens.push(SqlToken {
            text: std::mem::take(current),
            call: next == Some(b'('),
        });
    };
    while i < bytes.len() {
        let ch = bytes[i];
        match ch {
            b';' => {
                flush(&mut current, &mut tokens, None);
                return (tokens, true);
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                flush(&mut current, &mut tokens, None);
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'#' => {
                flush(&mut current, &mut tokens, None);
                i += 1;
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                flush(&mut current, &mut tokens, None);
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'\'' | b'"' | b'`' => {
                flush(&mut current, &mut tokens, None);
                let quote = ch;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && quote != b'`' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] != quote {
                        i += 1;
                        continue;
                    }
                    // Doubled `''` is an escaped quote inside a single-quoted literal.
                    if quote == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
                continue;
            }
            b'_' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => {
                current.push(ch.to_ascii_lowercase() as char);
            }
            other => {
                flush(&mut current, &mut tokens, Some(other));
            }
        }
        i += 1;
    }
    flush(&mut current, &mut tokens, None);
    (tokens, false)
}

/// Whether `query` is a single read-only statement. Rejects multi-statement input, write CTEs, and
/// `INTO OUTFILE`/`DUMPFILE`; allows write-keyword *function* forms like `REPLACE(...)` mid-expression.
fn read_only_query(query: &str) -> bool {
    let trimmed = query.trim().trim_start_matches('(').trim();
    if trimmed.is_empty() {
        return false;
    }
    let (tokens, has_separator) = sql_tokens(trimmed);
    if has_separator || tokens.is_empty() {
        return false;
    }
    match tokens[0].text.as_str() {
        "select" | "show" | "describe" | "desc" | "explain" | "with" => {}
        _ => return false,
    }
    for (idx, token) in tokens.iter().enumerate() {
        match token.text.as_str() {
            "insert" | "replace" => {
                // REPLACE(...)/INSERT(...) are string functions when used as a call mid-statement.
                if token.call && idx > 0 {
                    continue;
                }
                return false;
            }
            "update" | "delete" | "drop" | "create" | "alter" | "truncate" | "grant" | "revoke"
            | "call" | "do" | "load" | "copy" | "execute" | "merge" => return false,
            "outfile" | "dumpfile" => {
                if idx > 0 && tokens[idx - 1].text == "into" {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

// ===========================================================================
// Operation handlers
// ===========================================================================

fn op_test(input: Value, host: &mut Host) -> Result<Value, String> {
    let target = resolve_target(&input, host)?;
    require_postgres(&target)?;
    let (user, password, database) = resolve_credentials(&target, host)?;

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &target.host,
        port: target.port,
    })?;
    let result = (|| -> Result<Value, String> {
        let mut pg = PgClient::connect(host, cid, &user, &password, &database)?;
        let res = pg.simple_query("SELECT 1")?;
        let _ = res; // connectivity only; the value is unused
        Ok(json!({
            "status": "ok",
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "database": database,
            "server_version": pg.server_version.clone().unwrap_or_default(),
        }))
    })();
    host.conn_close(cid)?;
    result
}

fn op_query(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = flex_str(&input, "query").ok_or("`query` (string) required")?;
    if !read_only_query(&query) {
        return Err(READ_ONLY_MSG.into());
    }
    let max_rows = clamp(flex_i64(&input, "max_rows").unwrap_or(0), 100, 1000) as usize;
    let target = resolve_target(&input, host)?;
    require_postgres(&target)?;
    let (user, password, database) = resolve_credentials(&target, host)?;

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &target.host,
        port: target.port,
    })?;
    let shaped = (|| -> Result<Value, String> {
        let mut pg = PgClient::connect(host, cid, &user, &password, &database)?;
        let result = pg.simple_query(&query)?;
        let (rows, truncated) = bounded_rows(&result, max_rows);
        Ok(json!({
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "database": database,
            "columns": result.columns,
            "rows": rows,
            "row_count": rows_len(&result, max_rows),
            "truncated": truncated,
        }))
    })();
    host.conn_close(cid)?;
    let shaped = shaped?;

    // Contribute the result rows as searchable records (best-effort; matches the reference datasource).
    contribute_rows(host, &shaped, &query);
    Ok(shaped)
}

fn op_database_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let target = resolve_target(&input, host)?;
    require_postgres(&target)?;
    let (user, password, database) = resolve_credentials(&target, host)?;

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &target.host,
        port: target.port,
    })?;
    let result = (|| -> Result<Value, String> {
        let mut pg = PgClient::connect(host, cid, &user, &password, &database)?;
        let mut databases: Vec<Value> = Vec::new();
        // Databases.
        let db_res = pg.simple_query(
            "SELECT datname AS name, pg_get_userbyid(datdba) AS owner, \
             datname = current_database() AS current_db \
             FROM pg_database WHERE NOT datistemplate ORDER BY datname",
        )?;
        for row in &db_res.rows {
            let name = cell(&db_res, row, "name");
            if name.is_empty() {
                continue;
            }
            databases.push(json!({
                "name": name,
                "kind": "database",
                "owner": cell(&db_res, row, "owner"),
                "current": truthy(&cell(&db_res, row, "current_db")),
            }));
        }
        // Non-system schemas of the connected database.
        let schema_res = pg.simple_query(
            "SELECT schema_name AS name FROM information_schema.schemata \
             WHERE schema_name NOT IN ('pg_catalog','information_schema') \
             AND schema_name NOT LIKE 'pg_%' ORDER BY schema_name",
        )?;
        for row in &schema_res.rows {
            let name = cell(&schema_res, row, "name");
            if name.is_empty() {
                continue;
            }
            databases.push(json!({ "name": name, "kind": "schema" }));
        }
        Ok(json!({
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "count": databases.len(),
            "databases": databases,
        }))
    })();
    host.conn_close(cid)?;
    result
}

fn op_table_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let target = resolve_target(&input, host)?;
    require_postgres(&target)?;
    let (user, password, database) = resolve_credentials(&target, host)?;
    let schema = flex_str(&input, "schema").unwrap_or_default();
    let include_views = flex_bool(&input, "include_views");
    let max_results = clamp(flex_i64(&input, "max_results").unwrap_or(0), 200, 1000) as usize;

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &target.host,
        port: target.port,
    })?;
    let result = (|| -> Result<Value, String> {
        let mut pg = PgClient::connect(host, cid, &user, &password, &database)?;
        let relkinds = if include_views {
            "('r','p','v','m')"
        } else {
            "('r','p')"
        };
        let sql = format!(
            "SELECT n.nspname AS table_schema, c.relname AS table_name, c.relkind::text AS table_type, \
             c.reltuples::bigint AS row_estimate \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN {relkinds} AND n.nspname NOT IN ('pg_catalog','information_schema') \
             AND n.nspname NOT LIKE 'pg_%' AND ('{s}' = '' OR n.nspname = '{s}') \
             ORDER BY n.nspname, c.relname",
            s = pg_lit(&schema),
        );
        let res = pg.simple_query(&sql)?;
        let mut tables: Vec<Value> = Vec::new();
        let mut truncated = false;
        for row in &res.rows {
            if tables.len() >= max_results {
                truncated = true;
                break;
            }
            let name = cell(&res, row, "table_name");
            if name.is_empty() {
                continue;
            }
            let mut obj = Map::new();
            obj.insert("name".into(), json!(name));
            obj.insert("schema".into(), json!(cell(&res, row, "table_schema")));
            obj.insert(
                "type".into(),
                json!(normalize_table_type(&cell(&res, row, "table_type"))),
            );
            if let Some(est) = parse_i64(&cell(&res, row, "row_estimate")) {
                if est >= 0 {
                    obj.insert("row_estimate".into(), json!(est));
                }
            }
            tables.push(Value::Object(obj));
        }
        Ok(json!({
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "database": database,
            "count": tables.len(),
            "truncated": truncated,
            "tables": tables,
        }))
    })();
    host.conn_close(cid)?;
    result
}

fn op_table_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let target = resolve_target(&input, host)?;
    require_postgres(&target)?;
    let (user, password, database) = resolve_credentials(&target, host)?;
    let table = flex_str(&input, "table").ok_or("`table` (string) required")?;
    let schema = flex_str(&input, "schema").unwrap_or_default();

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &target.host,
        port: target.port,
    })?;
    let result = (|| -> Result<Value, String> {
        let mut pg = PgClient::connect(host, cid, &user, &password, &database)?;

        // Columns.
        let col_sql = format!(
            "SELECT column_name, ordinal_position, data_type, udt_name, is_nullable, column_default, \
             character_maximum_length FROM information_schema.columns \
             WHERE table_schema = COALESCE(NULLIF('{s}',''),'public') AND table_name = '{t}' \
             ORDER BY ordinal_position",
            s = pg_lit(&schema),
            t = pg_lit(&table),
        );
        let col_res = pg.simple_query(&col_sql)?;
        if col_res.rows.is_empty() {
            return Err(format!("table {table:?} not found"));
        }
        let mut columns: Vec<Value> = Vec::new();
        for row in &col_res.rows {
            let mut obj = Map::new();
            obj.insert("name".into(), json!(cell(&col_res, row, "column_name")));
            if let Some(pos) = parse_i64(&cell(&col_res, row, "ordinal_position")) {
                obj.insert("position".into(), json!(pos));
            }
            obj.insert("data_type".into(), json!(cell(&col_res, row, "data_type")));
            obj.insert(
                "nullable".into(),
                json!(cell(&col_res, row, "is_nullable").eq_ignore_ascii_case("YES")),
            );
            let default = cell(&col_res, row, "column_default");
            if !default.is_empty() {
                obj.insert("default".into(), json!(default));
            }
            if let Some(max) = parse_i64(&cell(&col_res, row, "character_maximum_length")) {
                obj.insert("max_length".into(), json!(max));
            }
            columns.push(Value::Object(obj));
        }

        // Primary key.
        let pk_sql = format!(
            "SELECT kcu.column_name FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu ON kcu.constraint_name = tc.constraint_name \
             AND kcu.constraint_schema = tc.constraint_schema \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
             AND tc.table_schema = COALESCE(NULLIF('{s}',''),'public') AND tc.table_name = '{t}' \
             ORDER BY kcu.ordinal_position",
            s = pg_lit(&schema),
            t = pg_lit(&table),
        );
        let pk_res = pg.simple_query(&pk_sql)?;
        let mut primary_key: Vec<String> = Vec::new();
        for row in &pk_res.rows {
            let name = cell(&pk_res, row, "column_name");
            if !name.is_empty() {
                primary_key.push(name);
            }
        }
        // Flag the PK columns inline.
        for col in columns.iter_mut() {
            let cname = col.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if primary_key.iter().any(|p| p == cname) {
                col.as_object_mut()
                    .unwrap()
                    .insert("primary_key".into(), json!(true));
            }
        }

        // Foreign keys.
        let fk_sql = format!(
            "SELECT tc.constraint_name, kcu.column_name, ccu.table_name AS referenced_table_name, \
             ccu.column_name AS referenced_column_name FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu ON kcu.constraint_name = tc.constraint_name \
             AND kcu.constraint_schema = tc.constraint_schema \
             JOIN information_schema.constraint_column_usage ccu ON ccu.constraint_name = tc.constraint_name \
             AND ccu.constraint_schema = tc.constraint_schema \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
             AND tc.table_schema = COALESCE(NULLIF('{s}',''),'public') AND tc.table_name = '{t}' \
             ORDER BY tc.constraint_name, kcu.ordinal_position",
            s = pg_lit(&schema),
            t = pg_lit(&table),
        );
        let fk_res = pg.simple_query(&fk_sql)?;
        let foreign_keys = group_foreign_keys(&fk_res);

        Ok(json!({
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "database": database,
            "schema": schema,
            "table": table,
            "columns": columns,
            "primary_key": primary_key,
            "foreign_keys": foreign_keys,
        }))
    })();
    host.conn_close(cid)?;
    result
}

fn op_index_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let target = resolve_target(&input, host)?;
    require_postgres(&target)?;
    let (user, password, database) = resolve_credentials(&target, host)?;
    let schema = flex_str(&input, "schema").unwrap_or_default();
    let table = flex_str(&input, "table").unwrap_or_default();

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &target.host,
        port: target.port,
    })?;
    let result = (|| -> Result<Value, String> {
        let mut pg = PgClient::connect(host, cid, &user, &password, &database)?;
        let sql = format!(
            "SELECT n.nspname AS table_schema, t.relname AS table_name, i.relname AS index_name, \
             ix.indisunique, ix.indisprimary, am.amname, pg_get_indexdef(ix.indexrelid) AS definition \
             FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_class t ON t.oid = ix.indrelid JOIN pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_am am ON am.oid = i.relam \
             WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' \
             AND ('{s}' = '' OR n.nspname = '{s}') AND ('{tb}' = '' OR t.relname = '{tb}') \
             ORDER BY n.nspname, t.relname, i.relname",
            s = pg_lit(&schema),
            tb = pg_lit(&table),
        );
        let res = pg.simple_query(&sql)?;
        let mut indexes: Vec<Value> = Vec::new();
        for row in &res.rows {
            let definition = cell(&res, row, "definition");
            indexes.push(json!({
                "name": cell(&res, row, "index_name"),
                "table": cell(&res, row, "table_name"),
                "schema": cell(&res, row, "table_schema"),
                "columns": parse_index_def_columns(&definition),
                "unique": truthy(&cell(&res, row, "indisunique")),
                "primary": truthy(&cell(&res, row, "indisprimary")),
                "method": cell(&res, row, "amname"),
                "definition": definition,
            }));
        }
        Ok(json!({
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "database": database,
            "count": indexes.len(),
            "indexes": indexes,
        }))
    })();
    host.conn_close(cid)?;
    result
}

/// MySQL is the residual; SQLite is unsupported by design. Both error clearly so a misrouted call is
/// never silently half-handled.
fn require_postgres(target: &SqlTarget) -> Result<(), String> {
    match target.dialect {
        Dialect::Postgres => Ok(()),
        Dialect::MySql => Err(
            "mysql is not yet supported by the flux sql plugin (residual): the PostgreSQL wire \
             client is implemented; a minimal MySQL handshake-v10 + query client is future work"
                .into(),
        ),
        Dialect::Sqlite => {
            Err("sqlite unsupported (needs a host file capability): conn.* is sockets only".into())
        }
    }
}

// ===========================================================================
// Output-shaping helpers
// ===========================================================================

/// The value of column `name` in `row` as a string (empty when NULL/absent).
fn cell(res: &QueryResult, row: &[Option<String>], name: &str) -> String {
    res.columns
        .iter()
        .position(|c| c == name)
        .and_then(|i| row.get(i))
        .and_then(|v| v.clone())
        .unwrap_or_default()
}

/// Up to `max_rows` rows as `{column: value|null}` objects, plus whether more rows were dropped.
fn bounded_rows(res: &QueryResult, max_rows: usize) -> (Vec<Value>, bool) {
    let truncated = res.rows.len() > max_rows;
    let rows = res
        .rows
        .iter()
        .take(max_rows)
        .map(|row| {
            let mut obj = Map::new();
            for (i, col) in res.columns.iter().enumerate() {
                let v = row.get(i).and_then(|c| c.clone());
                obj.insert(col.clone(), v.map(Value::String).unwrap_or(Value::Null));
            }
            Value::Object(obj)
        })
        .collect();
    (rows, truncated)
}

fn rows_len(res: &QueryResult, max_rows: usize) -> usize {
    res.rows.len().min(max_rows)
}

fn parse_i64(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

/// Postgres returns booleans as `t`/`f` over the text protocol.
fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "t" | "true" | "1" | "yes" | "y"
    )
}

fn normalize_table_type(value: &str) -> String {
    match value.trim().to_ascii_uppercase().as_str() {
        "BASE TABLE" | "TABLE" | "R" | "P" => "table".into(),
        "VIEW" | "V" => "view".into(),
        "M" => "materialized_view".into(),
        _ => value.trim().to_ascii_lowercase(),
    }
}

/// Group foreign-key rows (one per column) into `{name, columns, ref_table, ref_columns}`.
fn group_foreign_keys(res: &QueryResult) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: std::collections::HashMap<String, (String, Vec<String>, Vec<String>)> =
        std::collections::HashMap::new();
    for row in &res.rows {
        let name = cell(res, row, "constraint_name");
        let entry = by_name.entry(name.clone()).or_insert_with(|| {
            order.push(name.clone());
            (
                cell(res, row, "referenced_table_name"),
                Vec::new(),
                Vec::new(),
            )
        });
        let col = cell(res, row, "column_name");
        if !col.is_empty() {
            entry.1.push(col);
        }
        let refcol = cell(res, row, "referenced_column_name");
        if !refcol.is_empty() {
            entry.2.push(refcol);
        }
    }
    order
        .into_iter()
        .map(|name| {
            let (ref_table, columns, ref_columns) = by_name.remove(&name).unwrap();
            json!({
                "name": name,
                "columns": columns,
                "ref_table": ref_table,
                "ref_columns": ref_columns,
            })
        })
        .collect()
}

/// Best-effort column extraction from a `pg_get_indexdef` string `… (a, b)`.
fn parse_index_def_columns(definition: &str) -> Vec<String> {
    let open = definition.find('(');
    let close = definition.rfind(')');
    match (open, close) {
        (Some(o), Some(c)) if c > o => definition[o + 1..c]
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Escape a value for inline interpolation into a single-quoted Postgres literal. These introspection
/// queries take only fixed internal literals plus user `schema`/`table` filters; doubling `'` and
/// rejecting NUL keeps a hostile name from breaking out of the literal.
fn pg_lit(s: &str) -> String {
    s.replace('\'', "''").replace('\0', "")
}

/// Contribute query result rows to the host datasource index (best-effort).
fn contribute_rows(host: &mut Host, shaped: &Value, query: &str) {
    let Some(rows) = shaped.get("rows").and_then(|r| r.as_array()) else {
        return;
    };
    let columns: Vec<String> = shaped
        .get("columns")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let endpoint_url = shaped
        .get("endpoint_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut records = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let title = columns
            .iter()
            .find_map(|c| {
                row.get(c)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .map(String::from)
            .unwrap_or_else(|| format!("row {}", i + 1));
        let id = format!("{endpoint_url}\x00{query}\x00{i}");
        records.push(Record::new(
            Source::new("sql"),
            "sql.query_result",
            &id,
            &title,
            row.to_string(),
        ));
    }
    let _ = host.contribute(&records);
}

// ===========================================================================
// PostgreSQL wire-protocol client (hand-rolled over the host ConnStream)
// ===========================================================================

/// A parsed Simple Query result: ordered column names and text-form row values (`None` = SQL NULL).
struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
}

/// A minimal blocking PostgreSQL frontend speaking the v3 protocol over a host [`ConnStream`].
/// Implements StartupMessage, the Authentication subtypes (Ok / cleartext / MD5 / SASL SCRAM-SHA-256),
/// and the Simple Query ('Q') protocol. Extended query / COPY / binary formats are out of scope — the
/// introspection queries are all text-format Simple Query.
struct PgClient<'h, 'a> {
    stream: ConnStream<'h, 'a>,
    server_version: Option<String>,
}

impl<'h, 'a> PgClient<'h, 'a> {
    /// Open a connection: send the startup packet, complete authentication, and drain to the first
    /// `ReadyForQuery`. `database` defaults to `user` upstream when the DSN names none.
    fn connect(
        host: &'h mut Host<'a>,
        conn_id: u64,
        user: &str,
        password: &str,
        database: &str,
    ) -> Result<PgClient<'h, 'a>, String> {
        let mut client = PgClient {
            stream: ConnStream::new(host, conn_id),
            server_version: None,
        };
        client.startup(user, database)?;
        client.authenticate(user, password)?;
        client.drain_to_ready()?;
        Ok(client)
    }

    /// Send the StartupMessage: int32 length, int32 protocol 196608 (3.0), then NUL-terminated
    /// `key\0value\0` pairs, ended by a final NUL.
    fn startup(&mut self, user: &str, database: &str) -> Result<(), String> {
        let mut params: Vec<u8> = Vec::new();
        params.extend_from_slice(&196608i32.to_be_bytes());
        for (k, v) in [
            ("user", user),
            ("database", database),
            ("application_name", "flux-plugin-sql"),
            ("client_encoding", "UTF8"),
        ] {
            params.extend_from_slice(k.as_bytes());
            params.push(0);
            params.extend_from_slice(v.as_bytes());
            params.push(0);
        }
        params.push(0);
        let mut msg = Vec::with_capacity(params.len() + 4);
        msg.extend_from_slice(&((params.len() + 4) as i32).to_be_bytes());
        msg.extend_from_slice(&params);
        self.write_all(&msg)
    }

    /// Drive authentication until `AuthenticationOk`. Handles cleartext, MD5, and SCRAM-SHA-256.
    fn authenticate(&mut self, user: &str, password: &str) -> Result<(), String> {
        loop {
            let (tag, body) = self.read_message()?;
            match tag {
                b'R' => {
                    let code = be_i32(&body, 0)?;
                    match code {
                        0 => return Ok(()), // AuthenticationOk
                        3 => {
                            // Cleartext password.
                            self.send_password_message(password.as_bytes())?;
                        }
                        5 => {
                            // MD5: salt is the 4 bytes after the code.
                            let salt = body.get(4..8).ok_or("pg: short MD5 salt")?;
                            let token = md5_password(user, password, salt);
                            self.send_password_message(token.as_bytes())?;
                        }
                        10 => {
                            // SASL: NUL-separated mechanism list. Require SCRAM-SHA-256.
                            let mechs = parse_cstring_list(&body[4..]);
                            if !mechs.iter().any(|m| m == "SCRAM-SHA-256") {
                                return Err(format!(
                                    "pg: server offered SASL mechanisms {mechs:?}; only SCRAM-SHA-256 is supported"
                                ));
                            }
                            self.scram_authenticate(password)?;
                        }
                        2 => return Err("pg: KerberosV5 auth unsupported".into()),
                        6 => return Err("pg: SCM credential auth unsupported".into()),
                        7 | 8 => return Err("pg: GSSAPI/SSPI auth unsupported".into()),
                        9 => return Err("pg: SSPI auth unsupported".into()),
                        other => return Err(format!("pg: unsupported auth request {other}")),
                    }
                }
                b'E' => return Err(format!("pg: {}", parse_error(&body))),
                other => {
                    return Err(format!(
                        "pg: unexpected message {:?} during authentication",
                        other as char
                    ))
                }
            }
        }
    }

    /// Run the full SCRAM-SHA-256 SASL exchange (RFC 5802 / RFC 7677) after the server offered it.
    fn scram_authenticate(&mut self, password: &str) -> Result<(), String> {
        let client_nonce = gen_nonce();
        let client_first_bare = format!("n=,r={client_nonce}");
        let client_first = format!("n,,{client_first_bare}");

        // SASLInitialResponse ('p'): mechanism CString + int32 length + the initial response bytes.
        let mut init: Vec<u8> = Vec::new();
        init.extend_from_slice(b"SCRAM-SHA-256");
        init.push(0);
        init.extend_from_slice(&(client_first.len() as i32).to_be_bytes());
        init.extend_from_slice(client_first.as_bytes());
        self.send_message(b'p', &init)?;

        // AuthenticationSASLContinue (R, code 11): the server-first message.
        let (tag, body) = self.read_message()?;
        let server_first = self.expect_sasl(tag, &body, 11, "SASLContinue")?;
        let attrs = parse_scram_attrs(&server_first);
        let combined_nonce = attrs
            .get("r")
            .cloned()
            .ok_or("pg scram: server-first missing nonce")?;
        if !combined_nonce.starts_with(&client_nonce) {
            return Err("pg scram: server nonce does not extend the client nonce".into());
        }
        let salt_b64 = attrs
            .get("s")
            .ok_or("pg scram: server-first missing salt")?;
        let iterations: u32 = attrs
            .get("i")
            .and_then(|i| i.parse().ok())
            .ok_or("pg scram: server-first missing/invalid iteration count")?;
        let salt = base64_decode(salt_b64)?;

        // SaltedPassword = PBKDF2-HMAC-SHA256(password, salt, i).
        let salted = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let channel_binding = base64_encode(b"n,,"); // GS2 header, no channel binding.
        let client_final_no_proof = format!("c={channel_binding},r={combined_nonce}");
        let auth_message = format!("{client_first_bare},{server_first},{client_final_no_proof}");
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let client_final = format!("{client_final_no_proof},p={}", base64_encode(&proof));

        // SASLResponse ('p'): the client-final message.
        self.send_message(b'p', client_final.as_bytes())?;

        // AuthenticationSASLFinal (R, code 12): verify the server signature.
        let (tag, body) = self.read_message()?;
        let server_final = self.expect_sasl(tag, &body, 12, "SASLFinal")?;
        let final_attrs = parse_scram_attrs(&server_final);
        let server_sig_b64 = final_attrs
            .get("v")
            .ok_or("pg scram: server-final missing verifier")?;
        let server_key = hmac_sha256(&salted, b"Server Key");
        let expected_sig = hmac_sha256(&server_key, auth_message.as_bytes());
        if base64_decode(server_sig_b64)? != expected_sig {
            return Err("pg scram: server signature verification failed".into());
        }
        // The following AuthenticationOk is consumed by the authenticate() loop.
        Ok(())
    }

    /// Expect a SASL Authentication message (`R`) with the given sub-code, returning its UTF-8 payload.
    fn expect_sasl(
        &self,
        tag: u8,
        body: &[u8],
        want_code: i32,
        what: &str,
    ) -> Result<String, String> {
        if tag == b'E' {
            return Err(format!("pg: {}", parse_error(body)));
        }
        if tag != b'R' {
            return Err(format!(
                "pg scram: expected Authentication ({what}), got {:?}",
                tag as char
            ));
        }
        let code = be_i32(body, 0)?;
        if code != want_code {
            return Err(format!(
                "pg scram: expected auth code {want_code} ({what}), got {code}"
            ));
        }
        Ok(String::from_utf8_lossy(&body[4..]).into_owned())
    }

    /// Send a PasswordMessage ('p') with a NUL-terminated payload (cleartext/MD5 path).
    fn send_password_message(&mut self, payload: &[u8]) -> Result<(), String> {
        let mut body = payload.to_vec();
        body.push(0);
        self.send_message(b'p', &body)
    }

    /// Run a Simple Query ('Q'): send the NUL-terminated SQL, then parse frames until ReadyForQuery.
    /// Returns the columns + text rows of the (last) RowDescription/DataRow set; ErrorResponse fails.
    fn simple_query(&mut self, sql: &str) -> Result<QueryResult, String> {
        let mut body = sql.as_bytes().to_vec();
        body.push(0);
        self.send_message(b'Q', &body)?;

        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        let mut error: Option<String> = None;
        loop {
            let (tag, body) = self.read_message()?;
            match tag {
                b'T' => columns = parse_row_description(&body)?,
                b'D' => rows.push(parse_data_row(&body)?),
                b'C' => {} // CommandComplete — tag/row-count summary, ignored.
                b'E' => error = Some(parse_error(&body)),
                b'Z' => break, // ReadyForQuery
                b'N' => {}     // NoticeResponse — ignored.
                b'S' => {}     // ParameterStatus mid-stream — ignored.
                _ => {}        // Other async messages (e.g. 'A' NotificationResponse) — ignored.
            }
        }
        if let Some(err) = error {
            return Err(format!("pg: {err}"));
        }
        Ok(QueryResult { columns, rows })
    }

    /// Drain messages until the first ReadyForQuery, capturing `server_version` from ParameterStatus.
    fn drain_to_ready(&mut self) -> Result<(), String> {
        loop {
            let (tag, body) = self.read_message()?;
            match tag {
                b'Z' => return Ok(()), // ReadyForQuery
                b'S' => {
                    // ParameterStatus: name\0value\0.
                    let parts = parse_cstring_list(&body);
                    if parts.len() >= 2 && parts[0] == "server_version" {
                        self.server_version = Some(parts[1].clone());
                    }
                }
                b'E' => return Err(format!("pg: {}", parse_error(&body))),
                b'K' => {} // BackendKeyData — ignored (no cancel support).
                b'N' => {} // NoticeResponse — ignored.
                _ => {}    // Other startup messages — ignored.
            }
        }
    }

    // --- framing ---

    /// Send a tagged message: 1 byte tag, int32 length (incl. itself), then `body`.
    fn send_message(&mut self, tag: u8, body: &[u8]) -> Result<(), String> {
        let mut msg = Vec::with_capacity(body.len() + 5);
        msg.push(tag);
        msg.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        msg.extend_from_slice(body);
        self.write_all(&msg)
    }

    /// Read one tagged backend message: 1 byte tag + int32 length, then `length-4` body bytes.
    fn read_message(&mut self) -> Result<(u8, Vec<u8>), String> {
        let mut header = [0u8; 5];
        self.read_exact(&mut header)?;
        let tag = header[0];
        let len = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        if len < 4 {
            return Err(format!("pg: invalid message length {len}"));
        }
        let body_len = (len - 4) as usize;
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            self.read_exact(&mut body)?;
        }
        Ok((tag, body))
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(data)
            .map_err(|e| format!("pg: write failed: {e}"))
    }

    /// Read exactly `buf.len()` bytes, looping over the chunked `conn.read`; EOF mid-read is an error.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self
                .stream
                .read(&mut buf[filled..])
                .map_err(|e| format!("pg: read failed: {e}"))?;
            if n == 0 {
                return Err("pg: connection closed mid-message (EOF)".into());
            }
            filled += n;
        }
        Ok(())
    }
}

// ===========================================================================
// Wire-frame parsing
// ===========================================================================

fn be_i32(buf: &[u8], at: usize) -> Result<i32, String> {
    buf.get(at..at + 4)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "pg: truncated int32".into())
}

/// Parse RowDescription ('T'): int16 field count, then per field name\0 + 18 fixed bytes.
fn parse_row_description(body: &[u8]) -> Result<Vec<String>, String> {
    if body.len() < 2 {
        return Err("pg: short RowDescription".into());
    }
    let count = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut columns = Vec::with_capacity(count);
    let mut i = 2;
    for _ in 0..count {
        let start = i;
        while i < body.len() && body[i] != 0 {
            i += 1;
        }
        if i >= body.len() {
            return Err("pg: unterminated column name in RowDescription".into());
        }
        columns.push(String::from_utf8_lossy(&body[start..i]).into_owned());
        i += 1; // NUL
        i += 18; // tableOID(4) colAttr(2) typeOID(4) typeLen(2) typeMod(4) format(2)
        if i > body.len() {
            return Err("pg: truncated RowDescription field".into());
        }
    }
    Ok(columns)
}

/// Parse DataRow ('D'): int16 column count, then per column int32 length (-1 = NULL) + value bytes.
fn parse_data_row(body: &[u8]) -> Result<Vec<Option<String>>, String> {
    if body.len() < 2 {
        return Err("pg: short DataRow".into());
    }
    let count = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut values = Vec::with_capacity(count);
    let mut i = 2;
    for _ in 0..count {
        let len = be_i32(body, i)?;
        i += 4;
        if len < 0 {
            values.push(None);
        } else {
            let len = len as usize;
            let bytes = body.get(i..i + len).ok_or("pg: truncated DataRow value")?;
            values.push(Some(String::from_utf8_lossy(bytes).into_owned()));
            i += len;
        }
    }
    Ok(values)
}

/// Parse ErrorResponse ('E') into a human message (the 'M' field, with the 'S'/'C' prefix when present).
fn parse_error(body: &[u8]) -> String {
    let mut severity = String::new();
    let mut code = String::new();
    let mut message = String::new();
    let mut i = 0;
    while i < body.len() && body[i] != 0 {
        let field = body[i];
        i += 1;
        let start = i;
        while i < body.len() && body[i] != 0 {
            i += 1;
        }
        let value = String::from_utf8_lossy(&body[start..i]).into_owned();
        i += 1; // NUL
        match field {
            b'S' => severity = value,
            b'C' => code = value,
            b'M' => message = value,
            _ => {}
        }
    }
    match (severity.is_empty(), code.is_empty()) {
        (false, false) => format!("{severity} {code}: {message}"),
        _ => message,
    }
}

/// Split a buffer of NUL-terminated C strings into a list (a trailing empty terminator is dropped).
fn parse_cstring_list(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == 0 {
            if i > start {
                out.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
            }
            start = i + 1;
        }
    }
    out
}

/// Parse `key=value,key=value` SCRAM attributes (values may contain `=`).
fn parse_scram_attrs(s: &str) -> std::collections::HashMap<String, String> {
    s.split(',')
        .filter_map(|part| part.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ===========================================================================
// Crypto primitives (SCRAM / MD5)
// ===========================================================================

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256(data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().to_vec()
}

/// PBKDF2-HMAC-SHA256 with a single 32-byte output block (SCRAM uses dkLen = hashLen, so block 1 only).
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    // U1 = HMAC(password, salt || INT(1)); Ui = HMAC(password, Ui-1); result = U1 ^ U2 ^ … ^ Uc.
    let mut salted = salt.to_vec();
    salted.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &salted);
    let mut result = u.clone();
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (r, x) in result.iter_mut().zip(u.iter()) {
            *r ^= *x;
        }
    }
    result
}

/// `md5` PostgreSQL password token: `"md5" + md5_hex(md5_hex(password+user) + salt)`.
fn md5_password(user: &str, password: &str, salt: &[u8]) -> String {
    let inner = md5_hex(format!("{password}{user}").as_bytes());
    let mut outer_input = inner.into_bytes();
    outer_input.extend_from_slice(salt);
    format!("md5{}", md5_hex(&outer_input))
}

/// A small, self-contained MD5 (RFC 1321) — used only for the legacy MD5 auth token. Postgres MD5 is
/// not a security boundary (the server picks it), so a vendored MD5 here avoids another dependency.
fn md5_hex(input: &[u8]) -> String {
    let digest = md5_digest(input);
    let mut s = String::with_capacity(32);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn md5_digest(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// A 24-char alphanumeric client nonce (SCRAM forbids `,` and `=`; alphanumerics are always safe).
fn gen_nonce() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("pg scram: bad base64: {e}"))
}

// ===========================================================================
// Tests — one MockHost test per op (hand-crafted server frames) + a SCRAM unit test.
//
// HONESTY: these replay author-written PostgreSQL frames. They prove the frame parser, message
// assembly, and JSON shaping — NOT live interop with a real server. AuthenticationOk is used for the
// connect handshake to keep the canned bytes small; SCRAM is covered separately by its own unit test.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- frame builders (the server side) ----

    fn msg(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut m = vec![tag];
        m.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        m.extend_from_slice(body);
        m
    }

    /// AuthenticationOk + a server_version ParameterStatus + ReadyForQuery('I' = idle).
    fn connect_frames() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(msg(b'R', &0i32.to_be_bytes())); // AuthenticationOk
        let mut ps = b"server_version\0".to_vec();
        ps.extend_from_slice(b"16.2\0");
        out.extend(msg(b'S', &ps));
        out.extend(msg(b'Z', b"I"));
        out
    }

    fn row_description(cols: &[&str]) -> Vec<u8> {
        let mut body = (cols.len() as u16).to_be_bytes().to_vec();
        for c in cols {
            body.extend_from_slice(c.as_bytes());
            body.push(0);
            body.extend_from_slice(&0i32.to_be_bytes()); // table oid
            body.extend_from_slice(&0i16.to_be_bytes()); // col attr
            body.extend_from_slice(&25i32.to_be_bytes()); // type oid (text)
            body.extend_from_slice(&(-1i16).to_be_bytes()); // type len
            body.extend_from_slice(&(-1i32).to_be_bytes()); // type mod
            body.extend_from_slice(&0i16.to_be_bytes()); // format (text)
        }
        msg(b'T', &body)
    }

    /// A DataRow where `None` encodes a SQL NULL.
    fn data_row(values: &[Option<&str>]) -> Vec<u8> {
        let mut body = (values.len() as u16).to_be_bytes().to_vec();
        for v in values {
            match v {
                None => body.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(s) => {
                    body.extend_from_slice(&(s.len() as i32).to_be_bytes());
                    body.extend_from_slice(s.as_bytes());
                }
            }
        }
        msg(b'D', &body)
    }

    fn command_complete(tag: &str) -> Vec<u8> {
        let mut body = tag.as_bytes().to_vec();
        body.push(0);
        msg(b'C', &body)
    }

    fn ready() -> Vec<u8> {
        msg(b'Z', b"I")
    }

    /// One query response: RowDescription + the given DataRows + CommandComplete + ReadyForQuery.
    fn query_response(cols: &[&str], rows: &[Vec<Option<&str>>]) -> Vec<u8> {
        let mut out = row_description(cols);
        for r in rows {
            out.extend(data_row(r));
        }
        out.extend(command_complete("SELECT"));
        out.extend(ready());
        out
    }

    /// A MockHost with the standard endpoint + credentials and a single canned conn stream made of
    /// the connect frames followed by `responses` (concatenated; one chunk so `read_exact` reframes).
    fn host_with(responses: Vec<Vec<u8>>) -> MockHost {
        let mut stream = connect_frames();
        for r in responses {
            stream.extend(r);
        }
        MockHost::default()
            .with_endpoint("sql.endpoint", "postgres://app@db.test:5432/warehouse")
            .with_secret("username", "app")
            .with_secret("password", "secret")
            .with_conn_response(stream)
    }

    fn run(op: &str, input: Value, host: &mut MockHost) -> Result<Value, String> {
        manifest_builder().build().call(op, input, host)
    }

    #[test]
    fn manifest_declares_six_read_ops_and_conn_caps() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 6);
        let names: Vec<&str> = m.operations.iter().map(|o| o.name.as_str()).collect();
        for want in [
            "sql.test",
            "sql.query",
            "sql.database.list",
            "sql.table.list",
            "sql.table.show",
            "sql.index.list",
        ] {
            assert!(names.contains(&want), "missing op {want}");
        }
        assert!(m.capabilities.conn.iter().any(|c| c.contains("5432")));
        assert!(m.capabilities.secrets.contains(&"SQL_PASSWORD".to_string()));
        // All read ops are idempotent reads.
        for op in &m.operations {
            assert_eq!(op.effects, vec![Effect::Read]);
        }
    }

    #[test]
    fn test_op_probes_connectivity_and_reports_version() {
        // sql.test runs SELECT 1, which still returns a row response we craft.
        let mut host = host_with(vec![query_response(&["?column?"], &[vec![Some("1")]])]);
        let out = run("sql.test", json!({}), &mut host).expect("sql.test");
        assert_eq!(out["status"], "ok");
        assert_eq!(out["driver"], "postgres");
        assert_eq!(out["database"], "warehouse");
        assert_eq!(out["server_version"], "16.2");
        // Redacted URL keeps the user, hides the (absent) password — no `secret` leaked.
        assert_eq!(out["endpoint_url"], "postgres://app@db.test:5432/warehouse");
    }

    #[test]
    fn query_op_shapes_rows_and_rejects_writes() {
        let mut host = host_with(vec![query_response(
            &["id", "email"],
            &[
                vec![Some("1"), Some("ada@example.com")],
                vec![Some("2"), None],
            ],
        )]);
        let out = run(
            "sql.query",
            json!({"query": "select id, email from users order by id limit 10", "max_rows": 10}),
            &mut host,
        )
        .expect("sql.query");
        assert_eq!(out["columns"], json!(["id", "email"]));
        assert_eq!(out["row_count"], 2);
        assert_eq!(out["rows"][0]["email"], "ada@example.com");
        // SQL NULL → JSON null.
        assert_eq!(out["rows"][1]["email"], Value::Null);
        assert_eq!(out["truncated"], false);
        // The rows were contributed as searchable records.
        assert_eq!(host.contributed.borrow().len(), 2);

        // A write is rejected before any connection is dialed.
        let mut h2 = host_with(vec![]);
        let err = run("sql.query", json!({"query": "delete from users"}), &mut h2).unwrap_err();
        assert!(err.contains("read-only"), "err = {err}");
    }

    #[test]
    fn database_list_op_lists_databases_and_schemas() {
        let mut host = host_with(vec![
            // pg_database query.
            query_response(
                &["name", "owner", "current_db"],
                &[
                    vec![Some("warehouse"), Some("app"), Some("t")],
                    vec![Some("postgres"), Some("postgres"), Some("f")],
                ],
            ),
            // information_schema.schemata query.
            query_response(&["name"], &[vec![Some("public")], vec![Some("reporting")]]),
        ]);
        let out = run("sql.database.list", json!({}), &mut host).expect("database.list");
        assert_eq!(out["count"], 4);
        assert_eq!(out["databases"][0]["name"], "warehouse");
        assert_eq!(out["databases"][0]["kind"], "database");
        assert_eq!(out["databases"][0]["current"], true);
        assert_eq!(out["databases"][2]["kind"], "schema");
        assert_eq!(out["databases"][2]["name"], "public");
    }

    #[test]
    fn table_list_op_lists_tables_with_estimates() {
        let mut host = host_with(vec![query_response(
            &["table_schema", "table_name", "table_type", "row_estimate"],
            &[
                vec![Some("public"), Some("users"), Some("r"), Some("42")],
                vec![Some("public"), Some("active_users"), Some("v"), Some("-1")],
            ],
        )]);
        let out =
            run("sql.table.list", json!({"include_views": true}), &mut host).expect("table.list");
        assert_eq!(out["count"], 2);
        assert_eq!(out["tables"][0]["name"], "users");
        assert_eq!(out["tables"][0]["type"], "table");
        assert_eq!(out["tables"][0]["row_estimate"], 42);
        assert_eq!(out["tables"][1]["type"], "view");
        // A negative reltuples estimate is dropped, not surfaced.
        assert!(out["tables"][1].get("row_estimate").is_none());
    }

    #[test]
    fn table_show_op_describes_columns_pk_and_fks() {
        let mut host = host_with(vec![
            // columns
            query_response(
                &[
                    "column_name",
                    "ordinal_position",
                    "data_type",
                    "udt_name",
                    "is_nullable",
                    "column_default",
                    "character_maximum_length",
                ],
                &[
                    vec![
                        Some("id"),
                        Some("1"),
                        Some("integer"),
                        Some("int4"),
                        Some("NO"),
                        None,
                        None,
                    ],
                    vec![
                        Some("user_id"),
                        Some("2"),
                        Some("integer"),
                        Some("int4"),
                        Some("NO"),
                        None,
                        None,
                    ],
                ],
            ),
            // primary key
            query_response(&["column_name"], &[vec![Some("id")]]),
            // foreign keys
            query_response(
                &[
                    "constraint_name",
                    "column_name",
                    "referenced_table_name",
                    "referenced_column_name",
                ],
                &[vec![
                    Some("orders_user_id_fkey"),
                    Some("user_id"),
                    Some("users"),
                    Some("id"),
                ]],
            ),
        ]);
        let out = run("sql.table.show", json!({"table": "orders"}), &mut host).expect("table.show");
        assert_eq!(out["table"], "orders");
        assert_eq!(out["columns"][0]["name"], "id");
        assert_eq!(out["columns"][0]["nullable"], false);
        assert_eq!(out["columns"][0]["primary_key"], true);
        assert_eq!(out["primary_key"], json!(["id"]));
        assert_eq!(out["foreign_keys"][0]["ref_table"], "users");
        assert_eq!(out["foreign_keys"][0]["columns"], json!(["user_id"]));
        assert_eq!(out["foreign_keys"][0]["ref_columns"], json!(["id"]));
    }

    #[test]
    fn index_list_op_lists_indexes_with_columns() {
        let mut host = host_with(vec![query_response(
            &[
                "table_schema",
                "table_name",
                "index_name",
                "indisunique",
                "indisprimary",
                "amname",
                "definition",
            ],
            &[
                vec![
                    Some("public"),
                    Some("users"),
                    Some("users_pkey"),
                    Some("t"),
                    Some("t"),
                    Some("btree"),
                    Some("CREATE UNIQUE INDEX users_pkey ON public.users USING btree (id)"),
                ],
                vec![
                    Some("public"),
                    Some("users"),
                    Some("users_name_idx"),
                    Some("f"),
                    Some("f"),
                    Some("btree"),
                    Some("CREATE INDEX users_name_idx ON public.users USING btree (name)"),
                ],
            ],
        )]);
        let out = run("sql.index.list", json!({"table": "users"}), &mut host).expect("index.list");
        assert_eq!(out["count"], 2);
        assert_eq!(out["indexes"][0]["name"], "users_pkey");
        assert_eq!(out["indexes"][0]["unique"], true);
        assert_eq!(out["indexes"][0]["primary"], true);
        assert_eq!(out["indexes"][0]["columns"], json!(["id"]));
        assert_eq!(out["indexes"][1]["unique"], false);
        assert_eq!(out["indexes"][1]["columns"], json!(["name"]));
    }

    #[test]
    fn mysql_and_sqlite_route_to_clear_errors() {
        let mut mysql = MockHost::default()
            .with_endpoint("sql.endpoint", "mysql://root@db.test:3306/app")
            .with_secret("username", "root")
            .with_secret("password", "");
        let err = run("sql.test", json!({}), &mut mysql).unwrap_err();
        assert!(err.contains("mysql is not yet supported"), "err = {err}");

        let mut sqlite = MockHost::default().with_endpoint("sql.endpoint", "sqlite:///tmp/app.db");
        let err = run("sql.test", json!({}), &mut sqlite).unwrap_err();
        assert!(err.contains("sqlite unsupported"), "err = {err}");
    }

    // ---- unit tests for the pure helpers ----

    #[test]
    fn read_only_query_allows_reads_and_function_forms_rejects_writes() {
        for ok in [
            "select 1",
            "SELECT REPLACE(name, 'a', 'b') FROM users",
            "select insert('abcdef', 2, 3, 'xy')",
            "with x as (select 1) select * from x",
            "select 'delete from users' as text",
            "select 1 -- delete from users\n",
            "select /* drop table users */ 1",
        ] {
            assert!(read_only_query(ok), "should allow: {ok}");
        }
        for bad in [
            "delete from users",
            "select 1; delete from users",
            "with deleted as (delete from users returning id) select * from deleted",
            "select * from users into outfile '/tmp/users'",
            "INSERT INTO users VALUES (1)",
            "drop table users",
            "",
        ] {
            assert!(!read_only_query(bad), "should reject: {bad}");
        }
    }

    #[test]
    fn target_from_url_parses_dialect_host_port_db_and_redacts() {
        let t =
            target_from_url(None, "postgres://app:s3cr3t@db.test:6543/warehouse", None).unwrap();
        assert_eq!(t.dialect, Dialect::Postgres);
        assert_eq!(t.host, "db.test");
        assert_eq!(t.port, 6543);
        assert_eq!(t.database, "warehouse");
        assert_eq!(t.dsn_user.as_deref(), Some("app"));
        assert_eq!(t.dsn_password.as_deref(), Some("s3cr3t"));
        assert!(
            !t.safe_url.contains("s3cr3t"),
            "password must be redacted: {}",
            t.safe_url
        );
        assert!(t.safe_url.contains("xxxxx"));

        // Default port + percent-decoded password.
        let t = target_from_url(None, "postgresql://u:p%40ss@h/db", None).unwrap();
        assert_eq!(t.port, PG_DEFAULT_PORT);
        assert_eq!(t.dsn_password.as_deref(), Some("p@ss"));

        // Driver override wins over scheme; sqlite is rejected at parse time.
        assert!(target_from_url(None, "sqlite:///x.db", None).is_err());
        let t = target_from_url(Some("mysql"), "mysql://root@h:3306/app", None).unwrap();
        assert_eq!(t.dialect, Dialect::MySql);
        assert_eq!(t.port, 3306);
    }

    #[test]
    fn scram_client_derivation_matches_rfc7677_vector() {
        // RFC 7677 §3 SCRAM-SHA-256 example: user "user", password "pencil".
        // SaltedPassword/ClientKey/ClientProof are well-known for these inputs.
        let password = "pencil";
        let salt = base64_decode("W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
        let iterations = 4096u32;
        let salted = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);

        let client_first_bare = "n=user,r=rOprNGfwEbeRWgbNEkqO";
        let server_first =
            "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let client_final_no_proof = "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
        let auth_message = format!("{client_first_bare},{server_first},{client_final_no_proof}");
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        // The expected ClientProof from RFC 7677 §3.
        assert_eq!(
            base64_encode(&proof),
            "dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );

        // And the server-signature check our client performs round-trips.
        let server_key = hmac_sha256(&salted, b"Server Key");
        let server_sig = hmac_sha256(&server_key, auth_message.as_bytes());
        assert_eq!(
            base64_encode(&server_sig),
            "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
        );
    }

    #[test]
    fn md5_digest_matches_known_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }
}
