//! `sql` — a flux integration plugin for **read-only** SQL introspection and bounded query, ported
//! from the fluxplane `sql` plugin. It is the hardest plugin in the pack: real async SQL driver crates
//! own their own socket and cannot sit on flux's synchronous, host-proxied byte stream, so this plugin
//! carries a **hand-rolled PostgreSQL wire-protocol client** that runs entirely over the host `conn.*`
//! capability (via [`host_kit::ConnStream`]). The plugin opens no socket, reads no env, and never
//! holds a URL it dials: every dial goes **by endpoint reference** (`host.conn_dial_ref` — the named
//! manifest endpoint or a discovered `@endpoint/<id>`), and DSN metadata comes from the gated
//! non-secret `config` read.
//!
//! ## Host-terminated auth (D-31)
//! The plugin **never receives the password**. The host speaks the PostgreSQL startup + SCRAM-SHA-256
//! /MD5 handshake itself (`host.conn_authenticate`) using a credential it resolves host-side, and
//! hands the plugin a *post-auth* connection at the first `ReadyForQuery`. The plugin then drives the
//! Simple Query protocol over that same `conn_id`. It has no `credential` grant and no password in any
//! `secrets` grant — it holds only a credential *location* (a declared auth purpose for the static
//! endpoint, or the discovered endpoint's `credential_ref`/`endpoint_ref`), never a value.
//!
//! ## Dialects
//! - **PostgreSQL** — fully implemented: the host terminates StartupMessage → Authentication (Ok /
//!   cleartext / MD5 / SASL SCRAM-SHA-256); the plugin drives the Simple Query protocol. All six read
//!   ops run parameter-free, whitelisted introspection SQL over Simple Query and shape the rows into
//!   the same JSON the fluxplane reference returns.
//! - **MySQL / MariaDB** — implemented (D-196…D-198): the host terminates Handshake v10 +
//!   `mysql_native_password`; the plugin drives `COM_QUERY` and decodes the text protocol. The
//!   introspection ops carry per-dialect SQL, since `pg_class`/`pg_index` have no MySQL equivalent
//!   and the foreign-key metadata sits in a different place. **`sql.database.list` means something
//!   different per dialect** — MySQL treats schema and database as one object, so it returns
//!   databases where Postgres returns databases *and* the connected database's schemas.
//!   `caching_sha2_password` (MySQL 8.0+ default), `ed25519`, and `parsec` are not yet implemented
//!   and fail with an error naming the plugin and the workaround.
//! - **SQLite** — *unsupported by design*. SQLite is a local file and flux plugins have no filesystem
//!   capability (`conn.*` is sockets only); a host file capability would be required.
//!
//! ## Honesty note on interop confidence
//! The plugin's Simple Query client is exercised by `MockHost` tests that replay **hand-crafted**
//! server frames. Those prove the *frame parser and message assembly* are correct against bytes the
//! test author wrote; they are **not** a live-interop test against a real `postgres` server. The
//! host-terminated auth handshake (StartupMessage + the documented Authentication subtypes + SCRAM
//! including the server-signature check) lives in `flux-plugin`'s `pg` module and is covered there by
//! hermetic tests against a scripted PG-server stub; first contact with a real server is unverified.

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::time::Duration;

// ===========================================================================
// Manifest
// ===========================================================================

const PG_DEFAULT_PORT: u16 = 5432;
const MYSQL_DEFAULT_PORT: u16 = 3306;

// ===========================================================================
// Schema-only op input structs (D-36)
// ===========================================================================
// Each op's `input_schema` is derived from the structs below via schemars
// (`host_kit::read_op_typed::<T>`), instead of a hand-written `json!({...})` object, so the
// schema the model sees cannot drift from a separately-maintained literal. The structs are
// schema-only: handlers keep their existing `flex_str` / `flex_i64` / `flex_bool` extractors
// (the D-34 schema-only precedent).

/// SQL dialects. Matches the legacy `"enum": ["postgres", "mysql", "sqlite"]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum Driver {
    Postgres,
    Mysql,
    Sqlite,
}

/// Shared connection fields surfaced on every SQL op via `#[serde(flatten)]`.
///
/// Architectural split: `endpoint_ref` is optional in flux (defaults to `sql.endpoint`) and
/// `endpoint` carries a discovered endpoint object; fluxplane makes `endpoint_ref` required,
/// but flux resolves endpoints by either field.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ConnProps {
    /// A discovered endpoint reference (from `endpoint.select`). Secret-free; the only way to
    /// address a discovered endpoint.
    endpoint: Option<Value>,
    /// A registered SQL endpoint name (default `sql.endpoint`). A bare discovered
    /// `@endpoint/<id>` id is rejected — pass the full `endpoint` object instead.
    endpoint_ref: Option<String>,
    /// Dialect override: `postgres`, `mysql`, or `sqlite`.
    driver: Option<Driver>,
    /// Database override.
    database: Option<String>,
    /// Timeout as a duration such as `5s` or `1m`. Defaults to 10s if omitted.
    /// The flux `conn.*` host capability does not currently expose a per-call timeout,
    /// so this is parsed and validated but not enforced on the wire.
    timeout: Option<String>,
}

/// `sql.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TestInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    conn: ConnProps,
}

/// `sql.query`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct QueryInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    conn: ConnProps,
    /// Read-only SQL query.
    query: String,
    /// Max rows (default 100, capped 1000).
    max_rows: Option<i64>,
}

/// `sql.database.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DatabaseListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    conn: ConnProps,
}

/// `sql.table.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TableListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    conn: ConnProps,
    /// Schema filter.
    schema: Option<String>,
    /// Include views (and postgres materialized views).
    include_views: Option<bool>,
    /// Max tables (default 200, capped 1000).
    max_results: Option<i64>,
}

/// `sql.table.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TableShowInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    conn: ConnProps,
    /// Schema holding the table (postgres defaults to `public`).
    schema: Option<String>,
    /// Table or view name.
    table: String,
}

/// `sql.index.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct IndexListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    conn: ConnProps,
    /// Schema filter.
    schema: Option<String>,
    /// Limit to one table (default lists indexes across the schema).
    table: Option<String>,
}

// ===========================================================================
// Manifest
// ===========================================================================

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("sql", env!("CARGO_PKG_VERSION"))
        .capabilities(Caps {
            // Sockets only — the conn allow-list covers the two SQL ports. (SSRF-guarded host-side.)
            // Every dial goes **by reference** (`host.conn_dial_ref`): the host resolves the named
            // or discovered endpoint's host:port and applies the egress guard under this grant.
            conn: vec![
                format!("tcp:*:{PG_DEFAULT_PORT}"),
                format!("tcp:*:{MYSQL_DEFAULT_PORT}"),
            ],
            private_hosts: vec!["*".into()],
            // D-31: the plugin holds NO credential. It has no `secrets` grant (the password is never
            // read via `host.secret`) and no `credential` grant (the password is never materialized
            // into the plugin). The host terminates the auth handshake itself (`host.conn_authenticate`)
            // and hands back a post-auth connection — closing the last references-only gap.
            ..Default::default()
        })
        // The credential *location* the host resolves for the auth handshake it terminates on the
        // plugin's behalf (`host.conn_authenticate`). Declared here as an auth method so the host
        // knows which env keys back the "password" purpose for the static/named endpoint path; the
        // plugin never reads these — they are NOT in the `secrets` grant, so the `secret` capability
        // would refuse them. The discovered-endpoint path resolves the password from the endpoint's
        // own `credential_ref` instead.
        .auth(AuthMethod {
            purpose: "password".into(),
            env: vec!["SQL_PASSWORD".into(), "MYSQL_PASSWORD".into()],
            description: "SQL password (host-resolved for the terminated auth handshake; never read \
                          by the plugin)"
                .into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "sql.endpoint".into(),
            env: vec!["SQL_DSN".into(), "SQL_URL".into()],
            http_hosts: Vec::new(),
            description: "SQL connection DSN/URL, e.g. postgres://host:5432/db".into(),
            ..Default::default()
        })
        // The DSN doubles as declared non-secret config: the plugin reads it via `host.config("dsn")`
        // for connection *metadata* (dialect/database/username) — the host refuses secret-classified
        // keys and credential-bearing values, so this read can never hand back a password. The dial
        // itself still goes by reference against the `sql.endpoint` declaration above.
        .config(ConfigSpec {
            name: "dsn".into(),
            env: vec!["SQL_DSN".into(), "SQL_URL".into()],
            description: "SQL connection DSN (credential-free: put the password in SQL_PASSWORD, \
                          not the URL)"
                .into(),
        })
        .datasource(Declaration {
            name: "sql.query_rows".into(),
            entity: "sql.query_result".into(),
            description: Some("SQL query result rows.".into()),
            capabilities: vec!["search".into()],
            entity_schema: None,
        })
        .operation_flexible(
            read_op_typed::<TestInput>(
                "sql.test",
                "Test SQL endpoint connectivity with a SELECT 1 round trip; reports the server version.",
            ),
            op_test,
        )
        .operation_flexible(
            read_op_typed::<QueryInput>(
                "sql.query",
                "Run a bounded, read-only SQL query (SELECT/SHOW/DESCRIBE/EXPLAIN/WITH only) against the endpoint.",
            ),
            op_query,
        )
        .operation_flexible(
            read_op_typed::<DatabaseListInput>(
                "sql.database.list",
                "List databases. On PostgreSQL this also lists the connected database's non-system \
                 schemas (entries are tagged `kind: \"database\"` or `kind: \"schema\"`). On \
                 MySQL/MariaDB, where schema and database are the same object, every entry is \
                 `kind: \"database\"` and no schema entries are returned.",
            ),
            op_database_list,
        )
        .operation_flexible(
            read_op_typed::<TableListInput>(
                "sql.table.list",
                "List tables (optionally views) with a cheap row estimate where the engine keeps statistics.",
            ),
            op_table_list,
        )
        .operation_flexible(
            read_op_typed::<TableShowInput>(
                "sql.table.show",
                "Describe a table: columns with types and nullability, the primary key, and foreign keys.",
            ),
            op_table_show,
        )
        .operation_flexible(
            read_op_typed::<IndexListInput>(
                "sql.index.list",
                "List indexes across a schema or for one table, with columns and uniqueness.",
            ),
            op_index_list,
        )
}

