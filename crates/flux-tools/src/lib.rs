//! `flux-tools` — the built-in coding tools (`read`, `write`, `edit`, `bash`).
//!
//! Each implements [`flux_runtime::Tool`]: it declares its permission subjects (so rules and
//! approval can gate it), its [`ToolSpec`] (effects/risk), and its pre-execution [`IntentSet`],
//! and performs all IO through the guarded [`System`](flux_system::System). `bash` runs commands
//! via `sh -c` (an explicit, gated shell — `flux-system` itself never interprets argv as shell).

use std::time::Duration;

pub mod cargo;
pub mod extra;
pub mod groups;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_policy::wildcard_match;
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};

use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use std::sync::Arc;

const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
/// Upper bound on files visited by `glob`/`grep` before stopping (cost guard).
const WALK_FILE_CAP: usize = 10_000;
const DEFAULT_GLOB_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 200;
/// An unbounded `read` (no explicit offset/limit) over these caps returns guidance instead of dumping.
const READ_LINE_CAP: usize = 2000;
const READ_BYTE_CAP: usize = 256 * 1024;
/// Bytes sniffed for a NUL when detecting a binary file.
const BINARY_SNIFF: usize = 8192;
/// Cap on the number of unified-diff lines surfaced in an edit/write view.
const DIFF_LINE_CAP: usize = 200;

/// A single read-target intent for a path (used by the read-only `glob`/`grep` tools).
fn read_intent(path: &str) -> IntentSet {
    let mut set = IntentSet::new();
    set.push(Intent {
        behavior: IntentBehavior::FilesystemRead,
        target: IntentTarget::Path {
            path: path.to_string(),
        },
        role: IntentRole::ReadTarget,
        certainty: IntentCertainty::Certain,
    });
    set
}

fn str_param<'a>(params: &'a Value, key: &str, tool: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("{tool}: required string param `{key}` missing")))
}

/// Read an integer argument from `obj[key]`, accepting either a JSON number or a numeric string —
/// LLMs frequently emit `"120"` instead of `120`, and a strict `as_u64()` would silently drop it
/// (e.g. a paged `read` would fall back to an unbounded read and hit the large-file guard). Returns
/// `None` when the key is absent or the value isn't a non-negative integer.
fn u64_arg(obj: &Value, key: &str) -> Option<u64> {
    let v = obj.get(key)?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

// ---------------------------------------------------------------------------
// shared file-read / diff helpers
// ---------------------------------------------------------------------------

/// Decoding a file's bytes under the read guards.
enum Decoded {
    /// Decoded UTF-8 text (binary + UTF-8 checks passed).
    Text(String),
    /// A guard tripped: the message to surface, and whether it is an error (binary / bad UTF-8) or
    /// soft guidance (file too large — the planner should re-read a window, so NOT an error).
    Guard { message: String, is_error: bool },
}

/// Sniff `bytes` for a NUL (binary) then decode as UTF-8. Does NOT apply the line/byte cap — the
/// caller decides that, because an explicit `offset`/`limit` window bypasses the cap.
fn decode_text(path: &str, bytes: Vec<u8>) -> Decoded {
    if bytes.iter().take(BINARY_SNIFF).any(|&b| b == 0) {
        return Decoded::Guard {
            message: format!("{path} looks binary (NUL byte in first 8KB); not a text file"),
            is_error: true,
        };
    }
    match String::from_utf8(bytes) {
        Ok(s) => Decoded::Text(s),
        Err(_) => Decoded::Guard {
            message: format!("{path}: not valid UTF-8"),
            is_error: true,
        },
    }
}

/// Render `text` with right-aligned 1-based line numbers (`{n}\t{line}`) starting at `start_line` —
/// the model-facing *view* for `read`/`view`. The canonical content stays un-numbered.
fn number_lines(text: &str, start_line: usize) -> String {
    let count = text.lines().count();
    if count == 0 {
        return String::new();
    }
    let width = (start_line + count - 1).to_string().len();
    text.lines()
        .enumerate()
        .map(|(i, l)| format!("{:>width$}\t{l}", start_line + i, width = width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lexically normalize a workspace path to a stable read-set key, so `foo.rs`, `./foo.rs`, and
/// `a/../foo.rs` map to the same entry — otherwise the read-before-write guard misfires when a later
/// edit re-spells the path it read. Pure string work, no filesystem access (the jail still re-resolves
/// the real path for IO).
fn norm_key(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// Record that `path`'s current content has been seen (read or just written), with its mtime — the
/// baseline for the read-before-write guard. Best-effort.
async fn note_read(ctx: &ToolContext, path: &str) {
    if let Ok(m) = ctx.system.file_mtime(path).await {
        ctx.record_read(&norm_key(path), m);
    }
}

/// The read-before-write guard. Refuses to modify a file that changed on disk since its content was
/// last seen this session. `require_seen` (edit/patch) additionally refuses if the file was never
/// read or written this session — so the model is editing content it actually saw. `write`/`append`
/// pass `require_seen=false`: creating/overwriting/appending without a prior read is legitimate.
async fn guard_unchanged(ctx: &ToolContext, path: &str, require_seen: bool) -> Result<()> {
    match ctx.read_mtime(&norm_key(path)) {
        Some(seen) => {
            if let Ok(now) = ctx.system.file_mtime(path).await {
                if now > seen {
                    return Err(Error::Other(format!(
                        "{path} changed on disk since you last read it; re-read it before editing"
                    )));
                }
            }
            Ok(())
        }
        None if require_seen => Err(Error::Other(format!(
            "{path} must be read before editing (read it first so you see the current content)"
        ))),
        None => Ok(()),
    }
}

/// A capped unified diff of `before`→`after` (empty when equal), for an edit/write *view*.
fn unified_diff(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let diff = similar::TextDiff::from_lines(before, after);
    let full = diff
        .unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    let lines: Vec<&str> = full.lines().collect();
    if lines.len() > DIFF_LINE_CAP {
        let mut capped = lines[..DIFF_LINE_CAP].join("\n");
        capped.push_str("\n…[diff truncated]");
        capped
    } else {
        full
    }
}

/// Register all built-in tools into a registry.
pub fn register_builtins(registry: &mut ToolRegistry) {
    cargo::register_cargo(registry);
    extra::register_extra(registry);
    registry.register(Arc::new(ReadTool));
    registry.register(Arc::new(ReadManyTool));
    registry.register(Arc::new(WriteTool));
    registry.register(Arc::new(EditTool));
    registry.register(Arc::new(PatchTool));
    registry.register(Arc::new(AppendTool));
    registry.register(Arc::new(BashTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry.register(Arc::new(GitStageTool));
    registry.register(Arc::new(GitCommitTool));
    registry.register(Arc::new(GitStatusTool));
    registry.register(Arc::new(GitDiffTool));
    registry.register(Arc::new(GitLogTool));
    registry.register(Arc::new(GitPushTool));
    registry.register(Arc::new(GitCheckoutTool));
    registry.register(Arc::new(GitUnstageTool));
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "read",
            "Read a UTF-8 file from the workspace (raw text, safe to embed via `{{symbol}}`; you see a \
             line-numbered view). Optional `offset`/`limit` select a line range. Refuses binary files \
             and, for a very large file read whole, returns guidance to request a range instead.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path"},
                    "offset": {"type": "integer", "description": "0-based first line"},
                    "limit": {"type": "integer", "description": "Max lines to return"}
                },
                "required": ["path"]
            }),
        )
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemRead,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = str_param(&params, "path", "read")?;
        let bytes = ctx.system.read_file_bytes(path).await?;
        let total_bytes = bytes.len();
        let content = match decode_text(path, bytes) {
            Decoded::Text(s) => s,
            // Binary → error; too-large guidance is handled below (this path is binary/bad-UTF-8 only).
            Decoded::Guard { message, is_error } => {
                return Ok(if is_error {
                    ToolResult::error(message)
                } else {
                    ToolResult::ok(message)
                });
            }
        };
        let offset = u64_arg(&params, "offset").unwrap_or(0) as usize;
        let limit = u64_arg(&params, "limit").map(|n| n as usize);

        // Unbounded read: refuse to dump an over-cap file — return guidance (NOT an error) so the
        // planner re-reads a window. The model picked no window, so there's no clean value to bind.
        if offset == 0 && limit.is_none() {
            let line_count = content.lines().count();
            if line_count > READ_LINE_CAP || total_bytes > READ_BYTE_CAP {
                return Ok(ToolResult::ok(format!(
                    "{path} has {line_count} lines ({total_bytes} bytes); read a range with \
                     offset/limit (e.g. offset:0, limit:{READ_LINE_CAP})"
                )));
            }
            note_read(ctx, path).await;
            // Canonical = raw bytes (interpolation-clean); view = line-numbered.
            let view = number_lines(&content, 1);
            return Ok(ToolResult::ok_view(content, view));
        }

        // Explicit window: honor it (the model chose the range). `saturating_add` — attacker-supplied
        // offset/limit can otherwise overflow usize and panic.
        let lines: Vec<&str> = content.lines().collect();
        let end = match limit {
            Some(l) => offset.saturating_add(l).min(lines.len()),
            None => lines.len(),
        };
        let start = offset.min(lines.len());
        let slice = lines[start..end].join("\n");
        note_read(ctx, path).await;
        let view = number_lines(&slice, start + 1);
        Ok(ToolResult::ok_view(slice, view))
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write (create/overwrite) a UTF-8 file in the workspace.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = str_param(&params, "path", "write")?;
        let content = str_param(&params, "content", "write")?;
        // Soft guard: refuse only if we saw this file and it changed on disk since (don't clobber).
        guard_unchanged(ctx, path, false).await?;
        // Read prior content for a diff (a missing/binary file ⇒ empty `before` = all additions).
        let before = ctx.system.read_file(path).await.unwrap_or_default();
        ctx.system.write_file(path, content).await?;
        note_read(ctx, path).await; // we now know current content
        let status = format!("wrote {} bytes to {path}", content.len());
        Ok(edit_result(status, &unified_diff(path, &before, content)))
    }
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace a string in a workspace file. `old_string` must occur exactly \
                          once (or set `replace_all` to replace every occurrence). If the exact text \
                          isn't found, progressively looser matching is tried — trailing whitespace, \
                          then indentation drift, then anchoring on the first/last line of a block — \
                          and the result reports which strategy matched. Returns a unified diff."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = str_param(&params, "path", "edit")?;
        let old = str_param(&params, "old_string", "edit")?;
        let new = str_param(&params, "new_string", "edit")?;
        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Must have read (or written) this file this session, and it must not have changed since.
        guard_unchanged(ctx, path, true).await?;
        let content = ctx.system.read_file(path).await?;
        let count = content.matches(old).count();

        // Exact path (honors `replace_all` and the uniqueness guard).
        if count >= 1 {
            if count > 1 && !replace_all {
                return Err(Error::Other(format!(
                    "edit: `old_string` occurs {count} times in {path} (lines {}); pass replace_all \
                     or add surrounding context to make it unique",
                    occurrence_lines(&content, old)
                )));
            }
            let updated = if replace_all {
                content.replace(old, new)
            } else {
                content.replacen(old, new, 1)
            };
            ctx.system.write_file(path, &updated).await?;
            note_read(ctx, path).await;
            let n = if replace_all { count } else { 1 };
            let status = format!(
                "edited {path} ({n} replacement{})",
                if n != 1 { "s" } else { "" }
            );
            return Ok(edit_result(status, &unified_diff(path, &content, &updated)));
        }

        // Exact match failed → try progressively looser whitespace/indentation matching, so the model
        // doesn't burn a turn re-guessing the exact bytes.
        match fuzzy_locate(&content, old, new) {
            Ok((strategy, updated)) => {
                ctx.system.write_file(path, &updated).await?;
                note_read(ctx, path).await;
                let mut status = format!("edited {path} (matched via {})", strategy.label());
                if strategy.cautious() {
                    status.push_str(" — leading whitespace differed, verify the change");
                }
                Ok(edit_result(status, &unified_diff(path, &content, &updated)))
            }
            Err(FuzzErr::Ambiguous { strategy, lines }) => Err(Error::Other(format!(
                "edit: `old_string` not found exactly in {path}; a {}-tolerant match is ambiguous \
                 (lines {lines}); add surrounding context to make it unique",
                strategy.label()
            ))),
            Err(FuzzErr::NotFound) => Err(Error::Other(format!(
                "edit: `old_string` not found in {path}{}",
                not_found_hint(&content, old)
            ))),
        }
    }
}

