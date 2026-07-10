//! `flux-system` — the *only* place real filesystem/process/env IO happens.
//!
//! Every path is resolved against a [`Workspace`] root (plus optional `@named` roots) and is
//! rejected if it escapes — lexically (`..`) or via symlink (a path that canonicalizes outside
//! the root). Process execution is **argv-only** (no shell), so the model cannot inject shell
//! operators. Tools never touch `std::fs`/`Command` directly; they go through [`System`].

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flux_core::{Error, Result};

pub mod net;

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// Whether a filesystem permission subject will be used for a read or a write. Reads may resolve
/// through configured read-only roots; writes remain confined to writable workspace roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccess {
    Read,
    Write,
}

/// A bounded filesystem view: a primary root plus optional `@named` roots. All access is confined
/// to these roots.
///
/// Two access-widening knobs (C-21): `read_roots` are additional **read-only** roots — `resolve_read`
/// (reads/globs/greps) accepts a path under any of them, while `resolve` (writes) stays confined to the
/// primary root (+ named); `unconfined` lifts confinement entirely for both (the `--allow-all-paths` hatch).
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    named: HashMap<String, PathBuf>,
    /// Additional read-only roots reads may reach under; writes may not.
    read_roots: Vec<PathBuf>,
    /// When set, path confinement is lifted (read + write anywhere).
    unconfined: bool,
}

impl Workspace {
    /// Create a workspace rooted at `root` (canonicalized; must exist).
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|e| Error::Config(format!("workspace root: {e}")))?;
        Ok(Self {
            root,
            named: HashMap::new(),
            read_roots: Vec::new(),
            unconfined: false,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The additional read-only roots (C-21).
    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read_roots
    }

    /// Add a **read-only** allowed root (canonicalized; must exist) — reads/globs/greps may reach under
    /// it, writes stay confined to the primary root (C-21). Chainable.
    pub fn add_read_root(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let p = path
            .as_ref()
            .canonicalize()
            .map_err(|e| Error::Config(format!("read root {:?}: {e}", path.as_ref())))?;
        if !self.read_roots.contains(&p) {
            self.read_roots.push(p);
        }
        Ok(())
    }

    /// Lift path confinement entirely (read + write anywhere) — the explicit `--allow-all-paths` hatch.
    pub fn set_unconfined(&mut self, yes: bool) {
        self.unconfined = yes;
    }

    /// Whether confinement is lifted.
    pub fn is_unconfined(&self) -> bool {
        self.unconfined
    }

    /// Build a workspace at `cwd` and layer access-widening from the environment (C-21): `FLUX_ADD_DIRS`
    /// (a `:`-separated list of read-only roots) and `FLUX_ALLOW_ALL` (truthy → unconfined). This is the
    /// channel the CLI's `--add-dir`/`--allow-all-paths` flags export through, so `app run` and other
    /// in-process paths inherit the policy. A non-existent `FLUX_ADD_DIRS` entry is skipped (not fatal).
    pub fn from_env(cwd: impl AsRef<Path>) -> Result<Self> {
        let mut ws = Self::new(cwd)?;
        if let Ok(dirs) = std::env::var("FLUX_ADD_DIRS") {
            for d in dirs.split(':').filter(|s| !s.is_empty()) {
                let expanded = if let Some(rest) = d.strip_prefix('~') {
                    format!("{}{rest}", std::env::var("HOME").unwrap_or_default())
                } else {
                    d.to_string()
                };
                // Skip a missing/invalid extra root rather than failing the whole session.
                let _ = ws.add_read_root(&expanded);
            }
        }
        if env_truthy("FLUX_ALLOW_ALL") {
            ws.set_unconfined(true);
        }
        Ok(ws)
    }

    /// Register a `@name` root (canonicalized; must exist).
    pub fn add_named_root(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let p = path
            .as_ref()
            .canonicalize()
            .map_err(|e| Error::Config(format!("named root: {e}")))?;
        self.named.insert(name.into(), p);
        Ok(())
    }

    /// Whether a named root is configured.
    pub fn has_named_root(&self, name: &str) -> bool {
        self.named.contains_key(name)
    }

    /// Resolve a workspace-relative (or `@name/...`) path for a **write** — confined to the primary root
    /// and any `@named` root. Rejects `..` and symlink escapes. Use [`resolve_read`](Self::resolve_read)
    /// for reads, which additionally accepts the read-only roots (C-21).
    pub fn resolve(&self, input: &str) -> Result<PathBuf> {
        self.resolve_in(input, false)
    }

    /// Resolve a path for a **read** — confined to the primary root, any `@named` root, **or any read-only
    /// root** (C-21). Otherwise identical to [`resolve`](Self::resolve).
    pub fn resolve_read(&self, input: &str) -> Result<PathBuf> {
        self.resolve_in(input, true)
    }

    /// Return the physical permission identity for `input`, following every existing symlink while
    /// preserving a not-yet-existing tail for create paths. Workspace-relative inputs stay relative
    /// (so existing permission rules keep their shape); absolute inputs stay absolute; `@named`
    /// inputs retain that namespace when the physical target remains under the named root.
    pub fn path_identity(&self, input: &str, access: PathAccess) -> Result<String> {
        let resolved = match access {
            PathAccess::Read => self.resolve_read(input)?,
            PathAccess::Write => self.resolve(input)?,
        };
        let physical = canonicalize_existing_ancestor(&resolved)?;
        self.render_path_identity(input, &physical)
    }

    fn render_path_identity(&self, input: &str, physical: &Path) -> Result<String> {
        if let Some(rest) = input.strip_prefix('@') {
            let name = rest.split('/').next().unwrap_or(rest);
            if let Some(base) = self.named.get(name) {
                if let Ok(rel) = physical.strip_prefix(base) {
                    let rel = path_to_utf8(rel)?;
                    return Ok(if rel.is_empty() {
                        format!("@{name}")
                    } else {
                        format!("@{name}/{rel}")
                    });
                }
            }
        }

        let expanded = expand_home_input(input);
        if Path::new(expanded.as_ref()).is_absolute() {
            return path_to_utf8(physical);
        }
        if let Ok(rel) = physical.strip_prefix(&self.root) {
            let rel = path_to_utf8(rel)?;
            return Ok(if rel.is_empty() { ".".to_string() } else { rel });
        }
        // This is reachable only for an unconfined workspace or a named/read root whose namespace
        // could not be preserved. Keep the physical absolute identity rather than falling back to
        // the caller's alias.
        path_to_utf8(physical)
    }

    /// The shared resolver. `read_extra` widens the acceptable roots to include the read-only roots (the
    /// read path). When `unconfined` is set, confinement is lifted entirely.
    fn resolve_in(&self, input: &str, read_extra: bool) -> Result<PathBuf> {
        // A path containing a control byte (newline, CR, NUL, tab, …) is virtually always a
        // bug — typically an untrimmed command substitution flowing into the path, e.g.
        // `echo …` whose trailing newline becomes part of the filename. Such a file gets
        // created but is then unreadable by its apparent name: `glob` matches it via `*`,
        // yet every literal `read`/`stat` misses the hidden byte and fails with ENOENT.
        // Reject it loudly here instead of silently writing a poltergeist file.
        // Expand a leading `~` to the home directory so callers can write
        // `~/.flux/sessions.db` instead of needing the literal absolute path.
        let input = expand_home_input(input);
        let input = input.as_ref();

        if let Some(pos) = input.bytes().position(|b| b.is_ascii_control()) {
            return Err(Error::Config(format!(
                "path {input:?} contains a control byte (0x{:02x}) at offset {pos}; this is \
                 almost always an untrimmed value such as a trailing newline from `echo`",
                input.as_bytes()[pos]
            )));
        }

        let (base, rel) = self.base_for(input);

        let joined = if Path::new(rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            base.join(rel)
        };
        let norm = normalize_lexically(&joined);

        // The `--allow-all-paths` hatch: no root confinement, just the lexically-normalized path.
        if self.unconfined {
            return Ok(norm);
        }

        // Find the allowed root this path lives under: the primary root, any `@named` root, and — on the
        // read path — any read-only root (C-21).
        let mut container: Option<&PathBuf> = None;
        for r in std::iter::once(&self.root).chain(self.named.values()) {
            if norm.starts_with(r) {
                container = Some(r);
                break;
            }
        }
        if container.is_none() && read_extra {
            for r in &self.read_roots {
                if norm.starts_with(r) {
                    container = Some(r);
                    break;
                }
            }
        }
        let Some(base) = container else {
            return Err(Error::Config(format!(
                "path {input:?} escapes the workspace root {}",
                self.root.display()
            )));
        };

        // Symlink guard: walk the path component-by-component, chasing every symlink found in the
        // physically-existing prefix and rejecting any whose target escapes the matched root. Unlike
        // `Path::exists()` (which follows links, so a *dangling* symlink to an outside target reads
        // as "not existing"), this uses `symlink_metadata` and so also catches symlinks whose
        // targets don't exist yet — the case a plain parent-canonicalize misses on write.
        let resolved = resolve_within_root(base, &norm).map_err(|_| {
            Error::Config(format!("path {input:?} resolves outside the allowed roots"))
        })?;
        // A symlink target may itself contain an intermediate symlink. Canonicalizing the longest
        // existing ancestor catches that second-order alias while retaining a missing create tail.
        let physical = canonicalize_existing_ancestor(&resolved).map_err(|_| {
            Error::Config(format!("path {input:?} resolves outside the allowed roots"))
        })?;
        if !physical.starts_with(base) {
            return Err(Error::Config(format!(
                "path {input:?} resolves outside the allowed roots"
            )));
        }
        Ok(physical)
    }

    fn base_for<'a>(&self, input: &'a str) -> (PathBuf, &'a str) {
        if let Some(rest) = input.strip_prefix('@') {
            if let Some((name, tail)) = rest.split_once('/') {
                if let Some(base) = self.named.get(name) {
                    return (base.clone(), tail);
                }
            } else if let Some(base) = self.named.get(rest) {
                // A bare `@name` (no subpath) resolves to the named root itself.
                return (base.clone(), "");
            }
        }
        (self.root.clone(), input)
    }
}