fn main() -> Result<(), String> {
    manifest_builder().try_serve()
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

/// A resolved connection target: which database, how the host dials it (**by reference** — the
/// plugin never dials a host:port it parsed itself), plus a redacted URL for output.
#[derive(Debug, Clone)]
struct SqlTarget {
    dialect: Dialect,
    database: String,
    /// Username parsed from the DSN userinfo (non-secret connection metadata used for the startup
    /// message; the discovered path takes it from the bare ref URL, the static path from the DSN).
    dsn_user: Option<String>,
    /// A password-redacted form of the URL, surfaced as `endpoint_url`.
    safe_url: String,
    /// When this target came from a **discovered** endpoint (an `@endpoint/<id>` ref), how the host
    /// resolves it: the plugin dials by reference and the host terminates the auth handshake, never
    /// holding a URL with a password. `None` = a static/named manifest endpoint (DSN metadata from
    /// the gated `config` read, dial by the named ref, host-terminated auth by declared purpose).
    discovered: Option<DiscoveredSource>,
    /// The **named** manifest endpoint a static target dials by reference via `host.conn_dial_ref`
    /// (default `sql.endpoint`); the host resolves it (env → URL → host:port) and applies the
    /// egress guard. `None` for a discovered target (which dials by its
    /// [`DiscoveredSource::endpoint_ref`]) or a bare parsed URL that was never host-resolved.
    dial_ref: Option<String>,
    /// Operation timeout parsed from the input `timeout` field. Stored for parity with the
    /// fluxplane reference, which uses it as a context deadline; flux's `conn.*` host capability
    /// does not currently expose a per-call timeout, so this is parsed/validated but not
    /// enforced on the wire.
    timeout: Option<Duration>,
}

/// The host-resolved references a discovered-endpoint [`SqlTarget`] carries. The plugin passes these
/// references back to the host for the privileged operations (dial by ref, host-terminated auth); it
/// never holds the URL-with-password nor the credential value itself.
#[derive(Debug, Clone)]
struct DiscoveredSource {
    /// The `@endpoint/<id>` reference — dialed via `host.conn_dial_ref` (the host applies the egress
    /// guard) and, when no explicit `credential_ref` is present, passed to `host.conn_authenticate`
    /// so the host materializes the endpoint record's credential for the terminated handshake.
    endpoint_ref: String,
    /// The endpoint's `credential_ref` string (a *location*, never a value), when the weak
    /// `EndpointRef` carried one. Passed to `host.conn_authenticate`, which resolves it host-side.
    credential_ref: Option<String>,
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

/// Resolve the op input into a [`SqlTarget`]. Two input shapes are accepted, in priority order:
///
/// 1. A full weak `EndpointRef` JSON object in `endpoint` (the discovered-endpoint path — the agent
///    gets the object from `endpoint.select`). It is secret-free: its `url` is a bare
///    `postgres://user@host:port/db` (NO password), plus an optional `credential_ref` *location*.
///    The plugin parses database/dialect/username from that bare URL and dials + fetches the
///    password by reference.
/// 2. An `endpoint_ref` string naming a **static** manifest endpoint (or the default
///    `sql.endpoint`): the DSN metadata (dialect/database/username/safe_url) comes from the gated
///    non-secret `config` read (`host.config("dsn")` — the host refuses credential-bearing values,
///    so the DSN can never embed a password), and the dial goes **by reference** through
///    `host.conn_dial_ref(<name>)` (the host resolves env → URL → host:port).
///
/// A bare discovered `@endpoint/<id>` STRING in `endpoint_ref` is rejected with a clear error: it
/// relied on the retired `endpoint` URL-handback (which the real host never covered for discovered
/// ids) — pass the full `endpoint` object from `endpoint.select` instead.
fn resolve_target(input: &Value, host: &mut Host) -> Result<SqlTarget, String> {
    let driver = flex_str(input, "driver");
    let db_override = flex_str(input, "database");
    // Parse `timeout` once for every op; invalid values fail fast before any dial.
    let timeout = parse_duration_default(
        flex_str(input, "timeout").as_deref(),
        Duration::from_secs(10),
    )?;

    let mut target = if let Some(obj) = input.get("endpoint").filter(|v| v.is_object()) {
        // (1) A full weak EndpointRef object passed inline by the agent.
        target_from_endpoint_ref(obj, driver.as_deref(), db_override.as_deref())?
    } else {
        let endpoint_ref = flex_str(input, "endpoint_ref").unwrap_or_else(|| "sql.endpoint".into());
        if is_discovered_ref(&endpoint_ref) {
            return Err(format!(
                "discovered endpoint id {endpoint_ref:?} cannot be passed as `endpoint_ref`: the \
                 id-only lookup is retired — pass the full `endpoint` object from \
                 `endpoint.select` instead"
            ));
        }
        // (2) A static/named endpoint: DSN metadata via the gated non-secret `config` read; the
        // dial goes by the named reference, resolved host-side.
        let raw_dsn = host.config("dsn")?;
        let raw_dsn = raw_dsn.trim();
        if raw_dsn.is_empty() {
            return Err("endpoint has no DSN configured (set SQL_DSN or SQL_URL)".into());
        }
        let mut t = target_from_url(driver.as_deref(), raw_dsn, db_override.as_deref())?;
        t.dial_ref = Some(endpoint_ref);
        t
    };

    target.timeout = timeout;
    Ok(target)
}

/// Canonical prefix for a discovered endpoint id (`@endpoint/<id>`), matching
/// `flux_secret::endpoint::ENDPOINT_REF_PREFIX`. Kept local so the plugin pulls no extra crate edge.
const ENDPOINT_REF_PREFIX: &str = "@endpoint/";

/// Whether a reference id is a discovered `@endpoint/<id>` (vs a named/config endpoint).
fn is_discovered_ref(id: &str) -> bool {
    id.starts_with(ENDPOINT_REF_PREFIX)
}

/// Build a [`SqlTarget`] from a weak endpoint-reference object the agent passed inline (the
/// `flux_secret::endpoint::EndpointRef` JSON shape, parsed here without the crate edge). The ref's
/// `url` is bare (no password); host:port/database/dialect/username come from it, the password from
/// the gated `credential` capability against the ref's `credential_ref` (or the ref id).
fn target_from_endpoint_ref(
    endpoint: &Value,
    driver_override: Option<&str>,
    database_override: Option<&str>,
) -> Result<SqlTarget, String> {
    let url = endpoint
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("`endpoint` reference has no url")?;
    let id = endpoint
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or("`endpoint` reference has no id")?;
    // The protocol hint (or product) can stand in for the dialect when the URL scheme is generic.
    let driver = driver_override.map(str::to_string).or_else(|| {
        endpoint
            .get("protocol")
            .and_then(|v| v.as_str())
            .or_else(|| endpoint.get("product").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let mut target = target_from_url(driver.as_deref(), url, database_override)?;
    target.discovered = Some(DiscoveredSource {
        endpoint_ref: id,
        // The `credential_ref` is the structured `Ref` (a location) `endpoint.select` emits; render
        // it to the `scheme/...` string the host's `credential` capability parses. `None` = the host
        // looks the credential up by the endpoint ref instead.
        credential_ref: endpoint
            .get("credential_ref")
            .filter(|v| !v.is_null())
            .map(credential_ref_to_string)
            .transpose()?,
    });
    Ok(target)
}

/// Render a `credential_ref` (the structured `Ref` JSON `endpoint.select` emits, or already a
/// `scheme/...` string) into the `scheme/...` form the host's `credential` capability parses.
fn credential_ref_to_string(v: &Value) -> Result<String, String> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    let obj = v
        .as_object()
        .ok_or("`credential_ref` must be a string or object")?;
    let scheme = obj
        .get("scheme")
        .and_then(|s| s.as_str())
        .ok_or("`credential_ref` missing `scheme`")?;
    let slot = obj.get("slot").and_then(|s| s.as_str()).unwrap_or("");
    match scheme {
        // `env/KEY` uses only the slot; plugin/kubernetes use plugin/instance/slot.
        "env" => Ok(format!("env/{slot}")),
        "plugin" | "kubernetes" => {
            let plugin = obj.get("plugin").and_then(|s| s.as_str()).unwrap_or("");
            let instance = obj.get("instance").and_then(|s| s.as_str()).unwrap_or("");
            Ok(format!("{scheme}/{plugin}/{instance}/{slot}"))
        }
        other => Err(format!("unknown credential_ref scheme {other:?}")),
    }
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
    // The username is non-secret connection metadata. A password may still appear in an inline
    // weak-EndpointRef URL; it is only used LOCALLY to redact `safe_url` and is never stored on the
    // target or sent to the host — the host resolves the credential itself for the terminated
    // handshake (D-31). The static config-read DSN can never carry one (config refuses those).
    let (dsn_user, dsn_password_present) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((user, pass)) => (
                opt_nonempty(pct_decode(user)),
                opt_nonempty(pct_decode(pass)).is_some(),
            ),
            None => (opt_nonempty(pct_decode(u)), false),
        },
        None => (None, false),
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
        return Err(SQLITE_UNSUPPORTED.into());
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
        if dsn_password_present {
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
        database,
        dsn_user,
        safe_url,
        discovered: None,
        dial_ref: None,
        timeout: None,
    })
}

/// Where the host finds the connection credential for the auth handshake it terminates on the
/// plugin's behalf (D-31). Every variant is a *location*, never a value — the plugin holds no secret.
enum CredSource {
    /// Static/named endpoint: the declared "password" auth method (the host resolves its env).
    AuthPurpose(&'static str),
    /// Discovered endpoint with an explicit `credential_ref` (a `scheme/...` location).
    CredentialRef(String),
    /// Discovered endpoint: the host materializes the endpoint record's attached credential.
    EndpointRef(String),
}

impl CredSource {
    fn as_pg(&self) -> PgCredential<'_> {
        match self {
            CredSource::AuthPurpose(p) => PgCredential::AuthPurpose(p),
            CredSource::CredentialRef(r) => PgCredential::CredentialRef(r),
            CredSource::EndpointRef(r) => PgCredential::EndpointRef(r),
        }
    }
}

/// Resolve the effective `(user, database, credential-location)` for the handshake — **pure**, no
/// secret ever touches the plugin (D-31). Username + database are non-secret connection metadata
/// (from a discovered ref's bare URL, or the config-read DSN, which can never carry a password); the
/// Postgres database defaults to the user when unset (libpq behavior). The credential is only ever a
/// *location* the host resolves when it terminates the handshake:
///   - **discovered** endpoint → the ref's explicit `credential_ref`, else the `endpoint_ref` itself;
///   - **static/named** endpoint → the declared "password" auth method (host-side env).
fn resolve_connection(target: &SqlTarget) -> (String, String, CredSource) {
    let user = target
        .dsn_user
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| match target.dialect {
            Dialect::Postgres => "postgres".into(),
            Dialect::MySql => "root".into(),
            Dialect::Sqlite => String::new(),
        });
    let database = if target.database.trim().is_empty() {
        user.clone()
    } else {
        target.database.clone()
    };
    let cred = match &target.discovered {
        Some(disc) => match &disc.credential_ref {
            Some(cref) => CredSource::CredentialRef(cref.clone()),
            None => CredSource::EndpointRef(disc.endpoint_ref.clone()),
        },
        None => CredSource::AuthPurpose("password"),
    };
    (user, database, cred)
}

/// Dial the target through the host — always **by reference** (`host.conn_dial_ref`): a discovered
/// endpoint by its `@endpoint/<id>` ref, a static/named endpoint by its manifest endpoint name
/// (default `sql.endpoint`). Either way the host resolves the address, applies the egress guard,
/// and owns the socket; the plugin never dials a host:port it parsed itself.
fn dial(target: &SqlTarget, host: &mut Host) -> Result<u64, String> {
    match (&target.discovered, &target.dial_ref) {
        (Some(disc), _) => host.conn_dial_ref(&disc.endpoint_ref),
        (None, Some(named)) => host.conn_dial_ref(named),
        (None, None) => Err("sql target carries no endpoint reference to dial".into()),
    }
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

/// Parse a duration string in the style of Go's `time.ParseDuration` (`5s`, `1m`, `1h30m`).
/// Returns `fallback` when `value` is missing or empty. Errors on invalid syntax.
fn parse_duration_default(
    value: Option<&str>,
    fallback: Duration,
) -> Result<Option<Duration>, String> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(Some(fallback)),
        Some(s) => parse_duration(s).map(Some),
    }
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let mut nanos: u128 = 0;
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return Err(format!("timeout: invalid duration {s:?}"));
        }
        let n: u64 = s[start..i]
            .parse()
            .map_err(|_| format!("timeout: invalid duration {s:?}"))?;

        // Parse the unit at the current position, preferring longer tokens.
        let unit = if bytes[i..].starts_with(b"ns") {
            i += 2;
            "ns"
        } else if bytes[i..].starts_with(b"us") || bytes[i..].starts_with("µs".as_bytes()) {
            i += 2;
            "us"
        } else if bytes[i..].starts_with(b"ms") {
            i += 2;
            "ms"
        } else if bytes.get(i) == Some(&b's') {
            i += 1;
            "s"
        } else if bytes.get(i) == Some(&b'm') {
            i += 1;
            "m"
        } else if bytes.get(i) == Some(&b'h') {
            i += 1;
            "h"
        } else {
            return Err(format!("timeout: invalid duration {s:?}"));
        };

        let unit_nanos: u128 = match unit {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(format!("timeout: invalid duration {s:?}")),
        };
        nanos = nanos
            .checked_add(
                u128::from(n)
                    .checked_mul(unit_nanos)
                    .ok_or_else(|| format!("timeout: duration overflow {s:?}"))?,
            )
            .ok_or_else(|| format!("timeout: duration overflow {s:?}"))?;
    }
    if i == 0 {
        return Err(format!("timeout: invalid duration {s:?}"));
    }
    if nanos > u128::from(u64::MAX) {
        return Err(format!("timeout: duration overflow {s:?}"));
    }
    Ok(Duration::from_nanos(nanos as u64))
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
            "outfile" | "dumpfile" if idx > 0 && tokens[idx - 1].text == "into" => return false,
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
    let (user, database, cred) = resolve_connection(&target);

    let cid = dial(&target, host)?;
    let result = (|| -> Result<Value, String> {
        let mut client = SqlClient::connect(host, cid, &target, &user, &database, cred.as_pg())?;
        let res = client.query("SELECT 1")?;
        let _ = res; // connectivity only; the value is unused
        Ok(json!({
            "status": "ok",
            "endpoint_url": target.safe_url,
            "driver": target.dialect.label(),
            "database": database,
            "server_version": client.server_version(),
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
    let (user, database, cred) = resolve_connection(&target);

    let cid = dial(&target, host)?;
    let shaped = (|| -> Result<Value, String> {
        let mut client = SqlClient::connect(host, cid, &target, &user, &database, cred.as_pg())?;
        let result = client.query(&query)?;
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
    let (user, database, cred) = resolve_connection(&target);

    let cid = dial(&target, host)?;
    let result = (|| -> Result<Value, String> {
        let mut client = SqlClient::connect(host, cid, &target, &user, &database, cred.as_pg())?;
        let mut databases: Vec<Value> = Vec::new();
        match target.dialect {
            Dialect::Postgres => {
                // Postgres models database > schema > table, so this op reports BOTH levels: the
                // cluster's databases, then the connected database's non-system schemas.
                let db_res = client.query(
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
                let schema_res = client.query(
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
            }
            Dialect::MySql => {
                // MySQL/MariaDB treats schema and database as THE SAME OBJECT — there is no
                // intermediate level. So this op returns one `kind: "database"` entry per schema and
                // never a `kind: "schema"` one; `information_schema.schemata` here enumerates real
                // databases, not (as on Postgres) the schemas inside one. Same table name, different
                // meaning — see the op description and docs/designs/mariadb-support.md.
                let res = client.query(
                    "SELECT schema_name AS name, (schema_name = DATABASE()) AS current_db \
                     FROM information_schema.schemata \
                     WHERE schema_name NOT IN \
                     ('information_schema','mysql','performance_schema','sys') \
                     ORDER BY schema_name",
                )?;
                for row in &res.rows {
                    let name = cell(&res, row, "name");
                    if name.is_empty() {
                        continue;
                    }
                    // No per-database owner concept in MySQL — the key is omitted, not faked.
                    databases.push(json!({
                        "name": name,
                        "kind": "database",
                        "current": truthy(&cell(&res, row, "current_db")),
                    }));
                }
            }
            Dialect::Sqlite => return Err(SQLITE_UNSUPPORTED.into()),
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
    let (user, database, cred) = resolve_connection(&target);
    let schema = flex_str(&input, "schema").unwrap_or_default();
    let include_views = flex_bool(&input, "include_views");
    let max_results = clamp(flex_i64(&input, "max_results").unwrap_or(0), 200, 1000) as usize;

    let cid = dial(&target, host)?;
    let result = (|| -> Result<Value, String> {
        let mut client = SqlClient::connect(host, cid, &target, &user, &database, cred.as_pg())?;
        let sql = match target.dialect {
            Dialect::Postgres => {
                let relkinds = if include_views {
                    "('r','p','v','m')"
                } else {
                    "('r','p')"
                };
                format!(
                    "SELECT n.nspname AS table_schema, c.relname AS table_name, c.relkind::text AS table_type, \
                     c.reltuples::bigint AS row_estimate \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE c.relkind IN {relkinds} AND n.nspname NOT IN ('pg_catalog','information_schema') \
                     AND n.nspname NOT LIKE 'pg_%' AND ('{s}' = '' OR n.nspname = '{s}') \
                     ORDER BY n.nspname, c.relname",
                    s = pg_lit(&schema),
                )
            }
            Dialect::MySql => {
                // No pg_class equivalent; information_schema.tables carries the same facts.
                // `table_rows` is an estimate for InnoDB, matching pg's `reltuples` in spirit.
                let types = if include_views {
                    "('BASE TABLE','VIEW')"
                } else {
                    "('BASE TABLE')"
                };
                format!(
                    "SELECT table_schema AS table_schema, table_name AS table_name, \
                     table_type AS table_type, table_rows AS row_estimate \
                     FROM information_schema.tables \
                     WHERE table_type IN {types} AND table_schema NOT IN \
                     ('information_schema','mysql','performance_schema','sys') \
                     AND ('{s}' = '' OR table_schema = '{s}') \
                     ORDER BY table_schema, table_name",
                    s = my_lit(&schema),
                )
            }
            Dialect::Sqlite => return Err(SQLITE_UNSUPPORTED.into()),
        };
        let res = client.query(&sql)?;
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
    let (user, database, cred) = resolve_connection(&target);
    let table = flex_str(&input, "table").ok_or("`table` (string) required")?;
    let schema = flex_str(&input, "schema").unwrap_or_default();

    let cid = dial(&target, host)?;
    let result = (|| -> Result<Value, String> {
        let mut client = SqlClient::connect(host, cid, &target, &user, &database, cred.as_pg())?;

        // Columns. `information_schema.columns` is the one genuinely portable table here — only the
        // default schema differs: Postgres falls back to `public`, MySQL to the connected database.
        let col_sql = match target.dialect {
            Dialect::Postgres => format!(
                "SELECT column_name, ordinal_position, data_type, udt_name, is_nullable, column_default, \
                 character_maximum_length FROM information_schema.columns \
                 WHERE table_schema = COALESCE(NULLIF('{s}',''),'public') AND table_name = '{t}' \
                 ORDER BY ordinal_position",
                s = pg_lit(&schema),
                t = pg_lit(&table),
            ),
            Dialect::MySql => format!(
                "SELECT column_name AS column_name, ordinal_position AS ordinal_position, \
                 data_type AS data_type, is_nullable AS is_nullable, \
                 column_default AS column_default, \
                 character_maximum_length AS character_maximum_length \
                 FROM information_schema.columns \
                 WHERE table_schema = COALESCE(NULLIF('{s}',''), DATABASE()) AND table_name = '{t}' \
                 ORDER BY ordinal_position",
                s = my_lit(&schema),
                t = my_lit(&table),
            ),
            Dialect::Sqlite => return Err(SQLITE_UNSUPPORTED.into()),
        };
        let col_res = client.query(&col_sql)?;
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
        let pk_sql = match target.dialect {
            Dialect::Postgres => format!(
                "SELECT kcu.column_name FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu ON kcu.constraint_name = tc.constraint_name \
                 AND kcu.constraint_schema = tc.constraint_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                 AND tc.table_schema = COALESCE(NULLIF('{s}',''),'public') AND tc.table_name = '{t}' \
                 ORDER BY kcu.ordinal_position",
                s = pg_lit(&schema),
                t = pg_lit(&table),
            ),
            // MySQL names every primary key `PRIMARY`, so constraint names are unique per TABLE, not
            // per schema. The Postgres join (constraint_name + constraint_schema) would therefore
            // match every table's PK in the schema at once — hence the direct, table-scoped read.
            Dialect::MySql => format!(
                "SELECT column_name AS column_name FROM information_schema.key_column_usage \
                 WHERE constraint_name = 'PRIMARY' \
                 AND table_schema = COALESCE(NULLIF('{s}',''), DATABASE()) AND table_name = '{t}' \
                 ORDER BY ordinal_position",
                s = my_lit(&schema),
                t = my_lit(&table),
            ),
            Dialect::Sqlite => return Err(SQLITE_UNSUPPORTED.into()),
        };
        let pk_res = client.query(&pk_sql)?;
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

        // Foreign keys. The two engines expose the referenced side in structurally different places,
        // so this is a different query rather than a dialect-tweaked one: Postgres needs a third join
        // through `constraint_column_usage`, while MySQL carries `referenced_*` on
        // `key_column_usage` itself (non-FK rows have them NULL, which is the filter).
        let fk_sql = match target.dialect {
            Dialect::Postgres => format!(
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
            ),
            Dialect::MySql => format!(
                "SELECT constraint_name AS constraint_name, column_name AS column_name, \
                 referenced_table_name AS referenced_table_name, \
                 referenced_column_name AS referenced_column_name \
                 FROM information_schema.key_column_usage \
                 WHERE referenced_table_name IS NOT NULL \
                 AND table_schema = COALESCE(NULLIF('{s}',''), DATABASE()) AND table_name = '{t}' \
                 ORDER BY constraint_name, ordinal_position",
                s = my_lit(&schema),
                t = my_lit(&table),
            ),
            Dialect::Sqlite => return Err(SQLITE_UNSUPPORTED.into()),
        };
        let fk_res = client.query(&fk_sql)?;
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
    let (user, database, cred) = resolve_connection(&target);
    let schema = flex_str(&input, "schema").unwrap_or_default();
    let table = flex_str(&input, "table").unwrap_or_default();

    let cid = dial(&target, host)?;
    let result = (|| -> Result<Value, String> {
        let mut client = SqlClient::connect(host, cid, &target, &user, &database, cred.as_pg())?;
        let indexes: Vec<Value> = match target.dialect {
            Dialect::Postgres => {
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
                let res = client.query(&sql)?;
                let mut out = Vec::new();
                for row in &res.rows {
                    let definition = cell(&res, row, "definition");
                    out.push(json!({
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
                out
            }
            Dialect::MySql => {
                // `information_schema.statistics` returns ONE ROW PER INDEXED COLUMN, where pg's
                // pg_index returns one row per index — so this path groups rather than maps.
                let sql = format!(
                    "SELECT table_schema AS table_schema, table_name AS table_name, \
                     index_name AS index_name, non_unique AS non_unique, \
                     seq_in_index AS seq_in_index, column_name AS column_name, \
                     index_type AS index_type FROM information_schema.statistics \
                     WHERE table_schema NOT IN \
                     ('information_schema','mysql','performance_schema','sys') \
                     AND ('{s}' = '' OR table_schema = '{s}') AND ('{tb}' = '' OR table_name = '{tb}') \
                     ORDER BY table_schema, table_name, index_name, seq_in_index",
                    s = my_lit(&schema),
                    tb = my_lit(&table),
                );
                let res = client.query(&sql)?;
                group_mysql_indexes(&res)
            }
            Dialect::Sqlite => return Err(SQLITE_UNSUPPORTED.into()),
        };
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

/// SQLite is unsupported by design — it is a local file, and flux plugins have no filesystem
/// capability (`conn.*` is sockets only). Supporting it would need a new host file capability, not a
/// wire client. Postgres and MySQL/MariaDB are both implemented; this is the only dialect that errors.
const SQLITE_UNSUPPORTED: &str =
    "sqlite unsupported (needs a host file capability): flux plugins have no filesystem access and \
     conn.* is sockets only";

// ===========================================================================
// Output-shaping helpers
// ===========================================================================

/// The value of column `name` in `row` as a string (empty when NULL/absent).
///
/// **Matching is case-sensitive, and a miss is indistinguishable from a NULL** — both yield `""`.
/// That is why every dialect's introspection SQL aliases its projection explicitly (`... AS
/// table_name`): MySQL's `information_schema` columns are declared uppercase, so an unaliased
/// `SELECT table_name` that came back labelled `TABLE_NAME` would degrade to empty output on every
/// row rather than erroring — and the hand-crafted-frame tests, which build labels to match, could
/// not catch it.
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

/// `(schema, table, index_name)` — what identifies one MySQL index.
type MySqlIndexKey = (String, String, String);
/// `(unique, method, columns)` accumulated across that index's per-column rows.
type MySqlIndexEntry = (bool, String, Vec<String>);

/// Group MySQL `information_schema.statistics` rows — one per indexed column — into one entry per
/// index, matching the shape the Postgres path emits.
///
/// Two deliberate divergences from the Postgres entry, both because MySQL has no equivalent rather
/// than because they were overlooked:
/// - `primary` is `index_name = 'PRIMARY'`, which is how MySQL marks a primary key (there is no
///   `indisprimary` flag).
/// - `definition` is **omitted**, not synthesized: MySQL has no `pg_get_indexdef`, and emitting a
///   hand-assembled `CREATE INDEX` string would present a fabrication as server-reported DDL. The
///   useful content — `columns`, `unique`, `primary`, `method` — is all present.
///
/// **Known limitation — functional/expression indexes.** MySQL 8.0.13+ reports an index over an
/// expression with `COLUMN_NAME` NULL and the expression in a separate `EXPRESSION` column, so such
/// a part is dropped here and the index appears with fewer columns than it has. Reading `EXPRESSION`
/// as a fallback is *not* the fix: MariaDB — the dialect this epic primarily targets, and which has
/// no functional indexes (it indexes generated columns instead) — has no such column, so selecting
/// it would hard-fail every `index.list` on the main supported engine. Fixing this properly needs
/// per-engine version detection; until then the gap is recorded rather than papered over.
fn group_mysql_indexes(res: &QueryResult) -> Vec<Value> {
    let mut order: Vec<MySqlIndexKey> = Vec::new();
    let mut by_key: std::collections::HashMap<MySqlIndexKey, MySqlIndexEntry> =
        std::collections::HashMap::new();
    for row in &res.rows {
        let key = (
            cell(res, row, "table_schema"),
            cell(res, row, "table_name"),
            cell(res, row, "index_name"),
        );
        let entry = by_key.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (
                // non_unique is 0 for a unique index, 1 otherwise — the inverse of `unique`.
                !truthy(&cell(res, row, "non_unique")),
                cell(res, row, "index_type"),
                Vec::new(),
            )
        });
        let col = cell(res, row, "column_name");
        if !col.is_empty() {
            entry.2.push(col);
        }
    }
    order
        .into_iter()
        .map(|key| {
            let (unique, method, columns) = by_key.remove(&key).unwrap();
            let (schema, table, name) = key;
            json!({
                "primary": name == "PRIMARY",
                "name": name,
                "table": table,
                "schema": schema,
                "columns": columns,
                "unique": unique,
                "method": method,
            })
        })
        .collect()
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

/// The MySQL/MariaDB equivalent of [`pg_lit`].
///
/// **Not interchangeable with `pg_lit`.** MySQL treats `\` as an escape character inside string
/// literals by default (Postgres, with `standard_conforming_strings` on, does not), so doubling only
/// the quote would let a `\'` in an identifier terminate the literal and inject. The backslash is
/// escaped first so the quote-doubling that follows cannot be re-escaped.
///
/// Two connection-level assumptions this rests on, neither visible from inside the plugin:
/// - **An ASCII-compatible connection charset.** Backslash-doubling is only sound while no multi-byte
///   encoding can swallow the escape. The host fixes the charset to `utf8mb4` in the handshake
///   (`CHARSET_UTF8MB4` in `crates/flux-plugin/src/mysql.rs`) and never reports it back, so a
///   host-side charset change would silently reopen that injection class here. Change one, check the
///   other.
/// - **`NO_BACKSLASH_ESCAPES` is off** (the default). Under ANSI mode the server does not undo the
///   doubling, so a schema/table name containing a backslash simply matches nothing — a wrong-but-
///   safe empty result, not an injection.
fn my_lit(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "''")
        .replace('\0', "")
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

/// A minimal blocking PostgreSQL frontend over a host [`ConnStream`]. The startup + authentication
/// handshake is **host-terminated** (`host.conn_authenticate`, D-31) — the plugin never speaks it and
/// never holds the password; this client drives only the post-auth Simple Query ('Q') protocol.
/// Extended query / COPY / binary formats are out of scope — the introspection queries are all
/// text-format Simple Query.
struct PgClient<'h, 'a> {
    stream: ConnStream<'h, 'a>,
    server_version: Option<String>,
}

impl<'h, 'a> PgClient<'h, 'a> {
    /// Open a connection by asking the host to **terminate the auth handshake** on the already-dialed
    /// `conn_id`: the host speaks StartupMessage + SCRAM/MD5 itself using a credential it resolves
    /// host-side, and returns the negotiated parameters (`server_version`) plus a socket left at the
    /// first `ReadyForQuery`. The plugin never receives the password. `database` defaults to `user`
    /// upstream when the DSN names none (resolved by [`resolve_connection`]).
    fn connect(
        host: &'h mut Host<'a>,
        conn_id: u64,
        protocol: &str,
        user: &str,
        database: &str,
        credential: PgCredential<'_>,
        timeout: Option<std::time::Duration>,
    ) -> Result<PgClient<'h, 'a>, String> {
        // D-45: forward the per-call `timeout` (ms) as a read deadline — to the host handshake and to
        // the plugin's own Simple Query reads. The PostgreSQL wire protocol is request/response, so a
        // deadline on every read bounds the whole exchange; on elapsed the host returns
        // ErrorKind::TimedOut (the connection stays open — closed by the outer handler's conn_close).
        let timeout_ms = timeout.map(|d| d.as_millis().min(u64::MAX as u128) as u64);
        let handshake = host.conn_authenticate(
            conn_id,
            protocol,
            user,
            database,
            Some("flux-plugin-sql"),
            credential,
            timeout_ms,
        )?;
        let mut client = PgClient {
            stream: ConnStream::new(host, conn_id),
            server_version: handshake.server_version,
        };
        client.stream.set_read_deadline(timeout);
        Ok(client)
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
// MySQL / MariaDB wire-protocol client (hand-rolled over the host ConnStream)
// ===========================================================================

/// A minimal blocking MySQL/MariaDB frontend over a host [`ConnStream`]. The connection handshake is
/// **host-terminated** (`host.conn_authenticate`, D-196) — the plugin never speaks it and never holds
/// the password; this client drives only the post-auth `COM_QUERY` **text** protocol. Prepared
/// statements and the binary protocol are out of scope: the introspection queries are all text-format
/// reads, and text values map onto the same `Option<String>` cells the Postgres client produces.
struct MySqlClient<'h, 'a> {
    stream: ConnStream<'h, 'a>,
    server_version: Option<String>,
    /// The packet sequence id, reset to 0 at the start of every command.
    seq: u8,
}

/// The protocol's own maximum single-packet payload. A packet of exactly this size continues into
/// the next one.
const MYSQL_MAX_PACKET: usize = 0x00FF_FFFF;

/// A classic EOF packet's payload is **exactly 5 bytes** under `CLIENT_PROTOCOL_41` (`0xfe` +
/// 2-byte warning count + 2-byte status flags). Every OK packet — including the `0xfe`-headered one
/// that *replaces* EOF under `CLIENT_DEPRECATE_EOF` — is at least 7 (`0xfe` + a length-encoded
/// affected-rows + a length-encoded last-insert-id + status + warnings). That gap is what tells the
/// two apart without knowing which capability was negotiated.
const MYSQL_EOF_PAYLOAD_LEN: usize = 5;

/// Whether `payload` is a classic (pre-`CLIENT_DEPRECATE_EOF`) EOF packet.
///
/// A row cannot be mistaken for one: a row opening with `0xfe` is announcing an 8-byte
/// length-encoded integer, so it needs at least 9 bytes.
fn is_classic_eof(payload: &[u8]) -> bool {
    payload.first() == Some(&0xfe) && payload.len() == MYSQL_EOF_PAYLOAD_LEN
}

/// Upper bound on one **reassembled** logical payload, enforced as the continuation chain is
/// stitched. MySQL's 3-byte length field caps a single packet at 16 MiB, but a chain of full-size
/// packets is unbounded — a hostile or compromised endpoint could answer any query with an endless
/// stream and grow this buffer until the plugin subprocess is killed. The host reader enforces the
/// same invariant on the auth phase (`MAX_MESSAGE_BYTES` in `crates/flux-plugin/src/handshake.rs`);
/// this is its query-phase counterpart, set far higher because a legitimate result row may exceed
/// one packet, unlike an auth message.
const MYSQL_MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Whether `payload` ends a result set — either kind of terminator, since both carry `0xfe`.
fn is_result_set_terminator(payload: &[u8]) -> bool {
    payload.first() == Some(&0xfe) && payload.len() < MYSQL_MAX_PACKET
}

impl<'h, 'a> MySqlClient<'h, 'a> {
    /// Open a connection by asking the host to terminate the MySQL handshake on the already-dialed
    /// `conn_id`. The host speaks Handshake v10 + `mysql_native_password` using a credential it
    /// resolves host-side and hands back a socket sitting after the auth OK packet.
    fn connect(
        host: &'h mut Host<'a>,
        conn_id: u64,
        protocol: &str,
        user: &str,
        database: &str,
        credential: PgCredential<'_>,
        timeout: Option<std::time::Duration>,
    ) -> Result<MySqlClient<'h, 'a>, String> {
        let timeout_ms = timeout.map(|d| d.as_millis().min(u64::MAX as u128) as u64);
        let handshake = host.conn_authenticate(
            conn_id,
            protocol,
            user,
            database,
            Some("flux-plugin-sql"),
            credential,
            timeout_ms,
        )?;
        let mut client = MySqlClient {
            stream: ConnStream::new(host, conn_id),
            server_version: handshake.server_version,
            seq: 0,
        };
        client.stream.set_read_deadline(timeout);
        Ok(client)
    }

    /// Run a `COM_QUERY` (0x03) and decode the text-protocol result set.
    ///
    /// The response is one of: ERR, a bare OK (no result set — an empty `QueryResult`), or a
    /// length-encoded column count followed by that many column-definition packets, the rows, and a
    /// terminator.
    ///
    /// **`CLIENT_DEPRECATE_EOF` is not consulted.** That negotiated flag decides whether the
    /// intermediate EOF (after the column definitions) is present, and whether the terminator is an
    /// EOF packet or an OK packet — but **both terminators carry a `0xfe` header**, so the two shapes
    /// are distinguishable on the wire without knowing the flag. A `0xfe` that instead *opens a row*
    /// is a length-encoded-integer prefix announcing an 8-byte length, i.e. a cell of at least 2^24
    /// bytes, whose reassembled payload is necessarily `>= 0xFFFFFF`. So `0xfe` with a payload under
    /// that ceiling is unambiguously a terminator.
    ///
    /// Decoding by that rule rather than by the flag keeps the client correct under a host/plugin
    /// version skew, and avoids widening `HandshakeInfo` — a published 1.0.0 protocol-line type whose
    /// growth would be a semver break.
    fn query(&mut self, sql: &str) -> Result<QueryResult, String> {
        self.seq = 0;
        let mut payload = Vec::with_capacity(sql.len() + 1);
        payload.push(0x03);
        payload.extend_from_slice(sql.as_bytes());
        self.write_packet(&payload)?;

        let first = self.read_packet()?;
        match first.first() {
            None => return Err("mysql: empty response to COM_QUERY".into()),
            Some(0xff) => return Err(format!("mysql: {}", parse_mysql_err(&first[1..]))),
            // OK packet: a statement with no result set. Not an error — just no rows.
            Some(0x00) => {
                return Ok(QueryResult {
                    columns: Vec::new(),
                    rows: Vec::new(),
                })
            }
            // LOCAL INFILE request. We never negotiate CLIENT_LOCAL_FILES, so a server sending this
            // is misbehaving; refuse rather than follow it.
            Some(0xfb) => {
                return Err(
                    "mysql: server sent a LOCAL INFILE request, which this client never solicits \
                     and will not honour"
                        .into(),
                )
            }
            _ => {}
        }

        let mut cur = MyCursor::new(&first);
        let column_count = cur.lenenc_int("column count")? as usize;
        if column_count == 0 {
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
            });
        }

        // Column definitions: catalog, schema, table, org_table, NAME, org_name — `name` is the 5th
        // length-encoded string.
        let mut columns = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let packet = self.read_packet()?;
            let mut c = MyCursor::new(&packet);
            for _ in 0..4 {
                c.lenenc_bytes("column definition prefix")?;
            }
            let name = c.lenenc_bytes("column name")?;
            columns.push(String::from_utf8_lossy(name).into_owned());
            let _ = i;
        }

        // A server that did NOT negotiate CLIENT_DEPRECATE_EOF sends an intermediate EOF here, before
        // the rows; one that did goes straight to the rows (or, for an empty result set, straight to
        // the terminator). Peek once: swallow a *classic* EOF, and hand anything else to the row loop.
        let after_columns = self.read_packet()?;
        let mut pending = if is_classic_eof(&after_columns) {
            None
        } else {
            Some(after_columns)
        };

        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        loop {
            let packet = match pending.take() {
                Some(p) => p,
                None => self.read_packet()?,
            };
            match packet.first() {
                Some(0xff) => return Err(format!("mysql: {}", parse_mysql_err(&packet[1..]))),
                // Terminator: an EOF packet (pre-DEPRECATE_EOF) or an OK packet (post-) — both carry
                // the 0xfe header. See the doc comment above for why the length test is what
                // separates it from a row whose first cell opens with an 0xfe length prefix.
                Some(0xfe) if is_result_set_terminator(&packet) => break,
                None => return Err("mysql: empty packet in result set".into()),
                _ => {
                    let mut r = MyCursor::new(&packet);
                    let mut row = Vec::with_capacity(column_count);
                    for _ in 0..column_count {
                        row.push(r.lenenc_string_or_null("row value")?);
                    }
                    rows.push(row);
                }
            }
        }
        Ok(QueryResult { columns, rows })
    }

    // --- framing ---

    /// Write one packet: 3-byte little-endian payload length + 1-byte sequence id + payload.
    /// A payload larger than the 3-byte ceiling is split, as the protocol requires.
    fn write_packet(&mut self, payload: &[u8]) -> Result<(), String> {
        let mut offset = 0;
        loop {
            let chunk = std::cmp::min(MYSQL_MAX_PACKET, payload.len() - offset);
            let mut msg = Vec::with_capacity(chunk + 4);
            msg.extend_from_slice(&(chunk as u32).to_le_bytes()[..3]);
            msg.push(self.seq);
            msg.extend_from_slice(&payload[offset..offset + chunk]);
            self.seq = self.seq.wrapping_add(1);
            self.stream
                .write_all(&msg)
                .map_err(|e| format!("mysql: write failed: {e}"))?;
            offset += chunk;
            // A final chunk of exactly the maximum needs a trailing empty packet to mark the end.
            if chunk < MYSQL_MAX_PACKET {
                return Ok(());
            }
            if offset == payload.len() {
                let term = vec![0u8, 0, 0, self.seq];
                self.seq = self.seq.wrapping_add(1);
                self.stream
                    .write_all(&term)
                    .map_err(|e| format!("mysql: write failed: {e}"))?;
                return Ok(());
            }
        }
    }

    /// Read one logical payload, reassembling a `0xFFFFFF`-length continuation chain.
    fn read_packet(&mut self) -> Result<Vec<u8>, String> {
        let mut payload = Vec::new();
        loop {
            let mut header = [0u8; 4];
            self.read_exact(&mut header)?;
            let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
            self.seq = header[3].wrapping_add(1);
            if payload.len() + len > MYSQL_MAX_PAYLOAD_BYTES {
                return Err(format!(
                    "mysql: reassembled payload exceeds the {MYSQL_MAX_PAYLOAD_BYTES}-byte cap \
                     (refusing to buffer an unbounded packet chain)"
                ));
            }
            if len > 0 {
                let start = payload.len();
                payload.resize(start + len, 0);
                self.read_exact(&mut payload[start..])?;
            }
            if len < MYSQL_MAX_PACKET {
                return Ok(payload);
            }
        }
    }

    /// Read exactly `buf.len()` bytes, looping over the chunked `conn.read`; EOF mid-read is an error.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self
                .stream
                .read(&mut buf[filled..])
                .map_err(|e| format!("mysql: read failed: {e}"))?;
            if n == 0 {
                return Err("mysql: connection closed mid-packet (EOF)".into());
            }
            filled += n;
        }
        Ok(())
    }
}

/// A bounds-checked cursor over a MySQL packet payload, decoding the protocol's length-encoded
/// integers and strings. Every accessor names the field it reads so a truncated/hostile packet
/// produces a diagnosable error instead of a panic.
struct MyCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> MyCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn need(&self, n: usize, what: &str) -> Result<(), String> {
        if self.data.len() - self.pos < n {
            return Err(format!(
                "mysql: packet truncated reading {what} (need {n} bytes, {} left)",
                self.data.len() - self.pos
            ));
        }
        Ok(())
    }

    /// A length-encoded integer: `<0xfb` is the value itself; `0xfc`/`0xfd`/`0xfe` introduce a
    /// 2/3/8-byte little-endian value.
    fn lenenc_int(&mut self, what: &str) -> Result<u64, String> {
        self.need(1, what)?;
        let first = self.data[self.pos];
        self.pos += 1;
        let width = match first {
            0xfc => 2,
            0xfd => 3,
            0xfe => 8,
            0xfb => return Err(format!("mysql: unexpected NULL marker reading {what}")),
            v => return Ok(v as u64),
        };
        self.need(width, what)?;
        let mut buf = [0u8; 8];
        buf[..width].copy_from_slice(&self.data[self.pos..self.pos + width]);
        self.pos += width;
        Ok(u64::from_le_bytes(buf))
    }

    /// A length-encoded string's bytes.
    fn lenenc_bytes(&mut self, what: &str) -> Result<&'a [u8], String> {
        let len = self.lenenc_int(what)? as usize;
        self.need(len, what)?;
        let out = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(out)
    }

    /// A row cell: `0xfb` is SQL NULL, anything else a length-encoded string.
    fn lenenc_string_or_null(&mut self, what: &str) -> Result<Option<String>, String> {
        self.need(1, what)?;
        if self.data[self.pos] == 0xfb {
            self.pos += 1;
            return Ok(None);
        }
        let bytes = self.lenenc_bytes(what)?;
        Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
    }
}

/// Parse an ERR packet body (after the `0xff` header): 2-byte error code, then — with
/// CLIENT_PROTOCOL_41 — a `#` marker and a 5-byte SQLSTATE, then the message.
fn parse_mysql_err(body: &[u8]) -> String {
    if body.len() < 2 {
        return "server error (truncated ERR packet)".into();
    }
    let code = u16::from_le_bytes([body[0], body[1]]);
    let rest = &body[2..];
    let (sqlstate, message) = if rest.first() == Some(&b'#') && rest.len() >= 6 {
        (
            Some(String::from_utf8_lossy(&rest[1..6]).into_owned()),
            String::from_utf8_lossy(&rest[6..]).into_owned(),
        )
    } else {
        (None, String::from_utf8_lossy(rest).into_owned())
    };
    match sqlstate {
        Some(state) => format!("server error {code} ({state}): {message}"),
        None => format!("server error {code}: {message}"),
    }
}

// ===========================================================================
// Dialect-dispatching client
// ===========================================================================

/// The connected client for whichever dialect the target names. Each op opens one of these instead of
/// a `PgClient` directly, so the op bodies differ only in the SQL they send (D-198), not in how they
/// talk to the server.
enum SqlClient<'h, 'a> {
    Pg(PgClient<'h, 'a>),
    MySql(MySqlClient<'h, 'a>),
}

impl<'h, 'a> SqlClient<'h, 'a> {
    fn connect(
        host: &'h mut Host<'a>,
        conn_id: u64,
        target: &SqlTarget,
        user: &str,
        database: &str,
        credential: PgCredential<'_>,
    ) -> Result<SqlClient<'h, 'a>, String> {
        let protocol = target.dialect.label();
        match target.dialect {
            Dialect::Postgres => Ok(SqlClient::Pg(PgClient::connect(
                host,
                conn_id,
                protocol,
                user,
                database,
                credential,
                target.timeout,
            )?)),
            Dialect::MySql => Ok(SqlClient::MySql(MySqlClient::connect(
                host,
                conn_id,
                protocol,
                user,
                database,
                credential,
                target.timeout,
            )?)),
            // Rejected far earlier, at URL-parse time — a local file is not a socket.
            Dialect::Sqlite => Err(SQLITE_UNSUPPORTED.into()),
        }
    }

    fn query(&mut self, sql: &str) -> Result<QueryResult, String> {
        match self {
            SqlClient::Pg(c) => c.simple_query(sql),
            SqlClient::MySql(c) => c.query(sql),
        }
    }

    fn server_version(&self) -> String {
        match self {
            SqlClient::Pg(c) => c.server_version.clone().unwrap_or_default(),
            SqlClient::MySql(c) => c.server_version.clone().unwrap_or_default(),
        }
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

// ===========================================================================
// Tests — one MockHost test per op (hand-crafted server frames) + the host-terminated-auth contract.
//
// HONESTY: these replay author-written PostgreSQL frames over the POST-AUTH Simple Query protocol.
// They prove the frame parser, message assembly, and JSON shaping — NOT live interop with a real
// server. The auth handshake is host-terminated (D-31): the mock `conn.authenticate` returns the
// negotiated `server_version` without the plugin ever seeing a password; the wire-level SCRAM
// correctness (including the server-signature check) is covered by `flux-plugin`'s `pg` tests.
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

    /// A MockHost with the standard static setup — the credential-free DSN as non-secret config
    /// (metadata) and the named `sql.endpoint` ref (the by-reference dial target). The auth handshake
    /// is host-terminated (D-31: the mock `conn.authenticate` returns `server_version` — the plugin
    /// never reads a secret), so the canned conn stream carries only the POST-AUTH query `responses`
    /// (concatenated into one chunk so the plugin's `read_exact` reframes).
    fn host_with(responses: Vec<Vec<u8>>) -> MockHost {
        let mut stream = Vec::new();
        for r in responses {
            stream.extend(r);
        }
        MockHost::default()
            .with_config("dsn", "postgres://app@db.test:5432/warehouse")
            .with_endpoint_ref("sql.endpoint", "postgres://app@db.test:5432/warehouse")
            .with_conn_response(stream)
    }

    fn run(op: &str, input: Value, host: &mut MockHost) -> Result<Value, String> {
        manifest_builder().build().call(op, input, host)
    }

    // -----------------------------------------------------------------
    // MySQL / MariaDB wire frames (D-197)
    //
    // HONESTY: like the PostgreSQL frames above, these are hand-crafted by the test author. They
    // prove the frame parser and message assembly — NOT live interop with a real MariaDB.
    // -----------------------------------------------------------------

    /// Frame a MySQL packet: 3-byte little-endian payload length + 1-byte sequence id + payload.
    fn my_packet(seq: u8, payload: &[u8]) -> Vec<u8> {
        let mut m = (payload.len() as u32).to_le_bytes()[..3].to_vec();
        m.push(seq);
        m.extend_from_slice(payload);
        m
    }

    /// A length-encoded string (the short form is enough for every field these tests build).
    fn my_lenenc(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let n = bytes.len();
        if n < 251 {
            out.push(n as u8);
        } else if n < 65536 {
            out.push(0xfc);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        } else {
            out.push(0xfd);
            out.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
        }
        out.extend_from_slice(bytes);
        out
    }

    /// A protocol-41 column definition: catalog, schema, table, org_table, NAME, org_name, then the
    /// fixed tail the client skips.
    fn my_column_def(name: &str) -> Vec<u8> {
        let mut p = Vec::new();
        for field in ["def", "warehouse", "t", "t"] {
            p.extend(my_lenenc(field.as_bytes()));
        }
        p.extend(my_lenenc(name.as_bytes()));
        p.extend(my_lenenc(name.as_bytes()));
        p.push(0x0c);
        p.extend_from_slice(&45u16.to_le_bytes()); // charset
        p.extend_from_slice(&255u32.to_le_bytes()); // column length
        p.push(0xfd); // type VAR_STRING
        p.extend_from_slice(&[0, 0]); // flags
        p.push(0); // decimals
        p.extend_from_slice(&[0, 0]); // filler
        p
    }

    /// A classic EOF packet (pre-`CLIENT_DEPRECATE_EOF`): `0xfe`, warnings, status.
    fn my_eof() -> Vec<u8> {
        vec![0xfe, 0x00, 0x00, 0x02, 0x00]
    }

    /// The `CLIENT_DEPRECATE_EOF` result-set terminator: an OK packet that also carries the **`0xfe`
    /// header** (affected_rows, last_insert_id, status, warnings). Deliberately longer than
    /// [`my_eof`] so the two shapes are not accidentally byte-identical in tests.
    fn my_eof_ok() -> Vec<u8> {
        vec![0xfe, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]
    }

    fn my_err(code: u16, sqlstate: &str, message: &str) -> Vec<u8> {
        let mut p = vec![0xff];
        p.extend_from_slice(&code.to_le_bytes());
        p.push(b'#');
        p.extend_from_slice(sqlstate.as_bytes());
        p.extend_from_slice(message.as_bytes());
        p
    }

    /// A text-protocol row; `None` is SQL NULL (the `0xfb` marker).
    fn my_row(values: &[Option<&str>]) -> Vec<u8> {
        let mut p = Vec::new();
        for v in values {
            match v {
                Some(s) => p.extend(my_lenenc(s.as_bytes())),
                None => p.push(0xfb),
            }
        }
        p
    }

    /// A complete `COM_QUERY` result set. `with_eof` selects the pre-`CLIENT_DEPRECATE_EOF` shape
    /// (intermediate EOF after the column defs, EOF terminator) versus the post- shape (no
    /// intermediate EOF, OK terminator). The client must decode both without being told which.
    fn my_result_set(cols: &[&str], rows: &[Vec<Option<&str>>], with_eof: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut seq = 1u8;
        out.extend(my_packet(seq, &[cols.len() as u8]));
        seq += 1;
        for c in cols {
            out.extend(my_packet(seq, &my_column_def(c)));
            seq += 1;
        }
        if with_eof {
            out.extend(my_packet(seq, &my_eof()));
            seq += 1;
        }
        for r in rows {
            out.extend(my_packet(seq, &my_row(r)));
            seq += 1;
        }
        // Both terminators carry the 0xfe header; the shapes differ in the INTERMEDIATE EOF above.
        out.extend(my_packet(
            seq,
            &if with_eof { my_eof() } else { my_eof_ok() },
        ));
        out
    }

    /// The MySQL counterpart of [`host_with`] — a mysql DSN plus the named endpoint ref.
    fn mysql_host_with(responses: Vec<Vec<u8>>) -> MockHost {
        let mut stream = Vec::new();
        for r in responses {
            stream.extend(r);
        }
        MockHost::default()
            .with_config("dsn", "mariadb://app@db.test:3306/warehouse")
            .with_endpoint_ref("sql.endpoint", "mariadb://app@db.test:3306/warehouse")
            .with_conn_response(stream)
    }

    /// The SQL the plugin actually put on the wire, recovered from the recorded `conn.write` bytes
    /// (every COM_QUERY payload is `0x03` + the statement).
    fn sent_queries(host: &MockHost) -> Vec<String> {
        let buf = host.conn_buf.borrow().clone();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 4 <= buf.len() {
            let len = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], 0]) as usize;
            let start = i + 4;
            if start + len > buf.len() {
                break;
            }
            let payload = &buf[start..start + len];
            if payload.first() == Some(&0x03) {
                out.push(String::from_utf8_lossy(&payload[1..]).into_owned());
            }
            i = start + len;
        }
        out
    }

    #[test]
    fn manifest_declares_six_read_ops_and_conn_caps() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.iter().filter(|o| !o.internal).count(), 6);
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
        // D-31: the plugin holds NO credential — no `credential` grant and no password in `secrets`
        // (the host terminates the auth handshake and never hands the plugin a secret value).
        assert!(
            !m.capabilities.credential,
            "sql must not hold the `credential` grant (host-terminated auth)"
        );
        assert!(
            m.capabilities.secrets.is_empty(),
            "sql must not grant any `secrets` (the password is host-resolved): {:?}",
            m.capabilities.secrets
        );
        // The "password" auth method stays DECLARED so the host knows which env backs it — but that
        // env is not in the `secrets` grant, so the `secret` capability would refuse it to the plugin.
        assert!(m.auth.iter().any(|a| a.purpose == "password"));
        // D-32: the DSN is a declared non-secret config (read via `host.config("dsn")`), and the
        // named endpoint stays declared as the by-reference dial target.
        assert!(m.config.iter().any(|c| c.name == "dsn"));
        assert!(m.endpoints.iter().any(|e| e.name == "sql.endpoint"));
        // All read ops are idempotent reads.
        for op in m.operations.iter().filter(|o| !o.internal) {
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

    /// A MockHost for a DISCOVERED postgres endpoint: the conn stream carries the POST-AUTH query
    /// `responses`, the bare (no-password) URL is registered under the discovered `endpoint_ref` (so
    /// `conn_dial_ref` resolves it), and the password is registered so the HOST can resolve it for the
    /// terminated handshake (`conn.authenticate`) — never via a `secret` purpose, never inside a URL,
    /// and never handed to the plugin.
    fn discovered_host(
        endpoint_ref: &str,
        bare_url: &str,
        password: &str,
        responses: Vec<Vec<u8>>,
    ) -> MockHost {
        let mut stream = Vec::new();
        for r in responses {
            stream.extend(r);
        }
        MockHost::default()
            // The dial-by-ref resolution target (a bare URL — no password in it).
            .with_endpoint_ref(endpoint_ref, bare_url)
            // The password the host resolves for the terminated handshake, keyed by endpoint_ref.
            .with_credential(endpoint_ref, password)
            .with_conn_response(stream)
    }

    #[test]
    fn sql_queries_discovered_endpoint() {
        // The demo: an agent discovers a postgres endpoint and sql.query connects to it. The plugin
        // (and the model) never see a URL-with-password.
        let password = "k8s-scram-password";

        // (a) The PREFERRED shape: the full weak EndpointRef object passed inline, with a kubernetes
        // `credential_ref` (a location). The bare `url` carries NO password; the HOST resolves the
        // password against the `credential_ref` when it terminates the handshake — the plugin never
        // sees it.
        let endpoint = json!({
            "id": "@endpoint/pg-1",
            "url": "postgres://app@pg.monitoring.svc:5432/warehouse",
            "product": "postgres",
            "protocol": "postgres",
            "source": "discovered",
            // The `credential_ref` is a structured `Ref` (a location), exactly as `endpoint.select`
            // serializes it — never a value.
            "credential_ref": {"scheme": "kubernetes", "plugin": "monitoring", "instance": "pg-creds", "slot": "password"},
        });
        let mut host = MockHost::default()
            .with_endpoint_ref(
                "@endpoint/pg-1",
                "postgres://app@pg.monitoring.svc:5432/warehouse",
            )
            .with_credential("kubernetes/monitoring/pg-creds/password", password)
            .with_conn_response(query_response(
                &["id", "name"],
                &[vec![Some("1"), Some("ada")]],
            ));
        let out = run(
            "sql.query",
            json!({ "endpoint": endpoint, "query": "select id, name from users" }),
            &mut host,
        )
        .expect("sql.query against a discovered endpoint");
        assert_eq!(out["driver"], "postgres");
        assert_eq!(out["database"], "warehouse");
        assert_eq!(out["row_count"], 1);
        assert_eq!(out["rows"][0]["name"], "ada");
        // The bare (no-password) URL is surfaced; the password value never appears in the result.
        assert_eq!(
            out["endpoint_url"],
            "postgres://app@pg.monitoring.svc:5432/warehouse"
        );
        let dumped = out.to_string();
        assert!(
            !dumped.contains(password),
            "the password must never appear in the op's returned JSON: {dumped}"
        );

        // D-31 CONTRACT: on the PG path the plugin NEVER materializes the credential itself — it made
        // no `credential`/`secret` call, and no host call it made carried the password. The password
        // reaches the wire only inside the host-terminated `conn.authenticate`, which the plugin
        // invoked with a credential *location* (the `credential_ref`), not a value.
        let calls = host.calls.borrow();
        assert!(
            calls
                .iter()
                .all(|(cmd, _)| cmd != "credential" && cmd != "secret"),
            "the plugin must not call `credential`/`secret` on the host-terminated PG path: {:?}",
            calls.iter().map(|(c, _)| c).collect::<Vec<_>>()
        );
        assert!(
            calls
                .iter()
                .all(|(_, payload)| !payload.to_string().contains(password)),
            "no host-call payload the plugin sent may carry the password"
        );
        // It DID drive the flow by reference: dial-by-ref + a host-terminated conn.authenticate.
        assert!(calls.iter().any(|(cmd, _)| cmd == "conn.dial"));
        assert!(calls.iter().any(|(cmd, p)| cmd == "conn.authenticate"
            && p.get("credential_ref").is_some()
            && p.get("user").and_then(|u| u.as_str()) == Some("app")));

        // (b) The bare discovered `@endpoint/<id>` id-string shape is RETIRED with the `endpoint`
        // URL-handback it relied on (the real host never resolved discovered ids that way). Even
        // against a fully configured host it is rejected with a clear error pointing at the object
        // shape — before any dial or credential fetch.
        let mut host2 = discovered_host(
            "@endpoint/pg-1",
            "postgres://app@pg.monitoring.svc:5432/warehouse",
            password,
            vec![query_response(&["v"], &[vec![Some("ok")]])],
        );
        let err = run(
            "sql.test",
            json!({ "endpoint_ref": "@endpoint/pg-1" }),
            &mut host2,
        )
        .unwrap_err();
        assert!(
            err.contains("@endpoint/pg-1") && err.contains("endpoint.select"),
            "the id-string shape must point the caller at the `endpoint` object: {err}"
        );
        assert!(!err.contains(password), "no password in the error");
    }

    #[test]
    fn multi_instance_selection() {
        // Two different discovered postgres refs select two different endpoints — the ref drives the
        // target, no global state. Each carries its own bare URL + its own password.
        let ep = |id: &str, url: &str| {
            let name = id.trim_start_matches("@endpoint/").to_string();
            json!({
                "id": id,
                "url": url,
                "product": "postgres",
                "protocol": "postgres",
                "source": "discovered",
                "credential_ref": {"scheme": "kubernetes", "plugin": "ns", "instance": name, "slot": "password"},
            })
        };

        let mut host_a = MockHost::default()
            .with_endpoint_ref("@endpoint/pg-a", "postgres://ua@a.svc:5432/dba")
            .with_credential("kubernetes/ns/pg-a/password", "pw-a")
            .with_conn_response(query_response(&["db"], &[vec![Some("a")]]));
        let out_a = run(
            "sql.test",
            json!({ "endpoint": ep("@endpoint/pg-a", "postgres://ua@a.svc:5432/dba") }),
            &mut host_a,
        )
        .expect("instance a");
        assert_eq!(out_a["endpoint_url"], "postgres://ua@a.svc:5432/dba");
        assert_eq!(out_a["database"], "dba");

        let mut host_b = MockHost::default()
            .with_endpoint_ref("@endpoint/pg-b", "postgres://ub@b.svc:5432/dbb")
            .with_credential("kubernetes/ns/pg-b/password", "pw-b")
            .with_conn_response(query_response(&["db"], &[vec![Some("b")]]));
        let out_b = run(
            "sql.test",
            json!({ "endpoint": ep("@endpoint/pg-b", "postgres://ub@b.svc:5432/dbb") }),
            &mut host_b,
        )
        .expect("instance b");
        assert_eq!(out_b["endpoint_url"], "postgres://ub@b.svc:5432/dbb");
        assert_eq!(out_b["database"], "dbb");

        // Distinct targets selected purely by the passed ref.
        assert_ne!(out_a["endpoint_url"], out_b["endpoint_url"]);
        assert_ne!(out_a["database"], out_b["database"]);

        // A ref whose endpoint isn't configured in the host errors (the ref drives the dial/cred).
        let mut bare = MockHost::default();
        assert!(run(
            "sql.test",
            json!({ "endpoint": ep("@endpoint/pg-x", "postgres://ux@x.svc:5432/dbx") }),
            &mut bare,
        )
        .is_err());
    }

    #[test]
    fn sqlite_routes_to_a_clear_error() {
        // D-198: mysql/mariadb no longer belongs here — it is implemented. SQLite still does, and
        // still by design: it is a local file, not a socket.
        let mut sqlite = MockHost::default().with_config("dsn", "sqlite:///tmp/app.db");
        let err = run("sql.test", json!({}), &mut sqlite).unwrap_err();
        assert!(err.contains("sqlite unsupported"), "err = {err}");
    }

    // -----------------------------------------------------------------
    // D-197 / D-198: MySQL / MariaDB
    // -----------------------------------------------------------------

    #[test]
    fn mariadb_test_op_connects_and_reports_the_driver() {
        // The op that used to return "mysql is not yet supported" now completes.
        let mut host = mysql_host_with(vec![my_result_set(&["1"], &[vec![Some("1")]], false)]);
        let out = run("sql.test", json!({}), &mut host).expect("sql.test on mariadb");
        assert_eq!(out["status"], "ok");
        assert_eq!(out["driver"], "mysql");
        assert_eq!(out["database"], "warehouse");
    }

    #[test]
    fn mysql_query_decodes_columns_rows_and_nulls() {
        let mut host = mysql_host_with(vec![my_result_set(
            &["id", "email"],
            &[
                vec![Some("1"), Some("a@example.com")],
                vec![Some("2"), None],
            ],
            false,
        )]);
        let out = run(
            "sql.query",
            json!({"query": "SELECT id, email FROM users"}),
            &mut host,
        )
        .expect("sql.query on mariadb");
        assert_eq!(out["columns"], json!(["id", "email"]));
        assert_eq!(out["row_count"], 2);
        assert_eq!(out["rows"][0]["email"], "a@example.com");
        assert_eq!(
            out["rows"][1]["email"],
            Value::Null,
            "the 0xfb marker must decode to SQL NULL, not an empty string"
        );
    }

    #[test]
    fn mysql_decodes_both_deprecate_eof_result_set_shapes() {
        // The client is NOT told whether CLIENT_DEPRECATE_EOF was negotiated; it must decode either
        // shape from the wire. Both arms must produce identical output.
        for with_eof in [true, false] {
            let mut host = mysql_host_with(vec![my_result_set(
                &["n"],
                &[vec![Some("7")], vec![Some("8")]],
                with_eof,
            )]);
            let out = run("sql.query", json!({"query": "SELECT n FROM t"}), &mut host)
                .unwrap_or_else(|e| panic!("with_eof={with_eof}: {e}"));
            assert_eq!(out["row_count"], 2, "with_eof={with_eof}");
            assert_eq!(out["rows"][1]["n"], "8", "with_eof={with_eof}");
        }
    }

    #[test]
    fn mysql_decodes_an_empty_result_set_in_both_shapes() {
        // The case the intermediate-EOF peek can silently break: with no rows, the pre-DEPRECATE_EOF
        // stream is EOF-then-EOF while the post- stream is a single 0xfe OK. Both must yield zero
        // rows and neither may block waiting for a packet that is never coming.
        for with_eof in [true, false] {
            let mut host = mysql_host_with(vec![my_result_set(&["n"], &[], with_eof)]);
            let out = run("sql.query", json!({"query": "SELECT n FROM t"}), &mut host)
                .unwrap_or_else(|e| panic!("with_eof={with_eof}: {e}"));
            assert_eq!(out["row_count"], 0, "with_eof={with_eof}");
            assert_eq!(out["columns"], json!(["n"]), "with_eof={with_eof}");
        }
    }

    #[test]
    fn mysql_err_packet_surfaces_code_and_sqlstate() {
        let mut host = mysql_host_with(vec![my_packet(
            1,
            &my_err(1146, "42S02", "Table 'warehouse.nope' doesn't exist"),
        )]);
        let err = run(
            "sql.query",
            json!({"query": "SELECT * FROM nope"}),
            &mut host,
        )
        .unwrap_err();
        assert!(
            err.contains("1146") && err.contains("42S02") && err.contains("doesn't exist"),
            "err = {err}"
        );
    }

    #[test]
    fn mysql_reassembles_a_payload_split_at_the_packet_ceiling() {
        // A payload of exactly 0xFFFFFF continues into the next packet. Build one row whose single
        // cell fills the first packet to the brim, so the client must stitch both to decode it.
        let value_len = 0x00FF_FFFF - 4; // 4 = the 0xfd + 3-byte length prefix
        let value = "x".repeat(value_len);
        let mut row_payload = my_lenenc(value.as_bytes());
        assert_eq!(row_payload.len(), 0x00FF_FFFF);

        let mut stream = Vec::new();
        stream.extend(my_packet(1, &[1u8])); // column count
        stream.extend(my_packet(2, &my_column_def("blob")));
        // The row, split: a full-size packet then an empty continuation marking the end.
        let head: Vec<u8> = std::mem::take(&mut row_payload);
        stream.extend(my_packet(3, &head));
        stream.extend(my_packet(4, &[]));
        stream.extend(my_packet(5, &my_eof_ok()));

        let mut host = mysql_host_with(vec![stream]);
        let out = run(
            "sql.query",
            json!({"query": "SELECT blob FROM t"}),
            &mut host,
        )
        .expect("split payload must reassemble");
        assert_eq!(out["row_count"], 1);
        assert_eq!(
            out["rows"][0]["blob"].as_str().map(|s| s.len()),
            Some(value_len)
        );
    }

    #[test]
    fn mysql_refuses_an_unbounded_packet_chain() {
        // A hostile endpoint answers with an endless chain of full-size packets. Reassembly must hit
        // a ceiling rather than grow the buffer until the plugin subprocess is killed.
        let mut stream = Vec::new();
        stream.extend(my_packet(1, &[1u8]));
        stream.extend(my_packet(2, &my_column_def("blob")));
        // Five full-size continuation packets: 5 x 16 MiB overruns the 64 MiB cap.
        let full = vec![b'x'; MYSQL_MAX_PACKET];
        for seq in 3..8u8 {
            stream.extend(my_packet(seq, &full));
        }
        let mut host = mysql_host_with(vec![stream]);
        let err = run(
            "sql.query",
            json!({"query": "SELECT blob FROM t"}),
            &mut host,
        )
        .unwrap_err();
        assert!(
            err.contains("exceeds") && err.contains("cap"),
            "an unbounded chain must be refused at the ceiling: {err}"
        );
    }

    #[test]
    fn mysql_write_queries_are_still_rejected() {
        // The read-only guard is dialect-independent — it must hold on the MySQL path too.
        let mut host = mysql_host_with(vec![my_result_set(&["x"], &[], false)]);
        let err = run(
            "sql.query",
            json!({"query": "delete from users"}),
            &mut host,
        )
        .unwrap_err();
        assert!(err.contains("read-only"), "err = {err}");
    }

    #[test]
    fn mysql_table_list_reads_information_schema_not_pg_catalog() {
        let mut host = mysql_host_with(vec![my_result_set(
            &["table_schema", "table_name", "table_type", "row_estimate"],
            &[vec![
                Some("warehouse"),
                Some("orders"),
                Some("BASE TABLE"),
                Some("42"),
            ]],
            false,
        )]);
        let out = run("sql.table.list", json!({}), &mut host).expect("table.list on mariadb");
        assert_eq!(out["tables"][0]["name"], "orders");
        assert_eq!(out["tables"][0]["type"], "table");
        assert_eq!(out["tables"][0]["row_estimate"], 42);

        let sql = sent_queries(&host).join(" ");
        assert!(
            sql.contains("information_schema.tables"),
            "MySQL must not be sent pg_class: {sql}"
        );
        assert!(
            !sql.contains("pg_class") && !sql.contains("pg_namespace"),
            "no pg_catalog SQL may reach MySQL: {sql}"
        );
        assert!(
            sql.contains("performance_schema"),
            "MySQL system schemas must be filtered, not pg_catalog: {sql}"
        );
    }

    #[test]
    fn mysql_database_list_returns_databases_never_schemas() {
        // The semantic divergence, pinned: MySQL has no schema-vs-database distinction, so every
        // entry is `kind: "database"` — where the Postgres path also emits `kind: "schema"` rows.
        let mut host = mysql_host_with(vec![my_result_set(
            &["name", "current_db"],
            &[
                vec![Some("warehouse"), Some("1")],
                vec![Some("analytics"), Some("0")],
            ],
            false,
        )]);
        let out = run("sql.database.list", json!({}), &mut host).expect("database.list on mariadb");
        assert_eq!(out["count"], 2);
        assert_eq!(out["databases"][0]["kind"], "database");
        assert_eq!(out["databases"][0]["current"], true);
        assert_eq!(out["databases"][1]["current"], false);
        assert!(
            out["databases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["kind"] == "database"),
            "MySQL must never report a `schema` kind: {out}"
        );
    }

    #[test]
    fn mysql_table_show_uses_the_mysql_foreign_key_shape() {
        let mut host = mysql_host_with(vec![
            // Columns.
            my_result_set(
                &[
                    "column_name",
                    "ordinal_position",
                    "data_type",
                    "is_nullable",
                    "column_default",
                    "character_maximum_length",
                ],
                &[vec![
                    Some("id"),
                    Some("1"),
                    Some("int"),
                    Some("NO"),
                    None,
                    None,
                ]],
                false,
            ),
            // Primary key.
            my_result_set(&["column_name"], &[vec![Some("id")]], false),
            // Foreign keys.
            my_result_set(
                &[
                    "constraint_name",
                    "column_name",
                    "referenced_table_name",
                    "referenced_column_name",
                ],
                &[vec![
                    Some("fk_customer"),
                    Some("customer_id"),
                    Some("customers"),
                    Some("id"),
                ]],
                false,
            ),
        ]);
        let out = run("sql.table.show", json!({"table": "orders"}), &mut host)
            .expect("table.show on mariadb");
        assert_eq!(out["primary_key"], json!(["id"]));
        assert_eq!(out["columns"][0]["primary_key"], true);
        assert_eq!(out["foreign_keys"][0]["ref_table"], "customers");

        let sql = sent_queries(&host).join(" ");
        assert!(
            !sql.contains("constraint_column_usage"),
            "MySQL exposes referenced_* on key_column_usage; the pg 3-way join must not be sent: {sql}"
        );
        assert!(
            sql.contains("constraint_name = 'PRIMARY'"),
            "the PK read must be table-scoped via MySQL's PRIMARY constraint name: {sql}"
        );
    }

    #[test]
    fn mysql_index_list_groups_one_row_per_column_into_one_entry_per_index() {
        // information_schema.statistics returns a row PER COLUMN, unlike pg_index.
        let mut host = mysql_host_with(vec![my_result_set(
            &[
                "table_schema",
                "table_name",
                "index_name",
                "non_unique",
                "seq_in_index",
                "column_name",
                "index_type",
            ],
            &[
                vec![
                    Some("warehouse"),
                    Some("orders"),
                    Some("PRIMARY"),
                    Some("0"),
                    Some("1"),
                    Some("id"),
                    Some("BTREE"),
                ],
                vec![
                    Some("warehouse"),
                    Some("orders"),
                    Some("idx_cust_date"),
                    Some("1"),
                    Some("1"),
                    Some("customer_id"),
                    Some("BTREE"),
                ],
                vec![
                    Some("warehouse"),
                    Some("orders"),
                    Some("idx_cust_date"),
                    Some("1"),
                    Some("2"),
                    Some("created_at"),
                    Some("BTREE"),
                ],
            ],
            false,
        )]);
        let out = run("sql.index.list", json!({}), &mut host).expect("index.list on mariadb");
        assert_eq!(out["count"], 2, "three rows must group into two indexes");
        assert_eq!(out["indexes"][0]["name"], "PRIMARY");
        assert_eq!(out["indexes"][0]["primary"], true);
        assert_eq!(out["indexes"][0]["unique"], true);
        assert_eq!(out["indexes"][1]["name"], "idx_cust_date");
        assert_eq!(out["indexes"][1]["primary"], false);
        assert_eq!(
            out["indexes"][1]["unique"], false,
            "non_unique=1 must invert to unique=false"
        );
        assert_eq!(
            out["indexes"][1]["columns"],
            json!(["customer_id", "created_at"]),
            "multi-column indexes must keep seq_in_index order"
        );
        assert!(
            out["indexes"][0].get("definition").is_none(),
            "MySQL has no pg_get_indexdef; `definition` is omitted rather than fabricated"
        );
    }

    #[test]
    fn my_lit_escapes_backslashes_that_pg_lit_would_let_through() {
        // MySQL treats `\` as an escape character inside string literals, so doubling only the quote
        // (as pg_lit does) would let `\'` terminate the literal and inject.
        assert_eq!(my_lit(r"a\'b"), r"a\\''b");
        assert_eq!(pg_lit(r"a\'b"), r"a\''b");
        assert_eq!(my_lit("plain"), "plain");
        assert_eq!(my_lit("it's"), "it''s");
    }

    #[test]
    fn static_endpoint_dials_by_reference() {
        // D-32: the static/named path must dial via `host.conn_dial_ref("sql.endpoint")` — the
        // host resolves the address; the plugin never dials a host:port it parsed from the DSN.
        // A mock that has the DSN config but NO `with_endpoint_ref("sql.endpoint", ...)` entry
        // must therefore fail at the dial with the mock's missing-ref error (a plugin-side
        // `ConnTarget::Tcp` dial would have succeeded and then timed out reading frames).
        let mut host =
            MockHost::default().with_config("dsn", "postgres://app@db.test:5432/warehouse");
        let err = run("sql.test", json!({}), &mut host).unwrap_err();
        assert!(err.contains("no endpoint_ref"), "err = {err}");
    }

    #[test]
    fn static_endpoint_host_terminates_auth_by_purpose() {
        // D-31: on the static/named path the plugin authenticates by REFERENCE — it passes the
        // declared "password" auth purpose to the host-terminated `conn.authenticate`, never reading
        // a secret itself. It makes no `credential`/`secret` call, and no host call carries a value.
        let mut host = host_with(vec![query_response(&["?column?"], &[vec![Some("1")]])]);
        let out = run("sql.test", json!({}), &mut host).expect("sql.test");
        assert_eq!(out["status"], "ok");
        let calls = host.calls.borrow();
        assert!(
            calls
                .iter()
                .all(|(cmd, _)| cmd != "credential" && cmd != "secret"),
            "the static PG path must not call `credential`/`secret`: {:?}",
            calls.iter().map(|(c, _)| c).collect::<Vec<_>>()
        );
        // It authenticated by the declared purpose (a location), not a value.
        assert!(calls.iter().any(|(cmd, p)| cmd == "conn.authenticate"
            && p.get("auth_purpose").and_then(|v| v.as_str()) == Some("password")
            && p.get("user").and_then(|v| v.as_str()) == Some("app")));
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
        assert_eq!(t.database, "warehouse");
        assert_eq!(t.dsn_user.as_deref(), Some("app"));
        // An inline password (from a weak-ref URL) is redacted into `safe_url` and never stored on
        // the target or sent to the host — the host resolves the credential itself (D-31).
        assert_eq!(t.safe_url, "postgres://app:xxxxx@db.test:6543/warehouse");
        assert!(
            !t.safe_url.contains("s3cr3t"),
            "password must be redacted: {}",
            t.safe_url
        );

        // Default port + percent-decoded password (redacted).
        let t = target_from_url(None, "postgresql://u:p%40ss@h/db", None).unwrap();
        assert_eq!(
            t.safe_url,
            format!("postgres://u:xxxxx@h:{PG_DEFAULT_PORT}/db")
        );
        assert!(!t.safe_url.contains("p@ss"), "password must be redacted");

        // Driver override wins over scheme; sqlite is rejected at parse time.
        assert!(target_from_url(None, "sqlite:///x.db", None).is_err());
        let t = target_from_url(Some("mysql"), "mysql://root@h:3306/app", None).unwrap();
        assert_eq!(t.dialect, Dialect::MySql);
        assert_eq!(t.safe_url, "mysql://root@h:3306/app");
    }

    // NOTE (D-31): the SCRAM-SHA-256 RFC 7677 derivation + server-signature check and the MD5
    // vectors now live in `flux-plugin`'s `pg` module (the host terminates the auth handshake), and
    // are covered there by hermetic tests against a scripted PG-server stub.

    #[test]
    fn timeout_is_parsed_and_invalid_timeout_fails_fast() {
        // No server response is queued: an invalid timeout must fail before the plugin dials.
        let mut host = host_with(vec![]);
        let err = run("sql.test", json!({"timeout": "not-a-duration"}), &mut host).unwrap_err();
        assert!(err.contains("timeout"), "err = {err}");
    }

    #[test]
    fn timeout_is_enforced_on_read_when_no_server_data() {
        // D-45: a valid `timeout` forwards a per-read deadline to the host's `conn.read`. The auth
        // handshake is host-terminated (the mock `conn.authenticate` returns immediately), so with no
        // POST-AUTH query frames queued the first Simple Query read returns ErrorKind::TimedOut (not a
        // silent hang or a clean EOF) — surfaced as "timed out".
        let mut host = MockHost::default()
            .with_config("dsn", "postgres://app@db.test:5432/warehouse")
            .with_endpoint_ref("sql.endpoint", "postgres://app@db.test:5432/warehouse");
        let err = run("sql.test", json!({"timeout": "1ms"}), &mut host).unwrap_err();
        assert!(err.contains("timed out"), "err = {err}");
    }

    #[test]
    fn parse_duration_accepts_go_style_durations() {
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5x").is_err());
    }
}

// ===========================================================================
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    /// The normalized kind of one input property.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        Object,
        Enum(Vec<String>),
    }

    #[derive(Clone)]
    struct Prop {
        name: &'static str,
        kind: Kind,
    }

    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }

    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }

    /// The authoritative contract (post-D-36 re-audit + timeout port). All 7 ops carry the
    /// flattened `ConnProps` connection fields plus op-specific params.
    fn contracts() -> Vec<(&'static str, OpContract)> {
        let conn_props = || {
            vec![
                p("endpoint", Kind::Object),
                p("endpoint_ref", Kind::Str),
                p(
                    "driver",
                    Kind::Enum(vec!["postgres".into(), "mysql".into(), "sqlite".into()]),
                ),
                p("database", Kind::Str),
                p("timeout", Kind::Str),
            ]
        };
        vec![
            (
                "sql.test",
                OpContract {
                    props: conn_props(),
                    required: vec![],
                },
            ),
            (
                "sql.query",
                OpContract {
                    props: {
                        let mut v = conn_props();
                        v.push(p("query", Kind::Str));
                        v.push(p("max_rows", Kind::Int));
                        v
                    },
                    required: vec!["query"],
                },
            ),
            (
                "sql.database.list",
                OpContract {
                    props: conn_props(),
                    required: vec![],
                },
            ),
            (
                "sql.table.list",
                OpContract {
                    props: {
                        let mut v = conn_props();
                        v.push(p("schema", Kind::Str));
                        v.push(p("include_views", Kind::Bool));
                        v.push(p("max_results", Kind::Int));
                        v
                    },
                    required: vec![],
                },
            ),
            (
                "sql.table.show",
                OpContract {
                    props: {
                        let mut v = conn_props();
                        v.push(p("schema", Kind::Str));
                        v.push(p("table", Kind::Str));
                        v
                    },
                    required: vec!["table"],
                },
            ),
            (
                "sql.index.list",
                OpContract {
                    props: {
                        let mut v = conn_props();
                        v.push(p("schema", Kind::Str));
                        v.push(p("table", Kind::Str));
                        v
                    },
                    required: vec![],
                },
            ),
        ]
    }

    fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
        if let Some(obj) = node.as_object() {
            if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
                if let Some(name) = r
                    .strip_prefix("#/definitions/")
                    .or_else(|| r.strip_prefix("#/$defs/"))
                {
                    return defs.get(name).unwrap_or(node);
                }
            }
            if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
                for m in any {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
        }
        node
    }

    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }

    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "string" => {
                if let Some(e) = node.get("enum").and_then(|v| v.as_array()) {
                    let vals: Vec<String> = e
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    return Kind::Enum(vals);
                }
                Kind::Str
            }
            "object" | "" => Kind::Object,
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        let defs = schema
            .get("definitions")
            .or_else(|| schema.get("$defs"))
            .cloned()
            .unwrap_or(json!({}));
        assert_eq!(schema["type"], "object", "{op_name}: root type");

        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                let resolved = resolve(v, &defs);
                got.insert(k.as_str(), kind_of(resolved));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got.get(*name).unwrap_or_else(|| {
                panic!("{op_name}: missing property `{name}` in derived schema")
            });
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }

        let req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut req_set: Vec<&str> = req.clone();
        req_set.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req_set, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_legacy_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .filter(|o| !o.internal)
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}