/// Build the edit/write result: canonical `content` = the short status line (so it stays clean if
/// ever interpolated); the model-facing `view` = status + the unified diff (when non-empty).
fn edit_result(status: String, diff: &str) -> ToolResult {
    if diff.is_empty() {
        ToolResult::ok(status)
    } else {
        let view = format!("{status}\n\n{diff}");
        ToolResult::ok_view(status, view)
    }
}

/// A whitespace/indentation-tolerant match strategy, tried in order after an exact match fails.
enum FuzzStrategy {
    /// Leading indent matches; only trailing whitespace / a final newline differs.
    TrimTrailingWs,
    /// Per-line text matches after trimming ALL surrounding whitespace (the model's indent drifted).
    TrimAllWs,
    /// Only the first and last lines of a ≥3-line block are anchored (the middle drifted).
    BlockAnchor,
}

impl FuzzStrategy {
    fn label(&self) -> &'static str {
        match self {
            FuzzStrategy::TrimTrailingWs => "trailing-whitespace",
            FuzzStrategy::TrimAllWs => "indentation",
            FuzzStrategy::BlockAnchor => "block-anchor",
        }
    }
    /// Loose enough that the edit warrants a "verify" caution (re-based indentation / anchored block).
    fn cautious(&self) -> bool {
        !matches!(self, FuzzStrategy::TrimTrailingWs)
    }
}

/// Why a fuzzy match did not yield a unique edit.
enum FuzzErr {
    /// A strategy matched in more than one place — refuse rather than guess.
    Ambiguous {
        strategy: FuzzStrategy,
        lines: String,
    },
    /// No strategy matched.
    NotFound,
}

