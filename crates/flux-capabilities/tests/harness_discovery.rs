//! The harness discovery + scan layer (C-213), exercised as a library — **no `flux` binary**.
//!
//! Every assertion here was unwritable before the extraction: the knowledge lived in `or_else`
//! chains and private helpers inside `crates/flux-cli/src/usage.rs`, so the only way to observe
//! discovery precedence or the scan budget was to run `flux usage` against a real home directory.
//!
//! What is pinned, and why:
//! - **Precedence** — env override, then the `~` default, then "missing". Implicit in the old
//!   `or_else` chains and therefore the thing a move is most likely to alter silently.
//! - **The scan budget** — a correctness property (C-212), not a tuning knob: over-budget input is
//!   skipped *and counted*, never fatal.
//! - **Read-only** — no adapter may ever write another harness's state.

use std::fs;
use std::path::{Path, PathBuf};

use codewandler_flux_capabilities::harness::{
    jsonl_files, open_jsonl, open_sqlite_read_only, HarnessEnv, HarnessKind, HarnessLocation,
    JsonlLine, ScanBudget, SkipReason,
};

fn scratch(name: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("flux-harness-{name}-{}-{n}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

/// The `(env key, path below the override root, path below `~`)` triple for each harness.
fn cases() -> Vec<(
    HarnessKind,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
)> {
    vec![
        (
            HarnessKind::Flux,
            "FLUX_HOME",
            &["events.db"],
            &[".flux", "events.db"],
        ),
        (
            HarnessKind::Codex,
            "CODEX_HOME",
            &["sessions"],
            &[".codex", "sessions"],
        ),
        (
            HarnessKind::Claude,
            "CLAUDE_CONFIG_DIR",
            &["projects"],
            &[".claude", "projects"],
        ),
        (
            HarnessKind::Opencode,
            "OPENCODE_DATA_DIR",
            &["opencode.db"],
            &[".local", "share", "opencode", "opencode.db"],
        ),
    ]
}

fn join(root: &Path, parts: &[&str]) -> PathBuf {
    parts.iter().fold(root.to_path_buf(), |acc, p| acc.join(p))
}

#[test]
fn the_env_override_wins_over_the_home_default_for_every_harness() {
    let root = scratch("override");
    let home = root.join("home");
    let over = root.join("override");

    for (kind, key, under_override, under_home) in cases() {
        // Both candidates exist: only precedence can decide, so a reversed `or_else` is visible.
        let home_path = join(&home, under_home);
        let over_path = join(&over, under_override);
        fs::create_dir_all(home_path.parent().unwrap()).unwrap();
        fs::create_dir_all(over_path.parent().unwrap()).unwrap();
        fs::write(&home_path, "").unwrap();
        fs::write(&over_path, "").unwrap();

        let env = HarnessEnv::empty()
            .with("HOME", &home)
            .with(key, over.as_os_str());
        assert_eq!(
            kind.locate(&env),
            HarnessLocation::Found(over_path),
            "{}: the {key} override must beat ~",
            kind.id()
        );

        // An empty override is not an override — it must fall through to `~`, not resolve to "".
        let env = HarnessEnv::empty().with("HOME", &home).with(key, "");
        assert_eq!(
            kind.locate(&env),
            HarnessLocation::Found(home_path),
            "{}: an empty {key} falls back to ~",
            kind.id()
        );
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_resolvable_but_absent_root_is_not_found_and_an_unresolvable_one_is_unresolved() {
    let root = scratch("missing");
    let home = root.join("empty-home");
    fs::create_dir_all(&home).unwrap();

    for (kind, key, _, under_home) in cases() {
        // Resolvable (HOME is set) but nothing is there: the path is still reported, so the CLI can
        // say *where* it looked.
        let env = HarnessEnv::empty().with("HOME", &home);
        assert_eq!(
            kind.locate(&env),
            HarnessLocation::NotFound(join(&home, under_home)),
            "{}: a resolvable-but-absent root keeps its path",
            kind.id()
        );

        // Neither an override nor a HOME: there is no path to report at all.
        assert_eq!(
            kind.locate(&HarnessEnv::empty()),
            HarnessLocation::Unresolved,
            "{}: no {key} and no HOME is unresolvable, not a bogus relative path",
            kind.id()
        );
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_scan_budget_skips_and_counts_instead_of_failing() {
    let root = scratch("budget");
    for n in 0..4 {
        fs::write(root.join(format!("{n}.jsonl")), "{}\n").unwrap();
    }
    fs::write(root.join("ignored.txt"), "{}\n").unwrap();

    // The file-count budget truncates rather than erroring, and it is a real bound, not advice.
    let budget = ScanBudget {
        max_files: 2,
        ..ScanBudget::default()
    };
    let scan = jsonl_files(&root, budget).unwrap();
    assert_eq!(scan.files().len(), 2, "the file budget bounds the scan");
    assert!(scan
        .files()
        .iter()
        .all(|f| f.extension().unwrap() == "jsonl"));

    // The per-file byte budget degrades to a typed skip reason, never to an error.
    fs::write(root.join("0.jsonl"), "{\"a\":1}\n{\"b\":2}\n").unwrap();
    let small = ScanBudget {
        max_file_bytes: 4,
        ..ScanBudget::default()
    };
    assert_eq!(
        open_jsonl(&root.join("0.jsonl"), small).err(),
        Some(SkipReason::TooLarge)
    );
    assert_eq!(
        open_jsonl(&root.join("nope.jsonl"), ScanBudget::default()).err(),
        Some(SkipReason::Unreadable)
    );

    let lines = open_jsonl(&root.join("0.jsonl"), ScanBudget::default()).unwrap();
    let text: Vec<String> = lines
        .filter_map(|line| match line {
            JsonlLine::Text(t) => Some(t),
            JsonlLine::Unreadable => None,
        })
        .collect();
    assert_eq!(text, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);

    // An unreadable root is the one failure that propagates — it becomes the harness note.
    assert!(jsonl_files(&root.join("absent"), ScanBudget::default()).is_err());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_harness_database_opens_read_only() {
    let root = scratch("readonly");
    let db = root.join("opencode.db");
    let seed = rusqlite::Connection::open(&db).unwrap();
    seed.execute("create table message (id text primary key)", [])
        .unwrap();
    drop(seed);

    let conn = open_sqlite_read_only(&db).unwrap();
    // Reading is fine; writing another harness's state is not, and that is enforced by the open
    // flags rather than by adapter discipline.
    let count: i64 = conn
        .query_row("select count(*) from message", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let write = conn.execute("insert into message (id) values ('x')", []);
    assert!(write.is_err(), "a harness database must open read-only");

    let _ = fs::remove_dir_all(root);
}
