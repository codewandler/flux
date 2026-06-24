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

        // Symlink guard: canonicalize the path (or its parent for not-yet-existing files) and
        // re-check that the real target stays inside the root.
        if norm.exists() {
            let canon = norm
                .canonicalize()
                .map_err(|e| Error::Io(std::io::Error::other(e)))?;
            if !canon.starts_with(&base) {
                return Err(Error::Config(format!(
                    "path {input:?} resolves (via symlink) outside the workspace root"
                )));
            }
            return Ok(canon);
        }
        if let Some(parent) = norm.parent() {
            if parent.exists() {
                let cp = parent
                    .canonicalize()
                    .map_err(|e| Error::Io(std::io::Error::other(e)))?;
                if !cp.starts_with(&base) {
                    return Err(Error::Config(format!(
                        "path {input:?} resolves (via symlink) outside the workspace root"
                    )));
                }
                if let Some(file) = norm.file_name() {
                    return Ok(cp.join(file));
                }
            }
        }
        Ok(norm)
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
        let Some((program, args)) = argv.split_first() else {
            return Err(Error::Other("empty command".to_string()));
        };
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(self.workspace.root())
            .stdin(std::process::Stdio::null());

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
        Ok(ProcessOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
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
