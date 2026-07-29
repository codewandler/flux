# Event-store concurrent use — guarantees, rules, and hardening

**Scope:** `flux-events` (L2) · **Epic:** C-123 (stories C-124..C-126) ·
**Sources:** `crates/flux-events/src/store/{sqlite.rs,postgres.rs,mod.rs}`,
`docs/designs/tenant-event-substrate.md`

The question this answers: *does a global store (`~/.flux/events.db`) mean only one flux instance
at a time?* No — but the two backends have different concurrency envelopes, and reliable use means
staying inside them. This design states what the store already guarantees, the rules a concurrent
deployment must follow, the known limits, and the hardening work the epic funds.

---

## 1. What the store already guarantees (do not re-implement these)

### SQLite backend (the default, `EventStore::open`)

- **WAL mode** on every open (`sqlite.rs:253`) — any number of concurrent reader processes while
  one process writes. Readers never block the writer and vice versa.
- **`busy_timeout` = 5s** (`sqlite.rs:261`, story C-25) — a second writer *waits* for the WAL write
  lock instead of failing with `SQLITE_BUSY`. Proven by
  `concurrent_writers_wait_on_busy_timeout_instead_of_erroring` (`store/mod.rs:2487`).
- **`BEGIN IMMEDIATE` for every write transaction** (`begin_write`, `sqlite.rs:36`) — the write
  lock is taken up front, because SQLite refuses to run the busy handler on a deferred read→write
  upgrade. This is what makes the busy_timeout actually effective across processes.
- **In-process `Mutex<Connection>`** (`sqlite.rs:246`) — one process's own threads serialize
  before ever touching the file lock.
- **`synchronous = NORMAL`** under WAL (`sqlite.rs:266`) — durable against app crashes; only power
  loss can drop the last few committed transactions.
- **Durable uniqueness backstops:** `UNIQUE(id)` and `UNIQUE(stream, stream_seq)` on `events`
  (`sqlite.rs:288-289`). Even if two processes race, the database refuses a duplicate.
- **Idempotent append by stable id** (C-87, `sqlite.rs:315-349`): give `NewEvent.id` a stable
  caller id, and a retry — or a cross-process double-write race — returns the already-stored
  event instead of erroring. The `UNIQUE(id)` loser rolls back, re-reads, and succeeds as a no-op.
- **`stream_seq` is race-safe:** `MAX(stream_seq)+1` is computed *inside* the immediate write
  transaction (`sqlite.rs:325-331`), so two processes cannot mint the same sequence number.

### Postgres backend (`postgres` feature, for server deployments)

- Per-stream **`pg_advisory_xact_lock(hashtextextended($stream, 0))`** as the first statement of
  every append transaction (`postgres.rs:25-30`) — serializes appends per stream *across
  processes and replicas*, which a file DB structurally cannot do.
- **DDL advisory lock** on cold boot (`postgres.rs:105-108`, D-76) — N replicas booting
  simultaneously don't race `CREATE TABLE IF NOT EXISTS`.
- Same `UNIQUE` backstops, same idempotent-id recovery, same public `EventStore` API.
- Proven by the PG-gated tests `concurrent_cold_boots_serialize_bootstrap_ddl` and
  `concurrent_appends_to_one_stream_are_contiguous` (`store/mod.rs:2605,2662`).

---

## 2. Rules for reliable concurrent use

### R1 — pick the right topology

| Topology | Backend | Verdict |
|---|---|---|
| One interactive CLI/TUI | SQLite (default) | trivially fine |
| Daemon (`flux app run --serve`) + occasional CLI turns, same host | SQLite, shared `~/.flux/events.db` | supported by design (C-25); this is the scenario the busy_timeout exists for |
| Several agents/benchmark runs that don't need shared history | SQLite, **separate stores** via `--session-dir <DIR>` (`flux-cli/src/args.rs:26`) or `Storage::dir` in the SDK | preferred — zero contention by construction |
| Many sustained concurrent writers, one shared history | **Postgres** | the SQLite writer lock is global to the file; sustained parallel writers will queue and can exceed the 5s timeout |
| Multiple hosts / replicas | **Postgres only** | SQLite over NFS/SMB is unreliable (WAL requires coherent local file locking); never share an `events.db` across machines |

### R2 — one live session, one writing process

Streams are the unit of serialization. Concurrent processes appending to *different* streams only
contend for the brief WAL commit (SQLite) or not at all (PG per-stream locks). Two processes
interleaving writes into the *same* `s_<n>` is not corrupting (the transaction + UNIQUE backstops
hold), but it interleaves two conversations into one causal order — an application-level bug.
Keep the existing convention: the process that minted the session owns its writes for the turn.

