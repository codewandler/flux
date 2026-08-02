# Design: MariaDB / MySQL support in the `sql` plugin

**Status:** implemented 2026-07-28 (unreleased) · **Pillar:** Core · **Epic slug:** `mariadb-support` ·
**Stories:** [D-196](../stories/D-196-host-terminated-mysql-auth.md),
[D-197](../stories/D-197-mysql-wire-client.md),
[D-198](../stories/D-198-dialect-aware-introspection-ops.md)

Completes the residual [D-31](../stories/D-31-host-terminated-rawsocket-auth.md) left open — *"mysql
+ the then-planned second raw-socket host-termination consumer"* —
and the one [D-16](../stories/D-16-datastore-infra-plugins.md) recorded as *"MySQL/SQLite explicit
residuals"*. Prerequisite reading: [endpoint-discovery.md](endpoint-discovery.md), whose reference
invariant is the constraint that shapes every decision below.

## Why

An external user pointed a MariaDB endpoint at the `sql` plugin and got
`mysql is not yet supported by the flux sql plugin (residual)`. That is accurate — `mariadb`
normalizes to `Dialect::MySql` (`plugins/sql/src/main.rs:339`) and all six ops call
`require_postgres()` before doing anything (`:932,967,1003,1062,1134,1249`) — but it is a real
capability gap in the most widely deployed SQL engine after Postgres, and the seam D-31 promised has
now had a user walk into it.

## The question this design has to answer first

The obvious objection is that this should not need a design at all: a SQL connection is a TCP
connection, and Go solves exactly this with a dialer — `go-sql-driver/mysql` exposes
`RegisterDialContext` so a caller injects its own transport and the driver speaks the protocol on
top. flux already has the transport: `ConnStream` implements `std::io::Read + Write`
(`plugins/host-kit/src/lib.rs:818+`) over the host-proxied `conn.*` capability. Why not hand that to
a driver crate and be done?

Three reasons, and only the third is decisive.

1. **No Rust MySQL crate exposes a dialer seam.** Go's ecosystem is built on the `net.Conn`
   interface; Rust's MySQL crates take a URL or an options struct and dial internally.
   `mysql_async::Opts` and `sqlx::MySqlConnectOptions` expose host/port or a Unix **socket path** —
   never a caller-supplied stream. (`tokio-postgres::connect_raw` is the exception that proves the
   rule, and is why Postgres would have been the easy case had we needed it.)

2. **Async mismatch.** `ConnStream` is blocking — every `read` is a synchronous round-trip over the
   plugin's stdio protocol to the host. Driver crates want `AsyncRead + AsyncWrite` on a tokio
   runtime. Bridgeable with an adapter, but per-read deadlines (`set_read_deadline`, D-45) stop
   composing cleanly.

3. **Host-terminated auth makes driver crates unusable in principle.** This is the decisive one. The
   reference invariant says a plugin *"never receives a raw secret value"*; D-31 discharged it for
   raw-socket protocols by moving the handshake host-side, so the plugin is handed a **post-auth**
   `conn_id` (`crates/flux-plugin/src/pg.rs`). A driver crate insists on performing its *own*
   handshake, which requires the password inside the plugin. So the fork is: break the invariant, or
   teach the host MySQL's handshake and hand over a post-auth socket — at which point no driver crate
   is usable anyway, because none of them will resume from a mid-stream post-auth state.

**Decision: mirror the Postgres split.** Host speaks auth, plugin speaks the query protocol. This is
not a workaround for a missing dialer; it is what the invariant costs, and Postgres already paid it.

### Rejected: a "trusted plugin" tier that dials directly