fn expand_home_input(input: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = input.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let home = std::env::var("HOME").unwrap_or_default();
            return std::borrow::Cow::Owned(format!("{home}{rest}"));
        }
    }
    std::borrow::Cow::Borrowed(input)
}

fn path_to_utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Config(format!("resolved path {:?} is not valid UTF-8", path)))
}

/// Whether the environment variable `key` is set to a truthy value (`1`/`true`/`yes`/`on`).
/// The one owner of boolean `FLUX_*` env semantics: mere presence is NOT truthy, so an operator
/// exporting `FLUX_ALLOW_PRIVATE_NET=0` (or `FLUX_VERBOSE=false`) disables the signal instead of
/// silently enabling it.
pub fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Lexically normalize an absolute path (resolve `.`/`..` without touching the filesystem),
/// never popping above the root component.
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(pre) => out.push(pre.as_os_str().to_owned()),
            Component::RootDir => out.push(std::ffi::OsString::from("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                // Keep the leading root component; otherwise pop.
                if out.len() > 1 {
                    out.pop();
                }
            }
            Component::Normal(c) => out.push(c.to_owned()),
        }
    }
    let mut pb = PathBuf::new();
    for c in out {
        pb.push(c);
    }
    pb
}

/// Resolve `norm` (already lexically normalized and known to be under the canonical `base`) to a
/// real path, chasing every symlink encountered in the physically-existing prefix and rejecting
/// any hop that escapes `base`. The not-yet-existing tail (which therefore cannot contain symlinks)
/// is appended verbatim. This is the security boundary for writes: it catches dangling symlinks
/// that `Path::exists()` would skip.
fn resolve_within_root(base: &Path, norm: &Path) -> std::result::Result<PathBuf, ()> {
    let rel = norm.strip_prefix(base).map_err(|_| ())?;
    let mut real = base.to_path_buf();
    for comp in rel.components() {
        let mut node = real.join(comp.as_os_str());
        // Chase a chain of symlinks at this node, keeping every hop inside `base`.
        let mut hops = 0u32;
        while let Ok(meta) = std::fs::symlink_metadata(&node) {
            if !meta.file_type().is_symlink() {
                break;
            }
            hops += 1;
            if hops > 40 {
                return Err(()); // symlink loop / excessive indirection
            }
            let target = std::fs::read_link(&node).map_err(|_| ())?;
            let joined = if target.is_absolute() {
                target
            } else {
                node.parent().unwrap_or(base).join(target)
            };
            node = normalize_lexically(&joined);
            if !node.starts_with(base) {
                return Err(()); // symlink target escapes the workspace root
            }
        }
        real = node;
    }
    Ok(real)
}

/// Canonicalize the longest existing ancestor and append any missing tail unchanged. This gives
/// create paths the same physical identity as existing paths without requiring the leaf to exist.
fn canonicalize_existing_ancestor(path: &Path) -> Result<PathBuf> {
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut physical) => {
                for component in missing.iter().rev() {
                    physical.push(component);
                }
                return Ok(normalize_lexically(&physical));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(Error::Io(err));
                };
                missing.push(name.to_os_string());
                let Some(parent) = cursor.parent() else {
                    return Err(Error::Io(err));
                };
                cursor = parent;
            }
            Err(err) => return Err(Error::Io(err)),
        }
    }
}

const PROCESS_OUTPUT_CAP: usize = 1024 * 1024;
const OUTPUT_TRUNCATION_NOTICE: &str = "\n…[output truncated]";

/// Decode captured subprocess output, capping it at `max` bytes so a runaway command can't OOM the
/// host. Truncating a byte slice mid-codepoint is safe: `from_utf8_lossy` emits replacement chars
/// rather than panicking (unlike `String::truncate`, which panics off a char boundary).
#[cfg(test)]
fn capped_lossy(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let mut s = String::from_utf8_lossy(&bytes[..max]).into_owned();
        s.push_str(OUTPUT_TRUNCATION_NOTICE);
        s
    }
}

/// A stream captured while the child is running. `bytes` never exceeds [`PROCESS_OUTPUT_CAP`]; the
/// reader continues draining after the cap so a full pipe cannot deadlock the child.
struct BoundedCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

impl BoundedCapture {
    fn into_lossy(self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            text.push_str(OUTPUT_TRUNCATION_NOTICE);
        }
        text
    }
}

async fn capture_bounded<R>(mut reader: R) -> std::io::Result<BoundedCapture>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;

    let mut bytes = Vec::with_capacity(8192.min(PROCESS_OUTPUT_CAP));
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let room = PROCESS_OUTPUT_CAP.saturating_sub(bytes.len());
        let keep = read.min(room);
        bytes.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
    }
    Ok(BoundedCapture { bytes, truncated })
}

/// A process group created for one guarded child. On Unix every child starts as the leader of its
/// own group, allowing timeout/cancellation cleanup to stop descendants without signalling flux's
/// own process group. Other platforms retain direct-child `kill_on_drop` cleanup.
#[derive(Clone, Copy)]
struct ProcessGroup {
    #[cfg(unix)]
    id: Option<libc::pid_t>,
}

impl ProcessGroup {
    fn for_child(child: &tokio::process::Child) -> Self {
        #[cfg(unix)]
        {
            let id = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
            Self { id }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self {}
        }
    }

    fn terminate(self) {
        #[cfg(unix)]
        if let Some(id) = self.id.filter(|id| *id > 0) {
            // SAFETY: `id` is the positive PID of a child that `build_command` placed in a fresh
            // process group with PGID == PID. `SIGKILL` is used only for timeout/cancellation or to
            // clean up descendants after their direct parent has exited.
            let _ = unsafe { libc::killpg(id, libc::SIGKILL) };
        }
    }
}

/// Owns a child until it has been reaped. Its drop path is cancellation-safe: it terminates the
/// dedicated process group before Tokio's `kill_on_drop` handles the direct child.
struct GuardedChild {
    child: tokio::process::Child,
    group: ProcessGroup,
    reaped: bool,
}

impl GuardedChild {
    fn new(child: tokio::process::Child) -> Self {
        let group = ProcessGroup::for_child(&child);
        Self {
            child,
            group,
            reaped: false,
        }
    }

    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait().await;
        if status.is_ok() {
            self.reaped = true;
        }
        status
    }

    fn terminate_tree(&mut self) {
        self.group.terminate();
        let _ = self.child.start_kill();
    }

    fn terminate_descendants(&self) {
        self.group.terminate();
    }
}

