//! C-125: the real, OS-level cross-process proof that a shared `events.db` survives concurrent
//! writers. `concurrent_writers_wait_on_busy_timeout_instead_of_erroring`
//! (`src/store/mod.rs`) proves the busy_timeout/`BEGIN IMMEDIATE` machinery with two connections
//! in ONE process; real deployments (`flux app run --serve` + a CLI turn, C-25's scenario)
//! contend across separate OS *processes*, where file locking and WAL shared memory actually do
//! the work. This file spawns real child processes against one shared store and checks the
//! store's own invariants afterward — contiguous `stream_seq` per stream, zero lost or
//! duplicated writes, and (the C-87 idempotent path) exactly one stored event for a stable id
//! raced by several processes.
//!
//! Test-binary re-exec, no separate helper crate/binary: each test below is its OWN worker. Run
//! normally (`cargo test`), the role env vars are unset and [`run_worker_if_invoked`] is a no-op,
//! so the test body runs as the orchestrator — it spawns copies of `std::env::current_exe()`
//! (this very test binary), filtered with `--exact <own name>` and the role env vars set, so each
//! child re-enters the SAME test function, sees the env vars, and acts as a plain writer process
//! instead of spawning anything further.

use std::path::{Path, PathBuf};
use std::process::Command;

use flux_core::Message;
use flux_events::{EventStore, NewEvent};

/// Shared events.db path for a worker to open (unset ⇒ this invocation is the orchestrator).
const DB_ENV: &str = "FLUX_EVENTS_C125_DB";
/// Comma-separated stream ids a worker round-robins its appends across.
const STREAMS_ENV: &str = "FLUX_EVENTS_C125_STREAMS";
/// How many events a worker appends.
const COUNT_ENV: &str = "FLUX_EVENTS_C125_COUNT";
/// When set, every appended event carries this SAME stable id (the idempotent-path variant)
/// instead of a store-minted one.
const STABLE_ID_ENV: &str = "FLUX_EVENTS_C125_STABLE_ID";

struct TempPath(PathBuf);

impl TempPath {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "flux-events-c125-{tag}-{}.db",
            ulid::Ulid::generate()
        ));
        let _ = std::fs::remove_file(&p);
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

/// If this process was spawned as a worker (the role env vars are set), open the shared store and
/// append `COUNT_ENV` events round-robin across `STREAMS_ENV`, then return `true`. Returns `false`
/// (doing nothing) when the env vars are absent — an ordinary `cargo test` invocation of the test
/// function that calls this, which then falls through to orchestrate its own child processes.
fn run_worker_if_invoked() -> bool {
    let (Ok(db), Ok(streams_raw), Ok(count_raw)) = (
        std::env::var(DB_ENV),
        std::env::var(STREAMS_ENV),
        std::env::var(COUNT_ENV),
    ) else {
        return false;
    };
    let streams: Vec<String> = streams_raw.split(',').map(str::to_string).collect();
    let count: usize = count_raw.parse().expect("valid worker event count");
    let stable_id = std::env::var(STABLE_ID_ENV).ok();

    let store = EventStore::open(&db).expect("worker opens the shared store");
    for i in 0..count {
        let stream = &streams[i % streams.len()];
        let mut ev = NewEvent::message(Message::user_text(format!(
            "pid {} event {i}",
            std::process::id()
        )));
        if let Some(id) = &stable_id {
            ev = ev.with_id(id.clone());
        }
        store.append(stream, ev).expect("worker append");
    }
    true
}

/// Re-exec `std::env::current_exe()` filtered to just `test_name`, with the worker role env vars
/// set, and return the spawned child.
fn spawn_worker(
    test_name: &str,
    db: &Path,
    streams: &[String],
    count: usize,
    stable_id: Option<&str>,
) -> std::process::Child {
    let exe = std::env::current_exe().expect("current_exe for re-exec");
    let mut cmd = Command::new(exe);
    cmd.args([test_name, "--exact", "--nocapture"])
        .env(DB_ENV, db)
        .env(STREAMS_ENV, streams.join(","))
        .env(COUNT_ENV, count.to_string());
    if let Some(id) = stable_id {
        cmd.env(STABLE_ID_ENV, id);
    } else {
        cmd.env_remove(STABLE_ID_ENV);
    }
    cmd.spawn().expect("spawn worker process")
}