Considered and rejected. It is coherent — a policy tier where a first-party plugin holds a DSN with
credentials and opens its own socket, letting us use `mysql_async` unmodified. It is rejected because
it negates D-25…D-32: the invariant's value is that it is **absolute and testable** (`no password in
any host-call payload` is asserted by MockHost call-log tests today), and a trust tier makes it
conditional. `pg.rs` already calls `sql` *"the trusted `sql` plugin"* — trusted meaning
first-party/in-repo, and still credential-free. Trust must not come to mean "gets the password."

If an escape hatch is ever wanted, the right shape is a per-endpoint, user-declared
`endpoint.direct_dial` opt-in — the operator takes the risk explicitly on one endpoint — not a
plugin-level trust bit that silently widens every op. **Out of scope for this epic**; recorded here
so the option is not re-litigated from scratch.

## Architecture

| Layer | Component | Story |
|---|---|---|
| Host (L4) | `crates/flux-plugin/src/mysql.rs` — handshake v10 + `mysql_native_password`, dispatched from `pg::terminate_handshake` | D-196 |
| Plugin | `MySqlClient` in `plugins/sql/src/main.rs` — COM_QUERY over `ConnStream` | D-197 |
| Plugin | Per-dialect introspection SQL for all six ops | D-198 |

The existing dispatch seam (`crates/flux-plugin/src/pg.rs:579`) already matches on protocol and
errors for anything but Postgres. D-196 adds the `mysql`/`mariadb` arm; the function's home moves to
a neutral module so `pg.rs` stops being the dispatcher for a protocol it does not speak.

### D-196 — host-terminated auth

Server sends **Handshake v10**: protocol version, server version, connection id, `auth-plugin-data`
split across two fields (8 bytes + ≥12 bytes), capability flags in two halves, and the auth plugin
name. Host replies **HandshakeResponse41**: capability flags, max packet size, charset, 23 reserved
zero bytes, NUL-terminated username, the auth response, optional database, optional plugin name.
Then OK (`0x00`) or ERR (`0xff`).

`mysql_native_password` is the target: `SHA1(pw) XOR SHA1(scramble ‖ SHA1(SHA1(pw)))`. It is
MariaDB's default when a user is created without an explicit plugin, is statically linked into the
server, and covers MySQL 5.7 as well. Notably **simpler than the SCRAM-SHA-256 already shipped** — no
PBKDF2, no iteration-count ceiling, no server-signature verification, because the protocol offers no
server authentication to verify.

Explicitly **not** in round one, each with a distinct error naming the plugin:

- `caching_sha2_password` — MySQL 8.0+ default. Needs RSA public-key exchange or an existing TLS
  channel for the full-auth path; the fast path only works against a server-side cache we cannot rely
  on for a first connection. Substantial enough to be its own story.
- `ed25519` — MariaDB's shared library ships by default but the plugin is **not installed** by
  default.
- `parsec` — slated to become MariaDB's default in a future release (Community 11.6 / Enterprise
  11.8). Worth tracking; not yet common in the field.

Reusing D-31's hardening: the `MAX_MESSAGE_BYTES` ceiling applies unchanged (MySQL's 3-byte length
caps a single packet at 16 MiB, so the bound is cheap insurance against a hostile multi-packet
stream), and the resolved password stays redactor-registered and never crosses a plugin frame.

### D-197 — the wire client

Framing is 3-byte little-endian payload length + 1-byte sequence id. Payloads at `0xFFFFFF` continue
into a following packet — a real case for `SHOW CREATE TABLE` on wide schemas, so it must be handled,
not asserted away.

`COM_QUERY` (`0x03`) response is a small state machine: ERR, or OK, or a length-encoded column count
followed by N column-definition packets, optionally an EOF, the text-protocol rows, and a terminating
EOF/OK. Two decoding subtleties carry most of the risk:

- **Length-encoded integers** — `0xfb` means NULL in a row context but is a *length prefix* elsewhere;
  `0xfc/0xfd/0xfe` introduce 2/3/8-byte widths.
- **`CLIENT_DEPRECATE_EOF`** — whether the intermediate and terminating EOF packets exist at all
  depends on a capability flag negotiated back in D-196's handshake, so the two stories share the
  negotiated-capabilities value. The host must return it to the plugin in the handshake result.

Text protocol only. Every value arrives as a string, exactly like the Postgres client's
`Option<String>` cells, so `QueryResult` is reused unchanged and the output-shaping helpers below it
need no dialect awareness.

### D-198 — dialect-aware introspection

The wire work is necessary but not sufficient: the six ops' SQL is Postgres-specific, and in one case
Postgres-*semantic*.

- `table.list` (`:1087`) and `index.list` (`:1268`) read `pg_class` / `pg_index` / `pg_namespace` —
  no MySQL equivalent; rewrite against `information_schema.tables` and `information_schema.statistics`
  (or `SHOW INDEX`).
- `table.show` (`:1154,1188,1218`) reads `information_schema` but the **foreign-key shape differs**:
  Postgres needs a three-way join through `constraint_column_usage`, while MySQL puts
  `referenced_table_name` / `referenced_column_name` directly on `key_column_usage` — simpler, but a
  different query, not a dialect-tweaked one.
- `database.list` (`:1038`) is the semantic trap. It queries `information_schema.schemata`, which
  parses on both engines — but Postgres models database > schema > table while MySQL treats schema
  and database as **the same object**. So the op today lists *schemas inside the connected database*
  on Postgres and would list *actual databases* on MySQL. Same SQL, different meaning. The op's
  documented contract must say which it returns per dialect rather than pretend the difference away.

Filter lists differ too: Postgres excludes `pg_catalog`/`information_schema`; MySQL must exclude
`information_schema`/`mysql`/`performance_schema`/`sys`.

Mechanically: `require_postgres()` is deleted, replaced by per-op dialect dispatch. The read-only
guard (`:799,895`) is dialect-independent and stays as-is — `SELECT`/`SHOW`/`DESCRIBE`/`EXPLAIN`/
`WITH` is a valid read-only allowlist on both engines.

## Testing

Follows D-31's hermetic pattern exactly, and inherits its honesty caveat: `MockHost` tests replay
**hand-crafted** server frames, which prove the frame parser and message assembly against bytes the
test author wrote — *not* live interop. Specifically:

- D-196: scripted MySQL-server stub in `flux-plugin` — handshake v10 → native-password success;
  wrong-password ERR; a `caching_sha2_password` server rejected with the named error; an
  `AuthSwitchRequest` to an unsupported plugin rejected cleanly rather than hanging.
- D-196: the D-31 assertion re-run for this path — no password in any plugin frame, no `credential`
  callback.
- D-197: `MockHost` COM_QUERY replays — multi-column result set, NULL cells, ERR mid-stream, a
  `0xFFFFFF` split payload, and both `CLIENT_DEPRECATE_EOF` settings.
- D-198: per-dialect SQL assertions per op, and a test that the same op returns the documented
  (different) meaning on each dialect for `database.list`.

The gate itself cannot verify live interop, so that was done by hand.

**Live verification (2026-07-28)** — all six ops against MySQL **5.7.44** in the dev cluster's
`latest` namespace, via `kubectl port-forward` to `svc/vicidial-db`. The real Handshake v10 confirmed
every assumption this design makes: protocol version `0x0a`, `auth_plugin_data_len` `0x15`
(20 + NUL, split 8 + 12), capabilities `0xc1ffffff`, and auth plugin `mysql_native_password`. Real
result sets decoded correctly, including `NULL` versus `''`; a wrong password surfaced the server's
own `ERR 1045 (28000)`.

**What that does and does not prove.** MySQL 5.7 and MariaDB share this wire protocol and this auth
plugin, so the whole implemented path is exercised — but the server was *not* MariaDB, and MariaDB's
own options (`ed25519`, `parsec`) remain untested. The 22 unit tests are still hand-crafted frames.

The smoke test also demonstrated the packaging split in the *Non-goals* below: the registered `sql`
plugin was pack **v0.1.0**, and until it was replaced with a local build the new host binary met an
old plugin. Host and pack must ship together.

## Non-goals

- **SQLite** — unchanged and still unsupported by design: a local file, and plugins have no
  filesystem capability (`conn.*` is sockets only, `:527`). Would need a new host file capability.
- **Writes** — the plugin stays read-only on every dialect.
- The other D-31 residual was retired with the plugin that needed it (D-249).
- **`caching_sha2_password` / `ed25519` / `parsec`** — deliberate follow-ons, filed when D-196 lands
  rather than pre-emptively.
- A `direct_dial` escape hatch (see *Rejected* above).

## What changed during implementation

Two decisions above did not survive contact with the code. Recorded here so the design matches what
shipped rather than what was planned.

**The `CLIENT_DEPRECATE_EOF` plumbing was dropped.** D-196 was to return the negotiated capability
flags so D-197 could parse the result-set stream. It does return them (an additive `capabilities`
field on the `conn.authenticate` response), but the plugin does **not** read them, because carrying
them into the plugin's Rust API meant widening `HandshakeInfo` — a `codewandler-flux-host-kit`
**1.0.0** type with no `#[non_exhaustive]`, so a new public field is a semver break on the protocol
line C-143 deliberately separated from the flux version. It turned out to be unnecessary anyway: both
result-set terminators carry a `0xfe` header and the spec fixes their sizes — a classic EOF payload is
exactly 5 bytes (`0xfe` + warnings + status), every OK packet at least 7, and a row opening with
`0xfe` announces an 8-byte length-encoded integer so needs ≥ 9. Deciding from the bytes is not just
cheaper than the flag, it is *more* robust: it stays correct under host/plugin version skew, which a
negotiated flag would not.

**Two hazards the plan missed**, both found while writing the per-dialect SQL:

- **Escaping is not portable.** `pg_lit()` doubles `'` and stops there, which is correct for Postgres
  with `standard_conforming_strings`. MySQL treats `\` as an escape character inside string literals,
  so a schema or table name containing `\'` would have terminated the literal and injected. Hence
  `my_lit()`, which escapes the backslash first.
- **MySQL primary-key constraint names are not schema-unique.** Every MySQL PK is named `PRIMARY`, so
  the Postgres join on `constraint_name` + `constraint_schema` would have matched *every* table's
  primary key in the schema at once. The MySQL path reads `key_column_usage` scoped by table.

Both are the kind of thing "port the SQL" hides, which is the case for treating D-198 as design work
rather than translation.