### R3 — use stable event ids anywhere a retry can happen

Any writer that may retry (network daemons, channels, at-least-once triggers) must set
`NewEvent.id` to a caller-stable id so the append is idempotent (`sqlite.rs:317-321,342-347`).
Without it, a retry duplicates the event.

### R4 — treat append errors as retriable-once, then fail loudly

Under extreme contention SQLite can still return busy after the 5s window. Callers must not
silently drop the write — `flow_cmd.rs:685` states the invariant: a recording failure must be
visible at record time. The conversation write `?`-propagates and aborts the turn; keep it that way.
(A-102 removed `EventStore::record_message`, which this rule used to name; the rule is unchanged and
now binds the typed `SessionLog` writes that replaced it.)

### R5 — keep the WAL sidecar files together

`events.db-wal` / `events.db-shm` are part of the database while any process has it open (the SDK
test fixtures already treat them as such, `flux-sdk/src/test.rs:454-456`). Never copy or back up
`events.db` alone while writers are live; either quiesce writers or copy all three.

### R6 — prunes are safe to run concurrently, but schedule them off-peak

Every prune (`prune_empty_excluding`, `prune_inactive_excluding`, `prune_older_than`,
`prune_adhoc_older_than`) runs as one `BEGIN IMMEDIATE` transaction and is idempotent (a second
sweep at the same cutoff is a no-op — `store/mod.rs:1503,2086,2150`). They are correct under
concurrency, but a large delete holds the single SQLite write lock for its full duration, which
eats into every other writer's 5s budget. On a busy shared store, run retention when interactive
traffic is idle.

### R7 — don't add side-channel state

The `streams` registry (`msg_count`, `last_seq`, `model`) is updated inside the same transaction
as the event insert (`sqlite.rs:354-384`), so it can never drift from the log. Any new derived
state must follow the same pattern: same transaction, or a pure projection over the log —
never a second write path.

---

## 3. Known limits (accepted, documented)

- **SQLite has one writer lock for the whole file**, not per stream. Cross-stream write
  concurrency is queuing, not parallelism. This is fine at interactive rates (appends are
  single-digit-ms); it is the reason R1 routes sustained multi-writer loads to Postgres.
- **The 5s busy_timeout is a ceiling, not a guarantee.** A writer starved past it errors; R4
  covers the caller contract. If this is ever hit in practice, the fix is topology (R1), not a
  bigger timeout.
- **The idempotency pre-check is check-then-insert** — deliberately non-atomic, with the
  `UNIQUE(id)` + rollback + re-read recovery as the safety net (C-87). Don't "fix" it into a
  wider lock.
- **No cross-machine SQLite.** Not supported, not planned; Postgres is the multi-host answer.

---

## 4. The epic's implementation stories

1. **C-124 — append-contention visibility:** a counter/log line when an append waited on the busy
   handler longer than ~1s, so topology problems surface before they become 5s failures.
2. **C-125 — multi-process stress test:** a spawned-subprocess sibling of
   `concurrent_writers_wait_on_busy_timeout_instead_of_erroring` hammering N writers × M streams
   on one file store, asserting contiguous `stream_seq` and zero lost writes.
3. **C-126 — WAL checkpoint hygiene:** long-lived daemons keep the WAL from checkpointing while
   readers are pinned; a periodic `PRAGMA wal_checkpoint(TRUNCATE)` in the serve loop if
   `events.db-wal` growth is ever observed. **Confirmed, not hypothetical (2026-07-29):** a raw
   connection holding an open read transaction reliably blocks reclaim of everything after its
   snapshot — writes through the store keep growing `events.db-wal` (measured >200KB in a small
   test) with no shrink until that reader releases, even though nothing errors in the meantime.
   Shipped unconditionally (not gated behind observing growth in the wild) as
   `EventStore::checkpoint` (SQLite: a dedicated zero-busy-timeout connection so a contended
   attempt never blocks or errors; Postgres: no-op), invoked on a 5-minute tick from the built-in
   coding agent's `flux app run --serve` daemon only — the one topology that shares the
   persistent, file-backed store with occasional CLI turns (R1).

---

## 5. TL;DR

Multiple flux instances on one global SQLite store are supported and tested — concurrent readers
always, concurrent writers serialized with a 5s patience window. Reliability rules: prefer
separate `--session-dir` stores when history need not be shared; one process writes a given
session; stable event ids for anything that retries; never share the file across machines; move
to the Postgres backend when writers are sustained or distributed.