/// The leading-whitespace prefix of a line.
fn leading_ws(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

/// Byte range of the line window `[start, start+len)` over `cl` (lines from `split_inclusive('\n')`).
fn window_bytes(cl: &[&str], start: usize, len: usize) -> (usize, usize) {
    let s: usize = cl[..start].iter().map(|x| x.len()).sum();
    let e: usize = s + cl[start..start + len]
        .iter()
        .map(|x| x.len())
        .sum::<usize>();
    (s, e)
}

/// Splice `replacement` into `content` over the byte window `[s, e)`, matching the window's line
/// endings (CRLF) and preserving a trailing newline the window had (so the next line isn't merged).
fn splice_window(content: &str, s: usize, e: usize, replacement: &str) -> String {
    let matched = &content[s..e];
    let crlf = matched.contains("\r\n");
    let mut r = if crlf {
        replacement.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        replacement.to_string()
    };
    // Only re-add a trailing newline when there's a replacement to terminate; an empty `r` (a fuzzy
    // deletion) should drop the matched line(s) entirely rather than leave a blank line behind.
    if !r.is_empty() && matched.ends_with('\n') && !r.ends_with('\n') {
        r.push_str(if crlf { "\r\n" } else { "\n" });
    }
    format!("{}{r}{}", &content[..s], &content[e..])
}

/// Re-base `new`'s indentation onto the matched block: strip the model's base indent (`model_base`,
/// from `old`'s first line) and apply the file's base indent (`file_base`, from the matched first
/// line), per non-blank line. A no-op when the two bases are equal.
fn reindent(new: &str, model_base: &str, file_base: &str) -> String {
    if model_base == file_base {
        return new.to_string();
    }
    new.split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else if let Some(rest) = l.strip_prefix(model_base) {
                format!("{file_base}{rest}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Line-window start indices where `same(cl[i+j], ol[j])` holds for every `j` in the window.
fn line_window_hits(cl: &[&str], ol: &[&str], same: impl Fn(&str, &str) -> bool) -> Vec<usize> {
    let len = ol.len();
    if len == 0 || len > cl.len() {
        return Vec::new();
    }
    (0..=cl.len() - len)
        .filter(|&i| (0..len).all(|j| same(cl[i + j], ol[j])))
        .collect()
}

/// Resolve a strategy's hits: exactly one → `Some(Ok(rewrite))`; many → `Err(Ambiguous)`; none →
/// `None` (try the next strategy). `len` is the window length in lines.
fn resolve_hits(
    content: &str,
    cl: &[&str],
    hits: &[usize],
    len: usize,
    strategy: FuzzStrategy,
    new: &str,
    model_base: &str,
) -> std::result::Result<Option<(FuzzStrategy, String)>, FuzzErr> {
    match hits {
        [] => Ok(None),
        [i] => {
            let (s, e) = window_bytes(cl, *i, len);
            let replacement = if strategy.cautious() {
                reindent(new, model_base, leading_ws(cl[*i]))
            } else {
                new.to_string()
            };
            Ok(Some((strategy, splice_window(content, s, e, &replacement))))
        }
        many => Err(FuzzErr::Ambiguous {
            strategy,
            lines: many
                .iter()
                .take(10)
                .map(|i| (i + 1).to_string())
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// Try the fuzzy strategies in order; the first with a *unique* hit wins (returns the rewritten file).
/// A strategy that matches in multiple places yields an ambiguity error rather than guessing.
fn fuzzy_locate(
    content: &str,
    old: &str,
    new: &str,
) -> std::result::Result<(FuzzStrategy, String), FuzzErr> {
    let cl: Vec<&str> = content.split_inclusive('\n').collect();
    let ol: Vec<&str> = old.split_inclusive('\n').collect();
    if ol.is_empty() || ol.len() > cl.len() {
        return Err(FuzzErr::NotFound);
    }
    let model_base = leading_ws(ol[0]);

    // 1. trailing whitespace only (leading indent must still match) — splice `new` verbatim.
    let hits = line_window_hits(&cl, &ol, |a, b| a.trim_end() == b.trim_end());
    if let Some(res) = resolve_hits(
        content,
        &cl,
        &hits,
        ol.len(),
        FuzzStrategy::TrimTrailingWs,
        new,
        model_base,
    )? {
        return Ok(res);
    }
    // 2. full per-line trim (indentation drifted) — re-base `new` onto the matched block.
    let hits = line_window_hits(&cl, &ol, |a, b| a.trim() == b.trim());
    if let Some(res) = resolve_hits(
        content,
        &cl,
        &hits,
        ol.len(),
        FuzzStrategy::TrimAllWs,
        new,
        model_base,
    )? {
        return Ok(res);
    }
    // 3. block-anchor: only the first & last lines of a ≥3-line block are matched (middle drifted).
    if ol.len() >= 3 {
        let first = ol[0].trim();
        let last = ol[ol.len() - 1].trim();
        let len = ol.len();
        let hits: Vec<usize> = (0..=cl.len() - len)
            .filter(|&i| cl[i].trim() == first && cl[i + len - 1].trim() == last)
            .collect();
        if let Some(res) = resolve_hits(
            content,
            &cl,
            &hits,
            len,
            FuzzStrategy::BlockAnchor,
            new,
            model_base,
        )? {
            return Ok(res);
        }
    }
    Err(FuzzErr::NotFound)
}

/// Hint for a failed exact match: flag when a line with the same text exists but indented
/// differently (the agent should match the exact leading whitespace).
fn not_found_hint(content: &str, old: &str) -> String {
    let first = old.lines().next().unwrap_or("").trim();
    if !first.is_empty() && content.lines().any(|l| l.trim() == first) {
        " (a line with matching text exists but the indentation differs — match the exact leading \
         whitespace)"
            .to_string()
    } else {
        String::new()
    }
}

/// 1-based line numbers where `old` begins in `content` (capped at 10), for the not-unique error.
fn occurrence_lines(content: &str, old: &str) -> String {
    if old.is_empty() {
        return String::new();
    }
    let mut nums = Vec::new();
    let mut from = 0;
    while let Some(pos) = content[from..].find(old) {
        let abs = from + pos;
        nums.push((content[..abs].matches('\n').count() + 1).to_string());
        from = abs + old.len();
        if nums.len() >= 10 {
            break;
        }
    }
    nums.join(", ")
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

pub struct BashTool;

/// Parse a shell command into permission subjects (one per `&&`/`||`/`;`/`|`/newline segment),
/// shaped as `prog:args` (or bare `prog`) so rules like `Bash(git:*)` / `Bash(rm:*)` match.
///
/// Shell is Turing-complete, so this is **best-effort defense-in-depth**, not a sandbox (the real
/// boundary is the argv-only exec + the policy floor + destructive-intent escalation, which sees the
/// whole command). But it hardens the common evasions: leading `VAR=value` assignments are skipped
/// to find the real program, programs hidden inside `$(...)`/backtick substitutions are surfaced as
/// their own subjects (so a `Bash(rm:*)` deny still matches `echo $(rm -rf ~)`), and any command
/// using shell expansion we can't statically resolve gets a `<shell-expansion>` sentinel subject —
/// which no ordinary allow rule covers, so the call falls through to an approval prompt instead of
/// being silently authorized.
pub fn bash_subjects(command: &str) -> Vec<String> {
    let mut subjects = Vec::new();
    let mut obfuscated = false;

    // The top-level command plus any embedded command substitutions, so programs hidden inside
    // `$(...)`/backticks are surfaced too.
    let mut to_scan = vec![command.to_string()];
    let inner = extract_command_substitutions(command);
    if !inner.is_empty() {
        obfuscated = true;
        to_scan.extend(inner);
    }

    for cmd in &to_scan {
        for seg in cmd.split(['&', '|', ';', '\n']) {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            let mut toks = seg.split_whitespace().peekable();
            // Skip leading `VAR=value` environment assignments to find the real program.
            while toks.peek().is_some_and(|t| is_env_assignment(t)) {
                toks.next();
            }
            let Some(prog) = toks.next() else { continue };
            // A shell-expanded program name (`$IFS`, `${x}`, `` `…` ``) can't be matched reliably.
            if prog.contains('$') || prog.contains('`') {
                obfuscated = true;
            }
            let rest: Vec<&str> = toks.collect();
            if rest.is_empty() {
                subjects.push(prog.to_string());
            } else {
                subjects.push(format!("{prog}:{}", rest.join(" ")));
            }
        }
    }

    if subjects.is_empty() {
        subjects.push(command.trim().to_string());
    }
    if obfuscated {
        subjects.push("<shell-expansion>".to_string());
    }
    subjects
}

/// Whether `tok` is a leading `NAME=value` environment assignment (so it can be skipped to find the
/// real program in `X=1 rm -rf /`).
fn is_env_assignment(tok: &str) -> bool {
    match tok.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Extract the inner command strings of `$(...)` and `` `...` `` substitutions (one level), so a
/// program hidden inside one can still be surfaced as a permission subject.
fn extract_command_substitutions(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'(') {
            let mut depth = 1;
            let start = i + 2;
            let mut j = start;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            if depth == 0 {
                out.push(chars[start..j].iter().collect());
                i = j + 1;
                continue;
            }
        } else if chars[i] == '`' {
            if let Some(close) = (i + 1..chars.len()).find(|&k| chars[k] == '`') {
                out.push(chars[i + 1..close].iter().collect());
                i = close + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command (via `sh -c`) in the workspace root. Gated by \
                          permission rules and approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_secs": {"type": "integer"}
                },
                "required": ["command"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("command")
            .and_then(|v| v.as_str())
            .map(bash_subjects)
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(c) = params.get("command").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process {
                    command: c.to_string(),
                },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let command = str_param(&params, "command", "bash")?;
        let timeout = u64_arg(&params, "timeout_secs").unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
        let argv = vec!["sh".to_string(), "-c".to_string(), command.to_string()];
        let out = ctx.system.run(&argv, Duration::from_secs(timeout)).await?;
        let mut body = String::new();
        if !out.stdout.is_empty() {
            body.push_str(&out.stdout);
        }
        if !out.stderr.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&out.stderr);
        }
        if out.exit_code != 0 {
            body.push_str(&format!("\n[exit {}]", out.exit_code));
        }
        Ok(ToolResult {
            content: body,
            view: None,
            is_error: out.exit_code != 0,
        })
    }
}

// ---------------------------------------------------------------------------
// glob
// ---------------------------------------------------------------------------

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "glob",
            "List workspace files matching a glob pattern. `*` matches any characters (including \
             `/`), so `*.rs` finds all Rust files and `src/*` everything under src. Optional \
             `path` scopes the search to a subdirectory. Patterns match workspace-relative paths.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob, e.g. `*.rs` or `src/*`"},
                    "path": {"type": "string", "description": "Subdirectory to search (default `.`)"}
                },
                "required": ["pattern"]
            }),
        )
        .with_effects(vec![Effect::Read, Effect::Filesystem])
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        vec![params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string()]
    }

    fn intents(&self, params: &Value) -> IntentSet {
        read_intent(params.get("path").and_then(|v| v.as_str()).unwrap_or("."))
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let pattern = str_param(&params, "pattern", "glob")?;
        let base = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let files = ctx.system.walk_files(base, WALK_FILE_CAP).await?;
        let mut matches: Vec<String> = files
            .into_iter()
            .filter(|f| wildcard_match(pattern, f))
            .collect();
        matches.truncate(DEFAULT_GLOB_LIMIT);
        if matches.is_empty() {
            return Ok(ToolResult::ok("no files match"));
        }
        Ok(ToolResult::ok(matches.join("\n")))
    }
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "grep",
            "Search file contents by regular expression across the workspace (set `literal` for a \
             plain substring instead). Optional `glob` restricts which files are searched (e.g. \
             `*.rs`) and `path` scopes to a subdirectory. Returns `path:line: text` for each match.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex to find (or substring if `literal`)"},
                    "literal": {"type": "boolean", "description": "Treat `pattern` as a plain substring, not a regex"},
                    "glob": {"type": "string", "description": "Only search files matching this glob"},
                    "path": {"type": "string", "description": "Subdirectory to search (default `.`)"},
                    "max_results": {"type": "integer", "description": "Cap on matches (default 200)"}
                },
                "required": ["pattern"]
            }),
        )
        .with_effects(vec![Effect::Read, Effect::Filesystem])
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        vec![params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string()]
    }

    fn intents(&self, params: &Value) -> IntentSet {
        read_intent(params.get("path").and_then(|v| v.as_str()).unwrap_or("."))
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let pattern = str_param(&params, "pattern", "grep")?;
        let base = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let glob = params.get("glob").and_then(|v| v.as_str());
        let literal = params
            .get("literal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = u64_arg(&params, "max_results")
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_GREP_LIMIT);

        // Compile the matcher once. An invalid regex returns a clean error (no panic) so the planner
        // can repair it; `literal` falls back to plain substring search.
        let re = if literal {
            None
        } else {
            match regex::Regex::new(pattern) {
                Ok(r) => Some(r),
                Err(e) => return Ok(ToolResult::error(format!("grep: invalid regex: {e}"))),
            }
        };
        let is_match = |line: &str| match &re {
            Some(r) => r.is_match(line),
            None => line.contains(pattern),
        };

        let files = ctx.system.walk_files(base, WALK_FILE_CAP).await?;
        let mut out = Vec::new();
        'files: for f in files {
            if let Some(g) = glob {
                if !wildcard_match(g, &f) {
                    continue;
                }
            }
            // Best-effort: skip binary/non-UTF-8/unreadable files rather than failing the search.
            let Ok(content) = ctx.system.read_file(&f).await else {
                continue;
            };
            for (i, line) in content.lines().enumerate() {
                if is_match(line) {
                    let shown: String = if line.chars().count() > 200 {
                        let head: String = line.chars().take(200).collect();
                        format!("{head}…")
                    } else {
                        line.trim_end().to_string()
                    };
                    out.push(format!("{f}:{}: {shown}", i + 1));
                    if out.len() >= max {
                        break 'files;
                    }
                }
            }
        }
        if out.is_empty() {
            return Ok(ToolResult::ok("no matches"));
        }
        Ok(ToolResult::ok(out.join("\n")))
    }
}

