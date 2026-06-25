//! `flux-context` — assembles per-turn context from an ordered chain of providers.
//!
//! Each [`ContextProvider`] contributes an optional block; [`Projector::system_prompt`] appends
//! them to a base prompt wrapped in `<context source="...">` tags. v1 ships project-file context
//! (`CLAUDE.md`/`AGENTS.md`/`.flux/context.md`) and an environment summary; more providers (files,
//! memory, skills, datasource) plug in here later.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use flux_core::Result;

/// A source of context for a turn.
#[async_trait]
pub trait ContextProvider: Send + Sync {
    fn name(&self) -> &str;
    /// A formatted context block, or `None` if there's nothing to contribute.
    async fn render(&self) -> Result<Option<String>>;
}

/// Reads well-known project-context files under `root`.
pub struct ProjectFiles {
    root: PathBuf,
    files: Vec<String>,
}

impl ProjectFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            files: vec![
                "CLAUDE.md".into(),
                "AGENTS.md".into(),
                ".flux/context.md".into(),
            ],
        }
    }
}

#[async_trait]
impl ContextProvider for ProjectFiles {
    fn name(&self) -> &str {
        "project-files"
    }

    async fn render(&self) -> Result<Option<String>> {
        let mut out = String::new();
        for f in &self.files {
            if let Ok(content) = tokio::fs::read_to_string(self.root.join(f)).await {
                if !content.trim().is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(&format!("## {f}\n{}", content.trim_end()));
                }
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }
}

/// A short environment summary (working directory + OS).
pub struct EnvContext {
    root: PathBuf,
}

impl EnvContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl ContextProvider for EnvContext {
    fn name(&self) -> &str {
        "environment"
    }

