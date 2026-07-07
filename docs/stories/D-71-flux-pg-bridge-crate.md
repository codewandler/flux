---
id: D-71
title: flux-pg — the Postgres bridge crate (pool + panic-safe sync↔async bridge + DSN contract)
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "epic opener, no deps: the single sqlx owner; PgHandle bridges sync callers from ANY context (plain thread / tokio worker / current-thread rt) without panicking — the naive bridges all panic somewhere in that matrix"
---

# flux-pg — the Postgres bridge crate

## Goal
One new crate that owns everything Postgres-flavored — the sqlx dependency, the connection
pool, the DSN contract, and the sync↔async bridge — so `flux-events` and `flux-capabilities`
can add Postgres backends behind features without either of them touching a driver. The
default workspace build stays rusqlite-only and never needs a database.

## Acceptance
- [ ] New crate `crates/flux-pg`, registered at **L1** in the `flux-codegate` layer map
      (precedent: `flux-a2a`); deps: `flux-core`, `sqlx` (`runtime-tokio`, `postgres`,
      `tls-rustls`, **no macros**), `tokio` (`rt-multi-thread`). Nothing else in the
      workspace depends on it yet; default `cargo test --workspace` green with no DB.
- [ ] `PgHandle { rt: Runtime, pool: PgPool }` with `connect(url) -> Result<Arc<Self>>`,
      `block_on<T: Send>(&self, fut) -> T`, `pool()`. `block_on` = spawn onto the handle's
      own runtime + block on `std::sync::mpsc::recv()`; the module header documents why
      `Handle::block_on` (panics on worker threads), `block_in_place` (panics on
      current-thread runtimes), and the sync `postgres` crate (nested-runtime panic) are
      all wrong.
- [ ] Failing-first bridge matrix test: `block_on` returns correctly (no panic) from
      (a) a plain `std::thread`, (b) inside `#[tokio::test(flavor = "multi_thread")]`,
      (c) inside a current-thread runtime.
- [ ] DSN contract: flux-owned params `pool_max` (default 5), `acquire_timeout_ms`
      (default 5000), `schema` (→ `SET search_path` via `after_connect`; the test-isolation
      mechanism) are parsed and stripped; the remainder (e.g. `sslmode=require`) passes to
      `PgConnectOptions`. Userinfo percent-decoding and hyphenated user/db names covered by
      pure unit tests (no DB).
- [ ] Env-gated integration test (`TEST_POSTGRES_URL`, skip-with-notice when unset):
      connect, round-trip a query, verify `schema` isolation actually switches search_path.

## Progress
- (not started)

## Notes
- Design: [pg-backend.md](../designs/pg-backend.md) §1. Epic opener — D-73/D-74 build on it.
- `pool_max` default 5 is deliberately small (serverless-Postgres-friendly); consumers tune
  via the DSN.