// ---------------------------------------------------------------------------
// append
// ---------------------------------------------------------------------------

pub struct AppendTool;

#[async_trait]
impl Tool for AppendTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "append".into(),
            description:
                "Append text to a workspace file, creating it (and parent dirs) if absent. \
                          Lower-risk than `write`, which overwrites the whole file."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Low,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = str_param(&params, "path", "append")?;
        let content = str_param(&params, "content", "append")?;
        guard_unchanged(ctx, path, false).await?;
        ctx.system.append_file(path, content).await?;
        note_read(ctx, path).await;
        Ok(ToolResult::ok(format!(
            "appended {} bytes to {path}",
            content.len()
        )))
    }
}

// ---------------------------------------------------------------------------
// read_many
// ---------------------------------------------------------------------------

pub struct ReadManyTool;

/// Read one file for `read_many`, returning `(canonical_section, view_section)`.
async fn read_section(ctx: &ToolContext, path: &str) -> (String, String) {
    match ctx.system.read_file_bytes(path).await {
        Ok(bytes) => {
            let total_bytes = bytes.len();
            match decode_text(path, bytes) {
                Decoded::Text(s) => {
                    // Same caps as `read`: a survey shouldn't dump (and blow context on) a huge file.
                    let line_count = s.lines().count();
                    if line_count > READ_LINE_CAP || total_bytes > READ_BYTE_CAP {
                        let sec = format!(
                            "==> {path} <== ({line_count} lines, {total_bytes} bytes — too large to \
                             survey; read a range with `read` offset/limit)"
                        );
                        return (sec.clone(), sec);
                    }
                    note_read(ctx, path).await;
                    let numbered = number_lines(&s, 1);
                    (
                        format!("==> {path} <==\n{s}"),
                        format!("==> {path} <==\n{numbered}"),
                    )
                }
                Decoded::Guard { message, .. } => {
                    let sec = format!("==> {path} <== ({message})");
                    (sec.clone(), sec)
                }
            }
        }
        Err(e) => {
            let sec = format!("==> {path} <== (error: {e})");
            (sec.clone(), sec)
        }
    }
}

#[async_trait]
impl Tool for ReadManyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "read_many",
            "Read several files at once to survey them (each section is headed `==> path <==`). \
             For embedding one file's text into a later string, read it singly with `read` instead.",
            json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Workspace-relative paths to read"
                    }
                },
                "required": ["paths"]
            }),
        )
        .with_effects(vec![Effect::Read, Effect::Filesystem])
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        path_list(params)
    }

    fn intents(&self, params: &Value) -> IntentSet {
        // One read intent per path, so each file is gated/audited individually.
        let mut set = IntentSet::new();
        for p in path_list(params) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemRead,
                target: IntentTarget::Path { path: p },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let paths = path_list(&params);
        if paths.is_empty() {
            return Err(Error::Other(
                "read_many: `paths` must be a non-empty array of strings".to_string(),
            ));
        }
        let sections = futures::future::join_all(paths.iter().map(|p| read_section(ctx, p))).await;
        let mut canonical = Vec::with_capacity(sections.len());
        let mut view = Vec::with_capacity(sections.len());
        for (c, v) in sections {
            canonical.push(c);
            view.push(v);
        }
        Ok(ToolResult::ok_view(
            canonical.join("\n\n"),
            view.join("\n\n"),
        ))
    }
}

