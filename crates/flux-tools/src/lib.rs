//! `flux-tools` — the built-in coding tools (`read`, `write`, `edit`, `bash`).
//!
//! Each implements [`flux_runtime::Tool`]: it declares its permission subjects (so rules and
//! approval can gate it), its [`ToolSpec`] (effects/risk), and its pre-execution [`IntentSet`],
//! and performs all IO through the guarded [`System`](flux_system::System). `bash` runs commands
//! via `sh -c` (an explicit, gated shell — `flux-system` itself never interprets argv as shell).

use std::time::Duration;

pub mod cargo;
pub mod cognition;
pub mod command_invoke;
pub mod evidence;
pub mod extra;
pub mod flows;
pub mod groups;
pub mod reflect;
pub mod render;
mod skill_load;
pub mod toolchains;
pub mod transform;

pub use command_invoke::{register_command_invoke, try_register_command_invoke};
pub use evidence::{install_evidence, register_evidence, try_register_evidence};
pub use flows::{
    register_flows, try_register_flows, ResolvedStoredFlow, StoredFlowCatalog, StoredFlowEntry,
    StoredFlowKind,
};
pub use reflect::{install_reflect, register_reflect, try_register_reflect};
pub use render::{register_render, try_register_render};

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_policy::wildcard_match;
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};

use flux_spec::{
    tool_input_schema, AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty,
    IntentRole, IntentSet, IntentTarget, Risk, ToolSpec,
};
use std::sync::Arc;

const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
/// Upper bound on files visited by `glob`/`grep` before stopping (cost guard).
const WALK_FILE_CAP: usize = 10_000;
const DEFAULT_GLOB_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 200;
/// Per-file scan cap for `grep`: at most this many bytes of any single file are line-scanned, so one
/// giant matched file (a generated bundle, a data dump) can't dominate a search.
const GREP_FILE_BYTE_CAP: usize = 2 * 1024 * 1024;
/// An unbounded `read` (no explicit offset/limit) over these caps returns guidance instead of dumping.
const READ_LINE_CAP: usize = 2000;
const READ_BYTE_CAP: usize = 256 * 1024;
/// Hard ceiling on bytes any single `read` will materialize, even for an explicit offset/limit
/// window — far above `READ_BYTE_CAP` so legitimate large source/log files still page, but bounded so
/// a multi-GB file (or an endless FIFO) can't OOM/hang the host (C-79).
const MAX_READ_FILE_BYTES: usize = 16 * 1024 * 1024;
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

