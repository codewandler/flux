//! Walking a harness's on-disk state under a bounded budget.
//!
//! Every read here is of another harness's *own* files, at a path derived from the process
//! environment (see [`HarnessEnv`](super::HarnessEnv)) — never from model output, and never
//! written to. That is why the direct-IO waivers below are sound: there is no workspace to confine
//! these paths to (the whole point is that `~/.codex` is outside it), and `flux-system`'s guard is
//! about model-directed effects, which none of these are.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use flux_core::{Error, Result};
use rusqlite::{Connection, OpenFlags};

/// The default ceiling on how many `.jsonl` files one scan will list.
pub const MAX_JSONL_FILES: usize = 20_000;
/// The default ceiling on the size of a single `.jsonl` file a scan will open.
pub const MAX_JSONL_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// The ceiling a **message-shaped** scan puts on one `.jsonl` line (C-214).
///
/// The file cap alone does not bound memory here: a 200 MiB transcript is allowed to be one 200 MiB
/// line, and reading a line means materializing it. Well past any real record — a claude-code line
/// carrying a base64 image is single-digit MiB — so this only ever fires on pathological input.
pub const MAX_JSONL_LINE_BYTES: u64 = 8 * 1024 * 1024;
/// The ceiling a message-shaped scan puts on one extracted body (C-214).
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// The ceiling a message-shaped scan puts on the *total* body bytes it will extract (C-214).
///
/// This bounds a scan's **work**, not its memory — memory is bounded by
/// [`MAX_JSONL_LINE_BYTES`] and [`MAX_MESSAGE_BYTES`] plus the fact that extraction streams. What
/// it buys is that an unbounded tree ends in a reported stop rather than in a scan that never
/// returns.
///
/// Calibrated against real history rather than guessed: on the machine this was developed on, a
/// full scan of `~/.claude/projects` (2.2 GB of transcripts, 2 449 files) yields **473 MiB** of
/// message text across 537 637 messages, `~/.codex/sessions` 39 MiB and the opencode database
/// 36 MiB. An earlier 64 MiB guess truncated that claude-code history after 54 sessions — which is
/// how a budget turns into a bug report. This leaves ~4× headroom over a heavy real directory, and
/// when it does bite it says so
/// ([`MessageStats::budget_exhausted`](super::MessageStats::budget_exhausted)).
pub const MAX_MESSAGE_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// The ceiling a message-shaped scan puts on how many messages it will produce (C-214).
///
/// A bytes ceiling alone does not bound a consumer that keeps what it is handed: ten million empty
/// messages cost nothing in body bytes and still cost a gigabyte of record. Same calibration — the
/// heavy real directory above holds 537 637 messages.
pub const MAX_MESSAGES: usize = 5_000_000;

/// How much of a harness's state one scan will read before degrading.
///
/// This is a correctness property, not a performance knob (C-212): a harness log directory has no
/// natural bound, and the way a caller must find that out is a reported skip count, never an
/// unbounded read.
///
/// The budget has two halves. The **file-shaped** half ([`max_files`](Self::max_files),
/// [`max_file_bytes`](Self::max_file_bytes)) is what a per-turn projection like `flux usage` needs:
/// a token record is eight integers, so bounding the input bounds the output. The **body-shaped**
/// half is what message-level extraction needs (C-214) and it is not optional there — a message
/// record carries full text, so the same scan produces one to three orders of magnitude more
/// output and the input caps stop being sufficient. [`ScanBudget::default`] leaves the body half at
/// values that never bind a token-shaped scan; [`ScanBudget::for_messages`] is the tightened one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanBudget {
    /// The most `.jsonl` files one scan will list.
    pub max_files: usize,
    /// The largest `.jsonl` file one scan will open.
    pub max_file_bytes: u64,
    /// The longest single `.jsonl` line one scan will materialize. A longer line is drained and
    /// reported as [`JsonlLine::TooLarge`] without ever being held in memory.
    pub max_line_bytes: u64,
    /// The largest single message body one scan will extract.
    pub max_message_bytes: usize,
    /// The total extracted body bytes one scan will produce before it stops.
    pub max_message_total_bytes: u64,
    /// The most messages one scan will produce before it stops.
    pub max_messages: usize,
}