    async fn render(&self) -> Result<Option<String>> {
        Ok(Some(format!(
            "Working directory: {}\nOS: {}",
            self.root.display(),
            std::env::consts::OS
        )))
    }
}

/// Git working-tree context: branch, short status, recent commits, and unstaged diff stat. Renders
/// nothing when `root` isn't a git repository. This is host-side context-gathering at startup (like
/// [`ProjectFiles`]), not a model-facing tool.
pub struct GitContext {
    root: PathBuf,
}

impl GitContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// Run `git -C <root> <args>` and return trimmed stdout, or `None` on any failure (incl. not-a-repo).
async fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Keep at most `max` lines, appending a `… (+N more)` marker so a huge status/diff can't bloat the
/// prompt.
fn cap_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max {
        return s.to_string();
    }
    format!(
        "{}\n… (+{} more)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}

#[async_trait]
impl ContextProvider for GitContext {
    fn name(&self) -> &str {
        "git"
    }

    async fn render(&self) -> Result<Option<String>> {
        let Some(branch) = git(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"]).await else {
            return Ok(None); // not a git repo
        };
        let mut out = format!("Branch: {branch}");
        match git(&self.root, &["status", "--short"]).await.as_deref() {
            // Distinguish a genuinely empty status (clean) from a failed command (None): don't
            // claim "clean" when `git status` didn't actually run.
            Some("") => out.push_str("\nWorking tree: clean"),
            Some(status) => out.push_str(&format!(
                "\nWorking tree (git status --short):\n{}",
                cap_lines(status, 40)
            )),
            None => {}
        }
        if let Some(log) = git(&self.root, &["log", "--oneline", "-10"]).await {
            if !log.is_empty() {
                out.push_str(&format!("\nRecent commits:\n{log}"));
            }
        }
        if let Some(stat) = git(&self.root, &["diff", "--stat"]).await {
            if !stat.is_empty() {
                out.push_str(&format!("\nUnstaged changes:\n{}", cap_lines(&stat, 30)));
            }
        }
        Ok(Some(out))
    }
}

/// A compact signal of the project's shape: detected stack(s) + a sorted top-level listing. Lets the
/// agent orient without a `glob` round-trip. Shallow by design (no deep tree).
pub struct RepoSignal {
    root: PathBuf,
}

impl RepoSignal {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl ContextProvider for RepoSignal {
    fn name(&self) -> &str {
        "repo"
    }

    async fn render(&self) -> Result<Option<String>> {
        let Ok(mut rd) = tokio::fs::read_dir(&self.root).await else {
            return Ok(None);
        };
        let mut names = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            // Skip noise dotfiles but keep `.flux` (project config the agent should know about).
            if name.starts_with('.') && name != ".flux" {
                continue;
            }
            let is_dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            names.push(if is_dir { format!("{name}/") } else { name });
        }
        names.sort();

        let has = |f: &str| names.iter().any(|n| n == f);
        let mut stack: Vec<&str> = Vec::new();
        if has("Cargo.toml") {
            stack.push("Rust (Cargo)");
        }
        if has("package.json") {
            stack.push("Node.js");
        }
        if has("go.mod") {
            stack.push("Go");
        }
        if has("pyproject.toml") || has("setup.py") || has("requirements.txt") {
            stack.push("Python");
        }
        if has("pom.xml") || has("build.gradle") || has("build.gradle.kts") {
            stack.push("JVM (Maven/Gradle)");
        }
        if has("Gemfile") {
            stack.push("Ruby");
        }

        let shown = if names.len() > 60 {
            format!("{}  … (+{} more)", names[..60].join("  "), names.len() - 60)
        } else {
            names.join("  ")
        };
        let mut out = String::new();
        if !stack.is_empty() {
            out.push_str(&format!("Stack: {}\n", stack.join(", ")));
        }
        out.push_str(&format!("Top level: {shown}"));
        Ok(Some(out))
    }
}

/// Orders providers and projects them into a system prompt.
#[derive(Default)]
pub struct Projector {
    providers: Vec<Box<dyn ContextProvider>>,
}

impl Projector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, provider: Box<dyn ContextProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Build the full system prompt: `base` followed by each provider's block.
    pub async fn system_prompt(&self, base: &str) -> String {
        let mut out = base.to_string();
        for p in &self.providers {
            if let Ok(Some(block)) = p.render().await {
                out.push_str(&format!(
                    "\n\n<context source=\"{}\">\n{}\n</context>",
                    p.name(),
                    block
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-ctx-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn project_files_picks_up_claude_md() {
        let dir = temp_dir();
        std::fs::write(dir.join("CLAUDE.md"), "Use tabs, not spaces.").unwrap();
        let block = ProjectFiles::new(&dir).render().await.unwrap().unwrap();
        assert!(block.contains("## CLAUDE.md"));
        assert!(block.contains("Use tabs"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn project_files_none_when_absent() {
        let dir = temp_dir();
        assert!(ProjectFiles::new(&dir).render().await.unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn git_context_none_outside_repo() {
        // A plain directory (no .git) contributes nothing rather than erroring.
        let dir = temp_dir();
        assert!(GitContext::new(&dir).render().await.unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn repo_signal_detects_stack_and_lists_top_level() {
        let dir = temp_dir();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let block = RepoSignal::new(&dir).render().await.unwrap().unwrap();
        assert!(block.contains("Rust (Cargo)"), "got: {block}");
        assert!(block.contains("Cargo.toml"));
        assert!(block.contains("src/"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn projector_appends_context_blocks() {
        let dir = temp_dir();
        std::fs::write(dir.join("AGENTS.md"), "Project rules here.").unwrap();
        let projector = Projector::new()
            .with(Box::new(EnvContext::new(&dir)))
            .with(Box::new(ProjectFiles::new(&dir)));
        let sys = projector.system_prompt("BASE").await;
        assert!(sys.starts_with("BASE"));
        assert!(sys.contains("<context source=\"environment\">"));
        assert!(sys.contains("<context source=\"project-files\">"));
        assert!(sys.contains("Project rules here."));
        std::fs::remove_dir_all(&dir).ok();
    }
}