impl Drop for GuardedChild {
    fn drop(&mut self) {
        if !self.reaped {
            self.terminate_tree();
        }
    }
}

enum ProcessStop {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    CallerDropped,
}

type CaptureTask = tokio::task::JoinHandle<std::io::Result<BoundedCapture>>;

async fn capture_result(task: Option<CaptureTask>, stream: &str) -> Result<String> {
    let Some(task) = task else {
        return Ok(String::new());
    };
    let captured = task
        .await
        .map_err(|e| Error::Other(format!("{stream} capture task failed: {e}")))?
        .map_err(|e| Error::Other(format!("read child {stream}: {e}")))?;
    Ok(captured.into_lossy())
}

async fn discard_capture(task: Option<CaptureTask>) {
    if let Some(task) = task {
        let _ = task.await;
    }
}

/// Drive one child independently of its caller's future. The result channel closing is an explicit
/// cancellation signal, so dropping/aborting `run*` still leaves this task alive long enough to
/// kill the process group and reap the direct child.
async fn drive_process(
    child: tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    program: String,
    timeout: Duration,
    mut result_tx: tokio::sync::oneshot::Sender<Result<ProcessOutput>>,
) {
    let stdout_task = stdout.map(|stream| tokio::spawn(capture_bounded(stream)));
    let stderr_task = stderr.map(|stream| tokio::spawn(capture_bounded(stream)));
    let mut child = GuardedChild::new(child);

    let stop = tokio::select! {
        _ = result_tx.closed() => ProcessStop::CallerDropped,
        _ = tokio::time::sleep(timeout) => ProcessStop::TimedOut,
        status = child.wait() => ProcessStop::Exited(status),
    };

    match stop {
        ProcessStop::Exited(Ok(status)) => {
            // A command that backgrounds work can exit while descendants still hold the pipes.
            // Stop that work before awaiting EOF; the direct child has already been reaped.
            child.terminate_descendants();
            let stdout = capture_result(stdout_task, "stdout").await;
            let stderr = capture_result(stderr_task, "stderr").await;
            let result = match (stdout, stderr) {
                (Ok(stdout), Ok(stderr)) => Ok(ProcessOutput {
                    stdout,
                    stderr,
                    exit_code: status.code().unwrap_or(-1),
                }),
                (Err(err), _) | (_, Err(err)) => Err(err),
            };
            let _ = result_tx.send(result);
        }
        ProcessStop::Exited(Err(err)) => {
            child.terminate_tree();
            let _ = child.wait().await;
            discard_capture(stdout_task).await;
            discard_capture(stderr_task).await;
            let _ = result_tx.send(Err(Error::Other(format!("wait {program}: {err}"))));
        }
        ProcessStop::TimedOut => {
            child.terminate_tree();
            let cleanup = child.wait().await;
            discard_capture(stdout_task).await;
            discard_capture(stderr_task).await;
            let message = match cleanup {
                Ok(_) => format!("command timed out after {}s", timeout.as_secs()),
                Err(err) => format!(
                    "command timed out after {}s (failed to reap {program}: {err})",
                    timeout.as_secs()
                ),
            };
            let _ = result_tx.send(Err(Error::Other(message)));
        }
        ProcessStop::CallerDropped => {
            child.terminate_tree();
            let _ = child.wait().await;
            discard_capture(stdout_task).await;
            discard_capture(stderr_task).await;
        }
    }
}

async fn await_process(
    child: tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    program: String,
    timeout: Duration,
) -> Result<ProcessOutput> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(drive_process(
        child, stdout, stderr, program, timeout, result_tx,
    ));
    result_rx
        .await
        .map_err(|_| Error::Other("process driver stopped without a result".to_string()))?
}

// ---------------------------------------------------------------------------
// System (guarded IO)
// ---------------------------------------------------------------------------

/// Captured output of a subprocess.
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// A bounded host-file read admitted by an explicit physical path scope.
#[derive(Debug, Clone)]
pub struct ScopedFileRead {
    pub bytes: Vec<u8>,
    pub size: u64,
    pub truncated: bool,
}

/// Liveness of a [`ManagedChild`] (non-blocking snapshot from [`ManagedChild::status`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildStatus {
    /// Whether the child is still running (no exit observed yet).
    pub running: bool,
    /// The exit code once the child has exited (`None` while running or if it was signalled).
    pub exit_code: Option<i32>,
}

/// Per-stream output cap for a [`ManagedChild`] — bounds the in-memory buffer a long-lived child can
/// accumulate between reads so a chatty process can't OOM the host. Matches the spirit of the
/// `run_with_env` output cap (here per managed stream, drained on each [`ManagedChild::read_output`]).
const MANAGED_OUTPUT_CAP: usize = 256 * 1024;

