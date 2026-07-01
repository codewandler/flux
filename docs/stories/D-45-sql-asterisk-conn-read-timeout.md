---
id: D-45
title: Enforce sql/asterisk per-call read timeout through the host conn.read
pillar: Core
status: done
priority:
epic:
design:
note: Closes the D-40/D-41 deferred item — `timeout` is now wire-enforced, not just parsed.
---

# Enforce sql/asterisk per-call read timeout through the host conn.read

## Goal
The sql and asterisk plugins already parse/validate an input `timeout` (D-40/D-41), but the host
`conn.*` capability exposed no per-call read deadline, so the value was validated at input time and
then discarded — a hung server would hang the plugin forever. Plumb the deadline through the host's
`conn.read` so the parsed `timeout` is actually enforced, surfacing a `TimedOut` error instead of a
silent hang.

## Acceptance
- [x] The host `conn.read` accepts an optional `timeout_ms` and races `stream.read` against it; on
      elapsed it returns `timed_out: true` with an empty body and **leaves the connection open**
      (the plugin decides to retry or close). Failing-first: `conn_read_timeout_returns_timed_out_without_closing`.
- [x] `host-kit`'s `ConnStream` carries a per-read deadline (`set_read_deadline`) and surfaces a
      host timeout as `std::io::ErrorKind::TimedOut` (distinguishable from a clean EOF `Ok(0)`).
- [x] sql's `PgClient::connect` forwards the parsed `timeout` to the stream's read deadline.
      Failing-first: `timeout_is_enforced_on_read_when_no_server_data`.
- [x] asterisk's `with_ami` wrapper + `ami.ping` forward the parsed `timeout` to the stream's read
      deadline. Failing-first: `test_ami_ping_timeout_is_enforced_when_no_greeting`.
- [x] `cargo build --workspace` + plugin workspace build, `clippy -D warnings`, `fmt`, `flux-codegate`
      all green.

## Progress
- Host (`flux-plugin/src/lib.rs`): `conn.read` arm reads optional `timeout_ms`, races
  `stream.read(max)` against `tokio::time::timeout`; on elapsed returns `{data_b64:"", eof:false,
  timed_out:true}`. The connection map entry is untouched (open).
- `host-kit`: `Host::conn_read_timed(conn_id, max, timeout_ms)` adds `timeout_ms` to the request
  and maps a `timed_out` response to `Err("conn.read: timed out after Nms")`. `ConnStream` gains
  `read_deadline: Option<Duration>` + `set_read_deadline`; its `Read` impl forwards the deadline
  and maps the "timed out" host error to `ErrorKind::TimedOut` (other errors stay `Error::other`).
  `MockHost`'s `conn.read` returns `timed_out:true` when a deadline is set and no data is ready.
- sql: `PgClient::connect` takes `timeout: Option<Duration>` and calls `set_read_deadline` on the
  stream before the handshake; all 6 op handlers pass `target.timeout`.
- asterisk: `with_ami` takes `timeout: Option<Duration>` and sets the deadline on the stream; the
  7 `with_ami` callers + `ami.ping` pass `ami_timeout(&input)?`.
- Tests: 1 host (live loopback server that accepts but never writes), 2 sql, 2 asterisk — all green.

## Notes
- The user's concurrent C-09a WIP (`internal` op flag across `flux-plugin`/`host-kit`/`kubernetes`)
  was left fully intact — stashed together before this work, consistency restored, then re-applied
  untouched. This story's changes are orthogonal (the `conn.read` arm + `ConnStream` deadline).
- Semantics chosen: a timeout does **not** close the connection. The wire protocols (PostgreSQL v3,
  AMI) are request/response, so a deadline on every read bounds the whole exchange; on elapsed the
  plugin surfaces the error and the outer handler's `conn_close` cleans up. Closing on timeout
  would complicate retry semantics.
- The host deadline is per-`conn.read` call, not a session deadline — `ConnStream` forwards the same
  `Duration` on every read, so it effectively bounds each round-trip of the request/response loop.
