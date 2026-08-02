//! Protocol-neutral host-terminated authentication (D-31 · D-196).
//!
//! The host — not the plugin — speaks each raw-socket protocol's in-band authentication handshake,
//! so a plugin that needs an authenticated connection is handed a *post-auth* stream and never
//! receives the credential. This module owns the shared vocabulary ([`HandshakeParams`],
//! [`HandshakeResult`]) and the [`terminate_handshake`] dispatch; each protocol's wire work lives in
//! its own module ([`crate::pg`], [`crate::mysql`]).
//!
//! The dispatch lived in `pg.rs` while Postgres was the only terminated protocol. D-196 moved it
//! here so the Postgres module is no longer the router for protocols it does not speak.
//!
//! Scope, for every protocol: exactly the auth phase. The host speaks **no** SQL — the query
//! protocol stays in the plugin, driven over the same post-auth `conn_id`.

use std::collections::BTreeMap;
use std::time::Duration;

use flux_system::net::DialStream;

/// Upper bound on a single server message body, enforced *before* buffering it (C-84). The wire
/// length is a server-declared integer the host has not yet validated; a hostile/MITM'd endpoint can
/// declare a huge body and drive an unbounded `read_exact` allocation. Auth-phase messages are
/// kilobytes at most on both protocols, so a few-MB ceiling is orders of magnitude above anything
/// legitimate yet refuses the DoS.
///
/// MySQL's 3-byte length field already caps one packet at 16 MiB, but a multi-packet payload is
/// unbounded without this, so the check applies there too.
pub(crate) const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Connection parameters the host puts in the protocol's startup/handshake message. All non-secret
/// metadata the plugin already holds (from a discovered endpoint's bare URL or the config DSN) —
/// never the credential.
pub(crate) struct HandshakeParams {
    pub user: String,
    pub database: String,
    /// **Postgres only.** Sent as the `application_name` startup parameter, where it surfaces in
    /// `pg_stat_activity`. The MySQL terminator ignores it: the equivalent is a `program_name` entry
    /// in `CLIENT_CONNECT_ATTRS`, which this client does not negotiate, so a MariaDB connection is
    /// anonymous in `SHOW PROCESSLIST` and the slow log. Callers pass a name regardless; it is
    /// silently dropped on that path.
    pub application_name: String,
}

/// The negotiated connection state the host hands back to the plugin after a successful handshake.
/// Never the password. The connection is left ready for the plugin's query protocol.
#[derive(Debug, Default)]
pub(crate) struct HandshakeResult {
    pub parameters: BTreeMap<String, String>,
    pub backend_pid: Option<i32>,
    pub backend_key: Option<i32>,
    /// Negotiated protocol capability bits, for protocols that have them (MySQL); `None` for
    /// Postgres, which has no equivalent.
    ///
    /// Reported on the `conn.authenticate` response for **diagnosis only** — no plugin consumes it.
    /// It was originally intended to tell the `sql` plugin whether `CLIENT_DEPRECATE_EOF` was
    /// negotiated, but reaching the plugin means a new public field on `host_kit::HandshakeInfo`, a
    /// 1.0.0 protocol-line type with no `#[non_exhaustive]` — a semver break. The plugin decodes
    /// both result-set shapes from packet sizes instead, which is also skew-proof. Kept because a
    /// future terminator (`caching_sha2_password`) will need the negotiated flags host-side anyway.
    pub capabilities: Option<u32>,
}

impl HandshakeResult {
    pub fn server_version(&self) -> Option<&str> {
        self.parameters.get("server_version").map(String::as_str)
    }
}

/// Terminate `protocol`'s authentication handshake on an already-dialed `stream`.
///
/// The aliases match the `sql` plugin's own `normalize_dialect` (`plugins/sql/src/main.rs`) so a
/// dialect the plugin accepts never fails here on a naming mismatch alone.
pub(crate) async fn terminate_handshake(
    protocol: &str,
    stream: &mut DialStream,
    params: &HandshakeParams,
    password: &str,
    timeout: Option<Duration>,
) -> Result<HandshakeResult, String> {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "pg" | "pgx" => {
            crate::pg::authenticate(stream, params, password, timeout).await
        }
        "mysql" | "mariadb" => crate::mysql::authenticate(stream, params, password, timeout).await,
        other => Err(format!(
            "conn.authenticate: host-terminated auth is not implemented for protocol {other:?} \
             (postgres and mysql/mariadb are terminated); the gated `credential` capability \
             remains for other protocols"
        )),
    }
}