/// A host-managed background process spawned by [`System::spawn_background`] — a long-lived child
/// (e.g. `kubectl port-forward`) started in one call and stopped/queried in later ones.
///
/// stdout/stderr are continuously drained by background tasks into capped in-memory buffers, so the
/// child never blocks on a full pipe even if nothing reads it for a while.
/// [`read_output`](Self::read_output) drains what has accumulated, [`status`](Self::status) polls
/// liveness without blocking, and [`kill`](Self::kill) terminates the child and stops the drain
/// tasks. Dropping the handle kills the child (`kill_on_drop`).
pub struct ManagedChild {
    child: tokio::process::Child,
    group: Option<ProcessGroup>,
    stdout_buf: Arc<Mutex<Vec<u8>>>,
    stderr_buf: Arc<Mutex<Vec<u8>>>,
    stdout_task: Option<tokio::task::JoinHandle<()>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

impl ManagedChild {
    /// Drain and return whatever stdout/stderr has accumulated since the last call, clearing the
    /// buffers. Bytes are decoded with `from_utf8_lossy` (never panics off a UTF-8 boundary, the same
    /// guarantee as `String::from_utf8_lossy`; a multibyte codepoint straddling two reads degrades to a
    /// replacement char rather than erroring.
    pub fn read_output(&mut self) -> (String, String) {
        let out = drain_locked(&self.stdout_buf);
        let err = drain_locked(&self.stderr_buf);
        (
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    }

    /// Non-blocking liveness check (via `try_wait`): does not reap-block on a still-running child.
    pub fn status(&mut self) -> ChildStatus {
        match self.child.try_wait() {
            Ok(Some(es)) => {
                // Reaping makes the numeric PID available for reuse. Stop any descendants now,
                // then forget the group so a much later handle drop can never signal a reused ID.
                if let Some(group) = self.group.take() {
                    group.terminate();
                }
                ChildStatus {
                    running: false,
                    exit_code: es.code(),
                }
            }
            Ok(None) => ChildStatus {
                running: true,
                exit_code: None,
            },
            // A wait error (already reaped, etc.) → report not-running with an unknown code rather
            // than surfacing an error for a status poll.
            Err(_) => ChildStatus {
                running: false,
                exit_code: None,
            },
        }
    }

    /// Kill the child and abort the stdout/stderr drain tasks. Idempotent.
    pub fn kill(&mut self) {
        if let Some(group) = self.group {
            group.terminate();
        }
        let _ = self.child.start_kill();
        if let Some(t) = self.stdout_task.take() {
            t.abort();
        }
        if let Some(t) = self.stderr_task.take() {
            t.abort();
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Drain a shared byte buffer, returning its current contents and leaving it empty. Recovers from a
/// poisoned lock (the drain tasks only `extend`, so they can't poison, but be defensive).
fn drain_locked(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    let mut guard = buf.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *guard)
}

/// Continuously copy a child stream into `buf`, appending up to `cap` total bytes held at once. Once
/// the buffer is full (nothing has drained it yet), further bytes are discarded so a runaway child
/// can't grow host memory without bound. Runs as a spawned task; exits on EOF or read error.
async fn drain_stream<R>(mut reader: R, buf: Arc<Mutex<Vec<u8>>>, cap: usize)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break, // EOF
            Ok(n) => {
                let mut guard = buf.lock().unwrap_or_else(|e| e.into_inner());
                let room = cap.saturating_sub(guard.len());
                if room > 0 {
                    guard.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            Err(_) => break,
        }
    }
}

/// A host-managed **interactive** child whose stdin and stdout are piped back to the caller for a
/// bidirectional protocol (the plugin `flux.plugin.v1` NDJSON frames over stdin/stdout), with stderr
/// inherited so the child's diagnostics reach the terminal. Spawned through the same safety envelope
/// as every other flux subprocess (see [`System::spawn_interactive`]): argv-only, workspace-pinned
/// cwd, and a **cleared + allow-listed environment** — so the child cannot read the host's secrets.
/// `kill_on_drop`, so a dropped handle never leaks the process.
pub struct InteractiveChild {
    /// The child process handle (for `kill`/`wait`).
    pub child: tokio::process::Child,
    /// The child's stdin (the host writes request frames here).
    pub stdin: tokio::process::ChildStdin,
    /// The child's stdout (the host reads response frames here).
    pub stdout: tokio::process::ChildStdout,
}

/// A host-managed child wired to the Chrome DevTools **remote-debugging pipe**: a full-duplex socket
/// is mapped onto the child's fd 3 (it reads CDP commands) and fd 4 (it writes CDP responses/events),
/// and the parent's end is handed back as one [`tokio::net::UnixStream`]. Spawned through the same
/// safety envelope as every other flux subprocess (see [`System::spawn_debug_pipe`]): argv-only,
/// workspace-pinned cwd, cleared + allow-listed env. `kill_on_drop`, so a dropped handle never leaks
/// the process. Unix-only (the CDP pipe transport is a POSIX-fd mechanism).
#[cfg(unix)]
pub struct PipeChild {
    /// The child process handle (for `kill`/`wait`/reaping).
    pub child: tokio::process::Child,
    /// The parent end of the debug pipe — the host writes framed CDP commands and reads
    /// responses/events on this one full-duplex stream.
    pub pipe: tokio::net::UnixStream,
}

/// The guarded IO surface tools are given. All filesystem access is confined to the workspace;
/// process execution is argv-only.
#[derive(Debug, Clone)]
pub struct System {
    workspace: Workspace,
}

impl System {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Derive the physical permission identity for a caller-supplied workspace path.
    pub fn path_identity(&self, path: &str, access: PathAccess) -> Result<String> {
        self.workspace.path_identity(path, access)
    }

    /// Read a host file through an explicit exact, `/*`, or `/**` scope. Both the scope anchor and
    /// requested path are reduced to physical identities before matching, so a lexical in-scope
    /// symlink cannot reach an out-of-scope target. This is the guarded host-file seam used by
    /// plugin `fs.read`; it deliberately does not widen the workspace itself.
    pub async fn read_file_scoped(
        &self,
        path: &str,
        scope: &str,
        max_bytes: usize,
    ) -> Result<ScopedFileRead> {
        use tokio::io::AsyncReadExt as _;

        let requested = canonicalize_existing_ancestor(&self.host_path(path)?)?;
        let (scope_root, recursive, direct_children) = if let Some(root) = scope.strip_suffix("/**")
        {
            (root, true, false)
        } else if let Some(root) = scope.strip_suffix("/*") {
            (root, false, true)
        } else {
            (scope.trim_end_matches('/'), false, false)
        };
        let scope_root = canonicalize_existing_ancestor(&self.host_path(scope_root)?)?;
        let admitted = if recursive {
            requested == scope_root || requested.starts_with(&scope_root)
        } else if direct_children {
            requested
                .strip_prefix(&scope_root)
                .ok()
                .is_some_and(|rel| rel.components().count() == 1)
        } else {
            requested == scope_root
        };
        if !admitted {
            return Err(Error::Config(format!(
                "path {path:?} resolves outside scoped path {scope:?}"
            )));
        }

        let file = tokio::fs::File::open(&requested).await?;
        let metadata = file.metadata().await?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(usize::MAX)
                .min(max_bytes.saturating_add(1)),
        );
        let mut limited = file.take((max_bytes as u64).saturating_add(1));
        limited.read_to_end(&mut bytes).await?;
        let truncated = bytes.len() > max_bytes || metadata.len() > max_bytes as u64;
        bytes.truncate(max_bytes);
        Ok(ScopedFileRead {
            size: metadata.len().max(bytes.len() as u64),
            bytes,
            truncated,
        })
    }

    fn host_path(&self, input: &str) -> Result<PathBuf> {
        let expanded = expand_home_input(input);
        if let Some(pos) = expanded.bytes().position(|byte| byte.is_ascii_control()) {
            return Err(Error::Config(format!(
                "path {input:?} contains a control byte at offset {pos}"
            )));
        }
        let path = Path::new(expanded.as_ref());
        let joined;
        let path = if path.is_absolute() {
            path
        } else {
            joined = self.workspace.root().join(path);
            &joined
        };
        Ok(normalize_lexically(path))
    }

    /// Read a UTF-8 file from within the workspace (or any read-only root, C-21).
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let p = self.workspace.resolve_read(path)?;
        let bytes = tokio::fs::read(&p).await?;
        String::from_utf8(bytes).map_err(|_| Error::Other(format!("{path}: not valid UTF-8")))
    }

    /// Write a file within the workspace, creating parent directories (also confined).
    pub async fn write_file(&self, path: &str, contents: &str) -> Result<()> {
        let p = self.workspace.resolve(path)?;
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&p, contents).await?;
        Ok(())
    }

