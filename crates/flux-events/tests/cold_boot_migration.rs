//! C-230: the OS-level proof that **first boot** of a brand-new `events.db` is serialised across
//! processes.
//!
//! `multiprocess_stress.rs` (C-125) proves concurrent *writes* on a shared store, but every test
//! there bootstraps the schema in the orchestrator and drops that connection before spawning — so
//! its children always find a migrated schema. That structural choice is exactly why nothing
//! covered the cold-boot window: `SqliteEvents::init` probes `PRAGMA table_info(streams)` and then
//! `ALTER TABLE streams ADD COLUMN …` (SQLite has no `ADD COLUMN IF NOT EXISTS`), and a
//! check-then-act pair is not atomic. Two processes that both observe the column as absent both
//! issue the `ALTER`, and the loser dies with
//! `Other("event store: duplicate column name: account")`.
//!
//! This file therefore deliberately does **NOT** bootstrap: the orchestrator never opens the store,
//! so `WORKERS` real OS processes cold-boot the same non-existent file at once. It is the SQLite
//! sibling of D-76's `concurrent_cold_boots_serialize_bootstrap_ddl` (Postgres, `flux:ddl` advisory
//! lock) — see `src/store/sqlite.rs`'s `init` for why the chosen SQLite mechanism is a
//! `BEGIN IMMEDIATE` transaction rather than a lock file.
//!
//! Same test-binary re-exec harness as `multiprocess_stress.rs` (no helper crate/binary), with one
//! addition it does not need: the children are released by a shared **wall-clock instant**, not
//! merely by being spawned. Spawn latency alone (milliseconds) dwarfs the probe→`ALTER` window
//! (microseconds), so without the rendezvous the processes would queue up rather than collide.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flux_events::{EventContext, EventStore};

/// The `events.db` path a worker cold-boots (unset ⇒ this invocation is the orchestrator).
const DB_ENV: &str = "FLUX_EVENTS_C230_DB";
/// Unix-ms instant at which every worker in a round simultaneously calls `EventStore::open`.
const START_AT_ENV: &str = "FLUX_EVENTS_C230_START_AT_MS";
/// The worker's index within its round — used as its account tag, so the orchestrator can prove
/// every worker's write landed under the migrated context columns.
const INDEX_ENV: &str = "FLUX_EVENTS_C230_INDEX";

/// Processes cold-booting one fresh file simultaneously. Eight matches D-76's Postgres storm.
const WORKERS: usize = 8;
/// Independent fresh-file rounds per test run. Serialisation is a **safety** property, so a single
/// green storm proves little — each round is a fresh race on a fresh path.
const ROUNDS: usize = 4;

/// A throwaway store path that does **not** exist yet, plus its WAL sidecars, removed on drop.
struct TempPath(PathBuf);

impl TempPath {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "flux-events-c230-{tag}-{}.db",
            ulid::Ulid::generate()
        ));
        TempPath(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after the unix epoch")
        .as_millis()
}

