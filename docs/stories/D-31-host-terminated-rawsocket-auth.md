---
id: D-31
title: Host-terminated raw-socket auth (no credential to the plugin)
pillar: Core
status: done
priority: 10
epic: endpoint-discovery
design: docs/designs/endpoint-discovery.md
note: the endpoint epic is COMPLETE — the host now terminates the Postgres v3 handshake (full RFC 5802/7677 SCRAM-SHA-256 incl. server-signature verification, + MD5/cleartext) via the new conn.authenticate capability and hands sql a post-auth connection; sql's manifest grants NO credential and NO secrets (both removed), its SCRAM/MD5/crypto code is deleted, and MockHost call-log tests prove no password ever crosses a plugin frame; mysql/AMI follow-ons have a clear seam
---

# Host-terminated raw-socket auth (no credential to the plugin)

## Goal
Close the last gap in the references-only invariant for **raw-socket, in-band-auth** protocols. Today
HTTP plugins never see a credential (the host injects it), but a raw-socket plugin that speaks auth
in-band — Postgres SCRAM (`sql`), Asterisk AMI — receives the resolved credential value through the
gated `credential` capability (trusted plugin only, redacted, never the model). This story makes the
**host** terminate the protocol handshake so even those plugins never receive the credential: the
plugin gets a *post-auth* connection stream.

## Why
The epic's confirmed security decision (D-27) was: gated credential to the trusted plugin **now**,
host-terminated handshake auth as a **stricter future hardening**. This is that hardening. It removes
the one place a (trusted) plugin still holds a secret value, so the invariant becomes absolute, not
"absolute except for in-band-auth raw sockets." See [endpoint-discovery.md](../designs/endpoint-discovery.md)
— *Security model* and the *Future hardening* note.

## Acceptance
- [x] **Host-side Postgres auth** — the host performs the Postgres startup + SCRAM-SHA-256 / md5
      handshake using the materialized credential and hands `sql` a connection that is already
      authenticated (a post-auth `conn_id` + the negotiated parameters), so `sql` never calls the
      `credential` capability for Postgres. Failing-first test: a MockHost/integration test asserts the
      `sql` plugin frame never carries the password and no `credential` callback is made on the PG path.
- [x] **Protocol seam** — the handshake terminator is a host-side, per-protocol component (start with
      Postgres; AMI/mysql are follow-on), behind `flux_system::net`/the plugin host, not in the plugin.
- [x] **`credential` capability stays gated** for any protocol not yet host-terminated (no regression),
      and is removed from `sql`'s grant once PG is host-terminated.
- [x] Gate green: `cargo test -p flux-plugin -p flux-system` + the `sql` plugin tests; clippy `-D
      warnings`, fmt, `flux-codegate`.

## Progress
- **Done (2026-07-02).** The endpoint epic's last story:
  - **Host terminator:** new `crates/flux-plugin/src/pg.rs` — PostgreSQL v3 auth over
    `flux_system::net::DialStream`: StartupMessage → Authentication{Ok, cleartext, MD5, SASL
    SCRAM-SHA-256} → drain to first ReadyForQuery, capturing `server_version` ParameterStatus +
    BackendKeyData. SCRAM is full RFC 5802/7677 including client-final proof and
    **server-signature verification** (gs2 `n,,`, no channel binding); crypto helpers moved
    verbatim from the plugin. Speaks ONLY the auth phase — no SQL, no postgres client crate
    (deps: hmac + rand added to flux-plugin, both pre-existing workspace deps; sql dropped
    hmac/sha2/rand/base64).
  - **Capability:** `conn.authenticate` in SystemHostCaps — takes an already-dialed `conn_id`,
    protocol, user/database, and a credential *location* (`credential_ref`/`endpoint_ref` via the
    broker — cross-plugin gate + audit unchanged — or `auth_purpose` via the new
    `resolve_handshake_secret`, host-side like `resolve_user`); the resolved value is
    redactor-registered and used on the wire; the response carries only
    server_version/parameters/backend key. `pg::terminate_handshake` dispatches by protocol;
    non-postgres returns a clear "not yet host-terminated" error (mysql/AMI follow-ons; the gated
    `credential` capability remains for them — no regression).
  - **sql plugin:** startup/authenticate/SCRAM/MD5/drain + crypto code DELETED; `PgClient::connect`
    calls `host.conn_authenticate` and drives only post-auth Simple Query; manifest grants **no
    `credential` and no `secrets`** (all SQL_*/MYSQL_* secret keys and the `username` auth method
    removed; `password` stays declared so the host knows the env).
  - **Tests (failing-first, hermetic scripted PG-server stubs):** SCRAM success + parameter
    capture, wrong-password rejection, bad-server-signature rejection (MITM guard), MD5 success,
    RFC 7677 derivation vector, MD5 known vectors, end-to-end
    `conn_authenticate_terminates_handshake_without_returning_the_password`; sql-side MockHost
    call-log tests assert no credential/secret call and no password in any host-call payload;
    manifest test asserts the removed grants.
  - Both workspaces' gates green.
- **Residuals:** mysql + Asterisk AMI host-termination (seam in place, clear error, credential cap
  retained for them); the static/named path drops the former SQL_USERNAME env-override (username
  now comes from DSN userinfo / bare ref URL — deliberate simplification).

## Notes
- Larger, protocol-specific effort: the host must speak enough of each wire protocol to authenticate.
  Sequence: Postgres first (the demo path), then mysql, then AMI.
- Prior art: fluxplane resolved HTTP host-side via injected headers; raw-socket termination is the new
  piece. Keep the credential resolution path (cross-plugin gate + audit) unchanged — only *who* speaks
  the handshake moves to the host.
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
