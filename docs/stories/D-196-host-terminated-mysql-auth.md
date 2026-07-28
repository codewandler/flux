---
id: D-196
title: Host-terminated MySQL/MariaDB authentication (handshake v10 + mysql_native_password)
pillar: Core
status: done
priority:
epic: mariadb-support
design: docs/designs/mariadb-support.md
areas: [plugins, security]
note: closes the mysql half of the D-31 residual — the host speaks handshake v10 so the sql plugin still never receives the password
---

# Host-terminated MySQL/MariaDB authentication

## Goal
Teach the host to terminate the MySQL/MariaDB connection handshake, exactly as
[D-31](D-31-host-terminated-rawsocket-auth.md) did for Postgres, so the `sql` plugin is handed a
*post-auth* `conn_id` and never receives the credential. This is the first of the two residuals D-31
recorded: *"mysql + Asterisk AMI host-termination (seam in place, clear error, credential cap
retained for them)"*.

## Why
A user pointed a MariaDB endpoint at the `sql` plugin and hit
`mysql is not yet supported by the flux sql plugin (residual)`. Supporting it means speaking the
MySQL wire protocol, and the reference invariant dictates *who* speaks which half: the host owns
auth, the plugin owns queries. See [mariadb-support.md](../designs/mariadb-support.md) — *The
question this design has to answer first* — for why an off-the-shelf driver crate cannot be used
here, and why a "trusted plugin that dials directly" was rejected.

## Acceptance
- [x] **Handshake v10 + `mysql_native_password`** — new `crates/flux-plugin/src/mysql.rs` reads the
      server's Handshake v10, replies with HandshakeResponse41 carrying
      `SHA1(pw) XOR SHA1(scramble ‖ SHA1(SHA1(pw)))`, and drains to the OK packet. Failing-first test:
      a hermetic scripted MySQL-server stub asserts a successful auth and the captured server version
      + negotiated capability flags.
- [x] **Dispatch seam** — `terminate_handshake` (`crates/flux-plugin/src/pg.rs:579`) gains the
      `mysql`/`mariadb` arm. The function moves out of `pg.rs` into a protocol-neutral home so the
      Postgres module is no longer the dispatcher for a protocol it does not speak.
- [x] **Negotiated capabilities returned to the plugin** — `HandshakeResult` carries the negotiated
      capability flags, because `CLIENT_DEPRECATE_EOF` decides the shape of the result-set stream
      D-197 has to parse.
- [x] **Unsupported auth plugins error by name** — `caching_sha2_password`, `ed25519`, and `parsec`
      each produce a distinct, actionable error rather than a hang or a generic failure, including
      when the server requests one via `AuthSwitchRequest` mid-handshake. Failing-first test per
      plugin name.
- [x] **The invariant holds on the new path** — the D-31 assertions re-run for MySQL: no password in
      any plugin frame, no `credential` callback. Failing-first test asserts the MockHost call log.
- [x] **Bounded reads** — the `MAX_MESSAGE_BYTES` ceiling (C-84) applies to the MySQL path too, so a
      hostile server cannot drive an unbounded buffer across a multi-packet payload.
- [x] Gate green: `cargo test -p flux-plugin`, clippy `-D warnings`, fmt, `flux-codegate`.

## Progress
- **Done (2026-07-28).**
  - **Protocol-neutral seam:** new `crates/flux-plugin/src/handshake.rs` owns `HandshakeParams`,
    `HandshakeResult`, `MAX_MESSAGE_BYTES`, and `terminate_handshake`. `pg.rs` is no longer the
    dispatcher for protocols it does not speak; it imports the shared types and keeps only the
    Postgres wire work + SCRAM crypto. Clean cutover — no compat re-exports.
  - **MySQL terminator:** new `crates/flux-plugin/src/mysql.rs` — Handshake v10 parse (two-part
    scramble reassembly, capability halves, tolerant of a missing trailing NUL on the last field) →
    HandshakeResponse41 → OK/ERR, with one AuthSwitchRequest handled. `mysql_native_password` via a
    vendored SHA-1, following `pg.rs`'s vendored-MD5 precedent and rationale (the *server* picks the
    algorithm, so it is not a security boundary we chose; avoids a new dep in the published closure).
  - **Hardening:** `CLIENT_LOCAL_FILES` is masked off unconditionally — a hostile server must not be
    able to ask the *host* (which, unlike the plugin, has filesystem access) to read a local file via
    `LOAD DATA LOCAL INFILE`. `MAX_MESSAGE_BYTES` applies across the multi-packet reassembly loop,
    where MySQL's 3-byte length field otherwise bounds nothing. `mysql_clear_password` is refused.
  - **Capabilities handed on:** `HandshakeResult.capabilities` + a `capabilities` field on the
    `conn.authenticate` response, so D-197 knows whether `CLIENT_DEPRECATE_EOF` was negotiated.
  - **Tests (7, hermetic scripted MySQL-server stub over loopback):** native-password success with
    capability assertions (DEPRECATE_EOF on, LOCAL_FILES off); wrong password preserving ERR code +
    SQLSTATE; `caching_sha2_password` refused by name *with the workaround*; an `ed25519`
    AuthSwitchRequest refused by name rather than hanging; SHA-1 RFC 3174 known vectors; the
    empty-password convention; and the D-31 no-password-in-the-frame assertion re-run over
    `conn.authenticate` with `protocol: "mariadb"`.
  - Full root gate green: build, 128 test binaries, clippy `-D warnings`, fmt (both workspaces),
    `flux-codegate`.
