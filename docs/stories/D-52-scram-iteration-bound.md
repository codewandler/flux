---
id: D-52
title: "Bound the server-supplied SCRAM iteration count in the host-terminated PG handshake"
pillar: Core
status: done
priority:
epic: review-hardening
design: docs/designs/review-hardening.md
note: "host-side SCRAM feeds the server-supplied `i=` (up to u32::MAX) straight into PBKDF2 with no upper bound; a malicious/MITM'd Postgres endpoint sending i=2000000000 drives ~2B HMAC-SHA256 rounds — pure CPU the socket read-timeout never covers, pegging a core for minutes"
---

# Bound the server-supplied SCRAM iteration count in the host-terminated PG handshake

## Goal
Harden the host-terminated Postgres SCRAM handshake (D-31 — this is the **host's** `conn.authenticate`
in `flux-plugin`, not a plugin) against a hostile server. `scram_authenticate` parses the server-first
`i=` attribute into a `u32` with no upper bound (`crates/flux-plugin/src/pg.rs:179-182`) and feeds it to
`pbkdf2_hmac_sha256` (`:186`), whose `for _ in 1..iterations` runs that many HMAC-SHA256 rounds. Since
this is pure computation, the handshake `timeout` (which only guards socket reads) never fires, so a
discovered/compromised/MITM'd endpoint reporting `i=2000000000` pegs a CPU core for minutes with the op
effectively hung.

## Acceptance
- [x] Failing-first test: a server-first message with an iteration count above a sane ceiling (e.g. a
      documented `MAX_SCRAM_ITERATIONS`, generously above RFC-typical 4096–100000) is rejected with a clear
      error before any PBKDF2 work. Today any value up to `u32::MAX` is accepted and computed.
- [x] Fix: bound `i=` at parse time against a named maximum; reject over-limit counts.
- [x] Legitimate iteration counts still authenticate; existing SCRAM tests pass.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🔴 DoS hardening. Reaching it requires the operator to
  point flux at a malicious/MITM'd PG endpoint, which bounds exposure; the fix is a cheap one-line guard.
- 2026-07-03 fixed: added `pub(crate) const MAX_SCRAM_ITERATIONS: u32 = 1_000_000` in
  `crates/flux-plugin/src/pg.rs` with a doc comment on the rationale, and a bound check right after
  parsing `i=` in `scram_authenticate` that returns
  `Err("pg scram: server-first iteration count {i} exceeds the maximum of {MAX_SCRAM_ITERATIONS} ...")`
  before the salt is even decoded — strictly before the `pbkdf2_hmac_sha256` call. Added failing-first
  test `tests::pg_scram_rejects_iteration_count_above_ceiling` in `crates/flux-plugin/src/lib.rs`
  (new `ScramMode::HugeIterations` scripted server sends `i=MAX_SCRAM_ITERATIONS+1`); confirmed it fails
  pre-fix as a wrong-result (handshake actually succeeds / surfaces an unrelated EOF, not a hang — took
  ~17s in a debug build, not minutes) by temporarily disabling the guard, then restored the fix and
  reran green. Full existing SCRAM/MD5 suite (`pg_scram_handshake_succeeds_and_captures_parameters`,
  `pg_scram_rejects_wrong_password`, `pg_scram_rejects_bad_server_signature`, `pg_md5_handshake_succeeds`,
  `pg_scram_derivation_matches_rfc7677_vector`, `conn_authenticate_terminates_handshake_without_returning_the_password`)
  still passes at the legitimate 4096-iteration count. Gate green: `cargo test -p flux-plugin` (48 unit +
  4 integration tests), `cargo clippy -p flux-plugin --all-targets -- -D warnings` (clean),
  `cargo fmt -p flux-plugin --check` (clean).

## Notes
- Evidence: `crates/flux-plugin/src/pg.rs:179-182` (unbounded parse), `:186` (PBKDF2 loop).
- Residual of [D-31](D-31-host-terminated-rawsocket-auth.md). Design: [review-hardening](../designs/review-hardening.md).
