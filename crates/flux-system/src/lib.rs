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
pub mod sandbox;

use sandbox::{Confinement, Sandbox, SandboxSettings, SpawnPolicy};

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

    /// Create a workspace only when `root` exists. Missing optional control-plane roots return
    /// `None`; every other canonicalization failure remains an error.
    pub fn new_optional(root: impl AsRef<Path>) -> Result<Option<Self>> {
        let root = root.as_ref();
        match root.canonicalize() {
            Ok(root) => Ok(Some(Self {
                root,
                named: HashMap::new(),
                read_roots: Vec::new(),
                unconfined: false,
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Config(format!(
                "optional workspace root {}: {error}",
                root.display()
            ))),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Derive a workspace re-rooted at `root` (canonicalized; must exist) while preserving the
    /// full access posture of `self`: `@named` roots, read-only roots, and the unconfined flag.
    /// This is the seam a context-local worktree transition uses — the plain constructors would
    /// drop the widened roots and so break `@named`-root operations inside the new root (C-97).
    pub fn with_root(&self, root: impl AsRef<Path>) -> Result<Self> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|e| Error::Config(format!("workspace root: {e}")))?;
        Ok(Self {
            root,
            named: self.named.clone(),
            read_roots: self.read_roots.clone(),
            unconfined: self.unconfined,
        })
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

    /// The paths of every registered `@named` root — the write-capable set alongside the primary
    /// root. Used by [`sandbox::SpawnPolicy::for_workspace`] to derive a sandboxed spawn's
    /// writable set.
    pub fn named_roots(&self) -> impl Iterator<Item = &Path> {
        self.named.values().map(PathBuf::as_path)
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

/// The base directory worktree parents are allocated under: `$FLUX_WORKTREE_DIR`, else
/// `$HOME/.flux/worktrees` (beside the other `~/.flux` state), else the system temp dir. A real
/// on-disk default matters: `/tmp` is commonly a RAM-backed tmpfs, and a build inside an entered
/// worktree (`cargo build` → a multi-GB `target/`) would fill it and starve every process that
/// needs `/tmp`.
fn worktree_base_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FLUX_WORKTREE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => Path::new(&home).join(".flux").join("worktrees"),
        _ => std::env::temp_dir(),
    }
}

/// Allocate a fresh private parent directory for a context-local git worktree (C-97):
/// `<base>/flux-worktree-<pid>-<seq>` with owner-only permissions on Unix, where `<base>` comes
/// from [`worktree_base_dir`]. The directory is created outside any workspace root on purpose —
/// the caller derives a re-rooted [`System`] ([`System::rerooted`]) at the checkout inside it.
pub fn allocate_worktree_dir() -> Result<PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = worktree_base_dir();
    std::fs::create_dir_all(&base)
        .map_err(|e| Error::Config(format!("worktree base {}: {e}", base.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
    }
    let dir = base.join(format!("flux-worktree-{}-{seq}", std::process::id()));
    std::fs::create_dir(&dir)
        .map_err(|e| Error::Config(format!("worktree dir {}: {e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir.canonicalize()
        .map_err(|e| Error::Config(format!("worktree dir {}: {e}", dir.display())))
}

/// Remove a directory previously allocated by [`allocate_worktree_dir`]. Fail-closed: refuses any
/// path that is not directly under the resolved [`worktree_base_dir`] with the `flux-worktree-`
/// prefix, so a corrupted session state can never turn cleanup into an arbitrary recursive delete.
pub fn remove_worktree_dir(path: &Path) -> Result<()> {
    let tmp = worktree_base_dir()
        .canonicalize()
        .map_err(|e| Error::Config(format!("worktree base dir: {e}")))?;
    let ok = path.parent() == Some(tmp.as_path())
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("flux-worktree-"));
    if !ok {
        return Err(Error::Config(format!(
            "refusing to remove {:?}: not an allocated flux worktree dir",
            path
        )));
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Config(format!(
            "worktree dir cleanup {}: {e}",
            path.display()
        ))),
    }
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn capture_bounded_blocking<R>(mut reader: R) -> std::io::Result<BoundedCapture>
where
    R: std::io::Read,
{
    let mut bytes = Vec::with_capacity(8192.min(PROCESS_OUTPUT_CAP));
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
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

/// Callback invoked with each **complete line** a guarded child writes while it is still running
/// (C-158). Emission is line-oriented rather than per-read-chunk for two reasons: a surface wants
/// lines, and a fixed-size read can split a multi-byte codepoint, which per-chunk `from_utf8_lossy`
/// would turn into replacement characters. Buffering to the newline reassembles those bytes first.
///
/// The observer sees only what the capture actually keeps, so the `PROCESS_OUTPUT_CAP` bound governs
/// the observed stream too — a runaway child cannot push unbounded text through this callback.
/// Implementations must be cheap and non-blocking: they run inline on the drain task, so blocking
/// here stops draining the pipe and eventually stalls the child.
pub type OutputObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// Emit every complete line accumulated in `pending`, leaving any trailing partial line buffered.
fn emit_lines(pending: &mut Vec<u8>, observer: &OutputObserver) {
    while let Some(nl) = pending.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = pending.drain(..=nl).collect();
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches('\n').trim_end_matches('\r');
        if !text.is_empty() {
            observer(text);
        }
    }
}

async fn capture_bounded<R>(
    mut reader: R,
    observer: Option<OutputObserver>,
) -> std::io::Result<BoundedCapture>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;

    let mut bytes = Vec::with_capacity(8192.min(PROCESS_OUTPUT_CAP));
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let room = PROCESS_OUTPUT_CAP.saturating_sub(bytes.len());
        let keep = read.min(room);
        bytes.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
        if let Some(observer) = &observer {
            pending.extend_from_slice(&chunk[..keep]);
            emit_lines(&mut pending, observer);
        }
    }
    // A child that exits without a trailing newline still has a last line worth showing.
    if let Some(observer) = &observer {
        if !pending.is_empty() {
            pending.push(b'\n');
            emit_lines(&mut pending, observer);
        }
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
        Self::for_id(child.id())
    }

    fn for_id(id: Option<u32>) -> Self {
        #[cfg(unix)]
        {
            let id = id.and_then(|id| libc::pid_t::try_from(id).ok());
            Self { id }
        }
        #[cfg(not(unix))]
        {
            let _ = id;
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

/// Result of the synchronous, startup-safe guarded launcher used for backend preflight probes.
/// stderr is byte-capped exactly like normal captured runs; descendants are terminated before the
/// pipe reader is joined, so a fork that inherits stderr cannot hold startup open indefinitely.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct GuardedProbeOutput {
    pub(crate) status: Option<std::process::ExitStatus>,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug)]
pub(crate) enum GuardedProbeError {
    Spawn(std::io::Error),
    Other(String),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct SyncGuardedChild {
    child: std::process::Child,
    group: ProcessGroup,
    reaped: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl SyncGuardedChild {
    fn new(child: std::process::Child) -> Self {
        let group = ProcessGroup::for_id(Some(child.id()));
        Self {
            child,
            group,
            reaped: false,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn terminate_tree(&mut self) {
        self.group.terminate();
        let _ = self.child.kill();
    }

    fn terminate_descendants(&self) {
        self.group.terminate();
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait();
        if status.is_ok() {
            self.reaped = true;
        }
        status
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for SyncGuardedChild {
    fn drop(&mut self) {
        if !self.reaped {
            self.terminate_tree();
            let _ = self.child.wait();
            self.reaped = true;
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
    observer: Option<OutputObserver>,
) {
    // Both streams feed the one observer: a build tool's progress commonly lands on stderr, so
    // watching stdout alone would leave the most interesting ops looking silent.
    let stdout_task = stdout.map(|stream| tokio::spawn(capture_bounded(stream, observer.clone())));
    let stderr_task = stderr.map(|stream| tokio::spawn(capture_bounded(stream, observer)));
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
    observer: Option<OutputObserver>,
) -> Result<ProcessOutput> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(drive_process(
        child, stdout, stderr, program, timeout, result_tx, observer,
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
    sandbox: Sandbox,
}

impl System {
    /// Build a `System` with the sandbox **disabled** — env-free and infallible, so every
    /// hermetic test site (and any caller that doesn't want the environment consulted) is
    /// unaffected by the sandbox seam. Production entry points should use
    /// [`System::from_env`]/[`System::with_sandbox`] instead.
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            sandbox: Sandbox::disabled(),
        }
    }

    /// Build a `System` from `cwd` the way every production entry point should: a [`Workspace`]
    /// from the environment ([`Workspace::from_env`]) plus a [`Sandbox`] resolved from the
    /// environment ([`Sandbox::resolve`] over [`SandboxSettings::from_env`]). Fails only if the
    /// workspace root doesn't exist (sandbox resolution is infallible).
    pub fn from_env(cwd: impl AsRef<Path>) -> Result<Self> {
        let workspace = Workspace::from_env(cwd)?;
        let sandbox = Sandbox::resolve(SandboxSettings::from_env());
        Ok(Self { workspace, sandbox })
    }

    /// Attach an explicit sandbox posture — the builder counterpart to [`System::from_env`] for
    /// call sites that need a custom [`Workspace`] (extra named roots, a non-cwd root, …) and so
    /// cannot use `from_env`'s workspace construction directly.
    pub fn with_sandbox(mut self, sandbox: Sandbox) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Derive a `System` re-rooted at `root` while preserving both the workspace access posture
    /// (via [`Workspace::with_root`]) and the resolved sandbox. Spawned processes under the
    /// derived system run with the new root as cwd and the sandbox's writable set follows it
    /// automatically ([`sandbox::SpawnPolicy::for_workspace`] derives from the workspace root).
    pub fn rerooted(&self, root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            workspace: self.workspace.with_root(root)?,
            sandbox: self.sandbox.clone(),
        })
    }

    /// The resolved sandbox posture for this `System`.
    pub fn sandbox(&self) -> &Sandbox {
        &self.sandbox
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

    /// Read an optional UTF-8 control-plane file synchronously through the workspace guard.
    /// Missing files are `None`; path escapes, invalid UTF-8, and all other failures remain errors.
    /// Startup-time metadata loaders use this instead of owning raw project filesystem IO.
    pub fn read_optional_text(&self, path: &str) -> Result<Option<String>> {
        let resolved = self.workspace.resolve_read(path)?;
        match std::fs::read(&resolved) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| Error::Other(format!("{path}: not valid UTF-8"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
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

    /// Write raw bytes within the workspace, creating parent directories (also confined) — the
    /// binary counterpart to [`read_file_bytes`](Self::read_file_bytes), for payloads that are
    /// not UTF-8 text (rendered images, archives; L-78).
    pub async fn write_file_bytes(&self, path: &str, contents: &[u8]) -> Result<()> {
        let p = self.workspace.resolve(path)?;
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&p, contents).await?;
        Ok(())
    }

    /// Atomically replace a UTF-8 workspace file from a create-new sibling. Both the destination
    /// and its parent are resolved through the write guard before any open, so file and directory
    /// symlinks cannot redirect project control-plane persistence outside the workspace.
    pub fn write_file_atomic(&self, path: &str, contents: &str) -> Result<()> {
        use std::io::Write as _;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let destination = self.workspace.resolve(path)?;
        let parent = destination
            .parent()
            .ok_or_else(|| Error::Config(format!("path {path:?} has no parent")))?;
        std::fs::create_dir_all(parent)?;

        // Resolve again after creating the parent. This closes the common parent-symlink swap
        // window and gives the sibling its physical, guarded directory.
        let destination = self.workspace.resolve(path)?;
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::Config(format!("path {path:?} is not valid UTF-8")))?;
        let temp = destination.with_file_name(format!(
            ".{file_name}.tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));

        let write_result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;

            // A final identity check catches a destination or parent retarget before replacement.
            let final_destination = self.workspace.resolve(path)?;
            if final_destination != destination {
                return Err(Error::Config(format!(
                    "path {path:?} changed identity during atomic write"
                )));
            }
            std::fs::rename(&temp, &final_destination)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        write_result
    }

    /// Read the raw bytes of a file within the workspace (no UTF-8 decode). Used to sniff binary
    /// files (NUL bytes) and report byte sizes *before* a lossy text decode.
    pub async fn read_file_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let p = self.workspace.resolve_read(path)?;
        Ok(tokio::fs::read(&p).await?)
    }

    /// Read at most `max` bytes of a **regular** file (jailed). Returns `(bytes, truncated)` where
    /// `truncated` is true when the file is larger than `max`. Rejects non-regular files — a FIFO or
    /// device would otherwise block or stream endlessly and hang the caller. Use this instead of
    /// [`read_file_bytes`](Self::read_file_bytes) on any path whose size is attacker-influenced, to
    /// bound memory instead of slurping the whole file before a size check (C-79).
    pub async fn read_file_bytes_capped(&self, path: &str, max: usize) -> Result<(Vec<u8>, bool)> {
        use tokio::io::AsyncReadExt as _;
        let p = self.workspace.resolve_read(path)?;
        let file = tokio::fs::File::open(&p).await?;
        let meta = file.metadata().await?;
        if !meta.is_file() {
            return Err(Error::Other(format!(
                "{path}: not a regular file (refusing to read a directory, FIFO, or device)"
            )));
        }
        // Read one byte past the cap so we can report truncation, then trim back to `max`.
        let mut buf = Vec::new();
        file.take(max as u64 + 1).read_to_end(&mut buf).await?;
        let truncated = buf.len() > max;
        buf.truncate(max);
        Ok((buf, truncated))
    }

    /// Byte size of a file within the workspace/read-roots — a metadata call, so a caller
    /// enforcing a size cap can skip an oversized file WITHOUT paying a whole-file read first.
    pub async fn file_size(&self, path: &str) -> Result<u64> {
        let p = self.workspace.resolve_read(path)?;
        Ok(tokio::fs::metadata(&p).await?.len())
    }

    /// Whether a path exists inside the workspace/read roots.
    ///
    /// Unlike [`Path::exists`], this preserves IO errors and applies the workspace's lexical and
    /// symlink confinement before touching metadata. It is intended for guarded create/overwrite
    /// decisions at product surfaces.
    pub async fn path_exists(&self, path: &str) -> Result<bool> {
        let p = self.workspace.resolve_read(path)?;
        match tokio::fs::metadata(&p).await {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
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

    /// Maximum directory levels [`System::read_dir_text_files_with_nested`] descends below a skill
    /// root while searching for `nested_file` (e.g. `SKILL.md`). Depth 1 is the historical one-level
    /// `<name>/SKILL.md` shape; this bound additionally covers Claude's namespaced trees
    /// (`.claude/skills/<ns>/<name>/SKILL.md` is depth 2) plus headroom for a sub-namespace, while
    /// keeping a pathological tree from turning discovery into an unbounded walk.
    const NESTED_FILE_MAX_DEPTH: usize = 4;

    /// Read text files directly under `dir` plus `nested_file` from each descendant directory,
    /// bounded by [`System::NESTED_FILE_MAX_DEPTH`]. Every entry is resolved independently, so a symlinked
    /// file/directory is rejected rather than silently omitted. This is the guarded discovery shape
    /// used by project skills (`*.md` at the top level only, `SKILL.md` at any depth up to the
    /// bound) — Claude namespaced trees (`skills/<ns>/<name>/SKILL.md`) resolve the same as a flat
    /// `skills/<name>/SKILL.md` layout. A directory that directly contains `nested_file` claims its
    /// whole subtree: traversal does not descend past it, so skill-internal directories (e.g. a
    /// skill's own `references/`) never surface as separate entries.
    pub fn read_dir_text_files_with_nested(
        &self,
        dir: &str,
        extension: &str,
        nested_file: &str,
    ) -> Result<Vec<(String, String)>> {
        let root = self.workspace.resolve_read(dir)?;
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();

        let base = dir.trim_end_matches('/');
        let mut paths = Vec::new();
        for name in names {
            let child = if base.is_empty() {
                name.clone()
            } else {
                format!("{base}/{name}")
            };
            let resolved = self.workspace.resolve_read(&child)?;
            let metadata = std::fs::metadata(&resolved)?;
            if metadata.is_dir() {
                self.collect_nested_file(&child, nested_file, 1, &mut paths)?;
            } else if std::path::Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                == Some(extension)
            {
                paths.push(child);
            }
        }

        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let resolved = self.workspace.resolve_read(&path)?;
            let bytes = std::fs::read(&resolved)?;
            let content = String::from_utf8(bytes)
                .map_err(|_| Error::Other(format!("{path}: not valid UTF-8")))?;
            out.push((path, content));
        }
        Ok(out)
    }

    /// Depth-first search for `nested_file` under `dir_path` (a workspace-relative directory
    /// already known to exist), bounded by [`System::NESTED_FILE_MAX_DEPTH`] directory levels below the
    /// skill root. If `dir_path` itself contains `nested_file`, that claims the whole subtree and
    /// the search stops there (no further descent, so a skill's own supporting directories are
    /// never mistaken for nested skills). Otherwise every subdirectory is searched in sorted order.
    fn collect_nested_file(
        &self,
        dir_path: &str,
        nested_file: &str,
        depth: usize,
        out: &mut Vec<String>,
    ) -> Result<()> {
        let nested = format!("{dir_path}/{nested_file}");
        let nested_resolved = self.workspace.resolve_read(&nested)?;
        match std::fs::metadata(&nested_resolved) {
            Ok(metadata) if metadata.is_file() => {
                out.push(nested);
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        if depth >= Self::NESTED_FILE_MAX_DEPTH {
            return Ok(());
        }

        let resolved_dir = self.workspace.resolve_read(dir_path)?;
        let entries = match std::fs::read_dir(&resolved_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();

        for name in names {
            let child = format!("{dir_path}/{name}");
            let resolved = self.workspace.resolve_read(&child)?;
            if std::fs::metadata(&resolved)?.is_dir() {
                self.collect_nested_file(&child, nested_file, depth + 1, out)?;
            }
        }
        Ok(())
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

    /// Run a sandbox-backend preflight synchronously through the same [`Self::build_command`]
    /// choke point as every product subprocess. This exists because [`sandbox::Sandbox::resolve`]
    /// runs before a Tokio runtime is guaranteed to exist. The probe itself is explicitly exempt
    /// from sandbox wrapping (it *is* the wrapper being tested), but still gets argv-only launch,
    /// safe-env clearing, a dedicated process group, bounded stderr, deadline enforcement, and
    /// descendant cleanup.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn run_guarded_probe(
        argv: &[String],
        timeout: Duration,
    ) -> std::result::Result<GuardedProbeOutput, GuardedProbeError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let workspace = Workspace::new(&cwd)
            .map_err(|err| GuardedProbeError::Other(format!("probe workspace: {err}")))?;
        let system = Self::new(workspace);
        let mut cmd = system
            .build_command(argv, &[], true, Confinement::Exempt)
            .map_err(|err| GuardedProbeError::Other(format!("build probe command: {err}")))?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        let deadline = std::time::Instant::now() + timeout;
        let mut attempt = 0u32;
        let mut child = loop {
            match cmd.spawn() {
                Err(err)
                    if err.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && attempt < 5
                        && std::time::Instant::now() < deadline =>
                {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(10 * u64::from(attempt)));
                }
                Err(err) => return Err(GuardedProbeError::Spawn(err)),
                Ok(child) => break child,
            }
        };
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| GuardedProbeError::Other("probe stderr unavailable".to_string()))?;
        let mut child = SyncGuardedChild::new(child);
        let (capture_tx, capture_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("flux-probe-stderr".to_string())
            .spawn(move || {
                let _ = capture_tx.send(capture_bounded_blocking(stderr));
            })
            .map_err(|err| {
                GuardedProbeError::Other(format!("start probe stderr capture: {err}"))
            })?;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // A failed wrapper may fork and exit while its descendant keeps stderr open.
                    // Stop the whole group before awaiting EOF, matching `drive_process`'s normal
                    // captured-run cleanup.
                    child.terminate_descendants();
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    let captured = capture_rx.recv_timeout(remaining).map_err(|err| {
                        GuardedProbeError::Other(format!(
                            "probe stderr did not close before the {timeout:?} deadline: {err}"
                        ))
                    })?;
                    let stderr = captured
                        .map_err(|err| {
                            GuardedProbeError::Other(format!("read probe stderr: {err}"))
                        })?
                        .into_lossy();
                    return Ok(GuardedProbeOutput {
                        status: Some(status),
                        stderr,
                        timed_out: false,
                    });
                }
                Ok(None) if std::time::Instant::now() >= deadline => {
                    child.terminate_tree();
                    child.wait().map_err(|err| {
                        GuardedProbeError::Other(format!("reap timed-out probe: {err}"))
                    })?;
                    // Do not join the reader past the advertised deadline. Killing the process
                    // group closes ordinary inherited pipes; an adversarial setsid escape may keep
                    // the short-lived reader thread alive, but can no longer block startup.
                    return Ok(GuardedProbeOutput {
                        status: None,
                        stderr: String::new(),
                        timed_out: true,
                    });
                }
                Ok(None) => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    std::thread::sleep(remaining.min(Duration::from_millis(20)));
                }
                Err(err) => {
                    child.terminate_tree();
                    let _ = child.wait();
                    return Err(GuardedProbeError::Other(format!(
                        "wait on probe child failed: {err}"
                    )));
                }
            }
        }
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
        self.run_with_env_confinement(argv, env, timeout, Confinement::Sandboxed, None)
            .await
    }

    /// [`run`](Self::run) with a live line observer (C-158) — see
    /// [`run_with_env_observed`](Self::run_with_env_observed) for the guarantees.
    pub async fn run_observed(
        &self,
        argv: &[String],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Result<ProcessOutput> {
        self.run_with_env_observed(argv, &[], timeout, observer)
            .await
    }

    /// Like [`run_with_env`](Self::run_with_env), but additionally reports each **complete line** the
    /// child writes while it is still running (C-158), so a surface can show a long op progressing
    /// instead of a silent spinner.
    ///
    /// This changes nothing about the result: stdout/stderr are still captured in full and the
    /// returned [`ProcessOutput`] is byte-for-byte what `run_with_env` would have produced. The
    /// observer is a **view onto** the same capture, not a second, unbounded channel — see
    /// [`OutputObserver`] for the cap and the non-blocking requirement.
    pub async fn run_with_env_observed(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Result<ProcessOutput> {
        self.run_with_env_confinement(argv, env, timeout, Confinement::Sandboxed, Some(observer))
            .await
    }

    /// Launch a trusted host process outside this `System`'s child sandbox while retaining every
    /// other guarded-process invariant. This narrowly supports hosts such as the local-eval child
    /// `flux`: the host must keep network access for provider requests, while the sandbox posture is
    /// passed into it so its own shell/plugin descendants are confined at their spawn choke point.
    /// Model-selected executables must use [`Self::run_with_env`], never this exemption.
    pub async fn run_with_env_exempt(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ProcessOutput> {
        self.run_with_env_confinement(argv, env, timeout, Confinement::Exempt, None)
            .await
    }

    async fn run_with_env_confinement(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
        confinement: Confinement,
        observer: Option<OutputObserver>,
    ) -> Result<ProcessOutput> {
        let mut cmd = self.build_tokio_command(argv, env, true, confinement)?;
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
        await_process(
            child,
            Some(stdout),
            Some(stderr),
            program,
            timeout,
            observer,
        )
        .await
    }

    /// Scrub a command's environment to the minimal non-secret allow-list, then apply caller
    /// overrides (added last so they win). Shared by [`run_with_env`](Self::run_with_env) and
    /// [`run_with_env_streamed`](Self::run_with_env_streamed).
    fn apply_safe_env(cmd: &mut std::process::Command, env: &[(String, String)]) {
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
            // The nested-sandbox marker (D-130): a truly-sandboxed spawn sets this (see
            // `build_command`), and it must survive the env-clear so a child `flux` process sees
            // it and skips re-wrapping (`Sandbox::resolve`) instead of attempting to nest inside
            // its own containment.
            "FLUX_SANDBOXED",
            // C-207: the path to the kubeconfig, forwarded because the *surfacing* side already
            // honors it — `flux_runtime`'s `kubeconfig_present` surfaces the `endpoint` group when
            // `KUBECONFIG` is set — and a probe that reads a variable the executor drops offers ops
            // that cannot work. Dropping it here meant every surfaced `kubernetes.*` op ran a
            // `kubectl` that silently fell back to `~/.kube/config`. The alternative (stop honoring
            // `KUBECONFIG` when deciding to surface) was rejected: it hides the group from users
            // whose setup is fine to avoid mis-surfacing for users whose setup is unusual.
            // On the allow-list posture: this is a *path* to a config file, the same category as
            // `PATH` and `HOME` above, not a secret value — the deny-by-default rule is that flux
            // never forwards a credential from the host env, and a filename is not one. The file it
            // names does hold credentials, but reading it is precisely what `kubectl` needs, and is
            // no more than the `~/.kube/config` it already reads through the forwarded `HOME`.
            "KUBECONFIG",
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
    /// `spawn_interactive`, `spawn_debug_pipe`) layers only its own stdio on top of the command this
    /// returns, so the envelope has no bypass.
    ///
    /// `confinement` (D-130) is the OS-sandbox seam: for [`Confinement::Sandboxed`],
    /// [`Sandbox::ensure_available`] runs first (the fail-closed backstop — `require` + no usable
    /// backend refuses to spawn even if a caller skipped the CLI's startup preflight), then — when
    /// the sandbox is actually active — `argv` is rewritten to a backend-wrapper prefix via
    /// [`Sandbox::wrap_argv`] **before** `argv.split_first()`, so `current_dir`/`kill_on_drop`/
    /// `process_group`/`apply_safe_env` below apply to the wrapper process unchanged (it, not the
    /// original program, is what actually gets spawned). [`Confinement::Exempt`] skips all of this
    /// — the spawn is never wrapped and never subject to `require`.
    fn build_command(
        &self,
        argv: &[String],
        env: &[(String, String)],
        isolate_process_group: bool,
        confinement: Confinement,
    ) -> Result<std::process::Command> {
        if confinement == Confinement::Sandboxed {
            self.sandbox.ensure_available()?;
        }
        let wrapped;
        let argv: &[String] = if confinement == Confinement::Sandboxed && self.sandbox.is_active() {
            let policy = SpawnPolicy::for_workspace(&self.workspace, self.sandbox.settings());
            wrapped = self.sandbox.wrap_argv(argv, &policy)?;
            &wrapped
        } else {
            argv
        };
        let Some((program, args)) = argv.split_first() else {
            return Err(Error::Other("empty command".to_string()));
        };
        let mut cmd = std::process::Command::new(program);
        cmd.args(args).current_dir(self.workspace.root());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            if isolate_process_group {
                cmd.process_group(0);
            }
        }
        #[cfg(not(unix))]
        let _ = isolate_process_group;
        if confinement == Confinement::Sandboxed && self.sandbox.is_active() {
            self.sandbox.configure(&mut cmd)?;
        }
        Self::apply_safe_env(&mut cmd, env);
        if let Some((key, value)) = sandbox::sandbox_marker(confinement, &self.sandbox) {
            cmd.env(key, value);
        }
        Ok(cmd)
    }

    /// Tokio adapter over [`Self::build_command`]. Process construction and every safety setting
    /// stay owned by the synchronous base builder so startup-time backend probes can use the exact
    /// same choke point without creating or nesting a Tokio runtime.
    fn build_tokio_command(
        &self,
        argv: &[String],
        env: &[(String, String)],
        isolate_process_group: bool,
        confinement: Confinement,
    ) -> Result<tokio::process::Command> {
        let cmd = self.build_command(argv, env, isolate_process_group, confinement)?;
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.kill_on_drop(true);
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
        self.run_with_env_streamed_confinement(argv, env, timeout, Confinement::Sandboxed)
            .await
    }

    /// Streamed counterpart to [`Self::run_with_env_exempt`], for a trusted host whose terminal
    /// output is intentionally inherited (local eval `--watch`).
    pub async fn run_with_env_streamed_exempt(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ProcessOutput> {
        self.run_with_env_streamed_confinement(argv, env, timeout, Confinement::Exempt)
            .await
    }

    async fn run_with_env_streamed_confinement(
        &self,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
        confinement: Confinement,
    ) -> Result<ProcessOutput> {
        let mut cmd = self.build_tokio_command(argv, env, true, confinement)?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        let program = argv[0].clone();

        let child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {program}: {e}")))?;
        await_process(child, None, None, program, timeout, None).await
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
        let mut cmd = self.build_tokio_command(argv, env, true, Confinement::Sandboxed)?;
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
        let mut cmd = self.build_tokio_command(argv, &[], false, Confinement::Sandboxed)?;
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
    ///
    /// **`Confinement::Exempt` (D-130), deliberately, v1-only:** Chrome runs its own content
    /// sandbox, which needs to create a *nested* user namespace; forcing `--no-sandbox` on Chrome
    /// so it fits inside an outer bwrap/Seatbelt wrapper would trade a strong, purpose-built
    /// sandbox for a much weaker generic one — a net security loss, not a gain. It would also
    /// break the fd-3/4 `pre_exec` wiring below, which maps the CDP socketpair onto the fds Chrome
    /// itself expects to inherit — a wrapper's own `exec` of the real binary would need to
    /// preserve those fds across two `exec`s instead of one. Browser confinement stays handled by
    /// Chrome's own sandbox plus the env-clear spawn and CDP egress interception (D-124); revisit
    /// in a follow-up story, not this epic (see `docs/designs/process-sandboxing.md`).
    #[cfg(unix)]
    pub fn spawn_debug_pipe(&self, argv: &[String], env: &[(String, String)]) -> Result<PipeChild> {
        use std::os::unix::io::AsRawFd;
        use std::os::unix::process::CommandExt;

        let (parent_end, child_end) = std::os::unix::net::UnixStream::pair()
            .map_err(|e| Error::Other(format!("cdp socketpair: {e}")))?;
        let child_fd = child_end.as_raw_fd();

        let mut cmd = self.build_tokio_command(argv, env, false, Confinement::Exempt)?;
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
        let dir = sandbox::fixture_dir("sys-test");
        let ws = Workspace::new(&dir).unwrap();
        (dir, System::new(ws))
    }

    /// C-209: a transient `TMPDIR` must never capture another test's fixture root.
    ///
    /// Deterministic reproduction of the gate flake. The hijacker thread mutates `TMPDIR` exactly
    /// the way `sandbox`'s `wrap_argv_rejects_root_from_automatic_tmpdir_too` does — under
    /// `EnvGuard`, to a directory it then deletes — and holds it until the victim reports back or a
    /// deadline passes. A victim that reads the temp dir *bare* answers at once, from inside that
    /// window, and roots its fixture in the doomed directory; a victim that reads it under the same
    /// env lock cannot run until the hijacker is gone, so the deadline is what releases it and the
    /// root lands under the restored temp dir. Only the second outcome passes.
    #[test]
    fn a_transient_tmpdir_never_captures_a_fixture_root() {
        let transient = sandbox::fixture_dir("c209-transient");
        let (hijacked_tx, hijacked_rx) = std::sync::mpsc::channel();
        let (built_tx, built_rx) = std::sync::mpsc::channel();

        let doomed = transient.clone();
        let hijacker = std::thread::spawn(move || {
            let _env = sandbox::EnvGuard::new(&["TMPDIR"]);
            std::env::set_var("TMPDIR", &doomed);
            hijacked_tx.send(()).unwrap();
            // A guarded victim cannot answer while this thread holds the lock; an unguarded one
            // answers immediately. Either way the transient root is gone before TMPDIR is restored.
            let _ = built_rx.recv_timeout(Duration::from_millis(250));
            std::fs::remove_dir_all(&doomed).ok();
        });

        hijacked_rx.recv().unwrap();
        let dir = sandbox::fixture_dir("c209-victim");
        let _ = built_tx.send(());
        hijacker.join().unwrap();

        assert!(
            !dir.starts_with(&transient),
            "fixture root captured by a transient TMPDIR: {}",
            dir.display()
        );
        assert!(
            dir.is_dir(),
            "fixture root vanished with the transient TMPDIR: {}",
            dir.display()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-209 regression guard: the "read the process env under the env lock" invariant is only
    /// worth something if a new bare read cannot quietly reappear, so enforce it on the source
    /// rather than in a comment. No test module in this crate may call `std::env::temp_dir` itself
    /// — fixture roots come from `sandbox::fixture_path`/`fixture_dir` — and `sandbox`'s tests may
    /// reach `SpawnPolicy::for_workspace` (which reads `TMPDIR`/`CARGO_HOME`/`RUSTUP_HOME`/`HOME`)
    /// only through their own `workspace_policy` wrapper. Production code above each file's
    /// `mod tests` line — `worktree_base_dir`, the spawn path — is deliberately out of scope.
    #[test]
    fn no_bare_temp_dir_in_the_test_modules() {
        // Assembled at runtime so this test never matches its own source.
        let needle = format!("std::env::{}()", "temp_dir");
        for (file, src) in [
            ("lib.rs", include_str!("lib.rs")),
            ("sandbox.rs", include_str!("sandbox.rs")),
            ("net.rs", include_str!("net.rs")),
        ] {
            let (_, test_module) = src
                .split_once("\nmod tests {")
                .unwrap_or_else(|| panic!("{file} has no `mod tests` block to guard"));
            assert!(
                !test_module.contains(&needle),
                "{file}'s test module calls {needle} directly — build fixture roots through \
                 sandbox::fixture_path/fixture_dir, which read it under the sandbox env lock so a \
                 concurrent TMPDIR test cannot capture them (C-209)"
            );
        }

        // The same invariant for the policy builder: exactly one call, inside `workspace_policy`.
        let (_, sandbox_tests) = include_str!("sandbox.rs")
            .split_once("\nmod tests {")
            .expect("sandbox.rs has no `mod tests` block to guard");
        let policy_calls = sandbox_tests
            .matches(&format!("SpawnPolicy::{}(", "for_workspace"))
            .count();
        assert_eq!(
            policy_calls, 1,
            "sandbox.rs's test module must reach SpawnPolicy::for_workspace only through its \
             workspace_policy() wrapper, which reads the env under the sandbox env lock (C-209)"
        );
    }

    /// C-97: the re-root derive must preserve the *entire* access posture — dropping `@named`
    /// roots would break global-flow ops inside a worktree, and dropping `read_roots`/
    /// `unconfined` would silently narrow what the user granted.
    #[test]
    fn with_root_preserves_access_posture() {
        let (dir_a, _) = temp_workspace();
        let (dir_b, _) = temp_workspace();
        let (dir_named, _) = temp_workspace();
        let (dir_read, _) = temp_workspace();
        let mut ws = Workspace::new(&dir_a).unwrap();
        ws.add_named_root("global_flows", &dir_named).unwrap();
        ws.add_read_root(&dir_read).unwrap();
        ws.set_unconfined(true);

        let rerooted = ws.with_root(&dir_b).unwrap();
        assert_eq!(rerooted.root(), dir_b.canonicalize().unwrap());
        assert!(rerooted.has_named_root("global_flows"));
        assert_eq!(rerooted.read_roots(), ws.read_roots());
        assert!(rerooted.is_unconfined());

        // A missing target stays a hard error — never a silently mis-rooted workspace.
        assert!(ws.with_root(dir_b.join("missing")).is_err());
    }

    /// C-97: `System::rerooted` keeps the resolved sandbox (posture object identity is opaque, so
    /// assert via settings) while swapping the workspace root.
    #[test]
    fn rerooted_system_keeps_sandbox_and_moves_root() {
        let (dir_a, _) = temp_workspace();
        let (dir_b, _) = temp_workspace();
        let system = System::new(Workspace::new(&dir_a).unwrap());
        let rerooted = system.rerooted(&dir_b).unwrap();
        assert_eq!(rerooted.workspace().root(), dir_b.canonicalize().unwrap());
        assert_eq!(
            format!("{:?}", rerooted.sandbox().settings()),
            format!("{:?}", system.sandbox().settings()),
        );
    }

    /// C-97: the worktree-dir helpers are fail-closed — cleanup refuses anything that is not a
    /// directly-under-tmp `flux-worktree-*` allocation.
    #[test]
    fn worktree_dir_alloc_and_guarded_removal() {
        let (other, _) = temp_workspace();
        // Pin the base via FLUX_WORKTREE_DIR (under the env lock) so the test never touches the
        // real ~/.flux/worktrees and stays hermetic under parallel test threads. The fixture
        // helpers stay usable inside the guard — they detect that this thread already holds it.
        let _env = sandbox::EnvGuard::new(&["FLUX_WORKTREE_DIR"]);
        let base = sandbox::fixture_path("wt-base");
        std::env::set_var("FLUX_WORKTREE_DIR", &base);

        let dir = allocate_worktree_dir().unwrap();
        assert!(dir.is_dir());
        assert_eq!(dir.parent().unwrap(), base.canonicalize().unwrap());
        assert!(dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("flux-worktree-"));
        remove_worktree_dir(&dir).unwrap();
        assert!(!dir.exists());
        // Idempotent: a second removal of the same allocation is fine.
        remove_worktree_dir(&dir).unwrap();

        // Refusals: wrong prefix, nested path, workspace-shaped path, and an entry under a
        // DIFFERENT base than the resolved one (e.g. a stale /tmp allocation after the base moved).
        assert!(remove_worktree_dir(&other).is_err());
        assert!(remove_worktree_dir(&base.join("flux-worktree-x/nested")).is_err());
        let foreign = sandbox::fixture_dir("worktree-foreign");
        assert!(remove_worktree_dir(&foreign).is_err());
        assert!(foreign.exists());
        std::fs::remove_dir(&foreign).unwrap();
        assert!(other.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// The base falls back to `$HOME/.flux/worktrees` (never `/tmp`, which is commonly a
    /// RAM-backed tmpfs a worktree build would fill) when `FLUX_WORKTREE_DIR` is unset.
    #[test]
    fn worktree_base_prefers_home_flux_over_tmp() {
        let _env = sandbox::EnvGuard::new(&["FLUX_WORKTREE_DIR"]);
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(
                worktree_base_dir(),
                Path::new(&home).join(".flux").join("worktrees")
            );
        }
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
    async fn path_exists_is_guarded_and_preserves_missing() {
        let (dir, sys) = temp_workspace();
        assert!(!sys.path_exists("missing.txt").await.unwrap());
        sys.write_file("nested/present.txt", "ok").await.unwrap();
        assert!(sys.path_exists("nested/present.txt").await.unwrap());
        assert!(sys.path_exists("../outside.txt").await.is_err());
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
    async fn write_file_bytes_roundtrips_raw_including_nul() {
        let (dir, sys) = temp_workspace();
        // Bytes with an embedded NUL and invalid UTF-8 — this payload cannot ride the &str
        // write_file path at all; write_file_bytes must persist it byte-exact (L-78).
        let raw = [b'h', b'i', 0u8, 0xFF, b'!'];
        sys.write_file_bytes("b.bin", &raw).await.unwrap();
        assert_eq!(sys.read_file_bytes("b.bin").await.unwrap(), raw);
        assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), raw);
        // And the UTF-8 read path still rejects what landed.
        assert!(sys.read_file("b.bin").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_file_bytes_creates_nested_parents() {
        let (dir, sys) = temp_workspace();
        sys.write_file_bytes("sub/dir/b.bin", &[1u8, 2, 3])
            .await
            .unwrap();
        assert_eq!(
            sys.read_file_bytes("sub/dir/b.bin").await.unwrap(),
            [1u8, 2, 3]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_file_bytes_rejects_escape() {
        let (dir, sys) = temp_workspace();
        assert!(sys.write_file_bytes("../escape.bin", b"x").await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn write_file_bytes_rejects_read_only_root() {
        let ws_dir = sandbox::fixture_dir("sys-wsb");
        let ext_dir = sandbox::fixture_dir("sys-extb");

        let mut ws = Workspace::new(&ws_dir).unwrap();
        ws.add_read_root(&ext_dir).unwrap();
        let sys = System::new(ws);

        let ext_file = ext_dir.join("out.bin");
        // Writes stay confined to the primary root — a read-only root is never a write target.
        assert!(sys
            .write_file_bytes(ext_file.to_str().unwrap(), b"x")
            .await
            .is_err());

        std::fs::remove_dir_all(&ws_dir).ok();
        std::fs::remove_dir_all(&ext_dir).ok();
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
        let ws_dir = sandbox::fixture_dir("sys-ws");
        let ext_dir = sandbox::fixture_dir("sys-ext");
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
        let ws_dir = sandbox::fixture_dir("sys-ws2");
        let ext_dir = sandbox::fixture_dir("sys-ext2");
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
        let ws_dir = sandbox::fixture_dir("sys-unc");
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
    #[test]
    fn control_plane_reads_and_discovery_surface_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let (dir, sys) = temp_workspace();
        let outside = sandbox::fixture_dir("sys-metadata-outside");
        std::fs::write(outside.join("secret.md"), "OUTSIDE").unwrap();
        std::fs::create_dir_all(dir.join(".flux/skills")).unwrap();

        symlink(outside.join("secret.md"), dir.join(".flux/config.toml")).unwrap();
        symlink(
            outside.join("secret.md"),
            dir.join(".flux/skills/escaped.md"),
        )
        .unwrap();

        assert!(sys.read_optional_text(".flux/config.toml").is_err());
        assert!(sys
            .read_dir_text_files_with_nested(".flux/skills", "md", "SKILL.md")
            .is_err());

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn discovers_namespaced_skill_md_two_levels_deep() {
        // Claude namespaced trees: `.claude/skills/<ns>/<name>/SKILL.md` — one level deeper than
        // the historical `<name>/SKILL.md` shape.
        let (dir, sys) = temp_workspace();
        std::fs::create_dir_all(dir.join(".claude/skills/ns/foo")).unwrap();
        std::fs::write(
            dir.join(".claude/skills/ns/foo/SKILL.md"),
            "---\nname: foo\n---\nbody",
        )
        .unwrap();

        let files = sys
            .read_dir_text_files_with_nested(".claude/skills", "md", "SKILL.md")
            .unwrap();
        assert_eq!(
            files,
            vec![(
                ".claude/skills/ns/foo/SKILL.md".to_string(),
                "---\nname: foo\n---\nbody".to_string()
            )]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nested_skill_md_beyond_max_depth_is_not_found() {
        let (dir, sys) = temp_workspace();
        // One level past `System::NESTED_FILE_MAX_DEPTH`: never reached.
        let too_deep = dir.join(".claude/skills/a/b/c/d/e");
        std::fs::create_dir_all(&too_deep).unwrap();
        std::fs::write(too_deep.join("SKILL.md"), "---\nname: too-deep\n---\nx").unwrap();

        let files = sys
            .read_dir_text_files_with_nested(".claude/skills", "md", "SKILL.md")
            .unwrap();
        assert!(
            files.is_empty(),
            "SKILL.md beyond the max depth bound must not surface: {files:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_skill_directory_claims_its_subtree_so_references_is_not_a_separate_skill() {
        // A skill's own `references/` directory (containing its own `.md` files) must not surface
        // as a separate skill: the parent directory already claimed the subtree by having SKILL.md.
        let (dir, sys) = temp_workspace();
        std::fs::create_dir_all(dir.join(".claude/skills/foo/references")).unwrap();
        std::fs::write(
            dir.join(".claude/skills/foo/SKILL.md"),
            "---\nname: foo\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join(".claude/skills/foo/references/notes.md"),
            "internal notes, not a skill",
        )
        .unwrap();

        let files = sys
            .read_dir_text_files_with_nested(".claude/skills", "md", "SKILL.md")
            .unwrap();
        assert_eq!(
            files,
            vec![(
                ".claude/skills/foo/SKILL.md".to_string(),
                "---\nname: foo\n---\nbody".to_string()
            )]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_nested_symlink_escape_below_the_top_level() {
        use std::os::unix::fs::symlink;

        let (dir, sys) = temp_workspace();
        let outside = sandbox::fixture_dir("sys-nested-outside");
        std::fs::write(outside.join("SKILL.md"), "OUTSIDE").unwrap();
        std::fs::create_dir_all(dir.join(".claude/skills/ns")).unwrap();
        symlink(&outside, dir.join(".claude/skills/ns/escaped")).unwrap();

        let error = sys
            .read_dir_text_files_with_nested(".claude/skills", "md", "SKILL.md")
            .unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_control_plane_write_rejects_file_and_parent_symlink_escapes() {
        use std::os::unix::fs::symlink;

        for parent_escape in [false, true] {
            let (dir, sys) = temp_workspace();
            let outside = sandbox::fixture_dir("sys-atomic-outside");
            if parent_escape {
                symlink(&outside, dir.join(".flux")).unwrap();
            } else {
                std::fs::create_dir_all(dir.join(".flux")).unwrap();
                std::fs::write(outside.join("config.toml"), "outside").unwrap();
                symlink(outside.join("config.toml"), dir.join(".flux/config.toml")).unwrap();
            }

            assert!(sys
                .write_file_atomic(".flux/config.toml", "permissions = {}")
                .is_err());
            if !parent_escape {
                assert_eq!(
                    std::fs::read_to_string(outside.join("config.toml")).unwrap(),
                    "outside"
                );
            } else {
                assert!(!outside.join("config.toml").exists());
            }
            std::fs::remove_dir_all(&dir).ok();
            std::fs::remove_dir_all(&outside).ok();
        }
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
        let outside = sandbox::fixture_path("escape-target");
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
        let dir = sandbox::fixture_path("sys-path-id");
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

    /// D-130: `FLUX_SANDBOXED` is in `SAFE_ENV`, so a flux process that is itself already marked
    /// (inherited from an outer real sandbox) forwards the marker to ITS OWN children even though
    /// `apply_safe_env` clears the environment first — the nested-run detection
    /// (`Sandbox::resolve`) depends on the marker surviving every hop down the process tree, not
    /// just the first.
    #[tokio::test]
    async fn flux_sandboxed_marker_survives_env_clear_like_other_safe_env_entries() {
        let (dir, sys) = temp_workspace();
        // FIX G: take the SAME lock the `sandbox::tests` use so a concurrent test mutating
        // FLUX_SANDBOXED can't race the marker this test depends on. The guard is a struct wrapping
        // the lock (not a raw `MutexGuard`), so holding it across the `.await` is sound and does not
        // trip `clippy::await_holding_lock`; it restores FLUX_SANDBOXED on drop.
        let _env = sandbox::EnvGuard::new(&["FLUX_SANDBOXED"]);
        std::env::set_var("FLUX_SANDBOXED", "1");
        let out = sys
            .run(&["env".to_string()], Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            out.stdout.contains("FLUX_SANDBOXED=1"),
            "FLUX_SANDBOXED did not survive the env-clear: {}",
            out.stdout
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-207: `KUBECONFIG` is in `SAFE_ENV`, so a kubeconfig at a non-default path reaches the
    /// child. The `kubernetes` discovery signal surfaces the `endpoint` group when `KUBECONFIG` is
    /// set (`flux_runtime`'s `kubeconfig_present`); if the executor then dropped it, every op that
    /// signal surfaced would run a `kubectl` that silently fell back to `~/.kube/config`. The
    /// surfacing signal and the execution environment have to read the same source of truth.
    #[tokio::test]
    async fn kubeconfig_survives_env_clear_so_surfacing_and_execution_agree() {
        let (dir, sys) = temp_workspace();
        // Same lock the other env-mutating tests take, so a concurrent test can't race the value
        // this one depends on; the guard restores KUBECONFIG on drop.
        let _env = sandbox::EnvGuard::new(&["KUBECONFIG"]);
        std::env::set_var("KUBECONFIG", "/tmp/flux-c207-nondefault/kubeconfig.yaml");
        let out = sys
            .run(&["env".to_string()], Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            out.stdout
                .contains("KUBECONFIG=/tmp/flux-c207-nondefault/kubeconfig.yaml"),
            "KUBECONFIG did not survive the env-clear: {}",
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

    /// D-130 acceptance: the fail-closed backstop. A `Require`-mode sandbox with no usable backend
    /// (D-130 never resolves one) refuses to spawn at all — `run` returns a config error naming the
    /// unavailability reason — rather than silently falling back to running unconfined. This is the
    /// per-spawn backstop behind the CLI's startup preflight: it must hold even for a caller that
    /// somehow skipped that preflight (e.g. a `System` built directly with `with_sandbox`).
    #[tokio::test]
    async fn require_sandbox_with_unsupported_backend_fails_closed_on_run() {
        let (dir, sys) = temp_workspace();

        // Force discovery to fail on BOTH platforms regardless of whether *this* machine happens to
        // have a real, working backend (D-131/D-132 landed real discovery+probing): point bwrap
        // (Linux) AND sandbox-exec (macOS, FIX H — else macOS resolves a live Seatbelt backend and
        // this test's premise breaks) at nonexistent paths, then require it. The env mutation and
        // the synchronous `resolve()` run under the shared sandbox env lock (FIX G, via `EnvGuard`)
        // and are fully restored when the guard drops at the end of this block — so no std
        // `MutexGuard` is held across the `.await` further down. FLUX_SANDBOXED is cleared too, so a
        // stray marker can't make `resolve()` report `AlreadyConfined` (which would satisfy
        // `require` and defeat the test).
        let sandbox = {
            let _g = sandbox::EnvGuard::new(&[
                "FLUX_BWRAP_BIN",
                "FLUX_SANDBOX_EXEC_BIN",
                "FLUX_SANDBOX",
                "FLUX_SANDBOXED",
            ]);
            std::env::set_var(
                "FLUX_BWRAP_BIN",
                "/nonexistent/definitely-not-a-real-bwrap-d126",
            );
            std::env::set_var(
                "FLUX_SANDBOX_EXEC_BIN",
                "/nonexistent/definitely-not-a-real-sandbox-exec-d126",
            );
            std::env::set_var("FLUX_SANDBOX", "require");
            sandbox::Sandbox::resolve(sandbox::SandboxSettings::from_env())
        };
        assert!(
            !sandbox.is_active(),
            "an unresolvable backend path must leave `require` unsatisfiable"
        );
        let reason = sandbox
            .reason()
            .expect("an inactive sandbox always names an unavailability reason")
            .to_string();

        let sys = sys.with_sandbox(sandbox);
        let err = sys
            .run(&["true".to_string()], Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains(&reason),
            "error should name the reason ({reason:?}): {err}"
        );
        let out = sys
            .run_with_env_exempt(
                &["true".to_string()],
                &[("FLUX_SANDBOX".to_string(), "require".to_string())],
                Duration::from_secs(10),
            )
            .await
            .expect("the explicit trusted-host exemption bypasses only OS wrapping");
        assert_eq!(out.exit_code, 0);
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

    /// C-158: the observer must see lines WHILE the child runs, not one batch at the end — that is
    /// the whole point, and a capture that only flushed at exit would pass a naive "did we see the
    /// lines" assertion. The child sleeps between writes, so the run future is still pending when
    /// the first line is asserted.
    #[tokio::test]
    async fn observed_run_reports_lines_while_the_child_is_still_running() {
        let (dir, sys) = temp_workspace();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let first_line_at: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let sink = seen.clone();
        let stamp = first_line_at.clone();
        let observer: OutputObserver = Arc::new(move |line: &str| {
            if stamp.lock().unwrap().is_none() {
                *stamp.lock().unwrap() = Some(std::time::Instant::now());
            }
            sink.lock().unwrap().push(line.to_string());
        });

        let started = std::time::Instant::now();
        let out = sys
            .run_with_env_observed(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo one; sleep 0.4; echo two".to_string(),
                ],
                &[],
                Duration::from_secs(20),
                observer,
            )
            .await
            .unwrap();
        let finished_at = std::time::Instant::now();

        let lines = seen.lock().unwrap().clone();
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
        // The result is unchanged by observing it.
        assert_eq!(out.stdout, "one\ntwo\n");
        assert_eq!(out.exit_code, 0);

        // The liveness claim: the first line arrived well before the process exited.
        let first = first_line_at.lock().unwrap().expect("a line was observed");
        assert!(
            finished_at.duration_since(first) >= Duration::from_millis(200),
            "first line landed {:?} before exit — that is batched-at-exit, not live \
             (run took {:?})",
            finished_at.duration_since(first),
            finished_at.duration_since(started),
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A line split across two reads must reassemble, including a multi-byte codepoint straddling
    /// the boundary — the reason emission is line-oriented rather than per-chunk.
    #[test]
    fn emit_lines_reassembles_across_chunk_boundaries() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let observer: OutputObserver = Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string());
        });

        let mut pending = Vec::new();
        // "héllo\n" with the 2-byte 'é' split down the middle.
        let full = "héllo\nworld".as_bytes().to_vec();
        let split = 2; // mid-'é'
        pending.extend_from_slice(&full[..split]);
        emit_lines(&mut pending, &observer);
        assert!(
            seen.lock().unwrap().is_empty(),
            "a partial line must not be emitted"
        );
        pending.extend_from_slice(&full[split..]);
        emit_lines(&mut pending, &observer);
        assert_eq!(seen.lock().unwrap().clone(), vec!["héllo".to_string()]);
        assert_eq!(
            pending, b"world",
            "the trailing partial line stays buffered"
        );
    }

    /// Windows line endings must not leak a stray `\r` into a rendered row.
    #[test]
    fn emit_lines_strips_carriage_returns_and_blank_lines() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let observer: OutputObserver = Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string());
        });
        let mut pending = b"a\r\n\r\nb\n".to_vec();
        emit_lines(&mut pending, &observer);
        assert_eq!(
            seen.lock().unwrap().clone(),
            vec!["a".to_string(), "b".to_string()]
        );
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

    // -- D-131 live smokes: real bubblewrap confinement --------------------------------------
    //
    // Opt-in (`FLUX_LIVE_SANDBOX_SMOKE=1`), like every live-external test in this repo (mirrors
    // `flux-web::browser::live_smoke_open_goto_snapshot_close_no_orphan`): CI runners and most
    // dev machines either lack `bwrap` or run inside default-seccomp Docker where unprivileged
    // user namespaces are refused, so an auto-run keyed only on discovery would be
    // nondeterministic across environments. Double-gated: the env var AND a genuinely active
    // backend, else `eprintln!` + skip.

    /// Serializes the live smokes against **each other** (not the rest of this crate's — fully
    /// parallel — test suite). A real `bwrap --ro-bind / /` spawn creates a fresh mount namespace
    /// over the whole filesystem; running several concurrently under this binary's default
    /// multi-threaded test harness was empirically observed to starve the system enough to cause
    /// spurious failures even in unrelated, non-sandboxed tests (D-131 hardening). One at a time
    /// keeps the smokes reliable without forcing the whole suite to `--test-threads=1`.
    // `tokio::sync::Mutex`, not `std::sync::Mutex`: the guard is held across `.await` points in
    // every caller below, which `std::sync::MutexGuard` cannot do (not `Send`-safe across yields;
    // `clippy::await_holding_lock` correctly rejects it).
    static LIVE_SMOKE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn sandboxed_workspace(
        network: bool,
    ) -> Option<(tokio::sync::MutexGuard<'static, ()>, PathBuf, System)> {
        let guard = LIVE_SMOKE_LOCK.lock().await;
        if std::env::var("FLUX_LIVE_SANDBOX_SMOKE").is_err() {
            eprintln!(
                "SKIP live_smoke: set FLUX_LIVE_SANDBOX_SMOKE=1 to run against a real sandbox \
                 backend"
            );
            return None;
        }
        let settings = sandbox::SandboxSettings {
            mode: sandbox::SandboxMode::On,
            network,
            extra_writable: Vec::new(),
        };
        let sandbox = sandbox::Sandbox::resolve(settings);
        if !sandbox.is_active() {
            eprintln!(
                "SKIP live_smoke: no usable sandbox backend discovered ({:?})",
                sandbox.reason()
            );
            return None;
        }
        let (dir, sys) = temp_workspace();
        Some((guard, dir, sys.with_sandbox(sandbox)))
    }

    #[tokio::test]
    async fn live_smoke_sandboxed_run_writes_inside_workspace_ok() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(true).await else {
            return;
        };
        let out = sys
            .run(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo hi > inside.txt && cat inside.txt".to_string(),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(out.stdout.contains("hi"), "{}", out.stdout);
        assert!(dir.join("inside.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn live_smoke_sandboxed_write_outside_workspace_under_home_fails() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(true).await else {
            return;
        };
        let home = std::env::var("HOME").expect("HOME set for this smoke");
        let target = format!("{home}/.flux-sandbox-live-smoke-{}", std::process::id());
        std::fs::remove_file(&target).ok();

        let out = sys
            .run(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("echo pwned > {target}"),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_ne!(
            out.exit_code, 0,
            "write outside the workspace under $HOME must fail under sandbox: {}",
            out.stderr
        );
        assert!(
            !Path::new(&target).exists(),
            "the file must not have been created outside the workspace"
        );
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(&target).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_smoke_missing_configured_writable_is_created_and_usable() {
        let Some((_serial, dir, discovered_sys)) = sandboxed_workspace(true).await else {
            return;
        };
        let home = std::env::var("HOME").expect("HOME set for this smoke");
        let output_root = PathBuf::from(home).join(format!(
            ".flux-sandbox-new-output-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&output_root).ok();
        let sandbox = sandbox::Sandbox::resolve(sandbox::SandboxSettings {
            mode: sandbox::SandboxMode::On,
            network: true,
            extra_writable: vec![output_root.clone()],
        });
        assert!(sandbox.is_active(), "{:?}", sandbox.reason());
        let sys = System::new(Workspace::new(&dir).unwrap()).with_sandbox(sandbox);
        let target = output_root.join("written.txt");
        let out = sys
            .run(
                &["touch".to_string(), target.to_string_lossy().into_owned()],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(target.is_file());
        drop(discovered_sys);
        std::fs::remove_dir_all(&output_root).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn live_smoke_sandboxed_network_off_blocks_test_owned_loopback_listener() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(false).await else {
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });

        // `curl` doesn't need shell involvement to hit a bare IP:port; a nonzero exit (typically 7,
        // "Failed to connect") proves the sandboxed network namespace can't reach a listener that
        // is genuinely open in the OUTER (host) namespace — namespace-fresh loopback, not a
        // firewall rule.
        let out = sys
            .run(
                &[
                    "curl".to_string(),
                    "--max-time".to_string(),
                    "2".to_string(),
                    "-sS".to_string(),
                    format!("http://127.0.0.1:{port}/"),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_ne!(
            out.exit_code, 0,
            "connecting to a host-owned loopback listener must fail with network=off: {}",
            out.stderr
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn live_smoke_exempt_host_keeps_network_when_descendant_posture_is_closed() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(false).await else {
            return;
        };
        use tokio::io::AsyncWriteExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nprovider",
                )
                .await
                .unwrap();
        });
        let out = sys
            .run_with_env_exempt(
                &[
                    "curl".to_string(),
                    "--max-time".to_string(),
                    "2".to_string(),
                    "-sS".to_string(),
                    format!("http://127.0.0.1:{port}/"),
                ],
                &[
                    ("FLUX_SANDBOX".to_string(), "on".to_string()),
                    ("FLUX_SANDBOX_NET".to_string(), "0".to_string()),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(out.stdout, "provider");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_smoke_network_on_keeps_dns_but_masks_host_ipc_sockets() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(true).await else {
            return;
        };
        let out = sys
            .run(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "getent ahostsv4 example.com >/dev/null \
                     && test ! -S /run/dbus/system_bus_socket \
                     && test ! -S /run/systemd/resolve/io.systemd.Resolve \
                     && test ! -S /run/systemd/resolve/io.systemd.Resolve.Monitor"
                        .to_string(),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(
            out.exit_code, 0,
            "DNS should work without restoring host D-Bus/resolver sockets: {}",
            out.stderr
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_smoke_linked_worktree_git_add_can_update_external_metadata() {
        let Some((_serial, dir, parent_sys)) = sandboxed_workspace(true).await else {
            return;
        };
        let parent_sys_ref = &parent_sys;
        let run = move |argv: Vec<String>| async move {
            parent_sys_ref.run(&argv, Duration::from_secs(10)).await
        };
        let init = run(vec![
            "git".into(),
            "init".into(),
            "-q".into(),
            "main".into(),
        ])
        .await
        .unwrap();
        assert_eq!(init.exit_code, 0, "{}", init.stderr);
        std::fs::write(dir.join("main/file.txt"), "base\n").unwrap();
        let add = run(vec![
            "git".into(),
            "-C".into(),
            "main".into(),
            "add".into(),
            "file.txt".into(),
        ])
        .await
        .unwrap();
        assert_eq!(add.exit_code, 0, "{}", add.stderr);
        let commit = run(vec![
            "git".into(),
            "-C".into(),
            "main".into(),
            "-c".into(),
            "user.name=Flux Test".into(),
            "-c".into(),
            "user.email=flux@example.invalid".into(),
            "commit".into(),
            "-q".into(),
            "-m".into(),
            "base".into(),
        ])
        .await
        .unwrap();
        assert_eq!(commit.exit_code, 0, "{}", commit.stderr);
        let worktree = run(vec![
            "git".into(),
            "-C".into(),
            "main".into(),
            "worktree".into(),
            "add".into(),
            "-q".into(),
            "--detach".into(),
            "../linked".into(),
        ])
        .await
        .unwrap();
        assert_eq!(worktree.exit_code, 0, "{}", worktree.stderr);

        let linked_sys = System::new(Workspace::new(dir.join("linked")).unwrap())
            .with_sandbox(parent_sys.sandbox().clone());
        let add = linked_sys
            .run(
                &[
                    "sh".into(),
                    "-c".into(),
                    "echo changed >> file.txt && git add file.txt && git status --porcelain".into(),
                ],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(
            add.exit_code, 0,
            "linked-worktree index/object writes must reach the external common dir: {}",
            add.stderr
        );
        assert!(add.stdout.contains("M  file.txt"), "{}", add.stdout);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn live_smoke_sandboxed_spawn_interactive_round_trips_stdin_stdout() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(true).await else {
            return;
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let InteractiveChild {
            mut child,
            mut stdin,
            mut stdout,
        } = sys.spawn_interactive(&["cat".to_string()]).unwrap();
        stdin
            .write_all(b"hello-through-the-sandbox\n")
            .await
            .unwrap();
        drop(stdin); // close stdin so `cat` sees EOF and exits after echoing.

        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello-through-the-sandbox\n");
        let _ = child.wait().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_smoke_sandboxed_spawn_background_kill_leaves_no_orphan() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(true).await else {
            return;
        };
        // `--unshare-pid` gives the sandboxed tree its own pid namespace, so a pid written from
        // *inside* it (e.g. via `$$`) is meaningless to the host's `/proc` — unlike
        // `spawn_background_kill_stops_descendants` above. Identify the long-lived descendant by a
        // unique marker in its argv instead, and check for it with the host's own `pgrep -f` (which
        // sees every process regardless of pid-namespace nesting).
        let marker = format!(
            "flux-sandbox-orphan-smoke-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let script = format!("exec -a {marker} sleep 300");
        let mut child = sys
            .spawn_background(&["bash".to_string(), "-c".to_string(), script], &[])
            .unwrap();

        fn marker_visible_on_host(marker: &str) -> bool {
            std::process::Command::new("pgrep")
                .args(["-f", marker])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        }

        let mut seen = false;
        for _ in 0..300 {
            if marker_visible_on_host(&marker) {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            seen,
            "marked sandboxed process never became visible on the host"
        );

        child.kill();
        for _ in 0..200 {
            if !child.status().running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut gone = false;
        for _ in 0..300 {
            if !marker_visible_on_host(&marker) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            gone,
            "sandboxed descendant survived the wrapper's death (orphaned): {marker}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn live_smoke_sandboxed_exit_code_propagates() {
        let Some((_serial, dir, sys)) = sandboxed_workspace(true).await else {
            return;
        };
        let out = sys
            .run(
                &["sh".to_string(), "-c".to_string(), "exit 42".to_string()],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(
            out.exit_code, 42,
            "stdout={:?} stderr={:?}",
            out.stdout, out.stderr
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
