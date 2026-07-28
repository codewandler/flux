---
id: C-126
title: WAL checkpoint hygiene for long-lived daemons — bound events.db-wal growth
pillar: Core
status: done
epic: event-store-concurrent-use
design: docs/designs/event-store-concurrent-use.md
note: "a long-lived serve daemon with pinned readers can keep the WAL from checkpointing; a periodic wal_checkpoint(TRUNCATE) in the serve loop bounds the sidecar"
---

# WAL checkpoint hygiene for long-lived daemons — bound events.db-wal growth

## Goal
Under WAL mode, checkpointing back into the main database file needs a moment with no pinned
readers; a long-lived `flux app run --serve` daemon that always holds read snapshots can defer
checkpoints indefinitely, growing `events.db-wal` without bound. Add a periodic
`PRAGMA wal_checkpoint(TRUNCATE)` (or `PASSIVE` + escalation) driven from the serve loop so the
sidecar stays bounded on busy shared stores.

## Acceptance
- [ ] A checkpoint hook exists on `EventStore` (SQLite backend only; Postgres no-op) and the serve
      loop invokes it on an idle/periodic cadence — proven by a failing-first test that grows the
      WAL past a threshold and asserts the hook shrinks it.
- [ ] Checkpointing never blocks or errors a concurrent writer/reader (busy → skip, retry next
      tick — never a turn-visible failure).
- [ ] Interactive CLI behavior is unchanged (short-lived processes already checkpoint on close).

## Progress
- 2026-07-29: **Premise verified first, as instructed.** Held a raw `rusqlite::Connection`'s read
  transaction open (`BEGIN` + an actual `SELECT`, which is what pins a WAL snapshot — `BEGIN`
  alone pins nothing) on a second connection to the same file, then appended 200 ~4KB messages
  through the store. `events.db-wal` grew past 200KB and stayed there — SQLite's own automatic
  checkpoint cannot reclaim past a pinned reader's snapshot. So the design doc's §4/item-3
  "conditional hygiene" framing is confirmed real, not hypothetical; see the corresponding small
  addition to `docs/designs/event-store-concurrent-use.md`'s item 3.
- Implemented the hook. New pub API: `EventStore::checkpoint(&self) -> Result<()>`
  (`crates/flux-events/src/store/mod.rs`), dispatching to a new `EventBackend::checkpoint` trait
  method with a **default no-op body** — Postgres inherits it unchanged, no `postgres.rs` edit
  needed. SQLite (`crates/flux-events/src/store/sqlite.rs`): `SqliteEvents` gained a new field
  `checkpoint_conn: Option<Mutex<Connection>>` — a SEPARATE connection to the same file, opened
  with `busy_timeout(Duration::ZERO)`, used only for `PRAGMA wal_checkpoint(TRUNCATE)`; `None` for
  `EventStore::in_memory()` (no WAL sidecar to reclaim, `checkpoint()` is then a bare `Ok(())`). A
  `SQLITE_BUSY` from the checkpoint attempt is swallowed to `Ok(())`; any other error still
  propagates.
- Failing-first tests (`crates/flux-events/src/store/mod.rs`, `sqlite_tests` module):
  `checkpoint_hook_shrinks_the_wal_once_a_pinned_reader_releases_it` (the premise scenario above,
  end to end: grows past 200KB while pinned, `checkpoint()` while still pinned is a fast no-op
  `Ok(())` that discards nothing, then shrinks to < 1/10th the grown size once released),
  `checkpoint_hook_never_blocks_or_errors_under_writer_contention` (a `BEGIN IMMEDIATE` writer
  held on a separate connection — `checkpoint()` returns `Ok(())` in well under 500ms), and
  `checkpoint_hook_is_a_harmless_noop_for_an_in_memory_store`.
  - Verified failing-first by sabotage, twice: (a) stubbing `checkpoint()` to a bare `Ok(())`
    (no-op) fails the "shrinks" test (WAL size unchanged: `7659112 vs 7659112`); (b) reverting
    `checkpoint_conn`'s busy-timeout from `ZERO` back to the production 5s fails BOTH contention
    tests, each blocking the full 5.0s before failing — proof the zero-timeout dedicated
    connection is load-bearing, not incidental. Both reverted before landing (`git diff` after
    restore matched the intended change exactly, confirmed via a byte-identical `diff` against a
    pre-sabotage backup).
- Wiring (`crates/flux-cli/src/app_cmd.rs`, `run_app`): spawns a `tokio::spawn`ed background task
  ticking every `WAL_CHECKPOINT_INTERVAL` (5 minutes) that calls `agent.events.checkpoint()`,
  aborted right after `flux_server::serve(...)` returns. Wired into ONLY the built-in coding
  agent's `flux app run --serve <addr>` branch (no program path) — the one `--serve` shape that
  shares the persistent, file-backed `~/.flux/events.db` (via `build_agent`) with occasional CLI
  turns on the same host (R1's "daemon + occasional CLI turns" topology, C-25's scenario).
  Deliberately did NOT wire the OTHER `--serve` path (program-mode `flux app run <program.flux>
  --serve`, `flux_channels::serve`): that path's `app_events` is `EventStore::in_memory()` (see
  `run_app`, further down in the same file) — no WAL sidecar, so the hook would just no-op every
  tick; spawning a periodic task for nothing was skipped as needless complexity, not an oversight.
  Manually smoke-tested: `flux app run --serve 127.0.0.1:<port> --yes -m mock` boots, serves its
  agent card over HTTP, and shuts down cleanly with the new task running; no crash, no behavior
  change on a fresh/quiet store (`events.db-wal` present at 0 bytes).
- Interactive CLI unchanged: no other code path references the new hook or task — confirmed by
  reading (the wiring touches exactly one function's one branch) and by the full `flux-cli` test
  suite staying green.
- New pub API surface (flagging per the report format): `EventStore::checkpoint(&self) ->
  Result<()>` (additive, new method — not a breaking change). `EventBackend::checkpoint` is a new
  trait method too, but the trait itself is crate-private (`trait EventBackend` has no `pub`),
  so it is not public API.
- Gate (crate-scoped): `cargo test -p codewandler-flux-events` — 77 lib tests (was 74; +3 here)
  + 2 (C-125's file) + 1 doctest, all green. `cargo clippy -p codewandler-flux-events
  --all-targets -- -D warnings` — clean. `cargo fmt -p codewandler-flux-events` — clean.
  `cargo test -p flux-cli --bins` — 218 passed. `cargo clippy -p flux-cli --all-targets --
  -D warnings` — clean. `cargo fmt -p flux-cli -- --check` — clean.

## Notes
- Condition to verify first: whether `events.db-wal` growth is actually observed under a served
  workload — the design doc (section 4.3) files this as conditional hygiene, not a known bug.
- R6 applies: a TRUNCATE checkpoint competes for the write lock; schedule off-peak like prunes.
