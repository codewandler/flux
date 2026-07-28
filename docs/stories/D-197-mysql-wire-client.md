---
id: D-197
title: MySQL wire client in the sql plugin (COM_QUERY + text-protocol result sets)
pillar: Core
status: done
priority:
epic: mariadb-support
design: docs/designs/mariadb-support.md
areas: [plugins]
note: the plugin-side half — drives COM_QUERY over the post-auth conn_id, mirroring the Postgres Simple Query client
---

# MySQL wire client in the `sql` plugin

## Goal
Give the `sql` plugin a `MySqlClient` that drives `COM_QUERY` and decodes text-protocol result sets
over the post-auth `conn_id` from [D-196](D-196-host-terminated-mysql-auth.md), producing the same
`QueryResult` the Postgres client produces so every output-shaping helper is reused unchanged.

## Why
The host hands over an authenticated socket; something still has to speak SQL on it. Postgres's
Simple Query client lives in the plugin (`plugins/sql/src/main.rs:1478-1615`) and MySQL's belongs in
the same place, for the same reason: the host deliberately speaks **no** SQL
(`crates/flux-plugin/src/pg.rs:11`).

## Acceptance
- [x] **Packet framing** — 3-byte little-endian length + 1-byte sequence id, including **multi-packet
      payloads**: a payload of exactly `0xFFFFFF` continues into the next packet and must be
      reassembled. Failing-first test replays a split payload.
- [x] **`COM_QUERY` response state machine** — ERR, OK, or column-count → column definitions → rows →
      terminator, with `CLIENT_DEPRECATE_EOF` (negotiated in D-196) deciding whether the intermediate
      and terminating EOF packets are present. Failing-first test covers **both** settings of the flag.
- [x] **Length-encoded decoding** — integers (`0xfc`/`0xfd`/`0xfe` → 2/3/8-byte widths) and strings,
      with `0xfb` decoded as NULL in a row context and as a length prefix elsewhere. Failing-first
      test asserts a result set containing NULL cells.
- [x] **Errors surface, not hang** — an ERR packet mid-result-set becomes a plugin error carrying the
      server's message and SQLSTATE. Failing-first test.
- [x] **`QueryResult` reused** — text protocol yields `Option<String>` cells exactly like the Postgres
      path, so `cell()` and the output shapers need no dialect awareness.
- [x] **Read deadlines respected** — the client honours `ConnStream::set_read_deadline` (D-45) so a
      stalled server surfaces `ErrorKind::TimedOut` rather than blocking forever.
- [x] Gate green: the `plugins/` workspace tests, clippy `-D warnings`, fmt (both workspaces).

## Progress
- **Done (2026-07-28).** `MySqlClient` + a `SqlClient` dialect-dispatch enum in
  `plugins/sql/src/main.rs`; every op now opens a `SqlClient` instead of a `PgClient` directly.
- **Design change vs the acceptance as written.** The story assumed the client would consult
  `CLIENT_DEPRECATE_EOF`, negotiated in D-196 and plumbed through `HandshakeInfo`. It does **not**,
  and the plumbing was dropped, for two reasons:
  1. `HandshakeInfo` is a `codewandler-flux-host-kit` **1.0.0** type without `#[non_exhaustive]`, so
     adding a public field is a semver break on the protocol line C-143 deliberately separated.
  2. The flag is not needed. Both terminators carry a `0xfe` header, and the spec fixes their sizes:
     a classic EOF payload is **exactly 5 bytes** (`0xfe` + warnings + status) while every OK packet
     is **at least 7**. A row opening with `0xfe` announces an 8-byte length-encoded integer and so
     needs ≥ 9 bytes. Those gaps decide the shape from the bytes alone — and keep the client correct
     under host/plugin version skew, which a negotiated flag would not.
  The host still reports `capabilities` on the `conn.authenticate` response (additive JSON, no API
  change); nothing reads it yet.
- **Bug caught by the tests, worth recording:** the first implementation broke out of the row loop on
  the *intermediate* EOF, returning zero rows for every pre-DEPRECATE_EOF server; the fix peeks once
  after the column definitions and swallows only a *classic* EOF. The empty-result-set case is the
  one that makes this subtle (EOF-then-EOF vs a single `0xfe` OK) and has its own test in both shapes.
- **Tests (8):** columns/rows/NULL decoding, both DEPRECATE_EOF shapes, empty result sets in both
  shapes, ERR mid-stream preserving code + SQLSTATE, a payload split at the `0xFFFFFF` ceiling, an
  unbounded packet chain refused at the cap, and the read-only guard still rejecting writes.
- **Post-review fix (`/code-review`):** `read_packet` reassembled the `0xFFFFFF` continuation chain
  with **no ceiling** — the exact invariant `handshake.rs` had just been written to state, enforced
  on the host side but not re-established here. A hostile endpoint could answer any query with an
  endless chain of full-size packets and grow the buffer until the plugin subprocess was killed. Now
  capped at 64 MiB (far above the host's 4 MiB auth-phase bound, because a legitimate result row may
  legitimately exceed one packet), with a test.
- Both workspace gates green.
- **Live interop VERIFIED 2026-07-28** against MySQL 5.7.44: real result sets decoded end to end —
  173-row `table.list`, a 112-column `table.show`, and a `query` in which `NULL` came back as JSON
  `null` while an empty string came back as `""`, confirming the `0xfb` marker is distinguished from
  a zero-length lenenc string on real wire data. The server negotiates `CLIENT_DEPRECATE_EOF`, so the
  post-DEPRECATE_EOF terminator shape is the one exercised.

## Notes
- Text protocol only — no prepared statements, no binary protocol. Every value arrives as a string,
  which is what the existing shapers already expect.
- **Honesty caveat, inherited from the Postgres client** (`plugins/sql/src/main.rs:27-32`): `MockHost`
  tests replay hand-crafted server frames. They prove the frame parser and message assembly against
  bytes the test author wrote — they are *not* live interop against a real MariaDB. Say so when the
  epic closes.
- Design: [mariadb-support.md](../designs/mariadb-support.md).
