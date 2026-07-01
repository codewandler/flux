---
id: D-40
title: sql schemars migration + timeout parity port
pillar: Agent
status: done
priority:
epic:
design:
note: full D-36 per-plugin loop for sql — schemars migration (7 ops, shared ConnProps via #[schemars(flatten)]) + fluxplane parity re-audit + the `timeout` gap ported
---

# sql schemars migration + timeout parity port

## Goal

Full D-36 per-plugin loop for the `sql` conn-wave plugin: schemars migration of all 7 ops, a
fluxplane parity re-audit, and porting the gaps surfaced.

## Acceptance
- [x] All 7 sql ops (`test`/`query`/`database.list`/`table.list`/`table.show`/`index.list`)
      schemars-derived via `read_op_typed::<T>`; the `so()`/`merge()`/`conn_props()` helpers
      deleted. Handlers unchanged (schema-only structs; `flex_str`/`flex_i64`/`flex_bool`
      extraction stays, D-34 precedent).
- [x] Shared connection fields (`endpoint`/`endpoint_ref`/`driver`/`database`) factored into a
      `ConnProps` struct embedded via `#[serde(flatten)] #[schemars(flatten)]` (no 4×7 repetition);
      `Driver` is a derived enum (`postgres|mysql|sqlite`) so the schema emits the legacy enum.
- [x] `schema_contract` test encodes the pre-migration `so(merge(conn_props(), ...))` contract;
      `sql` in `MIGRATED_PLUGINS`; guard green.
- [x] **Parity re-audit** vs `~/projects/fluxplane/fluxplane-plugins/sql/` (operations.go +
      introspect.go). Matched: `driver`/`database`/`schema`/`table`/`include_views`/`max_results`/
      `max_rows`/`query`. Architectural split (do-not-port): `endpoint_ref` (flux makes it
      optional defaulting to `sql.endpoint`; fluxplane makes it required) + the flux-only
      `endpoint` object.
- [x] **`timeout` gap ported:** fluxplane `ConnInput.Timeout` (default 10s, Go duration) was
      missing from flux's `conn_props()`. Added `timeout: Option<String>` to `ConnProps` (all 7
      ops); `parse_duration`/`parse_duration_default` helpers; threaded through `resolve_target`
      (parsed once per op, defaults 10s, invalid values error before dialing). Failing-first tests.
- [x] Dev loop green: `cargo build/test/clippy -D warnings/fmt` for `sql` (17 tests).

## Notes
- **Host timeout limitation (RESOLVED by D-45):** the parsed duration is now wire-enforced.
  D-45 plumbed a per-read deadline through the host `conn.read` (`timeout_ms`) +
  `ConnStream::set_read_deadline`; sql's `PgClient::connect` forwards `target.timeout` to the
  stream. (Originally: `Host::conn_dial`/`conn_dial_ref` and `flux_system::net::dial_scoped` do
  not accept a per-call timeout, so the parsed duration was validated at input time but could not
  be enforced as a dial/query deadline. The worker reported this honestly rather than adding a
  fake enforcement path. A follow-up could plumb a timeout through the host `conn.*` capability —
  out of scope here (host-protocol change).)
- The flux-only `endpoint` object (a discovered endpoint reference from `endpoint.select`) has no
  fluxplane equivalent; kept as flux's reference-IO extension.