/// The main stress proof: `WORKERS` (>= 3, per the story's Acceptance) child *processes* each
/// append `PER_WORKER` events, round-robining across `STREAM_COUNT` streams of ONE shared file
/// store — so several processes race for the SAME stream's `stream_seq`, not merely the file's
/// write lock in the abstract. After every child exits successfully, each stream's `stream_seq`s
/// (including the seq-0 `SessionStarted`) must be exactly contiguous with no gaps or duplicates,
/// and the total appended count across all streams must equal `WORKERS * PER_WORKER` — nothing
/// lost, nothing duplicated.
#[test]
fn multi_process_writers_produce_contiguous_gapless_streams() {
    if run_worker_if_invoked() {
        return; // this invocation IS a spawned worker; nothing left to orchestrate.
    }

    const WORKERS: usize = 4;
    const STREAM_COUNT: usize = 2;
    const PER_WORKER: usize = 25;

    let db = TempPath::new("multi");
    let stream_ids: Vec<String> = {
        // The orchestrator's connection drops at the end of this block, before any child opens
        // the file — real separate processes, not a shared in-process handle.
        let orchestrator = EventStore::open(db.path()).unwrap();
        (0..STREAM_COUNT)
            .map(|_| orchestrator.create_session("m").unwrap())
            .collect()
    };

    let children: Vec<_> = (0..WORKERS)
        .map(|_| {
            spawn_worker(
                "multi_process_writers_produce_contiguous_gapless_streams",
                db.path(),
                &stream_ids,
                PER_WORKER,
                None,
            )
        })
        .collect();
    for (i, mut child) in children.into_iter().enumerate() {
        let status = child.wait().expect("join worker process");
        assert!(
            status.success(),
            "worker process {i} exited non-zero: {status:?}"
        );
    }

    let verifier = EventStore::open(db.path()).unwrap();
    let mut total_appended = 0usize;
    for stream in &stream_ids {
        let events = verifier.load_stream(stream, None).unwrap();
        let mut seqs: Vec<i64> = events.iter().map(|e| e.stream_seq).collect();
        seqs.sort_unstable();
        let head = verifier.head_seq(stream).unwrap();
        let expected: Vec<i64> = (0..=head).collect();
        assert_eq!(
            seqs, expected,
            "stream {stream} has gaps or duplicates in stream_seq: {seqs:?}"
        );
        total_appended += events.len() - 1; // minus the seed SessionStarted at seq 0
    }
    assert_eq!(
        total_appended,
        WORKERS * PER_WORKER,
        "some writes were lost or duplicated across the {WORKERS} worker processes"
    );
}

/// The idempotent stable-id path (C-87) proven across a process boundary: `WORKERS` processes
/// race to append the exact SAME `NewEvent.id` into the SAME stream. On the shared file, the
/// losing process's `UNIQUE(id)` insert fails, rolls back, and re-reads the winner's row instead
/// of erroring (`append_with_ts` in `src/store/sqlite.rs`) — so every worker process still exits
/// successfully, and exactly ONE event is ever stored for that id.
#[test]
fn multi_process_idempotent_append_stores_exactly_once() {
    if run_worker_if_invoked() {
        return;
    }

    const WORKERS: usize = 3;
    let stable_id = format!("c125-stable-{}", ulid::Ulid::generate());

    let db = TempPath::new("idempotent");
    let stream = {
        let orchestrator = EventStore::open(db.path()).unwrap();
        orchestrator.create_session("m").unwrap()
    };
    let streams = vec![stream.clone()];

    let children: Vec<_> = (0..WORKERS)
        .map(|_| {
            spawn_worker(
                "multi_process_idempotent_append_stores_exactly_once",
                db.path(),
                &streams,
                1,
                Some(&stable_id),
            )
        })
        .collect();
    for (i, mut child) in children.into_iter().enumerate() {
        let status = child.wait().expect("join worker process");
        assert!(
            status.success(),
            "worker process {i} exited non-zero: {status:?}"
        );
    }

    let verifier = EventStore::open(db.path()).unwrap();
    let events = verifier.load_stream(&stream, None).unwrap();
    let matching: Vec<_> = events.iter().filter(|e| e.id == stable_id).collect();
    assert_eq!(
        matching.len(),
        1,
        "exactly one event must be stored for the id raced by {WORKERS} processes, got: {matching:?}"
    );
    // Only ONE stream_seq was ever consumed for the N racing appends of the same id (1, not N) —
    // proves the losers re-read the winner rather than each minting their own seq.
    assert_eq!(verifier.head_seq(&stream).unwrap(), 1);
}
