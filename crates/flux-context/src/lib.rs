//! `flux-context` — assembles per-turn context from an ordered chain of providers.
//!
//! Each [`ContextProvider`] contributes an optional block; [`Projector::system_prompt`] appends
//! them to a base prompt wrapped in `<context source="...">` tags. v1 ships project-file context
//! (`CLAUDE.md`/`AGENTS.md`/`.flux/context.md`) and an environment summary; more providers (files,
//! memory, skills, datasource) plug in here later.

use std::path::PathBuf;

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