impl Default for ScanBudget {
    /// The file-shaped budget C-213 established, with the body half wide open: a line may be as
    /// long as the file it is in, exactly as it could before the body half existed. A token-shaped
    /// scan does not multiply its input, so nothing here binds it.
    fn default() -> Self {
        Self {
            max_files: MAX_JSONL_FILES,
            max_file_bytes: MAX_JSONL_FILE_BYTES,
            max_line_bytes: MAX_JSONL_FILE_BYTES,
            max_message_bytes: MAX_MESSAGE_BYTES,
            max_message_total_bytes: MAX_MESSAGE_TOTAL_BYTES,
            max_messages: MAX_MESSAGES,
        }
    }
}

impl ScanBudget {
    /// The budget message-level extraction runs under: the inherited file caps, plus the line and
    /// body caps that actually bound what a scan of years of transcripts costs.
    pub fn for_messages() -> Self {
        Self {
            max_line_bytes: MAX_JSONL_LINE_BYTES,
            ..Self::default()
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
        reader: BufReader::new(file),
        max_line_bytes: budget.max_line_bytes,
        buf: Vec::new(),
    })
}

fn too_large(path: &Path, budget: ScanBudget) -> bool {
    // flux-allow-direct-io: size probe for the scan budget on an environment-derived harness path;
    // metadata only, no content and no write.
    fs::metadata(path)
        .map(|m| m.len() > budget.max_file_bytes)
        .unwrap_or(false)
}

/// The lines of one harness JSONL file, read under [`ScanBudget::max_line_bytes`].
///
/// A line that cannot be read is surfaced as [`JsonlLine::Unreadable`] rather than ending the
/// iteration: one bad line in a year-old transcript must not discard the rest of the file. A line
/// *over budget* is surfaced as [`JsonlLine::TooLarge`] — and, the point of doing this by hand
/// rather than with [`BufRead::lines`], it is drained to the newline without being accumulated, so
/// the peak memory of a scan is the budget rather than the longest line in the tree.
pub struct JsonlLines {
    reader: BufReader<File>,
    max_line_bytes: u64,
    buf: Vec<u8>,
}

/// One line of a harness JSONL file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonlLine {
    /// A line that was read. It is *not* parsed — validity is the caller's problem.
    Text(String),
    /// A line that could not be read, or was not valid UTF-8. Count it as skipped and keep going.
    Unreadable,
    /// A line over [`ScanBudget::max_line_bytes`]. It was never materialized. Count it as skipped
    /// and keep going.
    TooLarge,
}

impl Iterator for JsonlLines {
    type Item = JsonlLine;

    fn next(&mut self) -> Option<Self::Item> {
        self.buf.clear();
        match read_capped_line(&mut self.reader, &mut self.buf, self.max_line_bytes) {
            Ok(CappedLine::Eof) => None,
            Ok(CappedLine::TooLarge) => Some(JsonlLine::TooLarge),
            Ok(CappedLine::Line) => Some(match String::from_utf8(std::mem::take(&mut self.buf)) {
                Ok(text) => JsonlLine::Text(text),
                Err(_) => JsonlLine::Unreadable,
            }),
            Err(_) => Some(JsonlLine::Unreadable),
        }
    }
}

/// What one call to [`read_capped_line`] found.
enum CappedLine {
    /// `buf` holds the line, newline stripped.
    Line,
    /// The line was longer than the cap; it was consumed and discarded, and `buf` is empty.
    TooLarge,
    /// Nothing left to read.
    Eof,
}

/// Read up to the next `\n`, accumulating at most `max` bytes.
///
/// The over-budget branch still consumes the whole line — the reader must land on the next record,
/// not resume mid-line and hand the caller a fragment that happens to parse.
fn read_capped_line(
    reader: &mut BufReader<File>,
    buf: &mut Vec<u8>,
    max: u64,
) -> io::Result<CappedLine> {
    let mut over = false;
    let mut any = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(match (any, over) {
                (false, _) => CappedLine::Eof,
                (true, true) => CappedLine::TooLarge,
                (true, false) => CappedLine::Line,
            });
        }
        any = true;
        let (taken, done) = match available.iter().position(|b| *b == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        if !over && buf.len() as u64 + taken as u64 > max {
            over = true;
            buf.clear();
            buf.shrink_to_fit();
        }
        if !over {
            buf.extend_from_slice(&available[..taken]);
        }
        reader.consume(taken);
        if done {
            if over {
                return Ok(CappedLine::TooLarge);
            }
            // Match `BufRead::lines`: strip the newline, and a CRLF's carriage return with it.
            if buf.last() == Some(&b'\n') {
                buf.pop();
            }
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(CappedLine::Line);
        }
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
