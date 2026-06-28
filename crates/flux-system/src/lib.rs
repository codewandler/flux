//! `flux-system` — the *only* place real filesystem/process/env IO happens.
//!
//! Every path is resolved against a [`Workspace`] root (plus optional `@named` roots) and is
//! rejected if it escapes — lexically (`..`) or via symlink (a path that canonicalizes outside
//! the root). Process execution is **argv-only** (no shell), so the model cannot inject shell
//! operators. Tools never touch `std::fs`/`Command` directly; they go through [`System`].

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use flux_core::{Error, Result};

pub mod net;

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// A bounded filesystem view: a primary root plus optional `@named` roots. All access is confined
/// to these roots.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    named: HashMap<String, PathBuf>,
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
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
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

    /// Resolve a workspace-relative (or `@name/...`) path to an absolute path guaranteed to live
    /// inside the corresponding root. Rejects `..` escapes and symlink escapes.
    pub fn resolve(&self, input: &str) -> Result<PathBuf> {
        // A path containing a control byte (newline, CR, NUL, tab, …) is virtually always a
        // bug — typically an untrimmed command substitution flowing into the path, e.g.
        // `echo …` whose trailing newline becomes part of the filename. Such a file gets
        // created but is then unreadable by its apparent name: `glob` matches it via `*`,
        // yet every literal `read`/`stat` misses the hidden byte and fails with ENOENT.
        // Reject it loudly here instead of silently writing a poltergeist file.
        // Expand a leading `~` to the home directory so callers can write
        // `~/.flux/sessions.db` instead of needing the literal absolute path.
        let input = if let Some(rest) = input.strip_prefix('~') {
            // `~` alone or `~/...` — expand to $HOME.
            if rest.is_empty() || rest.starts_with('/') {
                let home = std::env::var("HOME").unwrap_or_default();
                std::borrow::Cow::Owned(format!("{home}{rest}"))
            } else {
                // `~username/...` — not supported; leave as-is.
                std::borrow::Cow::Borrowed(input)
            }
        } else {
            std::borrow::Cow::Borrowed(input)
        };
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

        if !norm.starts_with(&base) {
            return Err(Error::Config(format!(
                "path {input:?} escapes the workspace root {}",
                base.display()
            )));
        }

        // Symlink guard: walk the path component-by-component, chasing every symlink found in the
        // physically-existing prefix and rejecting any whose target escapes the root. Unlike
        // `Path::exists()` (which follows links, so a *dangling* symlink to an outside target reads
        // as "not existing"), this uses `symlink_metadata` and so also catches symlinks whose
        // targets don't exist yet — the case a plain parent-canonicalize misses on write.
        resolve_within_root(&base, &norm).map_err(|_| {
            Error::Config(format!(
                "path {input:?} resolves outside the workspace root"
            ))
        })
    }

    fn base_for<'a>(&self, input: &'a str) -> (PathBuf, &'a str) {
        if let Some(rest) = input.strip_prefix('@') {
            if let Some((name, tail)) = rest.split_once('/') {
                if let Some(base) = self.named.get(name) {
                    return (base.clone(), tail);
                }
            }
        }
        (self.root.clone(), input)
    }
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

/// Decode captured subprocess output, capping it at `max` bytes so a runaway command can't OOM the
/// host. Truncating a byte slice mid-codepoint is safe: `from_utf8_lossy` emits replacement chars
/// rather than panicking (unlike `String::truncate`, which panics off a char boundary).
fn capped_lossy(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let mut s = String::from_utf8_lossy(&bytes[..max]).into_owned();
        s.push_str("\n…[output truncated]");
        s
    }
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

    /// Read a UTF-8 file from within the workspace.
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let p = self.workspace.resolve(path)?;
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
        let p = self.workspace.resolve(path)?;
        Ok(tokio::fs::read(&p).await?)
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
        let p = self.workspace.resolve(path)?;
        let meta = tokio::fs::metadata(&p).await?;
        Ok(meta.modified()?)
    }

    /// List the entries of a directory within the workspace (names only).
    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let p = self.workspace.resolve(path)?;
        let mut rd = tokio::fs::read_dir(&p).await?;
        let mut out = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
        out.sort();
        Ok(out)
    }

    /// Recursively list files under a workspace-relative directory, returning workspace-relative
    /// paths (sorted, capped at `max`). Symlinks are never followed (an escape guard), and the
    /// noisy `.git`/`target`/`node_modules` directories are skipped. Used by `glob`/`grep`.
    pub async fn walk_files(&self, base: &str, max: usize) -> Result<Vec<String>> {
        const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];
        let root = self.workspace.resolve(base)?;
        let ws_root = self.workspace.root().to_path_buf();
        let mut out = Vec::new();
        // A `base` that resolves to a single file → return just that file, so `grep`/`glob` scoped to
        // a file path search that file instead of silently finding nothing (`read_dir` on a file
        // errors, which would otherwise yield an empty walk and a misleading "no matches").
        if tokio::fs::metadata(&root)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            if let Ok(rel) = root.strip_prefix(&ws_root) {
                out.push(rel.to_string_lossy().into_owned());
            }
            return Ok(out);
        }
        let mut stack = vec![root];
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
                    continue; // never follow symlinks (could escape the workspace)
                }
                let path = entry.path();
                if ft.is_dir() {
                    let name = entry.file_name();
                    if SKIP_DIRS.iter().any(|s| name == std::ffi::OsStr::new(s)) {
                        continue;
                    }
                    stack.push(path);
                } else if ft.is_file() {
                    if let Ok(rel) = path.strip_prefix(&ws_root) {
                        out.push(rel.to_string_lossy().into_owned());
                        if out.len() >= max {
                            break;
                        }
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
        let Some((program, args)) = argv.split_first() else {
            return Err(Error::Other("empty command".to_string()));
        };
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(self.workspace.root())
            .stdin(std::process::Stdio::null());

        // Don't leak the agent's environment (which may hold ANTHROPIC_API_KEY and other secrets)
        // into model-spawned commands: start from an empty env and pass only a minimal, non-secret
        // set needed for programs to function, plus the caller's explicit overrides.
        Self::apply_safe_env(&mut cmd, env);

        let fut = cmd.output();
        let output = match tokio::time::timeout(timeout, fut).await {
            Ok(r) => r.map_err(|e| Error::Other(format!("spawn {program}: {e}")))?,
            Err(_) => {
                return Err(Error::Other(format!(
                    "command timed out after {}s",
                    timeout.as_secs()
                )))
            }
        };
        // Cap captured output so a command emitting gigabytes can't exhaust host memory.
        const MAX_OUTPUT: usize = 1024 * 1024;
        Ok(ProcessOutput {
            stdout: capped_lossy(&output.stdout, MAX_OUTPUT),
            stderr: capped_lossy(&output.stderr, MAX_OUTPUT),
            exit_code: output.status.code().unwrap_or(-1),
        })
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
        let Some((program, args)) = argv.split_first() else {
            return Err(Error::Other("empty command".to_string()));
        };
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(self.workspace.root())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        Self::apply_safe_env(&mut cmd, env);

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {program}: {e}")))?;
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(r) => r.map_err(|e| Error::Other(format!("wait {program}: {e}")))?,
            Err(_) => {
                let _ = child.start_kill();
                return Err(Error::Other(format!(
                    "command timed out after {}s",
                    timeout.as_secs()
                )));
            }
        };
        Ok(ProcessOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: status.code().unwrap_or(-1),
        })
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
}