/// Deserialize an op's JSON arguments into its typed input struct — the single source of truth
/// paired with the `schemars`-derived `input_schema`. Maps a serde error to the op-error style.
pub(crate) fn parse_params<T: serde::de::DeserializeOwned>(params: Value, tool: &str) -> Result<T> {
    serde_json::from_value(params)
        .map_err(|e| Error::Other(format!("{tool}: invalid arguments: {e}")))
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

/// Actionable guidance for a `read` that landed on a directory (C-32) — a weak model routinely
/// `read()`s a directory; the raw `Is a directory` io error used to propagate via `?` and halt the
/// plan node. Shared by the single-file and `read_section` (windowed/multi-file) paths so both give
/// the model the same repairable failure instead of a fatal one.
fn directory_read_guidance(path: &str) -> String {
    format!("`{path}` is a directory — list it with glob(\"{path}/**/*\") first, then read specific files")
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
    if let Ok(m) = ctx.system().file_mtime(path).await {
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
            if let Ok(now) = ctx.system().file_mtime(path).await {
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
pub fn try_register_builtins(registry: &mut ToolRegistry) -> Result<()> {
    let mut assembled = registry.clone();
    cargo::try_register_cargo(&mut assembled)?;
    toolchains::try_register_toolchains(&mut assembled)?;
    extra::try_register_extra(&mut assembled)?;
    assembled.try_register_all_from(
        "flux-tools core coding pack",
        vec![
            Arc::new(ReadTool) as Arc<dyn Tool>,
            Arc::new(ReadManyTool),
            Arc::new(WriteTool),
            Arc::new(EditTool),
            Arc::new(PatchTool),
            Arc::new(AppendTool),
            Arc::new(BashTool),
            Arc::new(ProcRunTool),
            Arc::new(GlobTool),
            Arc::new(GrepTool),
            Arc::new(GitStageTool),
            Arc::new(GitCommitTool),
            Arc::new(GitStatusTool),
            Arc::new(GitDiffTool),
            Arc::new(GitLogTool),
            Arc::new(GitMergeTool),
            Arc::new(GitRevertTool),
            Arc::new(GitPushTool),
            Arc::new(GitCheckoutTool),
            Arc::new(GitBranchTool),
            Arc::new(GitUnstageTool),
            Arc::new(GitHunksTool),
            Arc::new(GitStageHunksTool),
            Arc::new(GitWorktreeEnterTool),
            Arc::new(GitWorktreeLeaveTool),
        ],
    )?;
    cognition::try_register_cognition(&mut assembled)?;
    // Evidence primitives (`observe`/`evidence`): general-purpose audit ops any flow may use to emit
    // and read its own runtime observations.
    evidence::try_register_evidence(&mut assembled)?;
    // `command.invoke` (D-187): agent-side invocation of a discovered, agent-triggerable command
    // or skill. Always registered; surfacing is gated by its `agent_invoke` group (`groups.rs`) on
    // the `agent_triggerable` evidence signal (`flux_runtime::detect_signals`).
    command_invoke::try_register_command_invoke(&mut assembled)?;
    // `skill.load` (D-188): on-demand skill-body loading for the opt-in model-invoked catalog.
    // Always registered; the engine's per-turn surfacing narrows it back out whenever no session
    // has a non-empty catalog installed, which is the default (`FlowEngine::narrow_by_skill_catalog`).
    skill_load::try_register_skill_load(&mut assembled)?;
    *registry = assembled;
    Ok(())
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_builtins`] and propagate collision diagnostics.
pub fn register_builtins(registry: &mut ToolRegistry) {
    try_register_builtins(registry).expect("flux-tools built-in registration failed");
}

// ---------------------------------------------------------------------------
// typed op input structs (schemars-derived input_schema — single source of truth
// for the model-facing schema; handlers keep ad-hoc &Value parsing, so the structs
// are schema-only and carry #[allow(dead_code)]).
// ---------------------------------------------------------------------------

/// A string or an array of strings (read's `path` accepts a glob/path or a list).
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum StringOrVec {
    Single(String),
    Many(Vec<String>),
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    /// A single workspace-relative path (string), an array of paths, or a glob pattern (string containing * or ?)
    path: StringOrVec,
    /// 0-based first line (single-file only)
    #[serde(default)]
    offset: Option<u64>,
    /// Max lines to return (single-file only)
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: Option<bool>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BashInput {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProcRunInput {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GlobInput {
    /// Glob, e.g. `*.rs` or `src/*`
    pattern: String,
    /// Literal existing subdirectory to search (default `.`); put wildcards in `pattern`, not here
    #[serde(default)]
    path: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GrepInput {
    /// Regex to find (or substring if `literal`)
    pattern: String,
    /// Treat `pattern` as a plain substring, not a regex
    #[serde(default)]
    literal: Option<bool>,
    /// Only search files matching this glob
    #[serde(default)]
    glob: Option<String>,
    /// Subdirectory to search (default `.`)
    #[serde(default)]
    path: Option<String>,
    /// Cap on matches (default 200)
    #[serde(default)]
    max_results: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AppendInput {
    path: String,
    content: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadManyInput {
    /// Workspace-relative paths to read
    paths: Vec<String>,
}

/// A patch edit operation.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PatchOpKind {
    InsertBefore,
    InsertAfter,
    ReplaceRange,
    DeleteRange,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PatchEdit {
    op: PatchOpKind,
    /// 1-based anchor line in the ORIGINAL file
    line: u64,
    /// 1-based inclusive end (range ops)
    #[serde(default)]
    end_line: Option<u64>,
    /// Text to insert/replace with
    #[serde(default)]
    text: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PatchInput {
    path: String,
    edits: Vec<PatchEdit>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitStageInput {
    /// Workspace-relative paths to stage
    paths: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitCommitInput {
    /// Commit title
    message: String,
    /// Optional commit body (appended after a blank line)
    #[serde(default)]
    body: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitDiffInput {
    /// Restrict diff to this file (optional)
    #[serde(default)]
    path: Option<String>,
    /// Show staged (index) diff instead of unstaged
    #[serde(default)]
    staged: Option<bool>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitLogInput {
    /// Number of commits to show (default 10)
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitPushInput {
    /// Remote name (default `origin`)
    #[serde(default)]
    remote: Option<String>,
    /// Branch to push (default current branch)
    #[serde(default)]
    branch: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitCheckoutInput {
    /// Branch name to switch to or create
    branch: String,
    /// Create the branch if it doesn't exist
    #[serde(default)]
    create: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitBranchInput {
    /// Branch name to create (or to delete with `delete: true`)
    name: String,
    /// Delete the branch instead of creating it (safe delete: git refuses unmerged work and the
    /// checked-out branch)
    #[serde(default)]
    delete: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitMergeInput {
    /// Branch or ref to merge into the current branch
    branch: String,
    /// Always create a merge commit (`--no-ff`), even when a fast-forward is possible — the
    /// integration loop's audit trail
    #[serde(default)]
    no_ff: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitRevertInput {
    /// Commit to revert (a full or abbreviated sha, or a ref)
    commit: String,
    /// Mainline parent (`git revert -m N`), required when reverting a merge commit — usually 1:
    /// the branch the merge landed on
    #[serde(default)]
    mainline: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitUnstageInput {
    /// Files to unstage
    paths: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitHunksInput {
    /// Workspace-relative path of the file whose unstaged hunks to list
    path: String,
    /// Lines of context around each change (default 3, minimum 1). A smaller radius splits
    /// adjacent changes into separate, individually stageable hunks. Zero is rejected: it leaves
    /// `git apply` no context to verify against, which can stage a hunk at the wrong position.
    context: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitStageHunksInput {
    /// Workspace-relative path of the file to stage hunks from
    path: String,
    /// Hunk ids to stage, exactly as `git_hunks` reported them (e.g. `h1-1a2b3c4d5e6f7a8b`)
    hunks: Vec<String>,
    /// Context radius used when the ids were listed — must match the `git_hunks` call they came
    /// from (default 3, minimum 1)
    context: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitStatusInput {}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitWorktreeEnterInput {}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitWorktreeLeaveInput {}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FluxReloadInput {}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "read",
            "Read one UTF-8 file, a list of files, or a glob pattern. \
             A string `path` with `*` or `?` is auto-expanded as a glob; an array reads each \
             listed file. Single-file reads return a line-numbered view; multi-file reads return \
             sections headed `==> path <==`. Optional `offset`/`limit` apply only to single-file \
             reads. Refuses binary files and, for a very large file read whole, returns guidance \
             to request a range instead.",
            tool_input_schema::<ReadInput>(),
        )
        .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        read_path_list(params)
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        for p in read_path_list(params) {
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
        // Resolve the `path` param into a list of concrete paths.
        let paths = resolve_read_paths(ctx, &params).await?;

        // Multi-file path: use the same `read_section` machinery as `read_many`.
        if paths.len() != 1 {
            if paths.is_empty() {
                return Err(Error::Other(
                    "read: glob pattern matched no files".to_string(),
                ));
            }
            let sections =
                futures::future::join_all(paths.iter().map(|p| read_section(ctx, p))).await;
            let mut canonical = Vec::with_capacity(sections.len());
            let mut view = Vec::with_capacity(sections.len());
            for (c, v) in sections {
                canonical.push(c);
                view.push(v);
            }
            return Ok(ToolResult::ok_view(
                canonical.join("\n\n"),
                view.join("\n\n"),
            ));
        }

        // Single-file path (offset/limit paging applies here only).
        let path = &paths[0];
        if ctx.system().is_dir(path).await? {
            return Ok(ToolResult::error(directory_read_guidance(path)));
        }
        let offset = u64_arg(&params, "offset").unwrap_or(0) as usize;
        let limit = u64_arg(&params, "limit").map(|n| n as usize);

        // Stat BEFORE materializing: an unbounded read of an over-cap file returns guidance without
        // ever slurping it, so a multi-GB file can't OOM the host (C-79). The windowed branch below
        // streams within `READ_BYTE_CAP`, and the bounded read guards the rest.
        let stat_size = ctx.system().file_size(path).await?;
        if offset == 0 && limit.is_none() && stat_size > READ_BYTE_CAP as u64 {
            return Ok(ToolResult::ok(format!(
                "{path} is {stat_size} bytes (over the {READ_BYTE_CAP}-byte read cap); read a range \
                 with offset/limit (e.g. offset:0, limit:{READ_LINE_CAP})"
            )));
        }

        // Bounded read: never materialize more than `MAX_READ_FILE_BYTES`, and reject non-regular
        // files (a FIFO/device would otherwise stream forever and hang the tool).
        let (bytes, _over_cap) = ctx
            .system()
            .read_file_bytes_capped(path, MAX_READ_FILE_BYTES)
            .await?;
        let total_bytes = bytes.len();
        let content = match decode_text(path, bytes) {
            Decoded::Text(s) => s,
            Decoded::Guard { message, is_error } => {
                return Ok(if is_error {
                    ToolResult::error(message)
                } else {
                    ToolResult::ok(message)
                });
            }
        };

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

        // Explicit window: honor it (the model chose the range). Stream the lines and take only the
        // requested window instead of collecting EVERY line of the file into a `Vec<&str>` first — a
        // ranged read of a huge file must not allocate a slice-per-line for the whole thing. Bound
        // the assembled window by `READ_BYTE_CAP` too: the unbounded branch above already caps, but
        // the windowed branch didn't, so a range spanning enormous lines could blow the budget.
        let take = limit.unwrap_or(usize::MAX);
        let mut slice = String::new();
        let mut byte_capped = false;
        for line in content.lines().skip(offset).take(take) {
            // +1 for the rejoining '\n' (matches the old `join("\n")`), except before the first line.
            let extra = line.len() + usize::from(!slice.is_empty());
            if slice.len().saturating_add(extra) > READ_BYTE_CAP {
                byte_capped = true;
                break;
            }
            if !slice.is_empty() {
                slice.push('\n');
            }
            slice.push_str(line);
        }
        note_read(ctx, path).await;
        // `saturating_add` — an attacker-supplied `offset` near usize::MAX must not overflow; a huge
        // offset yields an empty window anyway, so the numbering base is immaterial there.
        let mut view = number_lines(&slice, offset.saturating_add(1));
        if byte_capped {
            view.push_str(&format!(
                "\n…[read window truncated at {READ_BYTE_CAP} bytes — narrow offset/limit for the rest]"
            ));
        }
        Ok(ToolResult::ok_view(slice, view))
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

pub struct WriteTool;

/// Arguments for the `write` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    /// Workspace path of the file to create or overwrite.
    path: String,
    /// Full UTF-8 contents to write.
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write (create/overwrite) a UTF-8 file in the workspace.".into(),
            input_schema: tool_input_schema::<WriteInput>(),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Medium,
            // C-191: overwriting a path with the same content converges, so repeating is safe —
            // but the call really acts every time and its outcome depends on the file that is
            // there, not on the input alone. `Conditional` says that; `Idempotent` would license
            // a replay from cache in place of the write.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        serde_json::from_value::<WriteInput>(params.clone())
            .map(|a| vec![a.path])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Ok(a) = serde_json::from_value::<WriteInput>(params.clone()) {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path { path: a.path },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: WriteInput = parse_params(params, "write")?;
        let path = args.path.as_str();
        let content = args.content.as_str();
        // Soft guard: refuse only if we saw this file and it changed on disk since (don't clobber).
        guard_unchanged(ctx, path, false).await?;
        // Read prior content for a diff (a missing/binary file ⇒ empty `before` = all additions).
        let before = ctx.system().read_file(path).await.unwrap_or_default();
        ctx.system().write_file(path, content).await?;
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
            input_schema: tool_input_schema::<EditInput>(),
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
        let args: EditInput = parse_params(params, "edit")?;
        let path = args.path.as_str();
        let old = args.old_string.as_str();
        let new = args.new_string.as_str();
        let replace_all = args.replace_all.unwrap_or(false);

        // An empty `old_string` matches at every position: `content.replace("", new)` would splice
        // `new_string` between every character and destroy the file. Refuse it outright (C-85). To
        // create a file or replace its whole contents, `write` is the right tool.
        if old.is_empty() {
            return Err(Error::Other(format!(
                "edit: `old_string` must not be empty (that would insert `new_string` between every \
                 character of {path}); use `write` to create or fully replace a file"
            )));
        }

        // Must have read (or written) this file this session, and it must not have changed since.
        guard_unchanged(ctx, path, true).await?;
        let content = ctx.system().read_file(path).await?;
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
            ctx.system().write_file(path, &updated).await?;
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
                ctx.system().write_file(path, &updated).await?;
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
            description: "Run a shell command (via `sh -c`) in the workspace root. The generic \
                          escape hatch — off by default; opt in via the `shell` group (config \
                          `enable_shell = true` or `FLUX_ENABLE_BASH=1`). Prefer the dedicated ops. \
                          Gated by permission rules and approval."
                .into(),
            input_schema: tool_input_schema::<BashInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            // The `shell` group (off by default) owns bash so it is not a core always-advertised op.
            group: Some("shell".into()),
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
        // C-158: when a surface installed a live channel, report each output line as it lands so a
        // long command shows progress instead of a silent spinner. The reporter redacts (it binds
        // the same `Redactor` the final result is scrubbed with), and the captured `out` below is
        // byte-for-byte what the unobserved `run` would have returned — the model's view of this op
        // is unchanged, only the surface's.
        let out = match ctx.progress_reporter("bash") {
            Some(reporter) => {
                let observer: flux_system::OutputObserver =
                    std::sync::Arc::new(move |line: &str| reporter.report(line));
                ctx.system()
                    .run_observed(&argv, Duration::from_secs(timeout), observer)
                    .await?
            }
            None => {
                ctx.system()
                    .run(&argv, Duration::from_secs(timeout))
                    .await?
            }
        };
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
        } else if body.is_empty() {
            // A silent success must stay legible: an empty result is indistinguishable from
            // "nothing ran", and the loop re-plans the already-succeeded command (A-05).
            body.push_str("[exit 0] (no output)");
        }
        Ok(ToolResult {
            content: body,
            view: None,
            is_error: out.exit_code != 0,
        })
    }
}

// ---------------------------------------------------------------------------
// proc.run
// ---------------------------------------------------------------------------

pub struct ProcRunTool;

fn proc_args(params: &Value) -> Vec<String> {
    params
        .get("args")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn proc_subject(params: &Value) -> Option<String> {
    let program = params.get("program").and_then(|v| v.as_str())?;
    let args = proc_args(params);
    if args.is_empty() {
        Some(program.to_string())
    } else {
        Some(format!("{program}:{}", args.join(" ")))
    }
}

#[async_trait]
impl Tool for ProcRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "proc.run".into(),
            description: "Run one argv-only process in the workspace root. This is the preferred \
                          generic process escape hatch when a dedicated op does not exist: no shell \
                          parsing, env cleared by flux-system, output capped, approval-gated, and \
                          hidden by default behind the `shell` group."
                .into(),
            input_schema: tool_input_schema::<ProcRunInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: Some("shell".into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        proc_subject(params).into_iter().collect()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(subject) = proc_subject(params) {
            set.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process { command: subject },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let program = str_param(&params, "program", "proc.run")?;
        let timeout = u64_arg(&params, "timeout_secs").unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
        let mut argv = vec![program.to_string()];
        argv.extend(proc_args(&params));
        let out = ctx
            .system()
            .run(&argv, Duration::from_secs(timeout))
            .await?;
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
             `path` scopes the search to one literal existing subdirectory and must not contain \
             wildcards. Patterns match workspace-relative paths. To inventory the whole workspace, \
             use `pattern: \"*\"` and omit `path`.",
            tool_input_schema::<GlobInput>(),
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
        let args: GlobInput = parse_params(params, "glob")?;
        let pattern = args.pattern.as_str();
        let base = args.path.as_deref().unwrap_or(".");
        if base.contains('*') || base.contains('?') {
            return Ok(ToolResult::error(
                "glob: `path` is a literal directory, not a pattern. Put all wildcards in \
                 `pattern`; to inventory the whole workspace, call glob with `pattern: \"*\"` and \
                 omit `path`."
                    .to_string(),
            ));
        }
        let files = ctx.system().walk_files(base, WALK_FILE_CAP).await?;
        let mut matches: Vec<String> = files
            .into_iter()
            .filter(|f| wildcard_match(pattern, f))
            .collect();
        matches.truncate(DEFAULT_GLOB_LIMIT);
        if matches.is_empty() {
            return Ok(ToolResult::ok("no files match"));
        }
        // C-10: the canonical VALUE is a JSON array so list-consuming plan nodes (`each`, `merge`)
        // compose on a glob result; the model-facing view stays the readable joined lines.
        Ok(ToolResult::ok_view(
            serde_json::to_string(&matches)?,
            matches.join("\n"),
        ))
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
            tool_input_schema::<GrepInput>(),
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

        let files = ctx.system().walk_files(base, WALK_FILE_CAP).await?;
        let mut out = Vec::new();
        'files: for f in files {
            if let Some(g) = glob {
                if !wildcard_match(g, &f) {
                    continue;
                }
            }
            // Best-effort: skip binary/non-UTF-8/unreadable files rather than failing the search.
            let Ok(content) = ctx.system().read_file(&f).await else {
                continue;
            };
            // Per-file scan guard: bound the line-scan at GREP_FILE_BYTE_CAP so one huge matched file
            // (a generated bundle, a data dump) can't dominate the search. The overall `max`-hits cap
            // already bounds output; this bounds the work per file. Best-effort, like the skip above.
            let mut scanned = 0usize;
            for (i, line) in content.lines().enumerate() {
                scanned += line.len() + 1;
                if scanned > GREP_FILE_BYTE_CAP {
                    break;
                }
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
            input_schema: tool_input_schema::<AppendInput>(),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            // C-191: `append` mutates a workspace file exactly as `write` does, and `Risk` is the
            // approval tier, not a blast-radius rating — `Risk::Low` is what the risk approver
            // auto-approves and what the plan prompt shows a human. "Lower-risk than `write`"
            // (above) is true of the blast radius and was never true of the tier.
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
        let args: AppendInput = parse_params(params, "append")?;
        let path = args.path.as_str();
        let content = args.content.as_str();
        guard_unchanged(ctx, path, false).await?;
        ctx.system().append_file(path, content).await?;
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
    // C-32: a directory in the list gets the same repairable guidance as the single-file path,
    // scoped to its own section — it doesn't halt the other paths in the same call.
    match ctx.system().is_dir(path).await {
        Ok(true) => {
            let sec = format!("==> {path} <== ({})", directory_read_guidance(path));
            return (sec.clone(), sec);
        }
        Ok(false) => {}
        Err(e) => {
            let sec = format!("==> {path} <== (error: {e})");
            return (sec.clone(), sec);
        }
    }
    match ctx.system().read_file_bytes(path).await {
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
            "Read several known files in one operation (each section is headed `==> path <==`). \
             Prefer this over sequential `read` calls once multiple relevant paths are known.",
            tool_input_schema::<ReadManyInput>(),
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
        let args: ReadManyInput = parse_params(params, "read_many")?;
        if args.paths.is_empty() {
            return Err(Error::Other(
                "read_many: `paths` must be a non-empty array of strings".to_string(),
            ));
        }
        let system = ctx.system();
        let existence =
            futures::future::join_all(args.paths.iter().map(|path| system.path_exists(path))).await;
        let all_missing = existence.iter().all(|result| matches!(result, Ok(false)));
        let sections =
            futures::future::join_all(args.paths.iter().map(|p| read_section(ctx, p))).await;
        let mut canonical = Vec::with_capacity(sections.len());
        let mut view = Vec::with_capacity(sections.len());
        for (c, v) in sections {
            canonical.push(c);
            view.push(v);
        }
        if all_missing {
            let repair = "No requested path exists. Do not guess another filename. Discover the \
                          workspace once with glob using `pattern: \"*\"` and no `path`, then read \
                          the returned relevant paths together.";
            canonical.push(repair.to_string());
            view.push(repair.to_string());
        }
        Ok(ToolResult::ok_view(
            canonical.join("\n\n"),
            view.join("\n\n"),
        ))
    }
}

/// Resolve the `path` param of `ReadTool` into a list of concrete workspace paths.
/// Accepts three shapes:
///   - a single string without glob metacharacters → `[path]`
///   - a single string containing `*` or `?`        → glob-expanded list
///   - a JSON array of strings                      → the array as-is
async fn resolve_read_paths(ctx: &ToolContext, params: &Value) -> Result<Vec<String>> {
    match params.get("path") {
        Some(Value::Array(arr)) => {
            let paths = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Ok(paths)
        }
        Some(Value::String(s)) if s.contains('*') || s.contains('?') => {
            // Treat as a glob pattern: walk the workspace and filter by wildcard.
            let mut files = ctx
                .system()
                .walk_files(".", WALK_FILE_CAP)
                .await
                .unwrap_or_default();
            files.retain(|f| wildcard_match(s, f));
            files.truncate(DEFAULT_GLOB_LIMIT);
            Ok(files)
        }
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        _ => Err(Error::Other(
            "read: `path` must be a string or an array of strings".to_string(),
        )),
    }
}

/// Extract the `path` param of `ReadTool` as a flat string list (for permission_subjects /
/// intents — best-effort, glob patterns are left unexpanded at spec time).
fn read_path_list(params: &Value) -> Vec<String> {
    match params.get("path") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => vec![],
    }
}

/// Extract the `paths` string array from params (empty when absent/malformed).
/// The single `path` argument as a one-element subject list, falling back to the op's own name
/// when the parameter is absent or blank.
///
/// The fallback matters: a `Write`-effect op reporting **no** subjects is unscoped, and an unscoped
/// write matches a `*` path grant — so a malformed call must still name something the approval gate
/// can reason about, never an empty list (AGENTS.md, "`permission_subjects` must be accurate").
fn single_path(params: &Value, fallback: &str) -> Vec<String> {
    match params.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => vec![p.to_string()],
        _ => vec![fallback.to_string()],
    }
}

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
            input_schema: tool_input_schema::<PatchInput>(),
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
        let content = ctx.system().read_file(path).await?;
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

        ctx.system().write_file(path, &updated).await?;
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
            input_schema: tool_input_schema::<GitStageInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            // C-191: staging the same paths twice converges, so repeating is safe — but what lands
            // in the index is whatever the working tree holds at call time, not a function of the
            // input, so the result must never be replayed from a cache.
            idempotency: Idempotency::Conditional,
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
        let args: GitStageInput = parse_params(params, "git_stage")?;
        if args.paths.is_empty() {
            return Err(Error::Other(
                "git_stage: `paths` must be a non-empty array of strings".to_string(),
            ));
        }
        let mut argv = vec!["git".to_string(), "add".to_string(), "--".to_string()];
        argv.extend(args.paths);
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
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
            input_schema: tool_input_schema::<GitCommitInput>(),
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
        let args: GitCommitInput = parse_params(params, "git_commit")?;
        let message = args.message.as_str();
        let full_message = match args.body.as_deref() {
            Some(b) if !b.trim().is_empty() => format!("{message}\n\n{b}"),
            _ => message.to_string(),
        };
        let argv = vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            full_message,
        ];
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
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
            input_schema: tool_input_schema::<GitStatusInput>(),
            output_schema: None,
            effects: vec![Effect::Process],
            // Stays `Low` on the strength of an explicit I1 allowlist entry in
            // `flux_spec::coherence` — fixed argv, observation only (C-191).
            risk: Risk::Low,
            // C-191: the answer is the working tree's, not the input's — it changes under the
            // agent's own edits, so a cached replay would hand back a stale tree.
            idempotency: Idempotency::Conditional,
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

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _: GitStatusInput = parse_params(params, "git_status")?;
        let argv = vec![
            "git".to_string(),
            "status".to_string(),
            "--short".to_string(),
        ];
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
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
            input_schema: tool_input_schema::<GitDiffInput>(),
            output_schema: None,
            effects: vec![Effect::Process],
            // Stays `Low` on the strength of an explicit I1 allowlist entry in
            // `flux_spec::coherence` — fixed argv, observation only (C-191).
            risk: Risk::Low,
            // C-191: the diff tracks the working tree, not the input — see `git_status`.
            idempotency: Idempotency::Conditional,
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
        let args: GitDiffInput = parse_params(params, "git_diff")?;
        let staged = args.staged.unwrap_or(false);
        // Fix behaviour as well as argv: without `--no-ext-diff`, repository/global Git config can
        // redirect this low-risk read operation into an arbitrary external program (C-218).
        let mut argv = vec![
            "git".to_string(),
            "diff".to_string(),
            "--no-ext-diff".to_string(),
            "--no-textconv".to_string(),
        ];
        if staged {
            argv.push("--staged".to_string());
        }
        if let Some(p) = args.path.as_deref() {
            argv.push("--".to_string());
            argv.push(p.to_string());
        }
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
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
            input_schema: tool_input_schema::<GitLogInput>(),
            output_schema: None,
            effects: vec![Effect::Process],
            // Stays `Low` on the strength of an explicit I1 allowlist entry in
            // `flux_spec::coherence` — fixed argv, observation only (C-191).
            risk: Risk::Low,
            // C-191: the log tracks the repository's commits, not the input — see `git_status`.
            idempotency: Idempotency::Conditional,
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
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
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
// The guarded git family's tree preconditions — stated once, here (C-249)
// ---------------------------------------------------------------------------
//
// Some `git_*` ops undo a failure with a **blanket restore**: `git merge --abort`,
// `git revert --abort`. A blanket restore is only safe when the state it restores is state THIS
// call created — otherwise it silently discards a half-finished, possibly hand-resolved operation
// the caller never committed, which is the invariant AGENTS.md holds hardest.
//
// Before C-249 each op re-derived that reasoning for itself: `git_worktree_leave` and `git_revert`
// grew the same clean-tree guard independently and `git_merge` grew none, which is how C-238's
// blocking defect existed. The policy is now one helper — [`require_tree_precondition`] — and every
// abort-capable op must call it, so the next merging or aborting op has to *decide* rather than
// inherit an accident. `crates/flux-tools/tests/git_tree_policy.rs` fails the suite if one doesn't.
//
// The policy has two parts, and they are deliberately not the same question:
//
//   1. **No operation of the same kind may already be in flight** (`MERGE_HEAD`, `REVERT_HEAD`,
//      `CHERRY_PICK_HEAD`). Mandatory for every abort-capable op, and it is what *licenses* the
//      abort further down: reaching that path proves the marker seen there is one this call made.
//   2. **A clean working tree.** Required only where the abort restores the *whole* tree rather
//      than just what this call staged. NOT universal, on purpose: a dirty index makes `git merge`
//      refuse to start (leaving no `MERGE_HEAD` and the tree untouched) and unrelated unstaged
//      edits survive `git merge --abort` intact, so refusing every dirty tree reflexively would
//      make the family unusable in exactly the multi-author situation `git_stage_hunks` exists to
//      serve. Where it IS required it is **flux policy, stricter than git** — git itself only
//      refuses a dirty index, or unstaged changes to the paths it would touch.

/// Whether an abort-capable `git_*` op additionally requires a clean working tree before it starts.
/// Both variants carry the *stated reason*: an op that declines the precondition has to say why it
/// is safe to, in the same place an op that requires it says why it is not.
#[derive(Clone, Copy)]
enum CleanTree {
    /// Required — this op's failure path restores more than it staged.
    Required(&'static str),
    /// Deliberately not required — a dirty tree is outside this op's blast radius.
    NotRequired(&'static str),
}

impl CleanTree {
    /// The stated reason, whichever way the decision went.
    fn because(self) -> &'static str {
        match self {
            Self::Required(why) | Self::NotRequired(why) => why,
        }
    }
}

/// The tree precondition one abort-capable `git_*` op states for one checkout. Ops that guard two
/// checkouts (`git_worktree_leave`) declare one of these per checkout.
struct TreePrecondition {
    /// The op name, so every refusal is attributable.
    op: &'static str,
    /// How the refusal names the checkout being guarded ("this checkout", "the worktree", …).
    subject: &'static str,
    /// `(in-progress marker ref, the git command that owns it)` — an op's failure path may only
    /// abort a marker it can prove it created, so any of these present at preflight is a refusal.
    in_flight: &'static [(&'static str, &'static str)],
    clean_tree: CleanTree,
    /// Op-specific trailing guidance (e.g. where the context is left), or empty.
    note: &'static str,
}

const GIT_MERGE_TREE: TreePrecondition = TreePrecondition {
    op: "git_merge",
    subject: "this checkout",
    in_flight: &[("MERGE_HEAD", "git merge")],
    clean_tree: CleanTree::NotRequired(
        "git refuses to start a merge that would overwrite local changes, and unrelated unstaged \
         edits survive `git merge --abort` — so uncommitted work is outside this op's blast radius",
    ),
    note: "",
};

const GIT_REVERT_TREE: TreePrecondition = TreePrecondition {
    op: "git_revert",
    subject: "this checkout",
    in_flight: &[
        ("REVERT_HEAD", "git revert"),
        ("CHERRY_PICK_HEAD", "git cherry-pick"),
    ],
    clean_tree: CleanTree::Required(
        "a conflicted revert is undone with a blanket `git revert --abort`, which cannot tell this \
         call's revert from edits that were already in the tree",
    ),
    note: "",
};

const GIT_WORKTREE_ENTER_TREE: TreePrecondition = TreePrecondition {
    op: "git_worktree_enter",
    subject: "the checkout",
    in_flight: &[("MERGE_HEAD", "git merge"), ("REVERT_HEAD", "git revert")],
    clean_tree: CleanTree::Required(
        "the session branches off this checkout's HEAD and `git_worktree_leave` later refuses to \
         merge unless `main` is still exactly there — uncommitted work would strand outside the \
         session and block the merge back",
    ),
    note: "",
};

const GIT_WORKTREE_LEAVE_SESSION_TREE: TreePrecondition = TreePrecondition {
    op: "git_worktree_leave",
    subject: "the worktree",
    in_flight: &[("MERGE_HEAD", "git merge"), ("REVERT_HEAD", "git revert")],
    clean_tree: CleanTree::Required(
        "leave never stages or commits, so anything uncommitted here is discarded with the \
         worktree",
    ),
    note: "The context stays in the worktree.",
};

const GIT_WORKTREE_LEAVE_ORIGINAL_TREE: TreePrecondition = TreePrecondition {
    op: "git_worktree_leave",
    subject: "the original checkout",
    in_flight: &[("MERGE_HEAD", "git merge")],
    clean_tree: CleanTree::Required(
        "the trial merge into `main` is ALWAYS aborted, and a blanket `git merge --abort` cannot \
         tell this call's trial from work that was already there",
    ),
    note: "Refusing to merge — the context stays in the worktree.",
};

/// A preflight that could not run git at all. Recoverable, never a plan-halting raw error: the
/// whole promise of a preflight is that a refusal leaves nothing behind (C-241 review).
fn preflight_unavailable(op: &str, what: &str, detail: &str) -> ToolResult {
    ToolResult::error(format!(
        "{op}: could not run `git {what}` to check its preconditions ({detail}); refusing to \
         start — nothing was changed"
    ))
}

/// `git status --porcelain`, as a refusal on any failure.
async fn tree_status(
    system: &flux_system::System,
    op: &str,
) -> std::result::Result<String, ToolResult> {
    match run_git(system, &["status", "--porcelain"]).await {
        Ok((true, status)) => Ok(status),
        Ok((false, out)) => Err(preflight_unavailable(op, "status --porcelain", &out)),
        Err(e) => Err(preflight_unavailable(
            op,
            "status --porcelain",
            &e.to_string(),
        )),
    }
}

/// The one dirty-tree refusal wording in the family.
///
/// `git status --porcelain` reports untracked (`??`) entries alongside tracked ones, and a plain
/// `git stash` leaves untracked files exactly where they are — so "commit or stash them first" is
/// advice the caller cannot follow, and an agent that follows it retries and fails identically.
/// Split the two and give each the remedy that actually clears it.
fn dirty_tree_refusal(p: &TreePrecondition, status: &str) -> ToolResult {
    let (untracked, tracked): (Vec<&str>, Vec<&str>) = status
        .lines()
        .filter(|l| !l.trim().is_empty())
        .partition(|l| l.starts_with("??"));
    let mut body = String::new();
    if !tracked.is_empty() {
        body.push_str(&format!(
            "\nTracked changes ({}) — `git commit` or `git stash` clears these:\n{}",
            tracked.len(),
            tracked.join("\n")
        ));
    }
    if !untracked.is_empty() {
        body.push_str(&format!(
            "\nUntracked files ({}) — a plain `git stash` does NOT clear these; use \
             `git stash -u`, `git clean -fd`, or move them aside:\n{}",
            untracked.len(),
            untracked.join("\n")
        ));
    }
    let note = if p.note.is_empty() {
        String::new()
    } else {
        format!(" {}", p.note)
    };
    ToolResult::error(format!(
        "{}: refusing — {} has uncommitted changes, and {}.{note}{body}",
        p.op,
        p.subject,
        p.clean_tree.because()
    ))
}

/// Enforce `p` against the checkout `system` is rooted at.
///
/// `Some(refusal)` is a recoverable `ToolResult::error` to return unchanged — nothing was created
/// and nothing was touched. `None` means the op may proceed, and (part 1 of the policy) that any
/// in-progress marker it later sees is one it created itself.
async fn require_tree_precondition(
    system: &flux_system::System,
    p: &TreePrecondition,
) -> Option<ToolResult> {
    for (marker, owner) in p.in_flight {
        let present = match run_git(system, &["rev-parse", "-q", "--verify", marker]).await {
            Ok((present, head)) => present.then_some(head),
            Err(e) => {
                return Some(preflight_unavailable(
                    p.op,
                    &format!("rev-parse -q --verify {marker}"),
                    &e.to_string(),
                ))
            }
        };
        let Some(head) = present else { continue };
        let status = match tree_status(system, p.op).await {
            Ok(s) => s,
            Err(refusal) => return Some(refusal),
        };
        let note = if p.note.is_empty() {
            String::new()
        } else {
            format!(" {}", p.note)
        };
        return Some(ToolResult::error(format!(
            "{}: `{owner}` is already in progress in {} ({marker} {head}) — refusing to start, \
             and refusing to abort the one in flight because its resolution may be uncommitted \
             work. Conclude it (`git commit`) or abandon it (`{owner} --abort`) by hand, then \
             retry.{note} Working tree:\n{status}",
            p.op, p.subject
        )));
    }
    if matches!(p.clean_tree, CleanTree::NotRequired(_)) {
        return None;
    }
    let status = match tree_status(system, p.op).await {
        Ok(s) => s,
        Err(refusal) => return Some(refusal),
    };
    if status.is_empty() {
        return None;
    }
    Some(dirty_tree_refusal(p, &status))
}

// ---------------------------------------------------------------------------
// git_merge
// ---------------------------------------------------------------------------

pub struct GitMergeTool;

#[async_trait]
impl Tool for GitMergeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_merge".into(),
            description: "Merge a branch or ref into the current branch (`git merge`). \
                          `no_ff: true` always creates a merge commit even when a fast-forward \
                          is possible — the integration loop's audit trail. A conflict is a \
                          recoverable error naming the conflicting files: the merge is aborted \
                          and the tree restored (never left half-merged, never auto-resolved). \
                          If a merge is ALREADY in progress this op refuses and aborts nothing, \
                          since that merge's resolution may be uncommitted work."
                .into(),
            input_schema: tool_input_schema::<GitMergeInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            // High like `git_worktree_leave`: a merge moves the integration branch's tip and can
            // conflict — the caller must be able to trust the reported outcome.
            risk: Risk::High,
            // A repeated merge of the same ref converges ("Already up to date."), but what lands
            // depends on both refs at call time — never replay a merge result from a cache.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        // C-238: name the merged ref — `git_merge:impl/x` matches a `git_merge:*` grant the way
        // `rm:…` matches `Bash(rm:*)`; fall back to the bare op name on malformed params so
        // subjects are never empty (a write with no subjects is forced to approval).
        params
            .get("branch")
            .and_then(|v| v.as_str())
            .map(|b| vec![format!("git_merge:{b}")])
            .unwrap_or_else(|| vec!["git_merge".to_string()])
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let branch = params.get("branch").and_then(|v| v.as_str()).unwrap_or("");
        let no_ff = params
            .get("no_ff")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: if no_ff {
                    format!("git merge --no-ff {branch}")
                } else {
                    format!("git merge {branch}")
                },
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: GitMergeInput = parse_params(params, "git_merge")?;
        let branch = args.branch;
        // Same guard as `git_checkout` (C-85): a model-chosen ref must never read as an option.
        let trimmed = branch.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            return Ok(ToolResult::error(format!(
                "git_merge: refusing ref {branch:?} — it is empty or looks like an option, \
                 not a ref"
            )));
        }
        let system = ctx.system();

        // The family's shared tree precondition (C-249). For `git_merge` that is the in-flight
        // check and deliberately NOT a clean tree — see `GIT_MERGE_TREE` for both reasons.
        //
        // The in-flight half is what licenses the abort further down: reaching that path PROVES no
        // merge was in progress when this call began, so the `MERGE_HEAD` seen there is one this
        // call created and aborting it destroys nothing it did not create.
        if let Some(refusal) = require_tree_precondition(&system, &GIT_MERGE_TREE).await {
            return Ok(refusal);
        }

        let mut argv = vec!["merge".to_string()];
        if args.no_ff.unwrap_or(false) {
            argv.push("--no-ff".to_string());
        }
        // Never open an editor for the merge message: the guarded system clears the environment,
        // and git's default message already names the merged branch — the audit trail the
        // integration loop needs.
        argv.push("--no-edit".to_string());
        argv.push(branch.clone());
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (ok, out) = run_git(&system, &argv).await?;
        if ok {
            return Ok(ToolResult::ok(if out.is_empty() {
                format!("merged {branch}")
            } else {
                out
            }));
        }
        // A failed merge with MERGE_HEAD present stopped on a conflict — and because the guard
        // above proved none was in flight beforehand, this MERGE_HEAD is ours. Name the unmerged
        // paths, abort, and hand back a recoverable error: never left half-merged silently.
        let (in_progress, _) =
            run_git(&system, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).await?;
        if in_progress {
            let (_, unmerged) =
                run_git(&system, &["diff", "--name-only", "--diff-filter=U"]).await?;
            let (aborted, abort_out) = run_git(&system, &["merge", "--abort"]).await?;
            if !aborted {
                return Ok(ToolResult::error(format!(
                    "git_merge: the merge of `{branch}` failed AND `git merge --abort` failed — \
                     the tree may be mid-merge; resolve by hand before any further git op.\n\
                     merge: {out}\nabort: {abort_out}"
                )));
            }
            // Report the count and the list from one source, so the message can never read
            // "conflicts in 0 file(s)" next to a list saying there were none.
            let paths: Vec<&str> = unmerged.lines().filter(|l| !l.is_empty()).collect();
            let detail = if paths.is_empty() {
                "git reported no unmerged paths".to_string()
            } else {
                format!("Conflicting files ({}):\n{}", paths.len(), paths.join("\n"))
            };
            return Ok(ToolResult::error(format!(
                "git_merge: the merge of `{branch}` conflicted; the merge was aborted and the \
                 tree restored to the state this call found it in. Resolve the conflict on the \
                 source branch and retry. {detail}"
            )));
        }
        Ok(ToolResult::error(format!("git merge failed: {out}")))
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
            input_schema: tool_input_schema::<GitPushInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::Network],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::Network],
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
        let args: GitPushInput = parse_params(params, "git_push")?;
        let remote = args.remote.unwrap_or_else(|| "origin".to_string());
        let mut argv = vec!["git".to_string(), "push".to_string(), remote];
        if let Some(b) = args.branch {
            argv.push(b);
        }
        let out = ctx.system().run(&argv, Duration::from_secs(60)).await?;
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
            input_schema: tool_input_schema::<GitCheckoutInput>(),
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
        let args: GitCheckoutInput = parse_params(params, "git_checkout")?;
        let branch = args.branch;
        let create = args.create.unwrap_or(false);

        // A model-chosen ref must never be interpretable as a pathspec: `git checkout .` (or `..`)
        // silently discards ALL uncommitted work. Reject path-shaped and option-shaped values, and
        // use `git switch`, which only ever changes branches — it never treats its argument as a
        // pathspec the way `git checkout` does (C-85).
        let trimmed = branch.trim();
        if trimmed.is_empty()
            || trimmed == "."
            || trimmed == ".."
            || trimmed.starts_with('-')
            || trimmed.contains("..")
        {
            return Ok(ToolResult::error(format!(
                "git_checkout: refusing branch name {branch:?} — it looks like a path or an option, \
                 not a branch (a value like `.` would discard uncommitted changes)"
            )));
        }

        let mut argv = vec!["git".to_string(), "switch".to_string()];
        if create {
            argv.push("-c".to_string());
        }
        argv.push(branch.clone());
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git switch failed [exit {}]: {body}",
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
// git_revert
// ---------------------------------------------------------------------------

pub struct GitRevertTool;

#[async_trait]
impl Tool for GitRevertTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_revert".into(),
            description: "Revert a commit by appending its inverse (`git revert --no-edit`); \
                          pass `mainline` (usually 1) to revert a merge commit. This is the \
                          integration loop's recovery op: a NEW commit undoes the target, so \
                          history is never rewritten and never reset. Requires a clean tree. A \
                          conflicted revert is a recoverable error naming the conflicting files; \
                          the revert is aborted and the tree left as the call found it."
                .into(),
            input_schema: tool_input_schema::<GitRevertInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            // High like `git_merge`: a revert moves the integration branch's tip and can
            // conflict — the caller must be able to trust the reported outcome.
            risk: Risk::High,
            // Reverting the same commit twice does not converge — the second attempt conflicts
            // against the first revert's own change.
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        // C-238: name the reverted commit — `git_revert:abc123` matches a `git_revert:*` grant
        // the way `rm:…` matches `Bash(rm:*)`; fall back to the bare op name on malformed params
        // so subjects are never empty (a write with no subjects is forced to approval).
        params
            .get("commit")
            .and_then(|v| v.as_str())
            .map(|c| vec![format!("git_revert:{c}")])
            .unwrap_or_else(|| vec!["git_revert".to_string()])
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let commit = params.get("commit").and_then(|v| v.as_str()).unwrap_or("");
        let mainline = params.get("mainline").and_then(|v| v.as_u64());
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: match mainline {
                    Some(n) => format!("git revert --no-edit -m {n} {commit}"),
                    None => format!("git revert --no-edit {commit}"),
                },
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: GitRevertInput = parse_params(params, "git_revert")?;
        let commit = args.commit;
        // Same guard as `git_merge` (C-85): a model-chosen ref must never read as an option.
        let trimmed = commit.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            return Ok(ToolResult::error(format!(
                "git_revert: refusing commit {commit:?} — it is empty or looks like an option, \
                 not a ref"
            )));
        }
        let system = ctx.system();

        // The family's shared tree precondition (C-249). For `git_revert` that is a clean tree —
        // DELIBERATELY stricter than git, which only refuses a dirty index or unstaged changes to
        // the paths it would touch — plus no revert/cherry-pick already in flight, which is what
        // licenses the blanket `git revert --abort` below. See `GIT_REVERT_TREE`.
        if let Some(refusal) = require_tree_precondition(&system, &GIT_REVERT_TREE).await {
            return Ok(refusal);
        }

        // Never open an editor for the revert message (same reason as `git_merge`): the guarded
        // system clears the environment, and git's default "Revert ..." subject is the audit trail
        // the integration loop needs.
        let mut argv = vec!["revert".to_string(), "--no-edit".to_string()];
        if let Some(n) = args.mainline {
            argv.push("-m".to_string());
            argv.push(n.to_string());
        }
        argv.push(commit.clone());
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (ok, out) = run_git(&system, &argv).await?;
        if ok {
            let (_, head) = run_git(&system, &["log", "-1", "--oneline"]).await?;
            return Ok(ToolResult::ok(if out.is_empty() {
                format!("reverted {commit} — {head}")
            } else {
                format!("{out}\nreverted {commit} — {head}")
            }));
        }

        // A revert that never started (bad rev, …) leaves no REVERT_HEAD and an untouched
        // tree; one that started and stopped is a conflict — name the files, then abort.
        let (started, _) =
            run_git(&system, &["rev-parse", "-q", "--verify", "REVERT_HEAD"]).await?;
        if !started {
            return Ok(ToolResult::error(format!(
                "git_revert: git revert `{commit}` failed: {out}"
            )));
        }
        let (_, conflicts) = run_git(&system, &["diff", "--name-only", "--diff-filter=U"]).await?;
        let (aborted, abort_out) = run_git(&system, &["revert", "--abort"]).await?;
        if !aborted {
            return Ok(ToolResult::error(format!(
                "git_revert: the revert of `{commit}` conflicted and the abort ALSO failed \
                 ({abort_out}); the tree is left mid-revert — resolve it or run \
                 `git revert --abort` manually. Conflicting files:\n{conflicts}"
            )));
        }
        // Count and list from one source, so the message can never read "conflicts in 0 file(s)"
        // alongside a list stating there were none.
        let paths: Vec<&str> = conflicts.lines().filter(|l| !l.is_empty()).collect();
        let detail = if paths.is_empty() {
            "git reported no unmerged paths".to_string()
        } else {
            format!("Conflicting files ({}):\n{}", paths.len(), paths.join("\n"))
        };
        Ok(ToolResult::error(format!(
            "git_revert: reverting `{commit}` conflicted; the revert was aborted and the tree is \
             clean. Reconcile and retry. {detail}"
        )))
    }
}

// ---------------------------------------------------------------------------
// git_branch
// ---------------------------------------------------------------------------

pub struct GitBranchTool;

#[async_trait]
impl Tool for GitBranchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_branch".into(),
            description: "Create a branch without switching to it (`git branch`), or delete one \
                          with `delete: true`. Deletion is the SAFE form (`git branch -d`): git \
                          itself refuses unmerged work and the checked-out branch — there is no \
                          force-delete."
                .into(),
            input_schema: tool_input_schema::<GitBranchInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            // Medium like `git_checkout` (which also creates branches): creation is benign, and
            // the only deletion offered is `-d`, which git refuses for unmerged or checked-out
            // branches — the destructive `-D` is not reachable.
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        // C-238: name the branch — `git_branch:impl/x` matches a `git_branch:*` grant the way
        // `rm:…` matches `Bash(rm:*)`; fall back to the bare op name on malformed params so
        // subjects are never empty (a write with no subjects is forced to approval).
        params
            .get("name")
            .and_then(|v| v.as_str())
            .map(|n| vec![format!("git_branch:{n}")])
            .unwrap_or_else(|| vec!["git_branch".to_string()])
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let delete = params
            .get("delete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: if delete {
                    format!("git branch -d {name}")
                } else {
                    format!("git branch {name}")
                },
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: GitBranchInput = parse_params(params, "git_branch")?;
        let name = args.name;
        let delete = args.delete.unwrap_or(false);
        // Same guard as `git_checkout` (C-85): a model-chosen name must never read as an option
        // (`git branch -D …` would force-delete) or a path.
        let trimmed = name.trim();
        if trimmed.is_empty()
            || trimmed == "."
            || trimmed == ".."
            || trimmed.starts_with('-')
            || trimmed.contains("..")
        {
            return Ok(ToolResult::error(format!(
                "git_branch: refusing branch name {name:?} — it looks like a path or an option, \
                 not a branch"
            )));
        }
        let mut argv = vec!["git".to_string(), "branch".to_string()];
        if delete {
            argv.push("-d".to_string());
        }
        argv.push(name.clone());
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
        let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            return Ok(ToolResult::error(format!(
                "git branch failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        Ok(ToolResult::ok(if body.is_empty() {
            if delete {
                format!("deleted {name}")
            } else {
                format!("created {name}")
            }
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
            input_schema: tool_input_schema::<GitUnstageInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            // C-191: `git restore --staged` mutates the index and runs a program — the same shape
            // as its twin `git_stage`, which has always been `Medium`. `Low` here was drift, not a
            // judgement that unstaging is cheaper than staging.
            risk: Risk::Medium,
            // C-191: converges on repeat, but what it removes depends on the index at call time.
            idempotency: Idempotency::Conditional,
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
        let args: GitUnstageInput = parse_params(params, "git_unstage")?;
        if args.paths.is_empty() {
            return Err(Error::Other(
                "git_unstage: `paths` must be a non-empty array".to_string(),
            ));
        }
        let mut argv = vec![
            "git".to_string(),
            "restore".to_string(),
            "--staged".to_string(),
        ];
        argv.extend(args.paths);
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
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
// git_hunks / git_stage_hunks (C-92)
//
// Hunk-level staging: the `git add -p` capability, minus the interactivity the guarded envelope
// cannot offer. `git_hunks` splits a file's index-vs-worktree diff into addressable units;
// `git_stage_hunks` stages a named subset of them.
//
// The selector is a CONTENT hash, not a position (design: docs/designs/hunk-level-staging.md).
// A positional index is the one shape that fails silently — if a coworker saves between the read
// and the stage, index 2 is now a different hunk and their work lands in our commit, which is the
// exact bug this pair exists to prevent. A content id simply stops matching, and the op refuses.
// ---------------------------------------------------------------------------

/// One addressable unit of an index-vs-worktree diff.
struct Hunk {
    /// Content-derived selector, `h{ordinal}-{16 hex}` — see [`hunk_id`].
    id: String,
    /// The `@@ -a,b +c,d @@` line, verbatim.
    header: String,
    /// The hunk verbatim (header line + body lines), ready to splice into a patch.
    patch: String,
    added: usize,
    removed: usize,
}

/// The stable identity of a hunk: a hash of the file it belongs to and the lines it changes,
/// **excluding** the `@@` line numbers.
///
/// Position is deliberately not part of the identity. Staging or editing an earlier hunk shifts
/// every later hunk's line numbers without changing what those hunks do, and re-keying them on
/// every edit would force a re-listing between each staged hunk. The `ordinal` prefix keeps the id
/// readable and disambiguates two byte-identical hunks in one file; because it is only ever
/// compared after the hash matches, a stale ordinal can misdirect nothing.
///
/// Uses `DefaultHasher` for the same reason `cognition::compute_fingerprint` does — constructed
/// directly it is SipHash-1-3 with a fixed zero key, deterministic across runs and processes, and
/// it keeps this crate free of a hashing dependency. The id is an integrity check against
/// concurrent edits, not a security boundary.
fn hunk_id(path: &str, ordinal: usize, body: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    format!("{path}\u{1}{body}").hash(&mut hasher);
    format!("h{ordinal}-{:016x}", hasher.finish())
}

/// Split one file's unified diff into its preamble (everything before the first `@@`, i.e. the
/// `diff --git`/`index`/`---`/`+++` lines) and its hunks.
///
/// Returns `Err` with a human-readable reason if the diff covers more than one file — one path per
/// call is what keeps `permission_subjects` exact.
fn split_hunks(path: &str, diff: &str) -> std::result::Result<(String, Vec<Hunk>), String> {
    if diff.matches("\ndiff --git ").count() + usize::from(diff.starts_with("diff --git ")) > 1 {
        return Err(format!(
            "`{path}` expanded to more than one file — pass a single file path, not a directory \
             or glob"
        ));
    }
    let mut preamble = String::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current: Option<(String, String)> = None; // (header, body)
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            if let Some((header, body)) = current.take() {
                hunks.push(build_hunk(path, hunks.len() + 1, header, body));
            }
            current = Some((line.trim_end_matches('\n').to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
        } else {
            preamble.push_str(line);
        }
    }
    if let Some((header, body)) = current.take() {
        hunks.push(build_hunk(path, hunks.len() + 1, header, body));
    }
    Ok((preamble, hunks))
}

/// Assemble one [`Hunk`] from its `@@` header and raw body.
fn build_hunk(path: &str, ordinal: usize, header: String, body: String) -> Hunk {
    let added = body.lines().filter(|l| l.starts_with('+')).count();
    let removed = body.lines().filter(|l| l.starts_with('-')).count();
    Hunk {
        id: hunk_id(path, ordinal, &body),
        patch: format!("{header}\n{body}"),
        header,
        added,
        removed,
    }
}

/// `git diff` argv for one path, pinned so the output cannot drift with the user's git config
/// (an external differ or `diff.noprefix` would otherwise produce something `git apply` can't read).
fn hunk_diff_argv(path: &str, context: u32) -> Vec<String> {
    [
        "git",
        "--no-pager",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain([format!("--unified={context}"), "--".to_string()])
    .chain([path.to_string()])
    .collect()
}

/// Smallest context radius either hunk op will accept.
///
/// Zero is rejected rather than supported. A `--unified=0` patch carries no context lines, so
/// `git apply` has nothing to verify the hunk against and anchors on the line numbers alone: it
/// then applies a pure insertion *at the wrong offset and exits 0* (observed against git 2.55.0 —
/// a hunk requested at line 6 of a 30-line file lands at EOF), which is exactly the silent
/// misapply these ops exist to prevent. `--unidiff-zero` makes that case land correctly, but only
/// by switching the verification off, so it trades the guarantee for the granularity. One line of
/// context restores the check and still splits far more finely than the default three.
const MIN_HUNK_CONTEXT: u32 = 1;

/// Validate a caller-supplied context radius, or explain the floor.
fn checked_context(context: Option<u32>, op: &str) -> std::result::Result<u32, String> {
    match context.unwrap_or(3) {
        c if c < MIN_HUNK_CONTEXT => Err(format!(
            "{op}: `context` must be at least {MIN_HUNK_CONTEXT} (got {c}). A zero-context diff \
             gives `git apply` no context lines to verify against, so a hunk can be staged at the \
             wrong position in the file without any error. Use `context: 1` for the finest \
             splitting that is still safe."
        )),
        c => Ok(c),
    }
}

/// Preamble markers that mean the diff is about the file's *existence or identity* rather than its
/// contents. Partial staging is defined on content hunks; these headers are copied verbatim beside
/// whatever hunk is selected, so honouring one while dropping the other hunks would stage a
/// deletion or a rename the caller never selected. Whole-file operations belong to `git_stage`.
const WHOLE_FILE_PREAMBLE_MARKERS: &[&str] = &[
    "deleted file mode",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "old mode ",
    "new mode ",
];

/// Guidance for selectors that name nothing in the current diff (C-32's repairable-failure
/// pattern). Names the stale ids, what is actually there now, and the op to re-run — a bare
/// "hunk not found" would leave the model with no move.
fn stale_hunk_guidance(path: &str, missing: &[String], live: &[Hunk]) -> String {
    let available = if live.is_empty() {
        format!("`{path}` now has no unstaged hunks at all")
    } else {
        format!(
            "the hunks in `{path}` are now: {}",
            live.iter()
                .map(|h| format!("{} {}", h.id, h.header))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    // Distinguish the two ways an id can go stale, because they call for the same repair but a
    // very different explanation. The hash half is content; the `h<n>-` half is position. If a
    // missing id's hash is still present under a different ordinal, the *content* is untouched and
    // the selection only lost its numbering — telling that caller "the file changed underneath" is
    // simply false, and it sends them looking for an edit nobody made.
    let renumbered: Vec<&String> = missing
        .iter()
        .filter(|want| {
            want.split_once('-').is_some_and(|(_, hash)| {
                live.iter()
                    .any(|h| h.id.split_once('-').is_some_and(|(_, live)| live == hash))
            })
        })
        .collect();
    let cause = if renumbered.len() == missing.len() {
        "these hunks still exist with the same content but under different ordinals — staging or \
         reverting an earlier hunk renumbers the ones after it, and the ordinal is part of the id"
    } else if renumbered.is_empty() {
        "the file changed underneath this selection"
    } else {
        "some of these were renumbered by an earlier staging and the rest no longer match the file"
    };
    format!(
        "no such hunk in `{path}`: {} — {cause}, so {available}. Nothing was staged. Re-run \
         git_hunks(\"{path}\") and select from the ids it returns.",
        missing.join(", ")
    )
}

pub struct GitHunksTool;

#[async_trait]
impl Tool for GitHunksTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_hunks".into(),
            description: "List the individually stageable hunks of one file's unstaged changes \
                          (index vs working tree). Each hunk gets a stable id to pass to \
                          `git_stage_hunks`. Optional `context` sets the context radius \
                          (default 3, minimum 1); a smaller radius splits adjacent changes apart."
                .into(),
            input_schema: tool_input_schema::<GitHunksInput>(),
            output_schema: None,
            effects: vec![Effect::Process],
            // Stays `Low` on the strength of an explicit I1 allowlist entry in
            // `flux_spec::coherence` — it is `git diff` with a parser attached: fixed argv, the
            // caller may only narrow it to a path, and it mutates nothing (C-92).
            risk: Risk::Low,
            // The hunks track the working tree, not the input — see `git_diff`.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        single_path(params, "git_hunks")
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        for p in single_path(params, "git_hunks") {
            set.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process {
                    command: format!("git diff -- {p}"),
                },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: GitHunksInput = parse_params(params, "git_hunks")?;
        let context = match checked_context(args.context, "git_hunks") {
            Ok(c) => c,
            Err(why) => return Ok(ToolResult::error(why)),
        };
        let argv = hunk_diff_argv(&args.path, context);
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
        if out.exit_code != 0 {
            let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
            return Ok(ToolResult::error(format!(
                "git diff failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        let (_, hunks) = match split_hunks(&args.path, &out.stdout) {
            Ok(split) => split,
            Err(why) => return Ok(ToolResult::error(why)),
        };
        if hunks.is_empty() {
            return Ok(ToolResult::ok(format!(
                "no unstaged hunks in `{}`",
                args.path
            )));
        }
        let mut body = format!(
            "{} hunk{} in `{}` (unstaged, context {context}) — pass ids to git_stage_hunks\n",
            hunks.len(),
            if hunks.len() == 1 { "" } else { "s" },
            args.path
        );
        for h in &hunks {
            body.push_str(&format!(
                "\n[{}] {}  +{} -{}\n{}",
                h.id, h.header, h.added, h.removed, h.patch
            ));
        }
        Ok(ToolResult::ok(body))
    }
}

// ---------------------------------------------------------------------------
// git_stage_hunks
// ---------------------------------------------------------------------------

pub struct GitStageHunksTool;

#[async_trait]
impl Tool for GitStageHunksTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_stage_hunks".into(),
            description: "Stage only the named hunks of one file (the `git add -p` equivalent) — \
                          use when another author has in-flight changes in the same file and only \
                          your own hunks belong in the commit. `hunks` are ids from `git_hunks`; \
                          pass the same `context` you listed them with. All-or-nothing: if any id \
                          no longer matches, nothing is staged."
                .into(),
            input_schema: tool_input_schema::<GitStageHunksInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            // Mutates the index and runs a program — the same shape as its whole-file twin
            // `git_stage`.
            risk: Risk::Medium,
            // Restaging the same hunks converges (they are simply gone from the unstaged diff on
            // the second call), but what lands in the index is a function of the working tree at
            // call time, so the result must never be replayed from a cache.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        single_path(params, "git_stage_hunks")
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        for p in single_path(params, "git_stage_hunks") {
            set.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process {
                    command: format!("git apply --cached -- {p}"),
                },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: GitStageHunksInput = parse_params(params, "git_stage_hunks")?;
        if args.hunks.is_empty() {
            return Err(Error::Other(
                "git_stage_hunks: `hunks` must be a non-empty array of ids from git_hunks"
                    .to_string(),
            ));
        }
        let context = match checked_context(args.context, "git_stage_hunks") {
            Ok(c) => c,
            Err(why) => return Ok(ToolResult::error(why)),
        };

        // Recompute the diff HERE rather than trusting whatever the caller last saw. The ids are
        // matched against the working tree as of this call, which is what makes a concurrent edit
        // a refusal instead of a wrong commit.
        let argv = hunk_diff_argv(&args.path, context);
        let out = ctx.system().run(&argv, Duration::from_secs(30)).await?;
        if out.exit_code != 0 {
            let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
            return Ok(ToolResult::error(format!(
                "git diff failed [exit {}]: {body}",
                out.exit_code
            )));
        }
        let (preamble, hunks) = match split_hunks(&args.path, &out.stdout) {
            Ok(split) => split,
            Err(why) => return Ok(ToolResult::error(why)),
        };

        // The preamble rides along with whichever hunks are selected, so a header describing a
        // whole-file change would be honoured even though the caller only picked content hunks.
        if let Some(marker) = WHOLE_FILE_PREAMBLE_MARKERS
            .iter()
            .find(|m| preamble.lines().any(|l| l.starts_with(**m)))
        {
            return Ok(ToolResult::error(format!(
                "git_stage_hunks: the diff for `{}` carries `{}`, which is a whole-file change \
                 (deletion, rename, copy or mode change) rather than a content edit. Staging part \
                 of it is not meaningful — use git_stage(\"{}\") to stage the whole file.",
                args.path,
                marker.trim(),
                args.path
            )));
        }

        let missing: Vec<String> = args
            .hunks
            .iter()
            .filter(|want| !hunks.iter().any(|h| &h.id == *want))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Ok(ToolResult::error(stale_hunk_guidance(
                &args.path, &missing, &hunks,
            )));
        }

        // Reassemble a patch from the selected hunks — verbatim bytes flux just read out of `git
        // diff`, never bytes supplied by the caller. Order follows the diff so the hunks stay
        // ascending, as `git apply` expects.
        let selected: Vec<&Hunk> = hunks
            .iter()
            .filter(|h| args.hunks.contains(&h.id))
            .collect();
        let mut patch = preamble;
        for h in &selected {
            patch.push_str(&h.patch);
        }

        // Deliberately NO `--recount`. Dropping earlier hunks does not invalidate the remaining
        // hunks' old-side headers — the index is the preimage, so their counts stay true — and
        // `--recount` would recompute the counts from the body it actually received, which turns
        // git's corrupt-patch check off on a patch flux assembled. That check is the last thing
        // standing between a truncated write and a partial stage reported as a complete success.
        let apply = vec![
            "git".to_string(),
            "apply".to_string(),
            "--cached".to_string(),
            "--whitespace=nowarn".to_string(),
            "-".to_string(),
        ];
        let out = ctx
            .system()
            .run_with_stdin(&apply, patch.as_bytes(), Duration::from_secs(30))
            .await?;
        if out.exit_code != 0 {
            let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
            // The ids all matched, so this is the *staged* side having moved under us. Recoverable
            // for the same reason a stale id is: `git apply` is all-or-nothing, so the index is
            // untouched and the agent can re-list and retry.
            return Ok(ToolResult::error(format!(
                "the selected hunks no longer apply to `{}` [git apply exit {}]: {body} — nothing \
                 was staged. Re-run git_hunks(\"{}\") and select from the ids it returns.",
                args.path, out.exit_code, args.path
            )));
        }
        Ok(ToolResult::ok(format!(
            "staged {} of {} hunk{} in `{}`: {}",
            selected.len(),
            hunks.len(),
            if hunks.len() == 1 { "" } else { "s" },
            args.path,
            selected
                .iter()
                .map(|h| h.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

// ---------------------------------------------------------------------------
// git_worktree_enter / git_worktree_leave (C-98 / C-99)
//
// Context-local worktree transitions: `enter` creates an isolated temporary git worktree under a
// private `/tmp/flux-worktree-*` parent and swaps ONLY this agent context's active guarded
// `System` to the checkout (no `set_current_dir`, no process-global state); `leave` merges the
// committed work back into the original `main` with `--no-ff` (after a no-commit trial merge that
// proves the real merge cannot strand `main` conflicted), removes the worktree and its generated
// branch, and restores the original root. Every git invocation is argv-only through a guarded
// `System` — never a shell.
// ---------------------------------------------------------------------------

/// Process-wide sequence for collision-resistant generated worktree branch names.
static WORKTREE_BRANCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Run `git <args>` argv-only through the given guarded system. Returns `(succeeded, combined
/// trimmed output)`; spawn-level failures propagate as errors.
async fn run_git(system: &flux_system::System, args: &[&str]) -> Result<(bool, String)> {
    let argv: Vec<String> = std::iter::once("git".to_string())
        .chain(args.iter().map(|s| (*s).to_string()))
        .collect();
    let out = system.run(&argv, Duration::from_secs(60)).await?;
    let body = format!("{}{}", out.stdout, out.stderr).trim().to_string();
    Ok((out.exit_code == 0, body))
}

pub struct GitWorktreeEnterTool;

#[async_trait]
impl Tool for GitWorktreeEnterTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_worktree_enter".into(),
            description: "Create an isolated temporary git worktree (on a generated \
                          `flux/worktree/...` branch off the current clean `main`) and move THIS \
                          agent context's working root into it. Requires a clean checkout on \
                          `main` and no active worktree session. Later, `git_worktree_leave` \
                          merges the committed work back into `main` and restores the original \
                          root."
                .into(),
            input_schema: tool_input_schema::<GitWorktreeEnterInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_worktree_enter".to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git worktree add -b flux/worktree/<generated> <tmp>/checkout <head>"
                    .to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _: GitWorktreeEnterInput = parse_params(params, "git_worktree_enter")?;
        if ctx.workspace_context().worktree_session().is_some() {
            return Ok(ToolResult::error(
                "git_worktree_enter: a worktree session is already active in this context; \
                 run git_worktree_leave first (nesting is not supported)"
                    .to_string(),
            ));
        }
        let system = ctx.system();
        let original_root = system.workspace().root().display().to_string();

        // Preflight 1: inside a git repository.
        let (ok, body) = run_git(&system, &["rev-parse", "--is-inside-work-tree"]).await?;
        if !ok || body != "true" {
            return Ok(ToolResult::error(format!(
                "git_worktree_enter: not inside a git repository: {body}"
            )));
        }
        // Preflight 2: on branch `main` (detached HEAD rejected).
        let (ok, branch_now) = run_git(&system, &["symbolic-ref", "--short", "HEAD"]).await?;
        if !ok {
            return Ok(ToolResult::error(format!(
                "git_worktree_enter: HEAD is detached (no branch checked out); requires a clean \
                 checkout of `main`: {branch_now}"
            )));
        }
        if branch_now != "main" {
            return Ok(ToolResult::error(format!(
                "git_worktree_enter: current branch is `{branch_now}`; requires `main`"
            )));
        }
        // Preflight 3: the family's shared tree precondition (C-249) — a clean checkout, and
        // nothing mid-merge/mid-revert. See `GIT_WORKTREE_ENTER_TREE`.
        if let Some(refusal) = require_tree_precondition(&system, &GIT_WORKTREE_ENTER_TREE).await {
            return Ok(refusal);
        }
        // Capture `main`'s HEAD — the base the eventual merge is verified against.
        let (ok, head) = run_git(&system, &["rev-parse", "HEAD"]).await?;
        if !ok || head.is_empty() {
            return Ok(ToolResult::error(format!(
                "git_worktree_enter: could not resolve HEAD: {head}"
            )));
        }

        let seq = WORKTREE_BRANCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let short_head = &head[..head.len().min(8)];
        let branch = format!("flux/worktree/{}-{seq}-{short_head}", std::process::id());

        let parent = flux_system::allocate_worktree_dir()?;
        let checkout = parent.join("checkout");
        let (ok, add_out) = run_git(
            &system,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &checkout.display().to_string(),
                &head,
            ],
        )
        .await?;
        if !ok {
            let _ = flux_system::remove_worktree_dir(&parent);
            return Ok(ToolResult::error(format!(
                "git_worktree_enter: git worktree add failed: {add_out}"
            )));
        }

        // Derive the re-rooted guarded system (same named/read roots, posture, sandbox — new root).
        let rerooted = match system.rerooted(&checkout) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                let _ = run_git(
                    &system,
                    &["worktree", "remove", &checkout.display().to_string()],
                )
                .await;
                let _ = run_git(&system, &["branch", "-d", &branch]).await;
                let _ = flux_system::remove_worktree_dir(&parent);
                return Ok(ToolResult::error(format!(
                    "git_worktree_enter: could not derive a system rooted at the worktree: {e}"
                )));
            }
        };
        let session = flux_runtime::WorktreeSession {
            original: system.clone(),
            base_commit: head.clone(),
            branch: branch.clone(),
            checkout: checkout.clone(),
            parent_dir: parent.clone(),
            phase: flux_runtime::WorktreePhase::Active,
        };
        if let Err(e) = ctx.workspace_context().enter_worktree(session, rerooted) {
            let _ = run_git(
                &system,
                &["worktree", "remove", &checkout.display().to_string()],
            )
            .await;
            let _ = run_git(&system, &["branch", "-d", &branch]).await;
            let _ = flux_system::remove_worktree_dir(&parent);
            return Ok(ToolResult::error(format!("git_worktree_enter: {e}")));
        }

        let result = serde_json::json!({
            "entered_worktree": true,
            "working_root": checkout.display().to_string(),
            "branch": branch,
            "base_commit": head,
            "original_root": original_root,
            "note": format!(
                "IMPORTANT: this context now operates inside the worktree at {} on branch {branch}. \
                 The session system prompt still describes the original root ({original_root}) — \
                 treat THIS result as ground truth for the working directory. All subsequent file \
                 and process operations run in the worktree until git_worktree_leave.",
                checkout.display()
            ),
        });
        Ok(ToolResult::ok(
            serde_json::to_string_pretty(&result).unwrap(),
        ))
    }
}

pub struct GitWorktreeLeaveTool;

impl GitWorktreeLeaveTool {
    /// A cleanup-pending error: the merge landed (phase `Merged`), only worktree/branch/dir removal
    /// remains. The session is NOT cleared so a retried `git_worktree_leave` completes cleanup
    /// without re-merging.
    fn cleanup_pending(step: &str, detail: &str) -> ToolResult {
        ToolResult::error(format!(
            "git_worktree_leave: merged, cleanup required — {step} failed: {detail}. The merge \
             into `main` already landed and will NOT be repeated; retry git_worktree_leave to \
             complete cleanup."
        ))
    }
}

#[async_trait]
impl Tool for GitWorktreeLeaveTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_worktree_leave".into(),
            description: "Integrate this context's temporary worktree back into the original \
                          `main` (a `--no-ff` merge, preceded by an aborted no-commit trial merge \
                          so `main` can never be stranded conflicted), then remove the worktree, \
                          delete the generated branch, and restore the original working root. \
                          Requires a clean (fully committed) worktree — it never stages or commits \
                          automatically."
                .into(),
            input_schema: tool_input_schema::<GitWorktreeLeaveInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git_worktree_leave".to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git merge --no-ff --no-edit flux/worktree/<generated>; git worktree \
                          remove; git branch -d"
                    .to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _: GitWorktreeLeaveInput = parse_params(params, "git_worktree_leave")?;
        let Some(session) = ctx.workspace_context().worktree_session() else {
            return Ok(ToolResult::error(
                "git_worktree_leave: no worktree session is active in this context; \
                 git_worktree_enter starts one"
                    .to_string(),
            ));
        };
        let original = session.original.clone();
        let branch = session.branch.clone();

        if session.phase == flux_runtime::WorktreePhase::Active {
            // (1) The worktree itself must be clean — leave never stages or commits. Shared tree
            // precondition (C-249); see `GIT_WORKTREE_LEAVE_SESSION_TREE`.
            let active = ctx.system();
            if let Some(refusal) =
                require_tree_precondition(&active, &GIT_WORKTREE_LEAVE_SESSION_TREE).await
            {
                return Ok(refusal);
            }
            // (2) The original `main` must be untouched since enter: still on `main`, clean, and
            // at the captured base commit. Otherwise error and stay in the worktree.
            let (ok, orig_branch) =
                run_git(&original, &["symbolic-ref", "--short", "HEAD"]).await?;
            if !ok || orig_branch != "main" {
                return Ok(ToolResult::error(format!(
                    "git_worktree_leave: the original checkout is no longer on `main` (now: \
                     {orig_branch}); refusing to merge — the context stays in the worktree"
                )));
            }
            // The original `main` must be clean, and free of a merge this call would otherwise
            // abort: the trial merge below is ALWAYS aborted. Shared tree precondition (C-249);
            // see `GIT_WORKTREE_LEAVE_ORIGINAL_TREE`.
            if let Some(refusal) =
                require_tree_precondition(&original, &GIT_WORKTREE_LEAVE_ORIGINAL_TREE).await
            {
                return Ok(refusal);
            }
            let (ok, orig_head) = run_git(&original, &["rev-parse", "HEAD"]).await?;
            if !ok || orig_head != session.base_commit {
                return Ok(ToolResult::error(format!(
                    "git_worktree_leave: original `main` has moved since enter (was {}, now \
                     {orig_head}); refusing to merge — the context stays in the worktree. \
                     Integrate the new `main` state manually, then retry.",
                    session.base_commit
                )));
            }
            // (3) Trial merge: `--no-commit --no-ff`, then ALWAYS abort. A conflicted trial proves
            // the real merge would conflict — abort restores `main` untouched and we stay in the
            // worktree. A clean trial leaves MERGE_HEAD staged (no commit), which the abort clears
            // (unless the trial was a no-op "Already up to date", which leaves nothing to abort).
            let (trial_ok, trial_out) =
                run_git(&original, &["merge", "--no-commit", "--no-ff", &branch]).await?;
            let trial_noop = trial_out.contains("Already up to date");
            if !trial_noop {
                let (abort_ok, abort_out) = run_git(&original, &["merge", "--abort"]).await?;
                if !abort_ok {
                    return Ok(ToolResult::error(format!(
                        "git_worktree_leave: trial-merge abort failed on the original `main` \
                         ({abort_out}); resolve the original checkout manually, then retry — the \
                         context stays in the worktree"
                    )));
                }
            }
            if !trial_ok {
                return Ok(ToolResult::error(format!(
                    "git_worktree_leave: the merge of `{branch}` into `main` would conflict; the \
                     trial merge was aborted and `main` is untouched. Reconcile in the worktree, \
                     commit, and retry — the context stays in the worktree.\n{trial_out}"
                )));
            }
            // Real merge — the trial proved it cannot leave `main` conflicted.
            let (ok, merge_out) =
                run_git(&original, &["merge", "--no-ff", "--no-edit", &branch]).await?;
            if !ok {
                let _ = run_git(&original, &["merge", "--abort"]).await;
                return Ok(ToolResult::error(format!(
                    "git_worktree_leave: git merge failed unexpectedly after a clean trial \
                     ({merge_out}); the merge was aborted and the context stays in the worktree"
                )));
            }
            ctx.workspace_context().mark_merged();
        }

        // Cleanup — reached in phase `Merged` (freshly merged above, or a retry after a partial
        // cleanup). Any failure keeps the session (phase Merged) so a retry finishes cleanup
        // without re-merging.
        if session.checkout.exists() {
            let (ok, out) = run_git(
                &original,
                &[
                    "worktree",
                    "remove",
                    &session.checkout.display().to_string(),
                ],
            )
            .await?;
            if !ok {
                return Ok(Self::cleanup_pending("git worktree remove", &out));
            }
        } else {
            // A retry after the checkout was already removed: drop the stale registration.
            let (ok, out) = run_git(&original, &["worktree", "prune"]).await?;
            if !ok {
                return Ok(Self::cleanup_pending("git worktree prune", &out));
            }
        }
        let (ok, out) = run_git(&original, &["branch", "-d", &branch]).await?;
        if !ok && !out.contains("not found") {
            return Ok(Self::cleanup_pending("git branch -d", &out));
        }
        if let Err(e) = flux_system::remove_worktree_dir(&session.parent_dir) {
            return Ok(Self::cleanup_pending(
                "temporary directory removal",
                &e.to_string(),
            ));
        }

        // Only after full cleanup: restore the original root and clear the session.
        ctx.workspace_context().leave_worktree()?;
        let (_, merge_commit) = run_git(&original, &["rev-parse", "HEAD"]).await?;
        let restored_root = original.workspace().root().display().to_string();
        let result = serde_json::json!({
            "left_worktree": true,
            "restored_root": restored_root,
            "merge_commit": merge_commit,
            "merged_branch": branch,
            "note": format!(
                "The worktree work was merged into `main` (merge commit {merge_commit}); the \
                 worktree and branch {branch} were removed. This context now operates at the \
                 restored original root {restored_root}."
            ),
        });
        Ok(ToolResult::ok(
            serde_json::to_string_pretty(&result).unwrap(),
        ))
    }
}

// ---------------------------------------------------------------------------
// flux_reload (dev mode only)
// ---------------------------------------------------------------------------

/// `flux_reload` — recompile flux-cli, then instruct a manual restart (dev mode only).
///
/// Safety: this tool is only registered when `--dev` is active. It runs `cargo build -p flux-cli`
/// synchronously through the guarded system (`ctx.system().run`, argv-only — never model input). It is
/// deliberately **rebuild-only**: replacing the running process image (`execv`, or spawning a
/// replacement) would be a direct OS-process seam outside `flux_system::System`'s single guarded path
/// (AGENTS.md, "One guarded path starts every OS process"). A re-exec cannot reuse
/// `System::build_command`'s env-clear/workspace-pin semantics without breaking session resume, so
/// rather than open a second, differently-guarded seam the tool stops after a successful build and
/// returns a manual-restart hint. On build failure it returns an error and the session continues
/// uninterrupted.
pub struct ReloadTool;

#[async_trait]
impl Tool for ReloadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flux_reload".into(),
            description: "Recompile flux-cli in place (dev mode only). On success the freshly built \
                          binary is on disk, but this still-running session keeps the OLD build, so \
                          the tool returns instructions to restart (exit and re-run with `--resume`) \
                          to load the new binary. It does not replace the running process."
                .into(),
            input_schema: tool_input_schema::<FluxReloadInput>(),
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

    async fn execute(&self, ctx: &ToolContext, params: Value) -> flux_core::Result<ToolResult> {
        let _: FluxReloadInput = parse_params(params, "flux_reload")?;
        // Run `cargo build -p flux-cli` via the guarded system (fixed argv — not model input).
        let argv = [
            "cargo".to_string(),
            "build".to_string(),
            "-p".to_string(),
            "flux-cli".to_string(),
        ];
        let out = ctx
            .system()
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

        // Rebuild succeeded. flux_reload is deliberately rebuild-only: replacing the running process
        // image (execv / a spawned replacement) is a direct OS-process seam outside
        // `flux_system::System`'s single guarded path, so we stop here and return a manual-restart
        // hint rather than opening a second, differently-guarded seam. See the `ReloadTool` doc.
        Ok(ToolResult::ok(reload_restart_hint()))
    }
}

/// The message `flux_reload` returns after a successful rebuild. Rebuild-only by design (see
/// [`ReloadTool`]): the freshly built binary is on disk, but this process still runs the previous
/// image, so the operator must restart to pick it up.
fn reload_restart_hint() -> &'static str {
    "rebuilt flux-cli successfully. this session is still running the previous build — exit and \
     re-run flux with `--resume` (or `-c`) to load the new binary."
}

/// Register extra tools available only in `--dev` mode.
pub fn try_register_dev_builtins(registry: &mut flux_runtime::ToolRegistry) -> Result<()> {
    registry.try_register_from("flux-tools developer pack", std::sync::Arc::new(ReloadTool))
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_dev_builtins`].
pub fn register_dev_builtins(registry: &mut flux_runtime::ToolRegistry) {
    try_register_dev_builtins(registry).expect("flux-tools developer pack registration failed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::{System, Workspace};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn ctx() -> (std::path::PathBuf, ToolContext) {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-tools-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
        (dir, c)
    }

    #[derive(Default)]
    struct RecordingProgressSink(std::sync::Mutex<Vec<flux_runtime::ToolProgress>>);

    impl flux_runtime::ToolProgressSink for RecordingProgressSink {
        fn emit(&self, progress: flux_runtime::ToolProgress) {
            self.0.lock().unwrap().push(progress);
        }
    }

    /// C-158 end-to-end through the REAL producer: a `bash` op with a surface-installed channel
    /// reports its own output lines while running, and the final `ToolResult` is unchanged. This is
    /// the assertion that keeps the seam from being dead plumbing — it runs the actual `BashTool`,
    /// not a synthetic event.
    #[tokio::test]
    async fn bash_reports_its_output_lines_while_running() {
        let (dir, ctx) = ctx();
        let sink = Arc::new(RecordingProgressSink::default());
        let installed: Arc<dyn flux_runtime::ToolProgressSink> = sink.clone();

        let result = flux_runtime::scope_runtime_turn(
            flux_runtime::RuntimeTurnContext::new().with_tool_progress_sink(installed),
            BashTool.execute(&ctx, json!({"command": "echo alpha; echo beta"})),
        )
        .await
        .unwrap();

        let lines: Vec<String> = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.line.clone())
            .collect();
        assert_eq!(lines, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(sink.0.lock().unwrap().iter().all(|p| p.tool == "bash"));
        // The model's view of the op is untouched by observing it.
        assert_eq!(result.content, "alpha\nbeta\n");
        assert!(!result.is_error);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-158: with no sink installed, `bash` behaves exactly as before — same result, and nothing
    /// reported anywhere.
    #[tokio::test]
    async fn bash_without_a_progress_sink_is_unchanged() {
        let (dir, ctx) = ctx();
        let result = BashTool
            .execute(&ctx, json!({"command": "echo alpha; echo beta"}))
            .await
            .unwrap();
        assert_eq!(result.content, "alpha\nbeta\n");
        assert!(!result.is_error);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-158: a secret in a running op's output is redacted on the LIVE channel too, not only in
    /// the final result — the reporter binds the context's redactor.
    #[tokio::test]
    async fn bash_progress_lines_are_redacted() {
        let (dir, ctx) = ctx();
        ctx.redactor.add_secret("swordfish-tenant-key");
        let sink = Arc::new(RecordingProgressSink::default());
        let installed: Arc<dyn flux_runtime::ToolProgressSink> = sink.clone();

        let result = flux_runtime::scope_runtime_turn(
            flux_runtime::RuntimeTurnContext::new().with_tool_progress_sink(installed),
            BashTool.execute(&ctx, json!({"command": "echo using swordfish-tenant-key"})),
        )
        .await
        .unwrap();

        let lines = sink.0.lock().unwrap().clone();
        assert_eq!(lines.len(), 1);
        assert!(
            !lines[0].line.contains("swordfish-tenant-key"),
            "secret reached the live channel: {:?}",
            lines[0].line
        );
        // The raw result is still raw HERE by design: `Executor::dispatch` is what redacts a tool's
        // returned content, one layer up. The point of this test is that the live channel does NOT
        // get to skip that layer — it redacts at the reporter instead, so both faces are covered.
        assert!(result.content.contains("swordfish-tenant-key"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-57: `flux_reload` is rebuild-only — it no longer replaces the running process image. The
    /// post-build path returns a manual-restart hint (not an `execv`), and the tool no longer
    /// advertises process replacement. The architecture guard against re-adding a raw
    /// `std::process::Command` lives in `flux-codegate`.
    #[test]
    fn flux_reload_is_rebuild_only_and_instructs_manual_restart() {
        let hint = reload_restart_hint();
        assert!(
            hint.contains("--resume"),
            "hint must tell the user how to restart: {hint}"
        );
        assert!(
            hint.to_lowercase().contains("exit"),
            "hint must ask the user to exit/restart: {hint}"
        );
        let desc = ReloadTool.spec().description.to_lowercase();
        assert!(
            desc.contains("restart"),
            "flux_reload must advertise a manual restart: {desc}"
        );
        assert!(
            !desc.contains("hot-reload") && !desc.contains("replaces the current process"),
            "flux_reload must no longer advertise replacing the running process: {desc}"
        );
    }

    /// C-85: an empty `old_string` matches everywhere; without this guard `replace_all` would splice
    /// `new_string` between every character and corrupt the file. It must be refused up front.
    #[tokio::test]
    async fn edit_rejects_empty_old_string() {
        let (_dir, c) = ctx();
        WriteTool
            .execute(&c, json!({"path": "a.rs", "content": "hello\n"}))
            .await
            .unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({"path": "a.rs", "old_string": "", "new_string": "X", "replace_all": true}),
            )
            .await
            .expect_err("an empty old_string must be refused, not applied");
        assert!(
            err.to_string().contains("must not be empty"),
            "explains the empty-old_string refusal: {err}"
        );
    }

    /// C-85: a model-chosen ref must never be interpretable as a pathspec — `git checkout .` discards
    /// uncommitted work. `.` (and other path/option-shaped values) is refused before git ever runs.
    #[tokio::test]
    async fn git_checkout_refuses_pathspec_like_ref() {
        let (_dir, c) = ctx();
        for bad in [".", "..", "-f", "../evil", "a..b"] {
            let r = GitCheckoutTool
                .execute(&c, json!({"branch": bad}))
                .await
                .expect("a refusal is a tool result, not an Err");
            assert!(
                r.is_error,
                "branch {bad:?} must be refused before git runs, got: {}",
                r.content
            );
            assert!(
                r.content.contains("refusing"),
                "names the refusal for {bad:?}: {}",
                r.content
            );
        }
    }

    /// C-79: an unbounded read of an over-cap file returns paging guidance. The guard now stats the
    /// file first, so the guidance is produced without materializing the whole file.
    #[tokio::test]
    async fn read_over_cap_file_returns_guidance_without_slurping() {
        let (dir, c) = ctx();
        // Comfortably over READ_BYTE_CAP (256 KiB), written directly to avoid any WriteTool cap.
        std::fs::write(dir.join("big.txt"), "x".repeat(300 * 1024)).unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "big.txt"}))
            .await
            .unwrap();
        assert!(
            !r.is_error,
            "over-cap read is guidance, not an error: {}",
            r.content
        );
        assert!(
            r.content.contains("read cap") && r.content.contains("offset/limit"),
            "over-cap unbounded read returns paging guidance: {}",
            r.content
        );
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
        // glob scoped to a single file lists exactly that file (canonical value = JSON array, C-10).
        let g = GlobTool
            .execute(&c, json!({"pattern": "*", "path": "a.rs"}))
            .await
            .unwrap();
        assert_eq!(g.content.trim(), r#"["a.rs"]"#);
        assert_eq!(g.view.as_deref(), Some("a.rs"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn windowed_read_streams_the_exact_window_and_honors_the_byte_cap() {
        let (dir, c) = ctx();
        // 500 numbered lines; read a small window out of the middle.
        let content: String = (0..500).map(|i| format!("line{i}\n")).collect();
        WriteTool
            .execute(&c, json!({"path": "big.txt", "content": content}))
            .await
            .unwrap();
        let r = ReadTool
            .execute(&c, json!({"path": "big.txt", "offset": 100, "limit": 5}))
            .await
            .unwrap();
        assert!(!r.is_error);
        // Canonical value is the raw slice: exactly the window, '\n'-joined, no trailing newline —
        // byte-identical to the old `lines[start..end].join("\n")`.
        assert_eq!(
            r.content, "line100\nline101\nline102\nline103\nline104",
            "exact window, no extra lines materialized"
        );
        assert!(
            r.view.as_deref().unwrap().contains("101"),
            "view numbered from offset+1: {:?}",
            r.view
        );

        // Byte cap: a window of very long lines is truncated with guidance rather than unbounded.
        let wide: String = (0..10)
            .map(|_| format!("{}\n", "x".repeat(40 * 1024)))
            .collect(); // ~400 KiB
        WriteTool
            .execute(&c, json!({"path": "wide.txt", "content": wide}))
            .await
            .unwrap();
        let capped = ReadTool
            .execute(&c, json!({"path": "wide.txt", "offset": 0, "limit": 10}))
            .await
            .unwrap();
        assert!(
            capped.content.len() <= READ_BYTE_CAP,
            "windowed canonical bounded by the byte cap: {} bytes",
            capped.content.len()
        );
        assert!(
            capped.view.as_deref().unwrap().contains("truncated"),
            "cap guidance present in the view"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_on_a_directory_returns_repairable_guidance_not_a_raw_io_error() {
        // C-32: weak models routinely `read()` a directory (s_362 did it six times in one orient
        // plan); the raw `Is a directory (os error 21)` propagated via `?` halted the plan node.
        // Directory reads must come back as a normal `is_error` ToolResult the loop can react to.
        let (dir, c) = ctx();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        WriteTool
            .execute(&c, json!({"path": "sub/a.rs", "content": "fn a() {}\n"}))
            .await
            .unwrap();

        // Whole-file read on a directory: guidance, not a halt.
        let r = ReadTool.execute(&c, json!({"path": "sub"})).await.unwrap();
        assert!(
            r.is_error,
            "directory read is a repairable failure, not silent success"
        );
        assert!(
            r.content.contains("sub"),
            "guidance names the path: {:?}",
            r.content
        );
        assert!(
            r.content.to_lowercase().contains("glob"),
            "guidance suggests glob: {:?}",
            r.content
        );

        // Windowed (offset/limit) read on the same directory: same guidance, not a halt.
        let windowed = ReadTool
            .execute(&c, json!({"path": "sub", "offset": 0, "limit": 5}))
            .await
            .unwrap();
        assert!(
            windowed.is_error,
            "windowed directory read is also repairable"
        );
        assert!(windowed.content.to_lowercase().contains("glob"));

        // Multi-path read (the `read_section` machinery `read_many` shares): a directory among
        // several paths gets guidance in its own section rather than halting the whole call.
        let multi = ReadTool
            .execute(&c, json!({"path": ["sub/a.rs", "sub"]}))
            .await
            .unwrap();
        assert!(
            multi.content.contains("fn a()"),
            "sibling file is still read: {:?}",
            multi.content
        );
        assert!(
            multi.content.to_lowercase().contains("glob"),
            "directory section carries the same guidance: {:?}",
            multi.content
        );

        // A genuinely missing file still errors exactly as today (unchanged).
        let missing = ReadTool
            .execute(&c, json!({"path": "sub/missing.rs"}))
            .await;
        assert!(
            missing.is_err(),
            "a missing file still errors: {:?}",
            missing
        );

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

    #[tokio::test]
    async fn proc_run_is_argv_only_and_reports_exit() {
        let (dir, c) = ctx();
        let r = ProcRunTool
            .execute(&c, json!({"program": "printf", "args": ["hello"]}))
            .await
            .unwrap();
        assert_eq!(r.content, "hello");
        assert!(!r.is_error);

        let r = ProcRunTool
            .execute(&c, json!({"program": "sh", "args": ["-c", "exit 4"]}))
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("[exit 4]"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn proc_run_subjects() {
        assert_eq!(
            proc_subject(&json!({"program": "git", "args": ["status"]})).as_deref(),
            Some("git:status")
        );
        assert_eq!(
            proc_subject(&json!({"program": "cargo"})).as_deref(),
            Some("cargo")
        );
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
                "all",
                "any",
                "append",
                "bash",
                "cargo_build",
                "cargo_check",
                "cargo_clippy",
                "cargo_fmt",
                "cargo_test",
                "cite",
                "coalesce",
                "command.invoke",
                "compare",
                "count_by",
                "cwd",
                "dedupe",
                "edit",
                "evidence",
                "file_stat",
                "filter",
                "first",
                "flatten",
                "gaps",
                "git_branch",
                "git_checkout",
                "git_commit",
                "git_diff",
                "git_hunks",
                "git_log",
                "git_merge",
                "git_push",
                "git_revert",
                "git_stage",
                "git_stage_hunks",
                "git_status",
                "git_unstage",
                "git_worktree_enter",
                "git_worktree_leave",
                "glob",
                "go_build",
                "go_test",
                "go_vet",
                "grep",
                "group_by",
                "has",
                "home_dir",
                "join",
                "keys",
                "last",
                "len",
                "make",
                "map",
                "merge",
                "merge_obj",
                "metrics",
                "need",
                "node_run",
                "now",
                "npm",
                "observe",
                "omit",
                "patch",
                "path_exists",
                "pick",
                "proc.run",
                "pytest",
                "python_run",
                "read",
                "read_many",
                "regex_extract",
                "regex_match",
                "review.aggregate",
                "review.normalize",
                "skill.load",
                "skip",
                "sort",
                "split",
                "sqlite_query",
                "sum",
                "sys_info",
                "top",
                "values",
                "write"
            ]
        );
        r.validate_authority_contracts()
            .expect("every built-in has a coherent typed authority contract");
    }

    #[test]
    fn bash_is_off_by_default_and_opts_in_via_shell_signal() {
        let groups = crate::groups::builtin_groups();
        let bash = BashTool.spec();
        let proc_run = ProcRunTool.spec();
        assert_eq!(
            bash.group.as_deref(),
            Some("shell"),
            "bash belongs to the shell group"
        );
        assert_eq!(
            proc_run.group.as_deref(),
            Some("shell"),
            "proc.run belongs to the shell group"
        );

        // Default: no `shell` signal → group inactive → bash NOT advertised to the model.
        let off = flux_evidence::resolve_active_groups(&groups, &[]);
        assert!(!off.contains("shell"));
        assert!(
            !flux_runtime::is_advertised(&bash, &groups, &off),
            "bash must be hidden from the catalog by default"
        );
        assert!(
            !flux_runtime::is_advertised(&proc_run, &groups, &off),
            "proc.run must be hidden from the catalog by default"
        );

        // Opt-in: a `shell` signal observation (what `detect_signals` emits when FLUX_ENABLE_BASH /
        // enable_shell is set) activates the group → bash is advertised.
        let sig = flux_evidence::Observation::signal("shell");
        let on = flux_evidence::resolve_active_groups(&groups, std::slice::from_ref(&sig));
        assert!(on.contains("shell"));
        assert!(
            flux_runtime::is_advertised(&bash, &groups, &on),
            "bash is advertised once shell is opted in"
        );
        assert!(
            flux_runtime::is_advertised(&proc_run, &groups, &on),
            "proc.run is advertised once shell is opted in"
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

    /// D-66: converting `append` to parse `AppendInput` via `parse_params` makes the already-published
    /// schema's `additionalProperties: false` (from `#[serde(deny_unknown_fields)]`) actually enforced —
    /// previously the ad-hoc `str_param` extraction silently ignored any extra key. This test pins the
    /// new, intentional behavior (see docs/archive/drift-reports.md's D-66 section).
    #[tokio::test]
    async fn append_rejects_unknown_field() {
        let (dir, c) = ctx();
        let err = AppendTool
            .execute(&c, json!({"path": "a.txt", "content": "x", "bogus": 1}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid arguments"), "got: {err}");
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
        assert!(!r.content.contains("Do not guess another filename"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_many_all_missing_gives_one_root_inventory_repair() {
        let (dir, c) = ctx();
        let r = ReadManyTool
            .execute(
                &c,
                json!({"paths": ["handbook/customer.md", "data/incidents.csv"]}),
            )
            .await
            .unwrap();
        assert!(r.content.contains("No requested path exists"));
        assert!(r.content.contains("Do not guess another filename"));
        assert!(r.content.contains("`pattern: \"*\"`"));
        assert!(r.content.contains("no `path`"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-66: the old `path_list` extraction (`filter_map`) silently dropped any non-string element
    /// from `paths` instead of failing. `ReadManyInput.paths: Vec<String>` now deserializes strictly —
    /// a non-string element hard-errors instead of being silently skipped. Documented, intentional
    /// tightening (matches the `additionalProperties: false` precedent); see drift-reports.md D-66.
    #[tokio::test]
    async fn read_many_rejects_non_string_path_element() {
        let (dir, c) = ctx();
        let err = ReadManyTool
            .execute(&c, json!({"paths": ["a.txt", 5]}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid arguments"), "got: {err}");
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
        // C-10: the canonical value is a JSON ARRAY (list-consuming plan nodes compose on it)…
        let files: Vec<String> =
            serde_json::from_str(&r.content).expect("glob content must be a JSON array of paths");
        assert!(files.iter().any(|f| f == "src/main.rs"));
        assert!(files.iter().any(|f| f == "src/lib.rs"));
        assert!(!files.iter().any(|f| f == "README.md"));
        // …while the model-facing view stays the readable joined lines.
        let view = r.view.expect("glob carries a readable view");
        assert!(view.contains("src/main.rs") && !view.contains('['));

        let none = GlobTool
            .execute(&c, json!({"pattern": "*.py"}))
            .await
            .unwrap();
        assert_eq!(none.content, "no files match");

        let wildcard_path = GlobTool
            .execute(&c, json!({"pattern": "**/*", "path": "src/*"}))
            .await
            .unwrap();
        assert!(wildcard_path.is_error);
        assert!(wildcard_path.content.contains("literal directory"));
        assert!(wildcard_path.content.contains("omit `path`"));
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

    /// A `ctx()` workspace with a git repo initialized in it (deterministic `main` default branch,
    /// fixed identity so `git commit` doesn't depend on the host's global config).
    fn git_ctx() -> (std::path::PathBuf, ToolContext) {
        let (dir, c) = ctx();
        let sh = |args: &[&str]| {
            let ok = std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&dir)
                .status()
                .unwrap()
                .success();
            assert!(ok, "command failed: {args:?}");
        };
        sh(&["git", "init", "-q", "-b", "main"]);
        sh(&["git", "config", "user.email", "test@example.com"]);
        sh(&["git", "config", "user.name", "flux-tools tests"]);
        (dir, c)
    }

    /// D-66: end-to-end coverage for the git ops converted to `parse_params` in this tranche
    /// (`git_stage`, `git_commit`, `git_status`, `git_diff`, `git_unstage`, `git_checkout`) — none had
    /// direct execute-level tests before (only a name assertion in `builtins_register`), so this pins
    /// their behavior for the conversion.
    #[tokio::test]
    async fn git_ops_stage_commit_status_diff_unstage_checkout() {
        let (dir, c) = git_ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "one\n"}))
            .await
            .unwrap();

        // git_stage
        let r = GitStageTool
            .execute(&c, json!({"paths": ["a.txt"]}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);

        // git_status shows the staged file.
        let r = GitStatusTool.execute(&c, json!({})).await.unwrap();
        assert!(r.content.contains("a.txt"), "got: {}", r.content);

        // git_commit (message + body).
        let r = GitCommitTool
            .execute(&c, json!({"message": "add a.txt", "body": "details here"}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);

        // Modify the file — unstaged diff shows it.
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "one\ntwo\n"}))
            .await
            .unwrap();
        let r = GitDiffTool.execute(&c, json!({})).await.unwrap();
        assert!(r.content.contains("+two"), "got: {}", r.content);

        // Stage it, then a staged diff shows it too.
        GitStageTool
            .execute(&c, json!({"paths": ["a.txt"]}))
            .await
            .unwrap();
        let r = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert!(r.content.contains("+two"), "got: {}", r.content);

        // git_unstage removes it from the index; the staged diff is empty again.
        let r = GitUnstageTool
            .execute(&c, json!({"paths": ["a.txt"]}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let r = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert_eq!(r.content, "no changes", "got: {}", r.content);

        // git_checkout creates and switches to a new branch.
        let r = GitCheckoutTool
            .execute(&c, json!({"branch": "feature", "create": true}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let out = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "feature");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-218: `git diff` must not honour repository/global external diff configuration. The op is
    /// low-risk because its fixed argv is expected to fix its behaviour as well as its executable.
    #[cfg(unix)]
    #[tokio::test]
    async fn git_diff_never_executes_an_external_diff_driver() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, c) = git_ctx();
        WriteTool
            .execute(&c, json!({"path": "tracked.txt", "content": "before\n"}))
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["tracked.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "baseline"}))
            .await
            .unwrap();
        WriteTool
            .execute(&c, json!({"path": "tracked.txt", "content": "after\n"}))
            .await
            .unwrap();

        let marker = dir.join("external-driver-ran");
        let driver = dir.join("external-diff-driver.sh");
        std::fs::write(
            &driver,
            format!("#!/bin/sh\ntouch -- {}\n", marker.display()),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&driver).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&driver, permissions).unwrap();
        assert!(std::process::Command::new("git")
            .args(["config", "diff.external", driver.to_str().unwrap()])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());

        let result = GitDiffTool.execute(&c, json!({})).await.unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(
            !marker.exists(),
            "git_diff executed configured diff.external at {}",
            driver.display()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-218 closure: `--no-ext-diff` does not disable a default-enabled textconv driver. Pin that
    /// second Git execution seam off explicitly as well.
    #[cfg(unix)]
    #[tokio::test]
    async fn git_diff_never_executes_a_textconv_driver() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, c) = git_ctx();
        WriteTool
            .execute(&c, json!({"path": "tracked.txt", "content": "before\n"}))
            .await
            .unwrap();
        WriteTool
            .execute(
                &c,
                json!({"path": ".gitattributes", "content": "tracked.txt diff=marker\n"}),
            )
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["tracked.txt", ".gitattributes"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "baseline"}))
            .await
            .unwrap();
        WriteTool
            .execute(&c, json!({"path": "tracked.txt", "content": "after\n"}))
            .await
            .unwrap();

        let marker = dir.join("textconv-driver-ran");
        let driver = dir.join("textconv-driver.sh");
        std::fs::write(
            &driver,
            format!("#!/bin/sh\ntouch -- {}\ncat -- \"$1\"\n", marker.display()),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&driver).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&driver, permissions).unwrap();
        assert!(std::process::Command::new("git")
            .args(["config", "diff.marker.textconv", driver.to_str().unwrap()])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());

        let result = GitDiffTool.execute(&c, json!({})).await.unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(!marker.exists(), "git_diff executed configured textconv");
        assert!(
            hunk_diff_argv("tracked.txt", 3)
                .iter()
                .any(|argument| argument == "--no-textconv"),
            "hunk diff argv must close the same textconv seam"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pull the `[h1-…]` selectors out of a `git_hunks` listing, in order.
    fn hunk_ids(listing: &str) -> Vec<String> {
        listing
            .lines()
            .filter_map(|l| l.strip_prefix('['))
            .filter_map(|l| l.split_once(']').map(|(id, _)| id.to_string()))
            .collect()
    }

    /// A committed 20-line baseline plus two independent edits — ours near the top, a coworker's
    /// near the bottom, far enough apart that `git diff -U3` reports them as two hunks.
    async fn split_author_file(c: &ToolContext) {
        let base: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        WriteTool
            .execute(c, json!({"path": "shared.txt", "content": base}))
            .await
            .unwrap();
        GitStageTool
            .execute(c, json!({"paths": ["shared.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(c, json!({"message": "baseline"}))
            .await
            .unwrap();
        let edited = base
            .replace("line 2\n", "line 2 OURS\n")
            .replace("line 18\n", "line 18 THEIRS\n");
        WriteTool
            .execute(c, json!({"path": "shared.txt", "content": edited}))
            .await
            .unwrap();
    }

    /// C-92, the motivating case: two authors edit one file and the agent stages **only its own**
    /// hunk. Whole-file `git_stage` cannot express this — it sweeps the coworker's in-flight hunk
    /// into the agent's commit, which is the incident that filed the story.
    #[tokio::test]
    async fn staging_one_hunk_leaves_the_other_authors_hunk_in_the_working_tree() {
        let (dir, c) = git_ctx();
        split_author_file(&c).await;

        // The read op surfaces two addressable hunks.
        let listed = GitHunksTool
            .execute(&c, json!({"path": "shared.txt"}))
            .await
            .unwrap();
        assert!(!listed.is_error, "{}", listed.content);
        let ids = hunk_ids(&listed.content);
        assert_eq!(ids.len(), 2, "expected two hunks, got:\n{}", listed.content);

        // Stage ONLY ours.
        let staged = GitStageHunksTool
            .execute(&c, json!({"path": "shared.txt", "hunks": [&ids[0]]}))
            .await
            .unwrap();
        assert!(!staged.is_error, "{}", staged.content);

        // The index holds exactly our hunk …
        let idx = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert!(
            idx.content.contains("+line 2 OURS"),
            "index is missing our hunk:\n{}",
            idx.content
        );
        assert!(
            !idx.content.contains("THEIRS"),
            "index swept the coworker's hunk:\n{}",
            idx.content
        );

        // … and the coworker's stays in the working tree, untouched.
        let wt = GitDiffTool.execute(&c, json!({})).await.unwrap();
        assert!(
            wt.content.contains("+line 18 THEIRS"),
            "the coworker's hunk was lost:\n{}",
            wt.content
        );
        assert!(
            !wt.content.contains("OURS"),
            "our hunk is still unstaged:\n{}",
            wt.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-92, the mirror case: staging only the **later** hunk. This is the one that exercises
    /// line-number drift — dropping the earlier hunk from the patch leaves every following hunk's
    /// `@@` new-side start stale, so it proves the reassembled patch is located by preimage
    /// context rather than by the counts we copied.
    #[tokio::test]
    async fn staging_only_the_later_hunk_still_applies_despite_the_shifted_line_numbers() {
        let (dir, c) = git_ctx();
        split_author_file(&c).await;

        let listed = GitHunksTool
            .execute(&c, json!({"path": "shared.txt"}))
            .await
            .unwrap();
        let ids = hunk_ids(&listed.content);
        assert_eq!(ids.len(), 2, "expected two hunks, got:\n{}", listed.content);

        let staged = GitStageHunksTool
            .execute(&c, json!({"path": "shared.txt", "hunks": [&ids[1]]}))
            .await
            .unwrap();
        assert!(!staged.is_error, "{}", staged.content);

        let idx = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert!(
            idx.content.contains("+line 18 THEIRS") && !idx.content.contains("OURS"),
            "the index should hold only the later hunk:\n{}",
            idx.content
        );
        let wt = GitDiffTool.execute(&c, json!({})).await.unwrap();
        assert!(
            wt.content.contains("+line 2 OURS") && !wt.content.contains("THEIRS"),
            "the earlier hunk should still be unstaged:\n{}",
            wt.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-92: a selector that no longer names a live hunk comes back as a **repairable**
    /// `ToolResult` error naming the stale id and the op to re-run — the C-32 guidance pattern —
    /// never a plan-halting `Err`, and never a partial stage.
    #[tokio::test]
    async fn stage_hunks_refuses_cleanly_when_the_file_changed_underneath() {
        let (dir, c) = git_ctx();
        split_author_file(&c).await;

        let listed = GitHunksTool
            .execute(&c, json!({"path": "shared.txt"}))
            .await
            .unwrap();
        let ids = hunk_ids(&listed.content);
        assert_eq!(ids.len(), 2, "expected two hunks, got:\n{}", listed.content);

        // The coworker saves again: our hunk's own content changes, so its content-derived id is
        // no longer the id of anything in the tree.
        let moved: String = (1..=20)
            .map(|n| match n {
                2 => "line 2 REWRITTEN BY SOMEONE ELSE\n".to_string(),
                18 => "line 18 THEIRS\n".to_string(),
                _ => format!("line {n}\n"),
            })
            .collect();
        WriteTool
            .execute(&c, json!({"path": "shared.txt", "content": moved}))
            .await
            .unwrap();

        let r = GitStageHunksTool
            .execute(&c, json!({"path": "shared.txt", "hunks": [&ids[0]]}))
            .await
            .unwrap();
        assert!(
            r.is_error,
            "a stale selector must fail, not stage something else:\n{}",
            r.content
        );
        assert!(
            r.content.contains(&ids[0]) && r.content.contains("git_hunks"),
            "the refusal must name the stale id and the repair op:\n{}",
            r.content
        );

        // Nothing was staged — the refusal is all-or-nothing.
        let idx = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert_eq!(
            idx.content, "no changes",
            "the index moved:\n{}",
            idx.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-92: both new ops declare a coherent spec (`flux_spec::metadata_violations`, which since
    /// C-210 reads `semantic_effects` too) and a **non-empty, path-scoped** subject — an unscoped
    /// write would otherwise match a `*` path grant.
    #[test]
    fn the_hunk_ops_declare_coherent_path_scoped_metadata() {
        let params = json!({"path": "src/engine.rs", "hunks": ["h1-0123456789abcdef"]});
        for tool in [
            Box::new(GitHunksTool) as Box<dyn Tool>,
            Box::new(GitStageHunksTool) as Box<dyn Tool>,
        ] {
            let spec = tool.spec();
            let violations = flux_spec::metadata_violations(&spec, &tool.semantic_effects());
            assert!(
                violations.is_empty(),
                "{} is incoherent: {violations:?}",
                spec.name
            );
            assert_eq!(
                tool.permission_subjects(&params),
                vec!["src/engine.rs".to_string()],
                "{} must scope approval to the named path",
                spec.name
            );
            assert!(
                !tool.intents(&params).intents.is_empty(),
                "{} must declare its intent",
                spec.name
            );
        }
    }

    /// C-92 rework, the blocking case: a zero-context diff carries no context lines, so `git apply`
    /// anchors on line numbers alone and silently stages a pure insertion at the **wrong offset**
    /// while exiting 0. Reproduced against git 2.55.0: a hunk requested at line 6 of a 30-line file
    /// landed at EOF. The op must refuse the radius rather than stage anything at all.
    ///
    /// No earlier test passed a non-default `context`, which is exactly why the gate stayed green
    /// over the defect.
    #[tokio::test]
    async fn zero_context_is_refused_instead_of_staging_at_the_wrong_position() {
        let (dir, c) = git_ctx();
        // A committed 30-line baseline with one insertion near the top and a coworker's far below,
        // the shape that misapplies: at context 0 the first hunk landed past line 30.
        let base: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        WriteTool
            .execute(&c, json!({"path": "wide.txt", "content": base}))
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["wide.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "baseline"}))
            .await
            .unwrap();
        let edited = base
            .replace("line 5\n", "line 5\nINSERT-OURS\n")
            .replace("line 20\n", "line 20\nINSERT-THEIRS\n");
        WriteTool
            .execute(&c, json!({"path": "wide.txt", "content": edited}))
            .await
            .unwrap();

        // The read op refuses the radius outright, so no id is ever minted at context 0.
        let listed = GitHunksTool
            .execute(&c, json!({"path": "wide.txt", "context": 0}))
            .await
            .unwrap();
        assert!(
            listed.is_error,
            "git_hunks must refuse context 0, got:\n{}",
            listed.content
        );
        assert!(
            listed.content.contains("at least 1"),
            "the refusal must name the floor: {}",
            listed.content
        );

        // And the staging op refuses independently — it is reachable with ids listed at another
        // radius, so it cannot rely on the read op having screened the value.
        let ids = hunk_ids(
            &GitHunksTool
                .execute(&c, json!({"path": "wide.txt", "context": 1}))
                .await
                .unwrap()
                .content,
        );
        assert_eq!(ids.len(), 2, "context 1 should still split the two edits");
        let staged = GitStageHunksTool
            .execute(
                &c,
                json!({"path": "wide.txt", "hunks": [&ids[0]], "context": 0}),
            )
            .await
            .unwrap();
        assert!(
            staged.is_error,
            "git_stage_hunks must refuse context 0, got:\n{}",
            staged.content
        );

        // The decisive assertion: nothing reached the index. At HEAD this failed with
        // `INSERT-OURS` staged at end-of-file, a position the caller never selected.
        let idx = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert!(
            !idx.content.contains("INSERT-OURS"),
            "a refused radius must stage nothing, but the index holds:\n{}",
            idx.content
        );
        drop(dir);
    }

    /// C-92 rework: the same file staged at a *legal* non-default radius must land where the
    /// caller pointed. Guards the fix from being "refuse everything" — context 1 is the finest
    /// radius that still gives `git apply` something to verify against.
    #[tokio::test]
    async fn staging_at_the_minimum_context_lands_at_the_requested_position() {
        let (dir, c) = git_ctx();
        let base: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        WriteTool
            .execute(&c, json!({"path": "wide.txt", "content": base}))
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["wide.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "baseline"}))
            .await
            .unwrap();
        let edited = base
            .replace("line 5\n", "line 5\nINSERT-OURS\n")
            .replace("line 20\n", "line 20\nINSERT-THEIRS\n");
        WriteTool
            .execute(&c, json!({"path": "wide.txt", "content": edited}))
            .await
            .unwrap();

        let ids = hunk_ids(
            &GitHunksTool
                .execute(&c, json!({"path": "wide.txt", "context": 1}))
                .await
                .unwrap()
                .content,
        );
        let staged = GitStageHunksTool
            .execute(
                &c,
                json!({"path": "wide.txt", "hunks": [&ids[0]], "context": 1}),
            )
            .await
            .unwrap();
        assert!(!staged.is_error, "{}", staged.content);

        let idx = GitDiffTool
            .execute(&c, json!({"staged": true}))
            .await
            .unwrap();
        assert!(
            idx.content.contains("INSERT-OURS") && !idx.content.contains("INSERT-THEIRS"),
            "only our hunk should be staged:\n{}",
            idx.content
        );
        // Position, not just presence: the insertion belongs right after `line 5`, and the hunk
        // header must name a low line number rather than the end of the file.
        let header = idx
            .content
            .lines()
            .find(|l| l.starts_with("@@"))
            .unwrap_or_default()
            .to_string();
        let old_start: u32 = header
            .split(['-', ','])
            .nth(1)
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(u32::MAX);
        assert!(
            old_start < 10,
            "the hunk must land near line 5, not at EOF — header was `{header}`"
        );
        drop(dir);
    }

    /// C-92 rework: the diff preamble rides along with whichever hunks are selected, so a header
    /// describing a whole-file change would be honoured even though only content hunks were
    /// picked. Deleting the file makes `git diff` emit `deleted file mode`; staging "one hunk" of
    /// that would stage the deletion the caller never selected.
    #[tokio::test]
    async fn whole_file_deletion_is_refused_rather_than_staged_as_a_hunk() {
        let (dir, c) = git_ctx();
        let base: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        WriteTool
            .execute(&c, json!({"path": "doomed.txt", "content": base}))
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["doomed.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "baseline"}))
            .await
            .unwrap();
        std::fs::remove_file(dir.join("doomed.txt")).unwrap();

        let listed = GitHunksTool
            .execute(&c, json!({"path": "doomed.txt"}))
            .await
            .unwrap();
        let ids = hunk_ids(&listed.content);
        if let Some(first) = ids.first() {
            let staged = GitStageHunksTool
                .execute(&c, json!({"path": "doomed.txt", "hunks": [first]}))
                .await
                .unwrap();
            assert!(
                staged.is_error,
                "staging a hunk of a deleted file must be refused, got:\n{}",
                staged.content
            );
            assert!(
                staged.content.contains("whole-file"),
                "the refusal should explain why: {}",
                staged.content
            );
            let idx = GitDiffTool
                .execute(&c, json!({"staged": true}))
                .await
                .unwrap();
            assert!(
                !idx.content.contains("deleted file"),
                "nothing should have been staged:\n{}",
                idx.content
            );
        }
        drop(dir);
    }

    /// C-92 rework (minor): staging one hunk renumbers the ones after it, and the ordinal is part
    /// of the id — so a still-valid selection can go stale by *position* while its content is
    /// untouched. The refusal must say that, rather than blaming an edit nobody made.
    #[tokio::test]
    async fn renumbering_after_a_stage_is_reported_as_renumbering_not_as_an_edit() {
        let (dir, c) = git_ctx();
        split_author_file(&c).await;
        let ids = hunk_ids(
            &GitHunksTool
                .execute(&c, json!({"path": "shared.txt"}))
                .await
                .unwrap()
                .content,
        );
        assert_eq!(ids.len(), 2);
        // Stage the first; the second keeps its content but becomes ordinal 1.
        GitStageHunksTool
            .execute(&c, json!({"path": "shared.txt", "hunks": [&ids[0]]}))
            .await
            .unwrap();
        let stale = GitStageHunksTool
            .execute(&c, json!({"path": "shared.txt", "hunks": [&ids[1]]}))
            .await
            .unwrap();
        assert!(stale.is_error, "{}", stale.content);
        assert!(
            stale.content.contains("renumber"),
            "the refusal must name renumbering, not a phantom edit: {}",
            stale.content
        );
        drop(dir);
    }

    /// D-66: `git_push` end-to-end against a local bare "remote" (no network needed).
    #[tokio::test]
    async fn git_push_pushes_to_a_local_remote() {
        let (dir, c) = git_ctx();
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "one\n"}))
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["a.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "init"}))
            .await
            .unwrap();

        // A local bare repo stands in for a remote.
        let remote_dir =
            std::env::temp_dir().join(format!("flux-tools-remote-{}", std::process::id()));
        std::fs::remove_dir_all(&remote_dir).ok();
        assert!(std::process::Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&remote_dir)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&dir)
            .status()
            .unwrap()
            .success());

        let r = GitPushTool
            .execute(&c, json!({"remote": "origin", "branch": "main"}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);

        // The remote now has the pushed commit.
        let out = std::process::Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(&remote_dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "remote missing branch: {out:?}");

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&remote_dir).ok();
    }

    // -----------------------------------------------------------------------
    // git_worktree_enter / git_worktree_leave (C-98 / C-99)
    // -----------------------------------------------------------------------

    /// Run `git <args>` in `dir` via std::process (test scaffolding only), asserting success, and
    /// return trimmed stdout.
    fn raw_git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}{}",
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A `git_ctx()` with one initial commit on `main` (worktree ops require a born branch).
    fn worktree_ctx() -> (std::path::PathBuf, ToolContext) {
        let (dir, c) = git_ctx();
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "init"]);
        (dir, c)
    }

    /// C-98 + C-99: the full round trip — enter moves ONLY this context's root into the temp
    /// worktree (process cwd untouched), committed work merges back into `main` with `--no-ff`,
    /// and cleanup removes the worktree, the branch, and the session.
    #[tokio::test]
    async fn git_worktree_enter_leave_round_trip() {
        let (dir, c) = worktree_ctx();
        let cwd_before = std::env::current_dir().unwrap();
        let original_root = c.system().workspace().root().to_path_buf();

        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        let working_root = v["working_root"].as_str().unwrap().to_string();
        let branch = v["branch"].as_str().unwrap().to_string();
        assert!(branch.starts_with("flux/worktree/"), "branch: {branch}");
        assert!(
            v["note"].as_str().unwrap().contains(&working_root),
            "result must state the new working root prominently"
        );

        // The context's active system moved to the checkout; the process cwd did NOT.
        let session = c.workspace_context().worktree_session().unwrap();
        assert_eq!(c.system().workspace().root(), session.checkout);
        assert_ne!(c.system().workspace().root(), original_root);
        assert_eq!(std::env::current_dir().unwrap(), cwd_before);

        // Work in the worktree through the ACTIVE system: write + stage + commit.
        WriteTool
            .execute(
                &c,
                json!({"path": "feature.txt", "content": "from the worktree\n"}),
            )
            .await
            .unwrap();
        assert!(
            session.checkout.join("feature.txt").exists(),
            "the write landed in the worktree"
        );
        assert!(
            !dir.join("feature.txt").exists(),
            "the original checkout is untouched"
        );
        let r = GitStageTool
            .execute(&c, json!({"paths": ["feature.txt"]}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let r = GitCommitTool
            .execute(&c, json!({"message": "add feature.txt"}))
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);

        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(
            v["restored_root"].as_str().unwrap(),
            original_root.display().to_string()
        );

        // The context is restored and the session cleared.
        assert!(c.workspace_context().worktree_session().is_none());
        assert_eq!(c.system().workspace().root(), original_root);

        // `main` contains the work via a real merge commit; worktree + branch are gone.
        assert!(dir.join("feature.txt").exists());
        let merges = raw_git(&dir, &["log", "--oneline", "--merges"]);
        assert!(
            !merges.is_empty(),
            "a --no-ff merge commit exists: {merges}"
        );
        assert_eq!(
            v["merge_commit"].as_str().unwrap(),
            raw_git(&dir, &["rev-parse", "HEAD"])
        );
        assert!(!session.parent_dir.exists(), "temp worktree dir removed");
        assert_eq!(raw_git(&dir, &["branch", "--list", &branch]), "");
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-98: a dirty checkout is rejected — no branch, no worktree, no session.
    #[tokio::test]
    async fn git_worktree_enter_rejects_dirty_main() {
        let (dir, c) = worktree_ctx();
        std::fs::write(dir.join("base.txt"), "modified\n").unwrap();
        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("uncommitted changes"), "{}", r.content);
        assert!(c.workspace_context().worktree_session().is_none());
        assert_eq!(raw_git(&dir, &["branch", "--list", "flux/worktree/*"]), "");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-98: entering from any branch but `main` is rejected.
    #[tokio::test]
    async fn git_worktree_enter_rejects_non_main_branch() {
        let (dir, c) = worktree_ctx();
        raw_git(&dir, &["checkout", "-q", "-b", "feature"]);
        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("requires `main`"), "{}", r.content);
        assert!(c.workspace_context().worktree_session().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-98: nesting is rejected — a second enter while a session is active is a recoverable
    /// error and leaves the active session untouched.
    #[tokio::test]
    async fn git_worktree_enter_rejects_nested_sessions() {
        let (dir, c) = worktree_ctx();
        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        let session = c.workspace_context().worktree_session().unwrap();

        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("already active"), "{}", r.content);
        let still = c.workspace_context().worktree_session().unwrap();
        assert_eq!(still.branch, session.branch);
        assert_eq!(c.system().workspace().root(), session.checkout);

        // Clean up through the real path.
        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-99: a dirty worktree blocks leave (never auto-stages/commits); the context stays in the
    /// worktree, and after committing the same leave succeeds.
    #[tokio::test]
    async fn git_worktree_leave_rejects_dirty_worktree() {
        let (dir, c) = worktree_ctx();
        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        WriteTool
            .execute(&c, json!({"path": "wip.txt", "content": "not committed\n"}))
            .await
            .unwrap();

        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("uncommitted changes"), "{}", r.content);
        let session = c.workspace_context().worktree_session().unwrap();
        assert_eq!(session.phase, flux_runtime::WorktreePhase::Active);
        assert_eq!(c.system().workspace().root(), session.checkout);

        // Commit, then leave succeeds.
        GitStageTool
            .execute(&c, json!({"paths": ["wip.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "wip"}))
            .await
            .unwrap();
        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-99: leave without a session is a recoverable error.
    #[tokio::test]
    async fn git_worktree_leave_requires_a_session() {
        let (dir, c) = worktree_ctx();
        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("no worktree session"), "{}", r.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-99: if original `main` moved since enter, leave refuses to merge; the context stays in
    /// the worktree and the original checkout is untouched.
    #[tokio::test]
    async fn git_worktree_leave_rejects_moved_main() {
        let (dir, c) = worktree_ctx();
        let r = GitWorktreeEnterTool.execute(&c, json!({})).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        let session = c.workspace_context().worktree_session().unwrap();

        // Commit in the worktree...
        WriteTool
            .execute(&c, json!({"path": "wt.txt", "content": "worktree\n"}))
            .await
            .unwrap();
        GitStageTool
            .execute(&c, json!({"paths": ["wt.txt"]}))
            .await
            .unwrap();
        GitCommitTool
            .execute(&c, json!({"message": "worktree change"}))
            .await
            .unwrap();
        // ...and independently move original `main`.
        std::fs::write(dir.join("main-moved.txt"), "moved\n").unwrap();
        raw_git(&dir, &["add", "main-moved.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "main moved"]);
        let main_head = raw_git(&dir, &["rev-parse", "HEAD"]);

        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("has moved"), "{}", r.content);
        // Still in the worktree, phase Active; `main` untouched by the refused leave.
        let still = c.workspace_context().worktree_session().unwrap();
        assert_eq!(still.phase, flux_runtime::WorktreePhase::Active);
        assert_eq!(c.system().workspace().root(), session.checkout);
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), main_head);

        std::fs::remove_dir_all(&session.parent_dir).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-99: a genuinely conflicting merge is caught by the no-commit trial merge and aborted —
    /// `main` is never stranded conflicted, and the context stays in the worktree. (Reached by
    /// hand-building the session: through the public op path the moved-`main` guard fires first,
    /// so the trial merge is the defense-in-depth layer this pins.)
    #[tokio::test]
    async fn git_worktree_leave_trial_merge_conflict_aborts_cleanly() {
        let (dir, c) = worktree_ctx();
        let base = raw_git(&dir, &["rev-parse", "HEAD"]);

        // A real worktree branched at base, with a conflicting edit committed.
        let parent = flux_system::allocate_worktree_dir().unwrap();
        let checkout = parent.join("checkout");
        let branch = "flux/worktree/conflict-test";
        raw_git(
            &dir,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &checkout.display().to_string(),
                &base,
            ],
        );
        std::fs::write(checkout.join("base.txt"), "worktree version\n").unwrap();
        raw_git(&checkout, &["add", "base.txt"]);
        raw_git(&checkout, &["commit", "-q", "-m", "worktree edit"]);

        // A conflicting edit committed on `main`.
        std::fs::write(dir.join("base.txt"), "main version\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "main edit"]);
        let main_head = raw_git(&dir, &["rev-parse", "HEAD"]);

        // A session whose base_commit matches current `main` (so preflights pass) but whose
        // branch conflicts — exactly what the trial merge exists to catch.
        let original = c.system();
        let checkout = checkout.canonicalize().unwrap();
        let rerooted = Arc::new(original.rerooted(&checkout).unwrap());
        c.workspace_context()
            .enter_worktree(
                flux_runtime::WorktreeSession {
                    original: original.clone(),
                    base_commit: main_head.clone(),
                    branch: branch.to_string(),
                    checkout: checkout.clone(),
                    parent_dir: parent.clone(),
                    phase: flux_runtime::WorktreePhase::Active,
                },
                rerooted,
            )
            .unwrap();

        let r = GitWorktreeLeaveTool.execute(&c, json!({})).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("would conflict"), "{}", r.content);

        // The trial was aborted: `main` clean, HEAD unchanged, no merge in progress; the context
        // is still in the worktree with the session intact (phase Active — nothing merged).
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), main_head);
        assert!(
            !dir.join(".git/MERGE_HEAD").exists(),
            "no merge in progress"
        );
        let still = c.workspace_context().worktree_session().unwrap();
        assert_eq!(still.phase, flux_runtime::WorktreePhase::Active);
        assert_eq!(c.system().workspace().root(), checkout);

        std::fs::remove_dir_all(&parent).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    // -----------------------------------------------------------------------
    // git_branch / git_merge (C-238)
    //
    // Driven through the registry by op NAME — the same surface a Program's `call` uses — so the
    // journey compiles against the merge base and fails there on the ops' absence (the
    // failing-first), rather than naming the tool structs directly.
    // -----------------------------------------------------------------------

    /// Dispatch one op call through the registry by name, as the engine would.
    async fn call_op(
        registry: &ToolRegistry,
        c: &ToolContext,
        name: &str,
        params: Value,
    ) -> ToolResult {
        let tool = registry
            .get(name)
            .unwrap_or_else(|| panic!("op `{name}` is not registered"));
        tool.execute(c, params).await.unwrap()
    }

    /// C-238: the serial-integration journey — create a branch without leaving `main`, land work
    /// on it, merge it back with `--no-ff`, and assert the merge commit and the tree.
    #[tokio::test]
    async fn git_ops_branch_create_merge_no_ff_journey() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // git_branch creates the branch; HEAD stays on `main`.
        let r = call_op(&registry, &c, "git_branch", json!({"name": "impl/item-1"})).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            raw_git(&dir, &["branch", "--list", "impl/item-1"]),
            "impl/item-1"
        );
        assert_eq!(raw_git(&dir, &["branch", "--show-current"]), "main");

        // Concrete subjects name the branch (never `*`, never empty).
        let tool = registry.get("git_branch").unwrap();
        assert_eq!(
            tool.permission_subjects(&json!({"name": "impl/item-1"})),
            vec!["git_branch:impl/item-1".to_string()]
        );

        // Land work on the branch through the existing family.
        let r = call_op(
            &registry,
            &c,
            "git_checkout",
            json!({"branch": "impl/item-1"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        WriteTool
            .execute(
                &c,
                json!({"path": "feature.txt", "content": "from the branch\n"}),
            )
            .await
            .unwrap();
        let r = call_op(
            &registry,
            &c,
            "git_stage",
            json!({"paths": ["feature.txt"]}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let r = call_op(
            &registry,
            &c,
            "git_commit",
            json!({"message": "add feature.txt"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let r = call_op(&registry, &c, "git_checkout", json!({"branch": "main"})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            !dir.join("feature.txt").exists(),
            "main does not have the work yet"
        );

        // git_merge --no-ff: a real merge commit lands the work on `main`.
        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "impl/item-1", "no_ff": true}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let merges = raw_git(&dir, &["log", "--oneline", "--merges"]);
        assert!(
            !merges.is_empty(),
            "a --no-ff merge commit exists: {merges}"
        );
        assert!(
            dir.join("feature.txt").exists(),
            "the merge landed the work"
        );
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");

        // Concrete subjects name the merged ref.
        let tool = registry.get("git_merge").unwrap();
        assert_eq!(
            tool.permission_subjects(&json!({"branch": "impl/item-1"})),
            vec!["git_merge:impl/item-1".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-238: a conflicting merge is a clean recoverable error that NAMES the conflicting files,
    /// and the tree is left consistent — never silently half-merged.
    #[tokio::test]
    async fn git_merge_conflict_is_recoverable_and_names_the_files() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // A branch that edits the same line `main` will edit.
        raw_git(&dir, &["checkout", "-q", "-b", "impl/conflict"]);
        std::fs::write(dir.join("base.txt"), "branch version\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "branch edit"]);
        raw_git(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("base.txt"), "main version\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "main edit"]);
        let pre_merge = raw_git(&dir, &["rev-parse", "HEAD"]);

        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "impl/conflict", "no_ff": true}),
        )
        .await;
        assert!(
            r.is_error,
            "a conflict is a recoverable error, got: {}",
            r.content
        );
        assert!(
            r.content.contains("base.txt"),
            "the error names the conflicting file: {}",
            r.content
        );
        assert!(
            r.content.contains("aborted"),
            "the error states the merge was aborted: {}",
            r.content
        );

        // The tree is consistent: HEAD unchanged, no merge in progress, nothing half-applied.
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), pre_merge);
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");
        assert!(
            !dir.join(".git/MERGE_HEAD").exists(),
            "no merge in progress"
        );

        // Recoverable: align the branch's content with `main` and the same op call succeeds.
        raw_git(&dir, &["checkout", "-q", "impl/conflict"]);
        std::fs::write(dir.join("base.txt"), "main version\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "align with main"]);
        raw_git(&dir, &["checkout", "-q", "main"]);
        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "impl/conflict", "no_ff": true}),
        )
        .await;
        assert!(!r.is_error, "the retried merge succeeds: {}", r.content);
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Run git and return (success, stdout+stderr) WITHOUT asserting — for setup steps that are
    /// meant to fail, like the conflicting merge that puts the tree mid-merge below.
    fn raw_git_try(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        let body = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), body.trim().to_string())
    }

    /// C-238 (review round 1): a merge already in progress belongs to the USER, not to this op.
    ///
    /// `git merge` refuses to start at all while `MERGE_HEAD` exists, so a naive conflict path
    /// reads that PRE-EXISTING `MERGE_HEAD` as "this call conflicted" and runs `git merge --abort`,
    /// which discards a hand-resolved but uncommitted merge resolution. That is the invariant
    /// AGENTS.md holds hardest ("Protect the user's worktree … never reset, discard"), so the op
    /// must refuse up front and abort nothing.
    #[tokio::test]
    async fn git_merge_refuses_when_a_merge_is_already_in_progress() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // `side` edits base.txt, and so does `main` — merging `side` conflicts. `other` branches
        // off `main` before that edit and touches a different file, so it would merge cleanly.
        raw_git(&dir, &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("base.txt"), "side version\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "side edit"]);
        raw_git(&dir, &["checkout", "-q", "-b", "other", "main"]);
        std::fs::write(dir.join("other.txt"), "other\n").unwrap();
        raw_git(&dir, &["add", "other.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "other edit"]);
        raw_git(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("base.txt"), "main version\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "main edit"]);

        // The USER starts a merge, hits the conflict, and hand-resolves it WITHOUT committing.
        let (merged, _) = raw_git_try(&dir, &["merge", "side"]);
        assert!(!merged, "the setup merge is supposed to conflict");
        const RESOLVED: &str = "carefully hand-resolved content\n";
        std::fs::write(dir.join("base.txt"), RESOLVED).unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        let user_merge_head = std::fs::read_to_string(dir.join(".git/MERGE_HEAD")).unwrap();
        let pre_head = raw_git(&dir, &["rev-parse", "HEAD"]);

        // Now the op is asked to merge something else entirely.
        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "other", "no_ff": true}),
        )
        .await;

        // THE INVARIANT: the user's uncommitted resolution is still there.
        assert_eq!(
            std::fs::read_to_string(dir.join("base.txt")).unwrap(),
            RESOLVED,
            "the user's hand-resolved, uncommitted merge must survive; op said: {}",
            r.content
        );
        // Their merge is untouched: same MERGE_HEAD, same HEAD, still in progress.
        assert_eq!(
            std::fs::read_to_string(dir.join(".git/MERGE_HEAD")).unwrap(),
            user_merge_head,
            "the in-flight merge must not be aborted"
        );
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), pre_head);

        // And the refusal is a recoverable error that says why — not a success, and not a claim
        // that the merge of `other` conflicted (it never started).
        assert!(r.is_error, "refusal is a recoverable error: {}", r.content);
        assert!(
            r.content.contains("already in progress"),
            "the error explains a merge is in flight: {}",
            r.content
        );
        assert!(
            !r.content.contains("conflicted"),
            "it must not claim the merge of `other` conflicted: {}",
            r.content
        );

        // Recoverable: once the user concludes their own merge, the same call succeeds.
        raw_git(&dir, &["commit", "-q", "--no-edit"]);
        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "other", "no_ff": true}),
        )
        .await;
        assert!(!r.is_error, "the retried merge succeeds: {}", r.content);
        assert_eq!(
            std::fs::read_to_string(dir.join("base.txt")).unwrap(),
            RESOLVED,
            "and the resolution is still the committed content"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-238: `git_branch` deletes with the SAFE delete (`-d` — git itself refuses unmerged work
    /// and the checked-out branch), and rejects option/path-shaped names like `git_checkout`
    /// does (C-85).
    #[tokio::test]
    async fn git_branch_delete_and_name_guards() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // Create + delete round trip.
        let r = call_op(&registry, &c, "git_branch", json!({"name": "scratch"})).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(raw_git(&dir, &["branch", "--list", "scratch"]), "scratch");
        let r = call_op(
            &registry,
            &c,
            "git_branch",
            json!({"name": "scratch", "delete": true}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(raw_git(&dir, &["branch", "--list", "scratch"]), "");

        // Deleting the checked-out branch is refused (by git, surfaced cleanly).
        let r = call_op(
            &registry,
            &c,
            "git_branch",
            json!({"name": "main", "delete": true}),
        )
        .await;
        assert!(r.is_error, "{}", r.content);
        assert_eq!(raw_git(&dir, &["branch", "--show-current"]), "main");

        // Option/path-shaped names are rejected before git ever runs.
        for bad in ["-D", "--force", ".", "..", "a..b", ""] {
            let r = call_op(&registry, &c, "git_branch", json!({"name": bad})).await;
            assert!(r.is_error, "name {bad:?} must be refused");
        }
        assert_eq!(
            raw_git(&dir, &["branch", "--format=%(refname:short)"]),
            "main"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-238: the integration rule's recovery op — revert a `--no-ff` merge with `-m 1`. A NEW
    /// commit undoes the merge (never a reset, history preserved), and the tree is byte-identical
    /// to the pre-merge state.
    #[tokio::test]
    async fn git_revert_mainline_one_restores_the_pre_merge_tree() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // A branch with work, merged into `main` with --no-ff through the family's own ops.
        let r = call_op(&registry, &c, "git_branch", json!({"name": "impl/item-2"})).await;
        assert!(!r.is_error, "{}", r.content);
        let r = call_op(
            &registry,
            &c,
            "git_checkout",
            json!({"branch": "impl/item-2"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        WriteTool
            .execute(
                &c,
                json!({"path": "feature.txt", "content": "from the branch\n"}),
            )
            .await
            .unwrap();
        let r = call_op(
            &registry,
            &c,
            "git_stage",
            json!({"paths": ["feature.txt"]}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let r = call_op(
            &registry,
            &c,
            "git_commit",
            json!({"message": "add feature.txt"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let r = call_op(&registry, &c, "git_checkout", json!({"branch": "main"})).await;
        assert!(!r.is_error, "{}", r.content);

        let pre_merge_tree = raw_git(&dir, &["rev-parse", "HEAD^{tree}"]);
        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "impl/item-2", "no_ff": true}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let merge_sha = raw_git(&dir, &["rev-parse", "HEAD"]);
        assert_ne!(
            raw_git(&dir, &["rev-parse", "HEAD^{tree}"]),
            pre_merge_tree,
            "the merge changed the tree"
        );

        // The recovery: revert the merge with -m 1.
        let r = call_op(
            &registry,
            &c,
            "git_revert",
            json!({"commit": merge_sha, "mainline": 1}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);

        // The tree is byte-identical to the pre-merge state...
        assert_eq!(
            raw_git(&dir, &["rev-parse", "HEAD^{tree}"]),
            pre_merge_tree,
            "the revert restored the pre-merge tree"
        );
        // ...and history was NOT rewritten: HEAD is a NEW revert commit on top of the merge,
        // which is still reachable.
        assert_eq!(
            raw_git(&dir, &["rev-parse", "HEAD~1"]),
            merge_sha,
            "the revert sits on top of the merge — no reset, no rewrite"
        );
        let log = raw_git(&dir, &["log", "--oneline"]);
        assert!(log.contains("Revert"), "a new revert commit exists: {log}");
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");

        // Concrete subjects name the reverted commit (never `*`, never empty).
        let tool = registry.get("git_revert").unwrap();
        assert_eq!(
            tool.permission_subjects(&json!({"commit": "abc123"})),
            vec!["git_revert:abc123".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-238: a conflicting revert is a clean recoverable error naming the files, with the
    /// sequencer aborted and the tree consistent — same contract as `git_merge`.
    #[tokio::test]
    async fn git_revert_conflict_is_recoverable_and_names_the_files() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // Two stacked edits to the same line; reverting the FIRST conflicts with the second.
        std::fs::write(dir.join("base.txt"), "v1\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "v1"]);
        let first = raw_git(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("base.txt"), "v2\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "v2"]);
        let pre_revert = raw_git(&dir, &["rev-parse", "HEAD"]);

        let r = call_op(&registry, &c, "git_revert", json!({"commit": first})).await;
        assert!(
            r.is_error,
            "a conflicting revert is a recoverable error, got: {}",
            r.content
        );
        assert!(
            r.content.contains("base.txt"),
            "the error names the conflicting file: {}",
            r.content
        );
        assert!(
            r.content.contains("aborted"),
            "the error states the revert was aborted: {}",
            r.content
        );
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), pre_revert);
        assert_eq!(raw_git(&dir, &["status", "--porcelain"]), "");
        assert!(
            !dir.join(".git/REVERT_HEAD").exists(),
            "no revert in progress"
        );

        // A non-conflicting (plain, non-merge) revert succeeds and appends a new commit.
        let r = call_op(&registry, &c, "git_revert", json!({"commit": pre_revert})).await;
        assert!(!r.is_error, "the clean revert succeeds: {}", r.content);
        assert_eq!(
            std::fs::read_to_string(dir.join("base.txt")).unwrap(),
            "v1\n",
            "the v2 edit is undone"
        );

        // Option-shaped / empty commits are rejected before git ever runs.
        for bad in ["--no-commit", "-e", ""] {
            let r = call_op(&registry, &c, "git_revert", json!({"commit": bad})).await;
            assert!(r.is_error, "commit {bad:?} must be refused");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // -----------------------------------------------------------------------
    // C-249: the family's shared tree precondition
    // -----------------------------------------------------------------------

    /// A checkout dirtied in both ways at once: one tracked modification, one untracked file.
    /// Every clean-tree refusal has to be actionable for BOTH.
    fn dirty_both_ways(dir: &std::path::Path) {
        std::fs::write(dir.join("base.txt"), "locally modified\n").unwrap();
        std::fs::write(dir.join("scratch.txt"), "untracked\n").unwrap();
    }

    /// The refusal must name the two kinds of dirtiness separately and give each a remedy that
    /// actually clears it: `git status --porcelain` reports untracked (`??`) entries too, and a
    /// plain `git stash` leaves those exactly where they are — so an agent told to "commit or
    /// stash them first" retries and fails identically.
    fn assert_actionable_dirty_refusal(content: &str) {
        assert!(
            content.contains("Tracked changes (1)"),
            "the refusal counts the tracked modifications: {content}"
        );
        assert!(
            content.contains("base.txt"),
            "the refusal names the tracked file: {content}"
        );
        assert!(
            content.contains("Untracked files (1)"),
            "the refusal counts the untracked files separately: {content}"
        );
        assert!(
            content.contains("scratch.txt"),
            "the refusal names the untracked file: {content}"
        );
        assert!(
            content.contains("git stash -u") && content.contains("git clean -fd"),
            "untracked entries get advice that clears them: {content}"
        );
        assert!(
            content.contains("a plain `git stash` does NOT clear these"),
            "the refusal says why bare `git stash` is not enough: {content}"
        );
    }

    /// C-249: `git_revert` and `git_worktree_enter` refuse a dirty tree with the SAME true
    /// wording — one policy, one message, tracked and untracked told apart.
    #[tokio::test]
    async fn dirty_tree_refusals_are_reconciled_and_actionable() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);
        let head = raw_git(&dir, &["rev-parse", "HEAD"]);
        dirty_both_ways(&dir);

        let revert = call_op(&registry, &c, "git_revert", json!({"commit": head})).await;
        assert!(
            revert.is_error,
            "a dirty tree blocks revert: {}",
            revert.content
        );
        assert_actionable_dirty_refusal(&revert.content);

        let enter = call_op(&registry, &c, "git_worktree_enter", json!({})).await;
        assert!(
            enter.is_error,
            "a dirty tree blocks enter: {}",
            enter.content
        );
        assert_actionable_dirty_refusal(&enter.content);
        assert!(c.workspace_context().worktree_session().is_none());

        // Nothing was created on either refusal, and the caller's work is untouched.
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), head);
        assert_eq!(raw_git(&dir, &["branch", "--list", "flux/worktree/*"]), "");
        assert_eq!(
            std::fs::read_to_string(dir.join("base.txt")).unwrap(),
            "locally modified\n"
        );
        assert!(dir.join("scratch.txt").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-249: the clean-tree precondition is scoped to what is actually dangerous. `git_merge`
    /// declares `CleanTree::NotRequired` — git refuses a merge that would overwrite local changes
    /// and unrelated edits survive `merge --abort` — so it must NOT grow a reflexive dirty-tree
    /// refusal, which would break the multi-author case `git_stage_hunks` exists to serve.
    #[tokio::test]
    async fn git_merge_still_works_on_a_dirty_tree() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        raw_git(&dir, &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("side.txt"), "side\n").unwrap();
        raw_git(&dir, &["add", "side.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "side"]);
        raw_git(&dir, &["checkout", "-q", "main"]);

        // Uncommitted work on unrelated paths, of both kinds.
        dirty_both_ways(&dir);

        let r = call_op(
            &registry,
            &c,
            "git_merge",
            json!({"branch": "side", "no_ff": true}),
        )
        .await;
        assert!(
            !r.is_error,
            "a dirty tree must not block an otherwise safe merge: {}",
            r.content
        );
        assert!(dir.join("side.txt").exists(), "the merge landed");
        // And the caller's uncommitted work is exactly as it was.
        assert_eq!(
            std::fs::read_to_string(dir.join("base.txt")).unwrap(),
            "locally modified\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("scratch.txt")).unwrap(),
            "untracked\n"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-249 generalises C-238's merge invariant to the whole family: a `git revert` already in
    /// flight belongs to whoever started it. `git_revert`'s failure path ends in a blanket
    /// `git revert --abort`, so it must refuse up front rather than abort a hand-resolved,
    /// uncommitted revert — the same hazard `git_merge` was fixed for.
    #[tokio::test]
    async fn git_revert_refuses_when_a_revert_is_already_in_progress() {
        let (dir, c) = worktree_ctx();
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);

        // Two stacked edits to the same line: reverting the first conflicts with the second.
        std::fs::write(dir.join("base.txt"), "v1\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "v1"]);
        let first = raw_git(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("base.txt"), "v2\n").unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        raw_git(&dir, &["commit", "-q", "-m", "v2"]);
        let second = raw_git(&dir, &["rev-parse", "HEAD"]);

        // The USER starts that revert by hand, hits the conflict, and resolves it WITHOUT
        // committing — exactly the state a blanket abort would destroy.
        let (reverted, _) = raw_git_try(&dir, &["revert", "--no-edit", &first]);
        assert!(!reverted, "the setup revert is supposed to conflict");
        const RESOLVED: &str = "carefully hand-resolved content\n";
        std::fs::write(dir.join("base.txt"), RESOLVED).unwrap();
        raw_git(&dir, &["add", "base.txt"]);
        let user_revert_head = std::fs::read_to_string(dir.join(".git/REVERT_HEAD")).unwrap();

        let r = call_op(&registry, &c, "git_revert", json!({"commit": second})).await;

        // THE INVARIANT: the user's uncommitted resolution, and their revert, both survive.
        assert_eq!(
            std::fs::read_to_string(dir.join("base.txt")).unwrap(),
            RESOLVED,
            "the hand-resolved, uncommitted revert must survive; op said: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(".git/REVERT_HEAD")).unwrap(),
            user_revert_head,
            "the in-flight revert must not be aborted"
        );
        assert_eq!(raw_git(&dir, &["rev-parse", "HEAD"]), second);
        assert!(r.is_error, "refusal is a recoverable error: {}", r.content);
        assert!(
            r.content.contains("already in progress"),
            "the error explains a revert is in flight: {}",
            r.content
        );
        assert!(
            !r.content.contains("conflicted"),
            "it must not claim this call's revert conflicted — it never started: {}",
            r.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