/// Extract the `paths` string array from params (empty when absent/malformed).
fn path_list(params: &Value) -> Vec<String> {
    params
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// patch (line-anchored multi-edit)
// ---------------------------------------------------------------------------

pub struct PatchTool;

/// A single line-anchored edit operation, parsed from the `edits` array.
struct PatchOp {
    kind: PatchKind,
    /// 1-based anchor line.
    line: usize,
    /// 1-based inclusive end line (range ops only; == `line` otherwise).
    end_line: usize,
    text: String,
    /// Position in the request, for stable ordering of inserts at the same anchor.
    idx: usize,
}

enum PatchKind {
    InsertBefore,
    InsertAfter,
    ReplaceRange,
    DeleteRange,
}

/// Split provided edit text into lines without endings (normalizing CRLF, dropping one trailing NL).
fn text_lines(text: &str) -> Vec<String> {
    let norm = text.replace("\r\n", "\n");
    let body = norm.strip_suffix('\n').unwrap_or(&norm);
    if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(String::from).collect()
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "patch".into(),
            description: "Apply several line-anchored edits to a file in one call. Each edit is \
                          `{op, line, end_line?, text?}` where op is insert_before, insert_after, \
                          replace_range, or delete_range. ALL line numbers refer to the ORIGINAL \
                          file (use `read`/numbered output to find them); overlapping edits are \
                          rejected. Returns a unified diff."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": {"type": "string", "enum": ["insert_before", "insert_after", "replace_range", "delete_range"]},
                                "line": {"type": "integer", "description": "1-based anchor line in the ORIGINAL file"},
                                "end_line": {"type": "integer", "description": "1-based inclusive end (range ops)"},
                                "text": {"type": "string", "description": "Text to insert/replace with"}
                            },
                            "required": ["op", "line"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: p.to_string(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = str_param(&params, "path", "patch")?;
        let edits_json = params
            .get("edits")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::Other("patch: `edits` must be an array".to_string()))?;
        guard_unchanged(ctx, path, true).await?;
        let content = ctx.system.read_file(path).await?;
        let crlf = content.contains("\r\n");
        let had_final_nl = content.ends_with('\n');
        let lines: Vec<String> = content.lines().map(str::to_string).collect();
        let total = lines.len();

        // Parse + validate every edit against ORIGINAL coordinates.
        let mut ops = Vec::with_capacity(edits_json.len());
        for (idx, e) in edits_json.iter().enumerate() {
            let op = e.get("op").and_then(|v| v.as_str()).unwrap_or("");
            let line = u64_arg(e, "line").unwrap_or(0) as usize;
            let end_line = u64_arg(e, "end_line").map(|n| n as usize).unwrap_or(line);
            let text = e
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = match op {
                "insert_before" => PatchKind::InsertBefore,
                "insert_after" => PatchKind::InsertAfter,
                "replace_range" => PatchKind::ReplaceRange,
                "delete_range" => PatchKind::DeleteRange,
                other => {
                    return Err(Error::Other(format!(
                        "patch: edit[{idx}] has unknown op `{other}`"
                    )))
                }
            };
            if line < 1 || line > total {
                return Err(Error::Other(format!(
                    "patch: edit[{idx}] line {line} out of range (file has {total} lines)"
                )));
            }
            if matches!(kind, PatchKind::ReplaceRange | PatchKind::DeleteRange)
                && (end_line < line || end_line > total)
            {
                return Err(Error::Other(format!(
                    "patch: edit[{idx}] end_line {end_line} out of range (line {line}, {total} lines)"
                )));
            }
            ops.push(PatchOp {
                kind,
                line,
                end_line,
                text,
                idx,
            });
        }

        // Conflict detection (all against ORIGINAL coords): ranges may not overlap, and an insert may
        // not target a line inside a modified range.
        let ranges: Vec<(usize, usize, usize)> = ops
            .iter()
            .filter(|o| matches!(o.kind, PatchKind::ReplaceRange | PatchKind::DeleteRange))
            .map(|o| (o.line, o.end_line, o.idx))
            .collect();
        for i in 0..ranges.len() {
            for j in (i + 1)..ranges.len() {
                let (s1, e1, a) = ranges[i];
                let (s2, e2, b) = ranges[j];
                if s1.max(s2) <= e1.min(e2) {
                    return Err(Error::Other(format!(
                        "patch: edit[{a}] and edit[{b}] modify overlapping line ranges"
                    )));
                }
            }
        }
        for o in &ops {
            if matches!(o.kind, PatchKind::InsertBefore | PatchKind::InsertAfter) {
                for (s, e, r) in &ranges {
                    if *s <= o.line && o.line <= *e {
                        return Err(Error::Other(format!(
                            "patch: edit[{}] inserts inside the range of edit[{r}]",
                            o.idx
                        )));
                    }
                }
            }
        }

        // Apply: build the output from the ORIGINAL lines, emitting inserts/replacements at their
        // original positions in a single pass.
        #[derive(Clone)]
        enum Status {
            Normal,
            Skip,
            Replace(Vec<String>),
        }
        let mut before: Vec<Vec<String>> = vec![Vec::new(); total];
        let mut after: Vec<Vec<String>> = vec![Vec::new(); total];
        let mut status: Vec<Status> = vec![Status::Normal; total];
        for o in &ops {
            let li = o.line - 1;
            match o.kind {
                PatchKind::InsertBefore => before[li].extend(text_lines(&o.text)),
                PatchKind::InsertAfter => after[li].extend(text_lines(&o.text)),
                PatchKind::ReplaceRange => {
                    status[li] = Status::Replace(text_lines(&o.text));
                    // 0-based indices line..=end_line-1 are subsumed by the replacement.
                    for s in &mut status[o.line..o.end_line] {
                        *s = Status::Skip;
                    }
                }
                PatchKind::DeleteRange => {
                    for s in &mut status[(o.line - 1)..o.end_line] {
                        *s = Status::Skip;
                    }
                }
            }
        }
        let mut out: Vec<String> = Vec::new();
        for idx in 0..total {
            out.append(&mut before[idx].clone());
            match &status[idx] {
                Status::Normal => out.push(lines[idx].clone()),
                Status::Replace(t) => out.extend(t.clone()),
                Status::Skip => {}
            }
            out.append(&mut after[idx].clone());
        }
        let ending = if crlf { "\r\n" } else { "\n" };
        let mut updated = out.join(ending);
        if had_final_nl && !updated.is_empty() {
            updated.push_str(ending);
        }

        ctx.system.write_file(path, &updated).await?;
        note_read(ctx, path).await;
        let status_line = format!("patched {path} ({} edits)", ops.len());
        Ok(edit_result(
            status_line,
            &unified_diff(path, &content, &updated),
        ))
    }
}

// ---------------------------------------------------------------------------
// git_stage
// ---------------------------------------------------------------------------

pub struct GitStageTool;

#[async_trait]
impl Tool for GitStageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_stage".into(),
            description: "Stage specific workspace files for the next git commit (`git add`). \
                          Pass a list of workspace-relative paths."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Workspace-relative paths to stage"
                    }
                },
                "required": ["paths"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        path_list(params)
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        for p in path_list(params) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemRead,
                target: IntentTarget::Path { path: p },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let paths = path_list(&params);
        if paths.is_empty() {
            return Err(Error::Other(
                "git_stage: `paths` must be a non-empty array of strings".to_string(),
            ));
        }
        let mut argv = vec!["git".to_string(), "add".to_string(), "--".to_string()];
        argv.extend(paths);
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git add failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "staged".to_string()
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// git_commit
// ---------------------------------------------------------------------------

pub struct GitCommitTool;

#[async_trait]
impl Tool for GitCommitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_commit".into(),
            description: "Create a git commit with the staged changes. `message` is the commit \
                          title (required); `body` is an optional multi-line description appended \
                          after a blank line."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string", "description": "Commit title"},
                    "body": {"type": "string", "description": "Optional commit body (appended after a blank line)"}
                },
                "required": ["message"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_commit".to_string()]
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        let msg = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: format!("git commit -m {msg:?}"),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let message = str_param(&params, "message", "git_commit")?;
        let full_message = match params.get("body").and_then(|v| v.as_str()) {
            Some(b) if !b.trim().is_empty() => format!("{message}\n\n{b}"),
            _ => message.to_string(),
        };
        let argv = vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            full_message,
        ];
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git commit failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(body))
    }
}

// ---------------------------------------------------------------------------
// git_status
// ---------------------------------------------------------------------------

pub struct GitStatusTool;

#[async_trait]
impl Tool for GitStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".into(),
            description:
                "Show the working tree status (like `git status --short`). Returns a list \
                          of modified, staged, and untracked files."
                    .into(),
            input_schema: json!({"type": "object", "properties": {}}),
            output_schema: None,
            effects: vec![Effect::Process],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_status".to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git status --short".to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let argv = vec![
            "git".to_string(),
            "status".to_string(),
            "--short".to_string(),
        ];
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git status failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "nothing to commit, working tree clean".to_string()
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// git_diff
// ---------------------------------------------------------------------------

pub struct GitDiffTool;

#[async_trait]
impl Tool for GitDiffTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_diff".into(),
            description: "Show unstaged changes (or staged changes with `staged: true`). Optional \
                          `path` restricts the diff to a specific file."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Restrict diff to this file (optional)"},
                    "staged": {"type": "boolean", "description": "Show staged (index) diff instead of unstaged"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_else(|| vec!["git_diff".to_string()])
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git diff".to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let staged = params
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut argv = vec!["git".to_string(), "diff".to_string()];
        if staged {
            argv.push("--staged".to_string());
        }
        if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
            argv.push("--".to_string());
            argv.push(p.to_string());
        }
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git diff failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "no changes".to_string()
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// git_log
// ---------------------------------------------------------------------------

pub struct GitLogTool;

#[async_trait]
impl Tool for GitLogTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_log".into(),
            description:
                "Show recent commits (hash + subject). Optional `limit` controls how many \
                          entries are returned (default 10)."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "Number of commits to show (default 10)"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_log".to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git log".to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let limit = u64_arg(&params, "limit").unwrap_or(10);
        let argv = vec![
            "git".to_string(),
            "log".to_string(),
            format!("-{limit}"),
            "--oneline".to_string(),
        ];
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git log failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "no commits".to_string()
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// git_push
// ---------------------------------------------------------------------------

pub struct GitPushTool;