/// Block until `start_at_ms`, sleeping while it is far off and busy-spinning the last few
/// milliseconds. Sub-millisecond alignment across processes is what makes the probe→`ALTER` window
/// reachable at all.
fn wait_for_release(start_at_ms: u128) {
    loop {
        let now = now_unix_ms();
        if now >= start_at_ms {
            return;
        }
        if start_at_ms - now > 4 {
            std::thread::sleep(Duration::from_millis(1));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// If this process was spawned as a worker, cold-boot the shared store at the appointed instant and
/// return `true`. Returns `false` when the role env vars are absent — an ordinary `cargo test`
/// invocation, which then orchestrates its own children.
///
/// A worker does not merely *construct* the store: it mints a session tagged with an
/// [`EventContext`] and reads it back through the account-scoped query, so a worker that survived
/// on a half-migrated schema (missing `account`, or a stale statement cache) still fails.
fn run_worker_if_invoked() -> bool {
    let (Ok(db), Ok(start_at_raw), Ok(index_raw)) = (
        std::env::var(DB_ENV),
        std::env::var(START_AT_ENV),
        std::env::var(INDEX_ENV),
    ) else {
        return false;
    };
    let start_at: u128 = start_at_raw.parse().expect("valid release instant");
    let account = format!("acct-{index_raw}");

    wait_for_release(start_at);

    let store = EventStore::open(&db).expect("worker cold-boots the fresh store");
    let ctx = EventContext {
        account: Some(account.clone()),
        ..EventContext::default()
    };
    let stream = store
        .create_session_with_context("m", &ctx)
        .expect("worker mints a session on the freshly migrated schema");
    let mine = store
        .list_for_account(&account, 16)
        .expect("worker reads back its own account-scoped session");
    assert_eq!(
        mine.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec![stream.as_str()],
        "the account-scoped read model must see exactly this worker's session"
    );
    true
}

/// Re-exec this very test binary filtered to `test_name`, in the worker role, with stderr piped so
/// a failing child's panic message can be quoted in the orchestrator's assertion.
fn spawn_cold_booter(test_name: &str, db: &Path, start_at_ms: u128, index: usize) -> std::process::Child {
    let exe = std::env::current_exe().expect("current_exe for re-exec");
    Command::new(exe)
        .args([test_name, "--exact", "--nocapture"])
        .env(DB_ENV, db)
        .env(START_AT_ENV, start_at_ms.to_string())
        .env(INDEX_ENV, index.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cold-boot worker process")
}

/// C-230: `WORKERS` processes open a **non-existent** `events.db` at the same instant, with **no**
/// orchestrator-side bootstrap. All of them must succeed, and the file they converge on must carry
/// one consistent schema — every worker's session present, each readable under its own account.
///
/// Before the fix this is red: the loser of the `PRAGMA table_info` → `ALTER TABLE` race exits with
/// `event store: duplicate column name: account`.
#[test]
fn concurrent_cold_boots_serialize_the_schema_migration() {
    if run_worker_if_invoked() {
        return; // this invocation IS a spawned worker; nothing left to orchestrate.
    }

    for round in 0..ROUNDS {
        // Deliberately never opened here — the absence of a bootstrap step IS the test.
        let db = TempPath::new(&format!("round{round}"));
        assert!(
            !db.path().exists(),
            "the store path must not exist before the workers race for it"
        );

        // Spawning is milliseconds; the race window is microseconds. Give every child time to
        // reach its spin loop, then release them all on one wall-clock instant.
        let start_at = now_unix_ms() + 400;
        let children: Vec<_> = (0..WORKERS)
            .map(|i| {
                spawn_cold_booter(
                    "concurrent_cold_boots_serialize_the_schema_migration",
                    db.path(),
                    start_at,
                    i,
                )
            })
            .collect();

        let mut failures = Vec::new();
        for (i, child) in children.into_iter().enumerate() {
            let out = child.wait_with_output().expect("join cold-boot worker");
            if !out.status.success() {
                failures.push(format!(
                    "--- round {round} cold-boot {i} exited {:?} ---\n{}{}",
                    out.status,
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {WORKERS} concurrent cold boots failed:\n{}",
            failures.len(),
            failures.join("\n")
        );

        // One consistent schema: every worker's session is registered, and the additive context
        // columns exist exactly once (a doubled `ALTER` or a partial migration would not survive
        // the per-account read below).
        let verifier = EventStore::open(db.path()).expect("verifier opens the migrated store");
        let all = verifier.list(WORKERS * 2).expect("list sessions");
        assert_eq!(
            all.len(),
            WORKERS,
            "round {round}: expected one session per cold-booting process, got {all:?}"
        );
        for i in 0..WORKERS {
            let account = format!("acct-{i}");
            assert_eq!(
                verifier.list_for_account(&account, 16).unwrap().len(),
                1,
                "round {round}: account {account} must own exactly one session"
            );
        }
    }
}
