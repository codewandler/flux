//! Walking a harness's on-disk state under a bounded budget.
//!
//! Every read here is of another harness's *own* files, at a path derived from the process
//! environment (see [`HarnessEnv`](super::HarnessEnv)) — never from model output, and never
//! written to. That is why the direct-IO waivers below are sound: there is no workspace to confine
//! these paths to (the whole point is that `~/.codex` is outside it), and `flux-system`'s guard is
//! about model-directed effects, which none of these are.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Lines};
use std::path::{Path, PathBuf};

use flux_core::{Error, Result};
use rusqlite::{Connection, OpenFlags};

/// The default ceiling on how many `.jsonl` files one scan will list.
pub const MAX_JSONL_FILES: usize = 20_000;
/// The default ceiling on the size of a single `.jsonl` file a scan will open.
pub const MAX_JSONL_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// How much of a harness's state one scan will read before degrading.
///
/// This is a correctness property, not a performance knob (C-212): a harness log directory has no
/// natural bound, and the way a caller must find that out is a reported skip count, never an
/// unbounded read. Consumers that multiply the output per file — message-level extraction carries
/// full bodies, where a token record carries five integers — tighten it rather than inherit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanBudget {
    /// The most `.jsonl` files one scan will list.
    pub max_files: usize,
    /// The largest `.jsonl` file one scan will open.
    pub max_file_bytes: u64,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            max_files: MAX_JSONL_FILES,
            max_file_bytes: MAX_JSONL_FILE_BYTES,
        }
    }
}

/// Why one input was passed over. Skipping is always counted, never fatal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The file is over [`ScanBudget::max_file_bytes`].
    TooLarge,
    /// The file could not be opened at all — permissions, or it went away mid-scan.
    Unreadable,
}

/// The `.jsonl` files found under a harness root, plus the entries skipped along the way.
#[derive(Clone, Debug, Default)]
pub struct JsonlScan {
    files: Vec<PathBuf>,
    skipped: usize,
}

impl JsonlScan {
    /// The files to read, sorted, truncated to the budget.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Entries that could not even be listed (an unreadable subdirectory or dirent).
    pub fn skipped(&self) -> usize {
        self.skipped
    }
}

/// Collect `.jsonl` files under `root`, returning the files plus the count of unreadable entries
/// skipped along the way.
///
/// Only an unreadable *root* propagates as an error; below the root, unreadable subdirectories and
/// entries get the same per-item tolerance as bad lines and oversized files, so one
/// permission-denied path cannot blank out the whole scan.
pub fn jsonl_files(root: &Path, budget: ScanBudget) -> Result<JsonlScan> {
    // flux-allow-direct-io: read another harness's own log tree at an environment-derived root
    // (never a model-supplied path); read-only, and outside any workspace by construction.
    let read = fs::read_dir(root)?;
    let mut scan = JsonlScan::default();
    collect_jsonl_files(read, budget, &mut scan);
    scan.files.sort();
    if scan.files.len() > budget.max_files {
        scan.files.truncate(budget.max_files);
    }
    Ok(scan)
}

fn collect_jsonl_files(read: fs::ReadDir, budget: ScanBudget, scan: &mut JsonlScan) {
    if scan.files.len() >= budget.max_files {
        return;
    }
    let mut entries = Vec::new();
    for entry in read {
        match entry {
            Ok(entry) => entries.push(entry),
            Err(_) => scan.skipped += 1,
        }
    }
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let Ok(ty) = entry.file_type() else {
            scan.skipped += 1;
            continue;
        };
        if ty.is_dir() {
            // flux-allow-direct-io: recursion below an environment-derived harness root; read-only
            // listing of that harness's own log tree.
            match fs::read_dir(&path) {
                Ok(read) => collect_jsonl_files(read, budget, scan),
                Err(_) => scan.skipped += 1,
            }
        } else if ty.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            scan.files.push(path);
        }
        if scan.files.len() >= budget.max_files {
            break;
        }
    }
}

/// Open one scanned file for line iteration, enforcing the per-file byte budget.
///
/// `Err` means the file must be **skipped and counted** by the caller rather than failing the
/// scan: it is over budget, or it could not be opened at all.
pub fn open_jsonl(path: &Path, budget: ScanBudget) -> std::result::Result<JsonlLines, SkipReason> {
    if too_large(path, budget) {
        return Err(SkipReason::TooLarge);
    }
    // flux-allow-direct-io: read one file of another harness's own log tree, reached only from an
    // environment-derived root; read-only, never a model-supplied path.
    let file = File::open(path).map_err(|_| SkipReason::Unreadable)?;
    Ok(JsonlLines {
        lines: BufReader::new(file).lines(),
    })
}

fn too_large(path: &Path, budget: ScanBudget) -> bool {
    // flux-allow-direct-io: size probe for the scan budget on an environment-derived harness path;
    // metadata only, no content and no write.
    fs::metadata(path)
        .map(|m| m.len() > budget.max_file_bytes)
        .unwrap_or(false)
}

/// The lines of one harness JSONL file.
///
/// A line that cannot be read is surfaced as [`JsonlLine::Unreadable`] rather than ending the
/// iteration: one bad line in a year-old transcript must not discard the rest of the file.
pub struct JsonlLines {
    lines: Lines<BufReader<File>>,
}

/// One line of a harness JSONL file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonlLine {
    /// A line that was read. It is *not* parsed — validity is the caller's problem.
    Text(String),
    /// A line that could not be read. Count it as skipped and keep going.
    Unreadable,
}

impl Iterator for JsonlLines {
    type Item = JsonlLine;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| match line {
            Ok(text) => JsonlLine::Text(text),
            Err(_) => JsonlLine::Unreadable,
        })
    }
}

/// Open a harness SQLite database **read-only**.
///
/// This is the only database entry point in this module, and it is the enforcement of the rule that
/// no adapter ever writes another harness's state: `SQLITE_OPEN_READ_ONLY` makes a stray `insert` a
/// runtime error rather than a matter of adapter discipline.
pub fn open_sqlite_read_only(db: &Path) -> Result<Connection> {
    // flux-allow-direct-io: read-only open of another harness's own database at an
    // environment-derived path; SQLITE_OPEN_READ_ONLY makes writes impossible, not merely absent.
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sqlite_err)
}

/// Whether `table` exists in an opened harness database. Harness schemas drift between releases, so
/// adapters probe rather than assume.
pub fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn
        .query_row(
            "select count(*) from sqlite_master where type = 'table' and name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(sqlite_err)?;
    Ok(exists > 0)
}

/// Whether `table` has `column` in an opened harness database.
pub fn sqlite_column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("pragma table_info({table})"))
        .map_err(sqlite_err)?;
    let mut rows = stmt.query([]).map_err(sqlite_err)?;
    while let Some(row) = rows.next().map_err(sqlite_err)? {
        let name: String = row.get(1).map_err(sqlite_err)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Map a rusqlite error onto the shared error type, preserving its rendering verbatim so a surface
/// note reads exactly as it did when the driver error surfaced directly.
fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Other(e.to_string())
}