- **Post-review hardening (same day, `/code-review`):**
  - **Auth-downgrade hole closed.** `auth_response_for` mapped an *empty* plugin name onto
    `mysql_native_password`, so the pre-4.1 "old auth switch request" (a bare `0xfe` packet with no
    name and no nonce) was answered with `native_password(pw, &[])` — a token derived from the
    password ALONE, which destroys the replay resistance the scheme rests on. An unnamed plugin is
    now refused, and a scramble shorter than the required 20 bytes is refused on every path. Two new
    tests (a scripted downgrade server, and `auth_response_for` directly).
  - **Encoding assumption enforced.** `send_response` always framed the auth response with a 1-byte
    length prefix, which is the `CLIENT_SECURE_CONNECTION` encoding, while `read_greeting` branched
    on that flag. The flag is now *required* rather than assumed — no untestable legacy branch.
  - **Off-by-one:** the "is there more greeting?" guard tested `remaining() >= 15` but the block
    reads 16 bytes, so a 15-byte tail died mid-read instead of reaching the pre-4.1 diagnosis.
  - **Docs corrected:** `HandshakeResult.capabilities` was documented as something "the plugin
    needs"; D-197 decided not to consume it, so the comment (and the test's message) said the
    opposite of the truth. Now documented as diagnostic-only, with the reason it is kept.
  - `pg.rs` kept an orphaned doc comment from the moved `terminate_handshake` — it had attached
    itself to `mod tests` and still called mysql a follow-on. Removed.
- **Live interop VERIFIED 2026-07-28** against MySQL 5.7.44 (dev cluster, ns `latest`). The real
  greeting matched every parser assumption: protocol version `0x0a`, `auth_plugin_data_len` `0x15`
  (= 20 + NUL, split 8 + 12), capability flags `0xc1ffffff` (PROTOCOL_41, SECURE_CONNECTION,
  PLUGIN_AUTH, DEPRECATE_EOF all set), auth plugin `mysql_native_password`. `sql.test` returned
  `server_version: "5.7.44"` captured from that handshake; a wrong password surfaced the server's own
  `ERR 1045 (28000)` rather than hanging.
- **Residual:** the server verified was MySQL 5.7, **not literally MariaDB** — same wire protocol and
  same auth plugin, so the shared path is exercised, but MariaDB's own build (and its `ed25519` /
  `parsec` options) is still untested.

## Notes
- `mysql_native_password` is MariaDB's default when a user is created without an explicit plugin, is
  statically linked into the server, and also covers MySQL 5.7. It is *simpler* than the
  SCRAM-SHA-256 already shipped — no PBKDF2, no iteration ceiling, and no server-signature check,
  because the protocol offers no server authentication to verify. The SHA-1 basis is the server's
  choice, not ours; we implement what MariaDB deploys.
- `caching_sha2_password` (MySQL 8.0+ default) is deliberately deferred: its full-auth path needs RSA
  public-key exchange or an existing TLS channel, and its fast path depends on a server-side cache
  that cannot be assumed on a first connection. File it as a follow-on when this lands.
- Does **not** make the plugin work end-to-end on its own — [D-197](D-197-mysql-wire-client.md) is
  required before any op returns rows.
- Design: [mariadb-support.md](../designs/mariadb-support.md).