#[async_trait]
impl Tool for GitPushTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_push".into(),
            description: "Push the current branch to its upstream remote. Optional `remote` \
                          (default `origin`) and `branch` (default current branch)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "remote": {"type": "string", "description": "Remote name (default `origin`)"},
                    "branch": {"type": "string", "description": "Branch to push (default current branch)"}
                }
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::Network],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_push".to_string()]
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let remote = params
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin");
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: format!("git push {remote}"),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let remote = params
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin")
            .to_string();
        let mut argv = vec!["git".to_string(), "push".to_string(), remote];
        if let Some(b) = params.get("branch").and_then(|v| v.as_str()) {
            argv.push(b.to_string());
        }
        let out = ctx.system.run(&argv, Duration::from_secs(60)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git push failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "pushed".to_string()
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// git_checkout
// ---------------------------------------------------------------------------

pub struct GitCheckoutTool;

#[async_trait]
impl Tool for GitCheckoutTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_checkout".into(),
            description: "Switch to a branch or create a new one. Set `create: true` to create \
                          the branch (equivalent to `git checkout -b`)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "branch": {"type": "string", "description": "Branch name to switch to or create"},
                    "create": {"type": "boolean", "description": "Create the branch if it doesn't exist"}
                },
                "required": ["branch"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_checkout".to_string()]
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let branch = params.get("branch").and_then(|v| v.as_str()).unwrap_or("");
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: format!("git checkout {branch}"),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let branch = str_param(&params, "branch", "git_checkout")?;
        let create = params
            .get("create")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut argv = vec!["git".to_string(), "checkout".to_string()];
        if create {
            argv.push("-b".to_string());
        }
        argv.push(branch.to_string());
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git checkout failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            format!("switched to {branch}")
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// git_unstage
// ---------------------------------------------------------------------------

pub struct GitUnstageTool;

#[async_trait]
impl Tool for GitUnstageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_unstage".into(),
            description: "Remove files from the git index (unstage) without losing working-tree \
                          changes. `paths` is a list of workspace-relative paths to unstage."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Files to unstage"
                    }
                },
                "required": ["paths"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        path_list(params)
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        for p in path_list(params) {
            set.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process {
                    command: format!("git restore --staged {p}"),
                },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let paths = path_list(&params);
        if paths.is_empty() {
            return Err(Error::Other(
                "git_unstage: `paths` must be a non-empty array".to_string(),
            ));
        }
        let mut argv = vec![
            "git".to_string(),
            "restore".to_string(),
            "--staged".to_string(),
        ];
        argv.extend(paths);
        let out = ctx.system.run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git unstage failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            "unstaged".to_string()
        } else {
            body
        }))
    }
}

// ---------------------------------------------------------------------------
// flux_reload (dev mode only)
// ---------------------------------------------------------------------------

/// `flux_reload` — recompile flux-cli and replace the current process (dev mode only).
///
/// Safety: this tool is only registered when `--dev` is active. It runs `cargo build -p flux-cli`
/// synchronously (via the guarded system), and on success replaces the current process with
/// `execv` using the original argv + `--resume`. On build failure it returns an error and
/// the session continues uninterrupted.
pub struct ReloadTool;

#[async_trait]
impl Tool for ReloadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flux_reload".into(),
            description: "Recompile flux-cli and hot-reload: replaces the current process with \
                          the freshly built binary, resuming the session. Dev mode only. Call \
                          this when you want to apply code changes without losing session state."
                .into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            output_schema: None,
            effects: vec![Effect::Process],
            risk: flux_spec::Risk::High,
            idempotency: flux_spec::Idempotency::NonIdempotent,
            access: vec![flux_spec::AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["flux_reload".to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "cargo build -p flux-cli".to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> flux_core::Result<ToolResult> {
        // Run `cargo build -p flux-cli` via the guarded system (fixed argv — not model input).
        let argv = [
            "cargo".to_string(),
            "build".to_string(),
            "-p".to_string(),
            "flux-cli".to_string(),
        ];
        let out = ctx
            .system
            .run(&argv, Duration::from_secs(300))
            .await
            .map_err(|e| {
                flux_core::Error::Other(format!("build failed. refusing to reload: {e}"))
            })?;

        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "build failed. refusing to reload:\n{}",
                out.stderr.trim()
            )));
        }

        // Build succeeded — replace the current process via execv.
        // Collect original argv, appending `--resume` if neither --resume nor -c is already present.
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "cannot locate current executable: {e}"
                )))
            }
        };
        let mut args: Vec<String> = std::env::args().collect();
        if !args
            .iter()
            .any(|a| a == "--resume" || a == "-c" || a == "--continue")
        {
            args.push("--resume".to_string());
        }

        // exec() replaces the process image (execv under the hood). Only returns on failure.
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args[1..]).exec();
        Ok(ToolResult::error(format!("execv failed: {err}")))
    }
}