    /// Read the raw bytes of a file within the workspace (no UTF-8 decode). Used to sniff binary
    /// files (NUL bytes) and report byte sizes *before* a lossy text decode.
    pub async fn read_file_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let p = self.workspace.resolve_read(path)?;
        Ok(tokio::fs::read(&p).await?)
    }

    /// Byte size of a file within the workspace/read-roots — a metadata call, so a caller
    /// enforcing a size cap can skip an oversized file WITHOUT paying a whole-file read first.
    pub async fn file_size(&self, path: &str) -> Result<u64> {
        let p = self.workspace.resolve_read(path)?;
        Ok(tokio::fs::metadata(&p).await?.len())
    }

    /// Whether `path` (resolved within the workspace/read-roots) is a directory — lets a read tool
    /// give actionable guidance ("list it with glob first") instead of failing on the raw `Is a
    /// directory` io error (C-32). Read-only, so it uses the same `resolve_read` jail as
    /// `file_mtime`/`read_file_bytes`; a path that doesn't resolve (escapes the workspace) still
    /// errors loudly, but a path that simply doesn't exist yields `Ok(false)` — the caller's own
    /// read call remains the source of truth for "missing".
    pub async fn is_dir(&self, path: &str) -> Result<bool> {
        let p = self.workspace.resolve_read(path)?;
        Ok(tokio::fs::metadata(&p)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false))
    }

    /// Append text to a file within the workspace, creating it (and parent directories) if absent.
    /// Goes through the same `resolve()` jail as `write_file` (including the dangling-symlink guard)
    /// before opening.
    pub async fn append_file(&self, path: &str, contents: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let p = self.workspace.resolve(path)?;
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .await?;
        f.write_all(contents.as_bytes()).await?;
        Ok(())
    }

    /// Last-modification time of a file within the workspace. Used by the read-before-write guard to
    /// detect a file that changed on disk since the model last read it.
    pub async fn file_mtime(&self, path: &str) -> Result<std::time::SystemTime> {
        let p = self.workspace.resolve_read(path)?;
        let meta = tokio::fs::metadata(&p).await?;
        Ok(meta.modified()?)
    }

    /// List the entries of a directory within the workspace (names only).
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let p = self.workspace.resolve_read(path)?;
        let mut rd = tokio::fs::read_dir(&p).await?;
        let mut out = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
        out.sort();
        Ok(out)
    }

    /// Read all UTF-8 files with `extension` directly under a workspace directory, returning
    /// `(workspace_path, content)` sorted by filename. Missing directories are treated as empty.
    ///
    /// This is synchronous for startup-time callers that cannot await yet, but it still resolves the
    /// directory and every child path through [`Workspace::resolve`], including symlink-escape checks.
    pub fn read_dir_text_files(&self, dir: &str, extension: &str) -> Result<Vec<(String, String)>> {
        let root = self.workspace.resolve_read(dir)?;
        let mut names = Vec::new();
        let rd = match std::fs::read_dir(&root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        for entry in rd {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if std::path::Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                != Some(extension)
            {
                continue;
            }
            names.push(name);
        }
        names.sort();

        let base = dir.trim_end_matches('/');
        let mut out = Vec::new();
        for name in names {
            let path = if base.is_empty() {
                name.clone()
            } else {
                format!("{base}/{name}")
            };
            let resolved = self.workspace.resolve_read(&path)?;
            let bytes = std::fs::read(&resolved)?;
            let content = String::from_utf8(bytes)
                .map_err(|_| Error::Other(format!("{path}: not valid UTF-8")))?;
            out.push((path, content));
        }
        Ok(out)
    }

    /// Recursively list files under a workspace-relative directory, returning workspace-relative
    /// paths (sorted, capped at `max`). Symlinks are never followed (an escape guard), and the
    /// noisy `.git`/`target`/`node_modules` directories are skipped. Used by `glob`/`grep`.
    pub async fn walk_files(&self, base: &str, max: usize) -> Result<Vec<String>> {
        const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];
        let root = self.workspace.resolve_read(base)?;
        let ws_root = self.workspace.root().to_path_buf();
        let mut out = Vec::new();
        // Render a walked file: workspace-relative when under the primary root, else absolute (a read-only
        // extra root, C-21) so a subsequent `read` resolves it via the same allowed roots.
        let render = |p: &Path| -> String {
            match p.strip_prefix(&ws_root) {
                Ok(rel) => rel.to_string_lossy().into_owned(),
                Err(_) => p.to_string_lossy().into_owned(),
            }
        };
        // A `base` that resolves to a single file → return just that file, so `grep`/`glob` scoped to
        // a file path search that file instead of silently finding nothing (`read_dir` on a file
        // errors, which would otherwise yield an empty walk and a misleading "no matches").
        if tokio::fs::metadata(&root)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            out.push(render(&root));
            return Ok(out);
        }
        // Walk the base; when the base is the whole workspace root, also walk each read-only extra root so
        // `glob`/`grep` see outside-cwd files (C-21).
        let mut stack = vec![root.clone()];
        if root == ws_root {
            stack.extend(self.workspace.read_roots().iter().cloned());
        }
        while let Some(dir) = stack.pop() {
            if out.len() >= max {
                break;
            }
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(_) => continue, // unreadable dir → skip, don't fail the whole walk
            };
            while let Some(entry) = rd.next_entry().await? {
                let ft = entry.file_type().await?;
                if ft.is_symlink() {
                    continue; // never follow symlinks (could escape a root)
                }
                let path = entry.path();
                if ft.is_dir() {
                    let name = entry.file_name();
                    if SKIP_DIRS.iter().any(|s| name == std::ffi::OsStr::new(s)) {
                        continue;
                    }
                    stack.push(path);
                } else if ft.is_file() {
                    out.push(render(&path));
                    if out.len() >= max {
                        break;
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Look up an environment variable (guarded entry point so reads can be audited later).
    pub fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    /// Execute a command as an explicit argv (NO shell). `argv[0]` is the program; the working
    /// directory is the workspace root.
    pub async fn run(&self, argv: &[String], timeout: Duration) -> Result<ProcessOutput> {
        self.run_with_env(argv, &[], timeout).await
    }

    /// Like [`run`](Self::run), but additionally sets the caller-chosen `env` entries on top of the
    /// minimal allow-list (each `(key, value)` overrides or adds to the forwarded environment).
    ///
    /// This exists for **trusted in-process callers** (e.g. the eval harness) that must control a
    /// child's environment — for instance to point a spawned `flux` at an isolated `HOME` so its
    /// session store doesn't collide with the parent's. The argv-only, `env_clear`, and output-cap
    /// guarantees of [`run`](Self::run) are unchanged; only the explicit, **non-model** entries in
    /// `env` are added (model input never reaches this map — it is built by Rust callers).
    pub async fn run_with_env(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ProcessOutput> {
        let mut cmd = self.build_command(argv, env, true)?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let program = argv[0].clone();

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {program}: {e}")))?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let mut child = GuardedChild::new(child);
                child.terminate_tree();
                let _ = child.wait().await;
                return Err(Error::Other("child stdout unavailable".to_string()));
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let mut child = GuardedChild::new(child);
                child.terminate_tree();
                let _ = child.wait().await;
                return Err(Error::Other("child stderr unavailable".to_string()));
            }
        };
        await_process(child, Some(stdout), Some(stderr), program, timeout).await
    }

    /// Scrub a command's environment to the minimal non-secret allow-list, then apply caller
    /// overrides (added last so they win). Shared by [`run_with_env`](Self::run_with_env) and
    /// [`run_with_env_streamed`](Self::run_with_env_streamed).
    fn apply_safe_env(cmd: &mut tokio::process::Command, env: &[(String, String)]) {
        cmd.env_clear();
        const SAFE_ENV: &[&str] = &[
            "PATH",
            "HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "TZ",
            "USER",
            "LOGNAME",
            "TMPDIR",
            // Non-secret diagnostics knobs so a plugin/subprocess author can turn on logging.
            "RUST_LOG",
            "RUST_BACKTRACE",
            // Non-secret toolchain locations so `cargo`/`rustup` (and the cargo_* tools) resolve a
            // toolchain even under an isolated HOME without `~/.rustup`.
            "RUSTUP_HOME",
            "CARGO_HOME",
            "RUSTUP_TOOLCHAIN",
        ];
        for key in SAFE_ENV {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
    }

    /// Build a child process command with flux's **single safety envelope** applied: argv-only (no
    /// shell; `program = argv[0]`), working directory pinned to the workspace root, and the
    /// environment cleared then restricted to the minimal non-secret allow-list plus the caller's
    /// explicit (non-model) overrides. Children are `kill_on_drop`; controlled run/background paths
    /// request a fresh Unix process group so they can terminate descendants. Interactive/debug-pipe
    /// children remain in flux's process group because their callers own the raw child handle and
    /// cannot yet perform group-aware cleanup. This is the **one place** flux constructs an OS
    /// process — every spawn mode (`run_with_env`, `run_with_env_streamed`, `spawn_background`,
    /// `spawn_interactive`) layers only its own stdio on top of the command this returns, so the
    /// envelope has no bypass.
    fn build_command(
        &self,
        argv: &[String],
        env: &[(String, String)],
        isolate_process_group: bool,
    ) -> Result<tokio::process::Command> {
        let Some((program, args)) = argv.split_first() else {
            return Err(Error::Other("empty command".to_string()));
        };
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(self.workspace.root())
            .kill_on_drop(true);
        #[cfg(unix)]
        if isolate_process_group {
            cmd.process_group(0);
        }
        #[cfg(not(unix))]
        let _ = isolate_process_group;
        Self::apply_safe_env(&mut cmd, env);
        Ok(cmd)
    }

    /// Like [`run_with_env`](Self::run_with_env) but **streams** the child's stdout/stderr straight to
    /// the parent terminal (inherited) instead of capturing them — for `flux eval --watch`, where the
    /// whole point is to watch the spawned agent work live. The returned [`ProcessOutput`] carries only
    /// the exit code (stdout/stderr are empty); the eval grades via the criterion and mines
    /// `events.db`, neither of which needs captured output.
    pub async fn run_with_env_streamed(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ProcessOutput> {
        let mut cmd = self.build_command(argv, env, true)?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        let program = argv[0].clone();

        let child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {program}: {e}")))?;
        await_process(child, None, None, program, timeout).await
    }

    /// Spawn a **long-lived background** child without awaiting it — for host-managed processes such
    /// as `kubectl port-forward` that start in one op call and are stopped/queried in later ones.
    ///
    /// Same safety envelope as [`run_with_env`](Self::run_with_env): argv-only (no shell;
    /// `program = argv[0]`), env **cleared** then restricted to the minimal allow-list plus the
    /// caller's explicit (non-model) `env` overrides, and the working directory pinned to the
    /// workspace root. stdout/stderr are **piped** and continuously drained into capped buffers
    /// (see [`ManagedChild`]); the child is `kill_on_drop` so a dropped handle never leaks a process.
    ///
    /// Must be called from within a Tokio runtime (it spawns drain tasks).
    pub fn spawn_background(
        &self,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<ManagedChild> {
        let mut cmd = self.build_command(argv, env, true)?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let program = &argv[0];

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {program}: {e}")))?;
        let group = ProcessGroup::for_child(&child);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other("managed child stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Other("managed child stderr unavailable".into()))?;
        let stdout_buf = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let stdout_task =
            tokio::spawn(drain_stream(stdout, stdout_buf.clone(), MANAGED_OUTPUT_CAP));
        let stderr_task =
            tokio::spawn(drain_stream(stderr, stderr_buf.clone(), MANAGED_OUTPUT_CAP));
        Ok(ManagedChild {
            child,
            group: Some(group),
            stdout_buf,
            stderr_buf,
            stdout_task: Some(stdout_task),
            stderr_task: Some(stderr_task),
        })
    }

    /// Spawn an **interactive** child for a bidirectional stdin/stdout protocol — used to launch a
    /// plugin subprocess for the `flux.plugin.v1` frame protocol. stdin and stdout are **piped** and
    /// handed back to the caller; stderr is **inherited** so the plugin's diagnostics reach the
    /// terminal. Same safety envelope as [`run_with_env`](Self::run_with_env) via
    /// [`build_command`](Self::build_command): argv-only, workspace-pinned cwd, env cleared then
    /// restricted to the minimal allow-list — so the plugin process **cannot read the host's
    /// secrets**; it must request them back through the gated host capabilities. `kill_on_drop`.
    pub fn spawn_interactive(&self, argv: &[String]) -> Result<InteractiveChild> {
        let mut cmd = self.build_command(argv, &[], false)?;
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {}: {e}", argv[0])))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Other("interactive child stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other("interactive child stdout unavailable".into()))?;
        Ok(InteractiveChild {
            child,
            stdin,
            stdout,
        })
    }

    /// Spawn a child wired to the Chrome DevTools **remote-debugging pipe**
    /// (`--remote-debugging-pipe`): a full-duplex `socketpair` is mapped onto the child's fd 3 (CDP
    /// command input) and fd 4 (CDP response/event output) via a `pre_exec` hook that calls only
    /// async-signal-safe `dup2`/`fcntl`; the parent keeps the other end ([`PipeChild::pipe`]). Same
    /// safety envelope as every other flux subprocess via [`build_command`](Self::build_command):
    /// argv-only (no shell), workspace-pinned cwd, env cleared + allow-listed — so the browser
    /// child cannot read the host's secrets. `kill_on_drop`. Unix-only.
    ///
    /// The caller passes `--remote-debugging-pipe` in `argv`; this method only wires the fds. Must be
    /// called from within a Tokio runtime (it registers the parent socket with the reactor).
    #[cfg(unix)]
    pub fn spawn_debug_pipe(&self, argv: &[String], env: &[(String, String)]) -> Result<PipeChild> {
        use std::os::unix::io::AsRawFd;
        use std::os::unix::process::CommandExt;

        let (parent_end, child_end) = std::os::unix::net::UnixStream::pair()
            .map_err(|e| Error::Other(format!("cdp socketpair: {e}")))?;
        let child_fd = child_end.as_raw_fd();

        let mut cmd = self.build_command(argv, env, false)?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        // SAFETY: the closure runs in the forked child before `exec` and touches only async-signal-safe
        // libc calls (`dup2`/`fcntl`) on an integer fd captured by value — no allocation, no locks.
        unsafe {
            cmd.as_std_mut().pre_exec(move || {
                // Map the socket onto fd 3 and fd 4. `dup2` clears CLOEXEC on a freshly created target
                // but is a no-op (leaving CLOEXEC) when target == child_fd — so clear CLOEXEC on both
                // explicitly, covering the case where the socketpair fd already landed on 3 or 4.
                if libc::dup2(child_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(child_fd, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(4, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {}: {e}", argv[0])))?;
        // The child holds its own copy of the socket (via fork); the parent drops the child end.
        drop(child_end);

        parent_end
            .set_nonblocking(true)
            .map_err(|e| Error::Other(format!("cdp pipe nonblocking: {e}")))?;
        let pipe = tokio::net::UnixStream::from_std(parent_end)
            .map_err(|e| Error::Other(format!("cdp pipe async: {e}")))?;
        Ok(PipeChild { child, pipe })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> (PathBuf, System) {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-sys-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::new(&dir).unwrap();
        (dir, System::new(ws))
    }

    /// `env_truthy` requires an explicit truthy VALUE — presence alone (or an explicit "off"
    /// value) must never count, since security-relevant grants (`FLUX_ALLOW_PRIVATE_NET`) gate
    /// on it. Uses a probe key no other test reads, so parallel test threads don't race it.
    #[test]
    fn env_truthy_requires_a_truthy_value() {
        let key = "FLUX_SYSTEM_ENV_TRUTHY_PROBE";
        std::env::remove_var(key);
        assert!(!env_truthy(key), "unset is off");
        for on in ["1", "true", "yes", "on"] {
            std::env::set_var(key, on);
            assert!(env_truthy(key), "{on:?} is on");
        }
        for off in ["0", "false", "no", "off", ""] {
            std::env::set_var(key, off);
            assert!(
                !env_truthy(key),
                "{off:?} is off — presence alone must not grant"
            );
        }
        std::env::remove_var(key);
    }

    /// `file_size` reports the jailed file's byte size without reading it, and refuses a path
    /// outside the workspace like every other read.
    #[tokio::test]
    async fn file_size_is_jailed_metadata() {
        let (dir, sys) = temp_workspace();
        sys.write_file("a.txt", "hello").await.unwrap();
        assert_eq!(sys.file_size("a.txt").await.unwrap(), 5);
        assert!(sys.file_size("../outside.txt").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let (dir, sys) = temp_workspace();
        sys.write_file("sub/a.txt", "hello").await.unwrap();
        assert_eq!(sys.read_file("sub/a.txt").await.unwrap(), "hello");
        let listing = sys.list_dir(".").await.unwrap();
        assert!(listing.contains(&"sub".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_file_bytes_returns_raw_including_nul() {
        let (dir, sys) = temp_workspace();
        // Bytes with an embedded NUL and invalid UTF-8 — read_file_bytes must NOT decode/error.
        let raw = [b'h', b'i', 0u8, 0xFF, b'!'];
        std::fs::write(dir.join("b.bin"), raw).unwrap();
        let got = sys.read_file_bytes("b.bin").await.unwrap();
        assert_eq!(got, raw);
        // The UTF-8 read path, by contrast, rejects it.
        assert!(sys.read_file("b.bin").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn append_creates_and_appends() {
        let (dir, sys) = temp_workspace();
        // Appending to a not-yet-existing nested path creates the file and its parent dir.
        sys.append_file("logs/run.txt", "line1\n").await.unwrap();
        sys.append_file("logs/run.txt", "line2\n").await.unwrap();
        assert_eq!(
            sys.read_file("logs/run.txt").await.unwrap(),
            "line1\nline2\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn append_rejects_escape() {
        let (dir, sys) = temp_workspace();
        assert!(sys.append_file("../escape.txt", "x").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn file_mtime_advances_after_write() {
        let (dir, sys) = temp_workspace();
        sys.write_file("m.txt", "a").await.unwrap();
        let t1 = sys.file_mtime("m.txt").await.unwrap();
        // A second write must not move mtime backwards (it's monotonic per file here).
        sys.write_file("m.txt", "ab").await.unwrap();
        let t2 = sys.file_mtime("m.txt").await.unwrap();
        assert!(t2 >= t1, "mtime should not go backwards");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn is_dir_distinguishes_directories_files_and_missing_paths() {
        // C-32: `ReadTool` consults this before reading, so a directory becomes guidance instead of
        // a raw `Is a directory` io error.
        let (dir, sys) = temp_workspace();
        sys.write_file("sub/a.txt", "x").await.unwrap();
        assert!(sys.is_dir("sub").await.unwrap(), "a directory reads true");
        assert!(
            !sys.is_dir("sub/a.txt").await.unwrap(),
            "a regular file reads false"
        );
        assert!(
            !sys.is_dir("does-not-exist").await.unwrap(),
            "a missing path reads false, not an error — the caller's own read is the source of \
             truth for \"missing\""
        );
        assert!(
            sys.is_dir("../escape").await.is_err(),
            "a workspace-escaping path still errors loudly"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn walk_files_lists_recursively_and_skips_noise() {
        let (dir, sys) = temp_workspace();
        sys.write_file("a.txt", "x").await.unwrap();
        sys.write_file("src/main.rs", "x").await.unwrap();
        sys.write_file("src/util/helper.rs", "x").await.unwrap();
        sys.write_file("target/junk.rs", "x").await.unwrap(); // should be skipped
        let mut files = sys.walk_files(".", 1000).await.unwrap();
        files.sort();
        assert_eq!(files, vec!["a.txt", "src/main.rs", "src/util/helper.rs"]);
        // a subtree base only returns that subtree
        let sub = sys.walk_files("src", 1000).await.unwrap();
        assert_eq!(sub, vec!["src/main.rs", "src/util/helper.rs"]);
        // max caps the count
        assert_eq!(sys.walk_files(".", 1).await.unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── C-21: read-only allowed roots + the unconfined hatch ──────────────────────────────────────

    /// A read-only extra root: reads reach under it, writes do not, and outside-all is still rejected.
    #[tokio::test]
    async fn read_root_allows_reads_but_not_writes() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ws_dir = std::env::temp_dir().join(format!("flux-sys-ws-{}-{n}", std::process::id()));
        let ext_dir = std::env::temp_dir().join(format!("flux-sys-ext-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("ref.txt"), "outside data").unwrap();

        let mut ws = Workspace::new(&ws_dir).unwrap();
        ws.add_read_root(&ext_dir).unwrap();
        let sys = System::new(ws);

        let ext_file = ext_dir.join("ref.txt");
        let ext_file = ext_file.to_str().unwrap();
        // Read under the read-only root works…
        assert_eq!(sys.read_file(ext_file).await.unwrap(), "outside data");
        // …but a write there is rejected — writes stay confined to the primary root.
        assert!(sys.write_file(ext_file, "nope").await.is_err());
        // A path outside ALL roots is still rejected, even for reads.
        assert!(sys.read_file("/etc/passwd").await.is_err());

        std::fs::remove_dir_all(&ws_dir).ok();
        std::fs::remove_dir_all(&ext_dir).ok();
    }

    /// `glob`/`grep` over the whole workspace also surface read-root files, as absolute paths that a
    /// subsequent `read` resolves.
    #[tokio::test]
    async fn walk_includes_read_roots_as_absolute() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ws_dir = std::env::temp_dir().join(format!("flux-sys-ws2-{}-{n}", std::process::id()));
        let ext_dir =
            std::env::temp_dir().join(format!("flux-sys-ext2-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("outside.txt"), "out").unwrap();

        let mut ws = Workspace::new(&ws_dir).unwrap();
        ws.add_read_root(&ext_dir).unwrap();
        let sys = System::new(ws);
        sys.write_file("inside.txt", "in").await.unwrap();

        let files = sys.walk_files(".", 1000).await.unwrap();
        assert!(
            files.iter().any(|f| f == "inside.txt"),
            "in-workspace file relative: {files:?}"
        );
        let ext_hit = files
            .iter()
            .find(|f| f.ends_with("outside.txt"))
            .expect("read-root file surfaced");
        assert!(
            Path::new(ext_hit).is_absolute(),
            "read-root hit is absolute: {ext_hit}"
        );
        // …and it reads back through the same allowed roots.
        assert_eq!(sys.read_file(ext_hit).await.unwrap(), "out");

        std::fs::remove_dir_all(&ws_dir).ok();
        std::fs::remove_dir_all(&ext_dir).ok();
    }

    /// The `--allow-all-paths` hatch: `unconfined` lifts confinement for both read and write.
    #[tokio::test]
    async fn unconfined_lifts_the_sandbox() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ws_dir = std::env::temp_dir().join(format!("flux-sys-unc-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&ws_dir).unwrap();
        let mut ws = Workspace::new(&ws_dir).unwrap();
        // Confined: an absolute outside path is rejected on both paths.
        assert!(ws.resolve_read("/etc/passwd").is_err());
        assert!(ws.resolve("/etc/passwd").is_err());
        ws.set_unconfined(true);
        // Unconfined: both resolve.
        assert!(ws.resolve_read("/etc/passwd").is_ok());
        assert!(ws.resolve("/etc/passwd").is_ok());
        std::fs::remove_dir_all(&ws_dir).ok();
    }

    #[tokio::test]
    async fn walk_files_does_not_follow_symlinks_out() {
        let (dir, sys) = temp_workspace();
        sys.write_file("real.txt", "x").await.unwrap();
        std::os::unix::fs::symlink("/etc", dir.join("etclink")).unwrap();
        let files = sys.walk_files(".", 1000).await.unwrap();
        // the symlinked dir is not traversed, so no /etc files appear
        assert!(files.iter().all(|f| !f.contains("etclink")));
        assert!(files.contains(&"real.txt".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_parent_escape() {
        let (dir, sys) = temp_workspace();
        let err = sys.read_file("../../etc/passwd").await.unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(sys.write_file("../escape.txt", "x").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_absolute_outside() {
        let (dir, sys) = temp_workspace();
        assert!(sys.read_file("/etc/passwd").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_control_char_in_path() {
        let (dir, sys) = temp_workspace();
        // A trailing newline (the `echo`/untrimmed-substitution bug) must be rejected outright,
        // not written as a file named `note.md\n` that `glob` sees but `read` can't open.
        let err = sys.write_file("note.md\n", "x").await.unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(sys.read_file("note.md\n").await.is_err());
        // an embedded NUL is likewise refused
        assert!(sys.write_file("a\0b.md", "x").await.is_err());
        // the clean name is unaffected
        sys.write_file("note.md", "x").await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_symlink_escape() {
        let (dir, sys) = temp_workspace();
        // a symlink inside the workspace pointing at /etc
        let link = dir.join("etclink");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        // reading through the symlink to a real outside file must be rejected
        let err = sys.read_file("etclink/hostname").await;
        assert!(err.is_err(), "expected symlink escape to be rejected");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_escape_hidden_in_symlink_targets_intermediate_component() {
        let (dir, sys) = temp_workspace();
        std::os::unix::fs::symlink("/etc", dir.join("stage")).unwrap();
        std::os::unix::fs::symlink("stage/hostname", dir.join("indirect")).unwrap();
        assert!(
            sys.read_file("indirect").await.is_err(),
            "an intermediate symlink inside another link target must not escape confinement"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_dangling_symlink_escape_on_write() {
        let (dir, sys) = temp_workspace();
        // A symlink inside the workspace pointing at a NON-EXISTENT outside target. `Path::exists()`
        // follows the link → false, so the old parent-only canonicalize let the write through.
        let outside = std::env::temp_dir().join(format!(
            "flux-escape-target-{}-{}.txt",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_file(&outside).ok();
        std::os::unix::fs::symlink(&outside, dir.join("evil")).unwrap();
        let err = sys.write_file("evil", "pwned").await;
        assert!(
            err.is_err(),
            "writing through a dangling out-of-root symlink must be rejected"
        );
        assert!(
            !outside.exists(),
            "the outside target must not have been created"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn allows_in_root_symlink_write() {
        let (dir, sys) = temp_workspace();
        // A symlink that stays inside the workspace is fine to write through.
        sys.write_file("realdir/.keep", "x").await.unwrap();
        std::os::unix::fs::symlink(dir.join("realdir"), dir.join("link")).unwrap();
        sys.write_file("link/a.txt", "hi").await.unwrap();
        assert_eq!(sys.read_file("realdir/a.txt").await.unwrap(), "hi");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn path_identity_follows_symlink_and_preserves_missing_create_tail() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-sys-path-id-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("allowed/real")).unwrap();
        std::os::unix::fs::symlink("real", dir.join("allowed/alias")).unwrap();
        let workspace = Workspace::new(&dir).unwrap();

        assert_eq!(
            workspace
                .path_identity("allowed/alias/new/deep.txt", PathAccess::Write)
                .unwrap(),
            "allowed/real/new/deep.txt"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_does_not_leak_parent_secrets() {
        let (dir, sys) = temp_workspace();
        std::env::set_var("FLUX_TEST_SECRET_ENVX", "topsecret-do-not-leak");
        let out = sys
            .run(&["env".to_string()], Duration::from_secs(10))
            .await
            .unwrap();
        std::env::remove_var("FLUX_TEST_SECRET_ENVX");
        assert!(
            !out.stdout.contains("topsecret-do-not-leak"),
            "subprocess inherited a parent-process secret: {}",
            out.stdout
        );
        assert!(!out.stdout.contains("FLUX_TEST_SECRET_ENVX"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_with_env_applies_caller_overrides() {
        let (dir, sys) = temp_workspace();
        // A caller-chosen entry is visible to the child even though it isn't in the allow-list, and
        // overrides the forwarded value when the key collides (HOME).
        let out = sys
            .run_with_env(
                &["env".to_string()],
                &[
                    (
                        "FLUX_EVAL_MARKER".to_string(),
                        "isolated-home-42".to_string(),
                    ),
                    ("HOME".to_string(), "/tmp/flux-eval-isolated".to_string()),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(
            out.stdout.contains("FLUX_EVAL_MARKER=isolated-home-42"),
            "caller override not applied: {}",
            out.stdout
        );
        assert!(
            out.stdout.contains("HOME=/tmp/flux-eval-isolated"),
            "HOME override not applied: {}",
            out.stdout
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capped_lossy_truncates_huge_output() {
        let big = vec![b'a'; 2 * 1024 * 1024];
        let s = capped_lossy(&big, 1024 * 1024);
        assert!(s.len() < big.len());
        assert!(s.contains("truncated"));
        // Small output is passed through verbatim.
        assert_eq!(capped_lossy(b"hello", 1024), "hello");
    }

    #[tokio::test]
    async fn runs_argv_without_shell() {
        let (dir, sys) = temp_workspace();
        let out = sys
            .run(
                &["printf".to_string(), "%s".to_string(), "hi".to_string()],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, "hi");
        assert_eq!(out.exit_code, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_captures_nonzero_exit() {
        let (dir, sys) = temp_workspace();
        let out = sys
            .run(&["false".to_string()], Duration::from_secs(10))
            .await
            .unwrap();
        assert_ne!(out.exit_code, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_pid_file(path: &Path) -> i32 {
        for _ in 0..200 {
            if let Ok(raw) = tokio::fs::read_to_string(path).await {
                if let Ok(pid) = raw.parse::<i32>() {
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("process did not publish its pid at {}", path.display());
    }

    #[cfg(target_os = "linux")]
    fn process_exists(pid: i32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    #[cfg(target_os = "linux")]
    fn process_is_live(pid: i32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // The command name is parenthesized and may contain spaces; the state is the first token
        // after the final `) `. A zombie has stopped executing even if PID 1 has not reaped it yet.
        stat.rsplit_once(") ")
            .and_then(|(_, rest)| rest.chars().next())
            .is_some_and(|state| state != 'Z')
    }

    #[cfg(target_os = "linux")]
    async fn assert_process_tree_stopped(parent: i32, descendant: i32) {
        for _ in 0..200 {
            if !process_exists(parent) && !process_is_live(descendant) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !process_exists(parent),
            "direct child {parent} was not killed and reaped"
        );
        assert!(
            !process_is_live(descendant),
            "descendant {descendant} survived process-tree cleanup"
        );
    }

    #[cfg(target_os = "linux")]
    fn sleeping_process_tree_argv() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s' \"$$\" > parent.pid; sleep 30 & printf '%s' \"$!\" > child.pid; wait"
                .to_string(),
        ]
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn run_timeout_kills_reaps_child_and_stops_descendants() {
        let (dir, sys) = temp_workspace();
        let err = sys
            .run(&sleeping_process_tree_argv(), Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");

        let parent = wait_for_pid_file(&dir.join("parent.pid")).await;
        let descendant = wait_for_pid_file(&dir.join("child.pid")).await;
        assert_process_tree_stopped(parent, descendant).await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelling_run_kills_reaps_child_and_stops_descendants() {
        let (dir, sys) = temp_workspace();
        let task = tokio::spawn(async move {
            sys.run(&sleeping_process_tree_argv(), Duration::from_secs(30))
                .await
        });
        let parent = wait_for_pid_file(&dir.join("parent.pid")).await;
        let descendant = wait_for_pid_file(&dir.join("child.pid")).await;

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_process_tree_stopped(parent, descendant).await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn streamed_run_timeout_kills_reaps_child_and_stops_descendants() {
        let (dir, sys) = temp_workspace();
        let err = sys
            .run_with_env_streamed(
                &sleeping_process_tree_argv(),
                &[],
                Duration::from_millis(200),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");

        let parent = wait_for_pid_file(&dir.join("parent.pid")).await;
        let descendant = wait_for_pid_file(&dir.join("child.pid")).await;
        assert_process_tree_stopped(parent, descendant).await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_caps_and_drains_both_output_streams_without_deadlock() {
        let (dir, sys) = temp_workspace();
        let out = sys
            .run(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "(head -c 4194304 /dev/zero | tr '\\0' o) & \
                     (head -c 4194304 /dev/zero | tr '\\0' e >&2) & wait"
                        .to_string(),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("[output truncated]"));
        assert!(out.stderr.contains("[output truncated]"));
        assert!(out.stdout.len() < 2 * 1024 * 1024);
        assert!(out.stderr.len() < 2 * 1024 * 1024);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_truncation_is_safe_across_a_utf8_boundary() {
        let (dir, sys) = temp_workspace();
        let out = sys
            .run(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "head -c 1048575 /dev/zero | tr '\\0' a; printf '\\303\\251z'".to_string(),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains('\u{fffd}'));
        assert!(out.stdout.ends_with("\n…[output truncated]"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spawn_background_reads_output_and_exit_code() {
        let (dir, sys) = temp_workspace();
        let mut child = sys
            .spawn_background(&["printf".to_string(), "hello-bg".to_string()], &[])
            .unwrap();
        // Drain across polls until the output shows up (the drain task copies asynchronously, so a
        // single read right after spawn can race the pipe).
        let mut out = String::new();
        for _ in 0..200 {
            let (o, _e) = child.read_output();
            out.push_str(&o);
            if out.contains("hello-bg") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(out, "hello-bg", "background stdout not captured");
        // After exit, status is non-blocking and reports the code.
        let mut st = child.status();
        for _ in 0..200 {
            if !st.running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            st = child.status();
        }
        assert!(!st.running, "child should have exited");
        assert_eq!(st.exit_code, Some(0));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spawn_background_kill_stops_running_child() {
        let (dir, sys) = temp_workspace();
        let mut child = sys
            .spawn_background(&["sleep".to_string(), "30".to_string()], &[])
            .unwrap();
        assert!(child.status().running, "a freshly spawned sleep should run");
        child.kill();
        let mut stopped = false;
        for _ in 0..200 {
            if !child.status().running {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(stopped, "killed child should stop running");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn spawn_background_kill_stops_descendants() {
        let (dir, sys) = temp_workspace();
        let mut child = sys
            .spawn_background(&sleeping_process_tree_argv(), &[])
            .unwrap();
        let parent = wait_for_pid_file(&dir.join("parent.pid")).await;
        let descendant = wait_for_pid_file(&dir.join("child.pid")).await;

        child.kill();
        for _ in 0..200 {
            if !child.status().running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_process_tree_stopped(parent, descendant).await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spawn_background_clears_parent_env_and_applies_overrides() {
        let (dir, sys) = temp_workspace();
        std::env::set_var("FLUX_TEST_BG_SECRET", "leak-me-not");
        let mut child = sys
            .spawn_background(
                &["env".to_string()],
                &[("FLUX_BG_MARKER".to_string(), "bg-42".to_string())],
            )
            .unwrap();
        std::env::remove_var("FLUX_TEST_BG_SECRET");
        // The drain task copies the pipe asynchronously, so keep draining until the marker shows up
        // rather than stopping at the first observed exit (the final bytes can lag the exit).
        let mut out = String::new();
        for _ in 0..200 {
            let (o, _e) = child.read_output();
            out.push_str(&o);
            if out.contains("FLUX_BG_MARKER=bg-42") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            out.contains("FLUX_BG_MARKER=bg-42"),
            "caller env override missing: {out}"
        );
        assert!(
            !out.contains("leak-me-not"),
            "background child inherited a parent secret: {out}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