/// Register extra tools available only in `--dev` mode.
pub fn register_dev_builtins(registry: &mut flux_runtime::ToolRegistry) {
    registry.register(std::sync::Arc::new(ReloadTool));
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::{System, Workspace};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn ctx() -> (std::path::PathBuf, ToolContext) {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-tools-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
        (dir, c)
    }

    #[tokio::test]
    async fn edit_tolerates_trailing_whitespace() {
        let (dir, c) = ctx();
        // The file's first line has trailing spaces the model won't reproduce in `old_string`.
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "fn main() {   \n    let x = 1;\n}\n"}),
            )
            .await
            .unwrap();
        let r = EditTool
            .execute(
                &c,
                json!({
                    "path": "a.rs",
                    "old_string": "fn main() {\n    let x = 1;",
                    "new_string": "fn main() {\n    let x = 2;"
                }),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "flexible edit should succeed: {}", r.content);
        let after = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        assert!(after.content.contains("let x = 2;"));
        assert!(after.content.ends_with("}\n"), "structure preserved");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grep_searches_a_single_file_path() {
        // Regression (dogfood F1): grep/glob scoped to a *file* path used to return "no matches"
        // because the underlying walk only ever `read_dir`'d the base (which errors on a file). A
        // file `path` must search that file.
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.rs", "content": "fn needle() {}\n"}))
            .await
            .unwrap();
        WriteTool
            .execute(&c, json!({"path": "b.rs", "content": "fn other() {}\n"}))
            .await
            .unwrap();
        // Scoped to the single file a.rs → must find the match.
        let hit = GrepTool
            .execute(&c, json!({"pattern": "needle", "path": "a.rs"}))
            .await
            .unwrap();
        assert!(!hit.is_error);
        assert!(
            hit.content.contains("a.rs:1:") && hit.content.contains("needle"),
            "grep on a file path must find the match, got: {:?}",
            hit.content
        );
        // A file path that lacks the pattern → a genuine "no matches" (not a false negative).
        let none = GrepTool
            .execute(&c, json!({"pattern": "needle", "path": "b.rs"}))
            .await
            .unwrap();
        assert_eq!(none.content, "no matches");
        // glob scoped to a single file lists exactly that file.
        let g = GlobTool
            .execute(&c, json!({"pattern": "*", "path": "a.rs"}))
            .await
            .unwrap();
        assert_eq!(g.content.trim(), "a.rs");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_preserves_crlf_line_endings() {
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "fn main() {\r\n    let x = 1;\r\n}\r\n"}),
            )
            .await
            .unwrap();
        // The model sends an LF old_string/new_string (it doesn't reproduce \r).
        EditTool
            .execute(
                &c,
                json!({
                    "path": "a.rs",
                    "old_string": "fn main() {\n    let x = 1;",
                    "new_string": "fn main() {\n    let y = 9;"
                }),
            )
            .await
            .unwrap();
        let after = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        assert!(after.content.contains("let y = 9;"));
        // Every newline is still part of a CRLF — no bare LF introduced into the CRLF file.
        assert_eq!(
            after.content.matches('\n').count(),
            after.content.matches("\r\n").count(),
            "edit must not introduce bare LFs into a CRLF file: {:?}",
            after.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_reports_occurrence_lines_when_ambiguous() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "x\nfoo\ny\nfoo\n"}))
            .await
            .unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "foo", "new_string": "bar"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("lines 2, 4"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_trim_all_ws_matches_reindented_old() {
        let (dir, c) = ctx();
        // File is tab-indented; the model's old/new use spaces. Previously this hard-errored; now the
        // indentation strategy recovers it AND re-bases the replacement onto the file's tab indent.
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "\tlet x = 1;\n\tlet y = 2;\n"}),
            )
            .await
            .unwrap();
        let r = EditTool
            .execute(
                &c,
                json!({
                    "path": "a.rs",
                    "old_string": "    let x = 1;",
                    "new_string": "    let x = 42;"
                }),
            )
            .await
            .unwrap();
        assert!(
            !r.is_error,
            "indentation strategy should apply: {}",
            r.content
        );
        assert!(r.content.contains("matched via indentation"));
        let after = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        // The new line keeps the file's TAB indentation, not the model's spaces.
        assert_eq!(
            after.content, "\tlet x = 42;\n\tlet y = 2;\n",
            "got: {:?}",
            after.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_block_anchor_recovers_middle_drift() {
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "fn f() {\n    a();\n    b();\n    c();\n}\n"}),
            )
            .await
            .unwrap();
        // First & last lines are right; the middle is paraphrased/wrong — block-anchor recovers it.
        let r = EditTool
            .execute(
                &c,
                json!({
                    "path": "a.rs",
                    "old_string": "fn f() {\n    WRONG_MIDDLE();\n    ALSO_WRONG();\n    c();\n}",
                    "new_string": "fn f() {\n    z();\n}"
                }),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "block-anchor should apply: {}", r.content);
        assert!(r.content.contains("matched via block-anchor"));
        let after = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        assert!(after.content.contains("z();") && !after.content.contains("a();"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_block_anchor_refuses_when_not_unique() {
        let (dir, c) = ctx();
        // Two ≥3-line blocks share the same first & last line — anchoring is ambiguous, so refuse.
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "if x {\n    p();\n}\nif x {\n    q();\n}\n"}),
            )
            .await
            .unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({
                    "path": "a.rs",
                    "old_string": "if x {\n    DRIFT();\n}",
                    "new_string": "if x {\n    z();\n}"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ambiguous"), "got: {err}");
        // File must be untouched on an ambiguous refusal.
        let after = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        assert!(after.content.contains("p();") && after.content.contains("q();"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_not_found_hints_on_present_text() {
        let (dir, c) = ctx();
        // A multi-line old whose first line's text exists (indented differently) but whose body does
        // not match any window → not-found with the indentation hint.
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "\tfn foo() {\n\t\treturn 1;\n\t}\n"}),
            )
            .await
            .unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({
                    "path": "a.rs",
                    "old_string": "    fn foo() {\n        return 999;",
                    "new_string": "x"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("indentation differs"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_refuses_without_prior_read() {
        let (dir, c) = ctx();
        // File created out-of-band (not via the tools), so its content was never "seen" this session.
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "hello", "new_string": "world"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be read before editing"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_refuses_when_file_changed_since_read() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "hello\n"}))
            .await
            .unwrap();
        ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        // Simulate the file having been read long ago and changed on disk since.
        c.read_times
            .lock()
            .unwrap()
            .insert("a.txt".to_string(), std::time::SystemTime::UNIX_EPOCH);
        let err = EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "hello", "new_string": "world"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed on disk"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_after_read_succeeds() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "hello\n"}))
            .await
            .unwrap();
        ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        let r = EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "hello", "new_string": "world"}),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let after = ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        assert!(after.content.contains("world"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_after_read_with_dot_slash_path_ok() {
        // The read-set key is normalized, so reading `f.txt` and editing `./f.txt` (same file, different
        // spelling) must NOT trip the "read it first" guard.
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "f.txt", "content": "hello\n"}))
            .await
            .unwrap();
        ReadTool
            .execute(&c, json!({"path": "f.txt"}))
            .await
            .unwrap();
        let r = EditTool
            .execute(
                &c,
                json!({"path": "./f.txt", "old_string": "hello", "new_string": "world"}),
            )
            .await
            .unwrap();
        assert!(
            !r.is_error,
            "re-spelled path should resolve to the same key: {}",
            r.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_many_guards_large_file() {
        // A survey must not dump an over-cap file — it returns guidance instead.
        let (dir, c) = ctx();
        let big = "x\n".repeat(READ_LINE_CAP + 1);
        WriteTool
            .execute(&c, json!({"path": "big.txt", "content": big}))
            .await
            .unwrap();
        let r = ReadManyTool
            .execute(&c, json!({"paths": ["big.txt"]}))
            .await
            .unwrap();
        assert!(
            r.content.contains("too large to survey"),
            "expected guidance, got: {}",
            r.content
        );
        assert!(
            !r.view().contains("\nx\nx\nx\n"),
            "should not dump the file body"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fuzzy_edit_empty_new_deletes_line() {
        // A fuzzy (indentation-drift) match with an empty `new_string` deletes the line cleanly —
        // no blank line left behind.
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "fn f() {\n\tlet x = 1;\n}\n"}),
            )
            .await
            .unwrap();
        let r = EditTool
            .execute(
                &c,
                // 4-space indent won't match the file's tab exactly → fuzzy TrimAllWs path.
                json!({"path": "a.rs", "old_string": "    let x = 1;", "new_string": ""}),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let after = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        assert_eq!(
            after.content, "fn f() {\n}\n",
            "line removed, no blank line"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_view_includes_diff() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "alpha\nbeta\n"}))
            .await
            .unwrap();
        let r = EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "beta", "new_string": "gamma"}),
            )
            .await
            .unwrap();
        // Canonical content is the short status (clean); the view carries the unified diff.
        assert!(r.content.starts_with("edited a.txt"));
        let view = r.view.expect("edit attaches a diff view");
        assert!(
            view.contains("-beta") && view.contains("+gamma"),
            "got: {view}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_view_includes_diff_on_overwrite() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "one\ntwo\n"}))
            .await
            .unwrap();
        let r = WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "one\nTWO\n"}))
            .await
            .unwrap();
        let view = r.view.expect("overwrite attaches a diff view");
        assert!(
            view.contains("-two") && view.contains("+TWO"),
            "got: {view}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_new_file_diff_all_additions() {
        let (dir, c) = ctx();
        let r = WriteTool
            .execute(&c, json!({"path": "new.txt", "content": "x\ny\n"}))
            .await
            .unwrap();
        // A brand-new file diffs against empty → all-additions; status still leads.
        assert!(r.content.starts_with("wrote"));
        if let Some(view) = r.view {
            assert!(view.contains("+x") && view.contains("+y"), "got: {view}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_with_overflowing_offset_limit_does_not_panic() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "l1\nl2\nl3\n"}))
            .await
            .unwrap();
        // Attacker-supplied offset/limit near usize::MAX must not overflow-panic.
        let r = ReadTool
            .execute(
                &c,
                json!({"path": "a.txt", "offset": u64::MAX, "limit": u64::MAX}),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.is_empty(), "offset past EOF yields no lines");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_read_edit_roundtrip() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "line1\nline2\n"}))
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        assert_eq!(r.content, "line1\nline2\n");

        EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "line2", "new_string": "LINE2"}),
            )
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        assert!(r.content.contains("LINE2"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_refuses_binary_file() {
        let (dir, c) = ctx();
        // A file with an embedded NUL is binary — read must refuse with a clear, non-UTF-8 message.
        std::fs::write(dir.join("img.bin"), [b'P', b'N', b'G', 0u8, 1, 2, 3]).unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "img.bin"}))
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("binary"), "got: {}", r.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_large_file_returns_guidance_not_dump() {
        let (dir, c) = ctx();
        let big: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        WriteTool
            .execute(&c, json!({"path": "big.txt", "content": big}))
            .await
            .unwrap();
        // Unbounded read of an over-cap file → guidance, NOT the 3000 lines, and NOT an error.
        let r = ReadTool
            .execute(&c, json!({"path": "big.txt"}))
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("3000 lines"), "got: {}", r.content);
        assert!(r.content.contains("offset/limit"));
        assert!(!r.content.contains("line 2999"), "must not dump the file");
        // An explicit window of the same file returns the slice (numbered in the view).
        let w = ReadTool
            .execute(&c, json!({"path": "big.txt", "offset": 0, "limit": 5}))
            .await
            .unwrap();
        assert!(!w.is_error);
        assert!(w.content.contains("line 0") && w.content.contains("line 4"));
        assert!(!w.content.contains("line 5"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_view_has_line_numbers_content_is_raw() {
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "fn a() {}\nfn b() {}\n"}),
            )
            .await
            .unwrap();
        let r = ReadTool.execute(&c, json!({"path": "a.rs"})).await.unwrap();
        // Canonical content = raw bytes (clean to interpolate): no line-number/TAB prefixes.
        assert_eq!(r.content, "fn a() {}\nfn b() {}\n");
        // The model-facing view IS line-numbered.
        let view = r.view.expect("read sets a numbered view");
        assert!(
            view.contains("1\tfn a()") && view.contains("2\tfn b()"),
            "got: {view}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_offset_limit() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "n.txt", "content": "a\nb\nc\nd"}))
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "n.txt", "offset": 1, "limit": 2}))
            .await
            .unwrap();
        assert_eq!(r.content, "b\nc");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_coerces_string_offset_limit() {
        // LLMs often emit offset/limit as strings ("1"); they must be honored, not silently dropped
        // (which would fall through to an unbounded read / the large-file guard).
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "n.txt", "content": "a\nb\nc\nd"}))
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "n.txt", "offset": "1", "limit": "2"}))
            .await
            .unwrap();
        assert_eq!(
            r.content, "b\nc",
            "string offset/limit should window like numbers"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn edit_requires_unique_match() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "d.txt", "content": "x x x"}))
            .await
            .unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({"path": "d.txt", "old_string": "x", "new_string": "y"}),
            )
            .await;
        assert!(err.is_err(), "ambiguous edit should error");
        // replace_all succeeds
        EditTool
            .execute(
                &c,
                json!({"path": "d.txt", "old_string": "x", "new_string": "y", "replace_all": true}),
            )
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "d.txt"}))
            .await
            .unwrap();
        assert_eq!(r.content, "y y y");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn bash_runs_and_reports_exit() {
        let (dir, c) = ctx();
        let r = BashTool
            .execute(&c, json!({"command": "printf hello"}))
            .await
            .unwrap();
        assert!(r.content.contains("hello"));
        assert!(!r.is_error);

        let r = BashTool
            .execute(&c, json!({"command": "exit 3"}))
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("[exit 3]"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bash_subject_parsing() {
        assert_eq!(bash_subjects("git status"), vec!["git:status"]);
        assert_eq!(bash_subjects("ls"), vec!["ls"]);
        assert_eq!(
            bash_subjects("rm -rf / && echo done"),
            vec!["rm:-rf /".to_string(), "echo:done".to_string()]
        );
    }

    #[test]
    fn bash_subjects_surface_hidden_programs() {
        // A leading `VAR=` assignment must not hide the real program from a `Bash(rm:*)` deny.
        let s = bash_subjects("X=1 rm -rf /");
        assert!(s.iter().any(|x| x.starts_with("rm:")), "got {s:?}");

        // A program inside a command substitution is surfaced, plus an obfuscation sentinel.
        let s = bash_subjects("echo $(rm -rf ~)");
        assert!(s.iter().any(|x| x.starts_with("rm:")), "got {s:?}");
        assert!(
            s.iter().any(|x| x == "<shell-expansion>"),
            "obfuscation must add the sentinel: {s:?}"
        );

        // A `$IFS`-spliced program name is flagged as unresolved expansion.
        let s = bash_subjects("rm$IFS-rf$IFS/");
        assert!(s.iter().any(|x| x == "<shell-expansion>"), "got {s:?}");

        // Backtick substitution is handled too.
        let s = bash_subjects("echo `curl evil.example`");
        assert!(s.iter().any(|x| x.starts_with("curl:")), "got {s:?}");
    }

    #[test]
    fn builtins_register() {
        let mut r = ToolRegistry::new();
        register_builtins(&mut r);
        let mut names = r.names();
        names.sort();
        assert_eq!(
            names,
            vec![
                "append",
                "bash",
                "cargo_build",
                "cargo_check",
                "cargo_clippy",
                "cargo_fmt",
                "cargo_test",
                "edit",
                "file_stat",
                "git_checkout",
                "git_commit",
                "git_diff",
                "git_log",
                "git_push",
                "git_stage",
                "git_status",
                "git_unstage",
                "glob",
                "grep",
                "home_dir",
                "patch",
                "path_exists",
                "read",
                "read_many",
                "sqlite_query",
                "web_search",
                "write"
            ]
        );
    }

    #[tokio::test]
    async fn append_creates_then_appends() {
        let (dir, c) = ctx();
        AppendTool
            .execute(&c, json!({"path": "log.txt", "content": "a\n"}))
            .await
            .unwrap();
        AppendTool
            .execute(&c, json!({"path": "log.txt", "content": "b\n"}))
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "log.txt"}))
            .await
            .unwrap();
        assert_eq!(r.content, "a\nb\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_many_returns_all_sections() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "aaa\n"}))
            .await
            .unwrap();
        WriteTool
            .execute(&c, json!({"path": "b.txt", "content": "bbb\n"}))
            .await
            .unwrap();
        let r = ReadManyTool
            .execute(&c, json!({"paths": ["a.txt", "b.txt", "missing.txt"]}))
            .await
            .unwrap();
        assert!(r.content.contains("==> a.txt <==") && r.content.contains("aaa"));
        assert!(r.content.contains("==> b.txt <==") && r.content.contains("bbb"));
        // A missing path shows an error section but does not fail the whole call.
        assert!(r.content.contains("==> missing.txt <== (error"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn patch_applies_against_original_coords() {
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "a.txt", "content": "one\ntwo\nthree\nfour\n"}),
            )
            .await
            .unwrap();
        // insert after line 1 + replace lines 3..3 — both resolved against the ORIGINAL line numbers.
        let r = PatchTool
            .execute(
                &c,
                json!({
                    "path": "a.txt",
                    "edits": [
                        {"op": "insert_after", "line": 1, "text": "ONE-AND-A-HALF"},
                        {"op": "replace_range", "line": 3, "end_line": 3, "text": "THREE!"}
                    ]
                }),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let after = ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        assert_eq!(
            after.content, "one\nONE-AND-A-HALF\ntwo\nTHREE!\nfour\n",
            "got: {:?}",
            after.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn patch_rejects_overlapping_edits() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "1\n2\n3\n4\n"}))
            .await
            .unwrap();
        let err = PatchTool
            .execute(
                &c,
                json!({
                    "path": "a.txt",
                    "edits": [
                        {"op": "replace_range", "line": 1, "end_line": 3, "text": "x"},
                        {"op": "delete_range", "line": 2, "end_line": 2}
                    ]
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("overlapping"), "got: {err}");
        // File untouched on a conflict.
        let after = ReadTool
            .execute(&c, json!({"path": "a.txt"}))
            .await
            .unwrap();
        assert_eq!(after.content, "1\n2\n3\n4\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn patch_rejects_out_of_range_line() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "1\n2\n"}))
            .await
            .unwrap();
        let err = PatchTool
            .execute(
                &c,
                json!({"path": "a.txt", "edits": [{"op": "insert_after", "line": 9, "text": "x"}]}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("out of range"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn glob_matches_by_pattern() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "src/main.rs", "content": "fn main(){}"}))
            .await
            .unwrap();
        WriteTool
            .execute(&c, json!({"path": "src/lib.rs", "content": "//lib"}))
            .await
            .unwrap();
        WriteTool
            .execute(&c, json!({"path": "README.md", "content": "# doc"}))
            .await
            .unwrap();

        let r = GlobTool
            .execute(&c, json!({"pattern": "*.rs"}))
            .await
            .unwrap();
        assert!(r.content.contains("src/main.rs"));
        assert!(r.content.contains("src/lib.rs"));
        assert!(!r.content.contains("README.md"));

        let none = GlobTool
            .execute(&c, json!({"pattern": "*.py"}))
            .await
            .unwrap();
        assert_eq!(none.content, "no files match");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grep_matches_by_regex() {
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "a.rs", "content": "fn handler() {}\n// not a fn here\nfn other() {}\n"}),
            )
            .await
            .unwrap();
        // Regex `fn \w+\(` matches the two fn definitions, not the prose line.
        let r = GrepTool
            .execute(&c, json!({"pattern": r"fn \w+\("}))
            .await
            .unwrap();
        assert!(
            r.content.contains("a.rs:1:") && r.content.contains("a.rs:3:"),
            "got: {}",
            r.content
        );
        assert!(!r.content.contains("a.rs:2:"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grep_literal_escape_hatch() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "a.b\naxb\n"}))
            .await
            .unwrap();
        // literal:true → `a.b` matches only the literal "a.b", not the regex-wildcard "axb".
        let r = GrepTool
            .execute(&c, json!({"pattern": "a.b", "literal": true}))
            .await
            .unwrap();
        assert!(r.content.contains("a.txt:1:"));
        assert!(
            !r.content.contains("a.txt:2:"),
            "literal must not match axb: {}",
            r.content
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grep_invalid_regex_errors_cleanly() {
        let (dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "x\n"}))
            .await
            .unwrap();
        // An unbalanced group is a clean error, not a panic.
        let r = GrepTool.execute(&c, json!({"pattern": "("})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("invalid regex"), "got: {}", r.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grep_finds_lines_with_glob_filter() {
        let (dir, c) = ctx();
        WriteTool
            .execute(
                &c,
                json!({"path": "src/a.rs", "content": "let x = 1;\nfn target() {}\n"}),
            )
            .await
            .unwrap();
        WriteTool
            .execute(
                &c,
                json!({"path": "notes.txt", "content": "target in text\n"}),
            )
            .await
            .unwrap();

        // restricted to *.rs → only the rust hit
        let r = GrepTool
            .execute(&c, json!({"pattern": "target", "glob": "*.rs"}))
            .await
            .unwrap();
        assert!(r.content.contains("src/a.rs:2:"));
        assert!(!r.content.contains("notes.txt"));

        // unrestricted → both
        let all = GrepTool
            .execute(&c, json!({"pattern": "target"}))
            .await
            .unwrap();
        assert!(all.content.contains("src/a.rs"));
        assert!(all.content.contains("notes.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
