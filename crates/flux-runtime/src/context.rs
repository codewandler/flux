//! `context` — assembles project context from an ordered chain of providers (the `context` module
//! of `flux-runtime`, folded in from the former `flux-context` crate).
//!
//! Each [`ContextProvider`] contributes an optional block; [`Projector::system_prompt`] appends
//! them to a base prompt wrapped in `<context source="...">` tags. It ships project-file context
//! (`CLAUDE.md`/`AGENTS.md`/`.flux/context.md`), an environment summary, git working-tree state,
//! repo shape, and path-scoped guidance fragments ([`ContextFragments`]).
//!
//! **Assembly happens once, at surface startup — not per turn.** The result is a `String` on
//! `AgentSpec`, so every provider here runs exactly once per session and the whole block sits in
//! the cache-stable prompt prefix. A provider that varied its output within a session would
//! invalidate that prefix; scope any relevance filtering against a session-stable signal (as
//! [`ContextFragments`] does with the git working set), never against per-turn state.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use flux_core::{Error, Result};
use flux_policy::wildcard_match;

/// Where path-scoped guidance fragments live, relative to the project root (A-97). A flat
/// directory, matching how `.flux` already houses skills, agents, and flows — so what can load is
/// auditable with one `ls`, without a tree walk or a mention syntax.
const FRAGMENT_DIR: &str = ".flux/context.d";

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
        // Project files are repository-controlled inputs. Build a deliberately confined workspace
        // here instead of borrowing the agent's possibly widened (`--add-dir`/`--allow-all-paths`)
        // tool workspace: automatic prompt context must never inherit an operator's tool-path
        // escape hatch.
        let system = flux_system::System::new(flux_system::Workspace::new(&self.root)?);
        let mut out = String::new();
        for f in &self.files {
            let content = match system.read_file(f).await {
                Ok(content) => content,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(Error::Config(format!(
                        "project context `{f}` is unreadable or outside the workspace: {error}"
                    )))
                }
            };
            if !content.trim().is_empty() {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&format!("## {f}\n{}", content.trim_end()));
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
///
/// Routes through flux's **single guarded spawn path** ([`flux_system::System::run`]) rather than a
/// raw `Command`: argv-only, environment cleared to the minimal allow-list (`PATH`/`HOME` keep git's
/// config resolving), output captured + capped, and a wall-clock timeout so a wedged git can't hang
/// startup. `-C <root>` keeps the query scoped to the repo regardless of the workspace-pinned cwd.
async fn git(root: &Path, args: &[&str]) -> Option<String> {
    // Attach the env-resolved OS-sandbox posture (`FLUX_SANDBOX*`, exported by the CLI at startup) so
    // this throwaway startup-context `System` honors `require` like every model-facing spawn; a bare
    // `System::new` would default to `Sandbox::disabled()` and silently escape confinement. Read-only
    // `git -C` runs fine confined.
    let system = flux_system::System::new(flux_system::Workspace::new(root).ok()?).with_sandbox(
        flux_system::sandbox::Sandbox::resolve(flux_system::sandbox::SandboxSettings::from_env()),
    );
    let mut argv = vec![
        "git".to_string(),
        "-C".to_string(),
        root.display().to_string(),
    ];
    argv.extend(args.iter().map(|a| a.to_string()));
    let out = system
        .run(&argv, std::time::Duration::from_secs(10))
        .await
        .ok()?;
    if out.exit_code != 0 {
        return None;
    }
    Some(out.stdout.trim().to_string())
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

/// How many fragments in `.flux/context.d` are collected before discovery stops. Guidance is
/// hand-authored, so this is a runaway guard, not a working limit.
const FRAGMENT_SCAN_CAP: usize = 256;

/// Path-scoped guidance fragments from `.flux/context.d/*.md` (A-97).
///
/// A fragment is a markdown file whose optional `globs:` frontmatter names the paths it applies to.
/// It contributes its body only when the repository's **working set** — what `git status --short`
/// reports as changed — contains a matching path; a fragment declaring no `globs` always
/// contributes. That lets a large repo carry per-subsystem conventions without paying for all of
/// them on every turn, the gap [`ProjectFiles`] leaves (it reads its whole fixed file list
/// unconditionally).
///
/// **The working set is resolved exactly once, here, at context-assembly time.** Guidance lands in
/// the cache-stable system prompt, so a fragment set that varied within a session would invalidate
/// the prompt prefix on every change — the failure mode A-95 was filed for. Scoping against the
/// working set (rather than per-turn paths) keeps the prefix frozen for the session by construction.
pub struct ContextFragments {
    root: PathBuf,
}

impl ContextFragments {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// Frontmatter a fragment may declare. Every field is optional: a bare markdown file with no
/// frontmatter at all is a valid, always-loaded fragment.
#[derive(Debug, Default, serde::Deserialize)]
struct FragmentMeta {
    /// Paths this fragment applies to, in [`flux_policy::wildcard_match`] syntax (`*` spans `/`, so
    /// `crates/flux-lang/**` matches `crates/flux-lang/src/parse.rs`). Empty = always applies.
    #[serde(default)]
    globs: Vec<String>,
}

/// Extract the changed paths from `git status --short` output.
///
/// Each line is `XY <path>`: two status columns, a space, then the path. Rename/copy entries read
/// `old -> new` and the new name is the one that exists on disk. git quotes paths containing
/// spaces or specials when `core.quotepath` is set. The two status columns are always ASCII, so
/// slicing at byte 3 is safe for UTF-8 paths.
fn changed_paths_from_status(status: &str) -> Vec<String> {
    status
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            let path = &line[3..];
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            let path = path.trim().trim_matches('"');
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

#[async_trait]
impl ContextProvider for ContextFragments {
    fn name(&self) -> &str {
        "context-fragments"
    }

    async fn render(&self) -> Result<Option<String>> {
        // Same confinement stance as `ProjectFiles`: fragments are repository-controlled inputs, so
        // read them through a workspace pinned to the project root rather than the agent's possibly
        // widened (`--add-dir`/`--allow-all-paths`) tool workspace.
        let system = flux_system::System::new(flux_system::Workspace::new(&self.root)?);
        // Read only the top level, never a tree: `System::walk_files` recurses (rightly, for
        // `glob`/`grep`), which would let `context.d/sub/x.md` reach the prompt from a path the
        // flat-directory contract above says is never scanned — C-206. The workspace still resolves
        // the directory, so the confinement stance is unchanged.
        // An absent fragment directory is the overwhelmingly common case, not a misconfiguration —
        // unlike `ProjectFiles`' explicitly-named files, whose absence it also tolerates.
        let Ok(dir) = system.workspace().resolve_read(FRAGMENT_DIR) else {
            return Ok(None);
        };
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            return Ok(None);
        };
        let mut files: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            if files.len() >= FRAGMENT_SCAN_CAP {
                break;
            }
            // `DirEntry::file_type` doesn't follow symlinks, so this one test drops both
            // subdirectories and symlinks (the escape guard `walk_files` applies too) in one go.
            if !entry
                .file_type()
                .await
                .map(|t| t.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") {
                files.push(format!("{FRAGMENT_DIR}/{name}"));
            }
        }

        // Sort: directory order is filesystem-dependent, and an unstable fragment order would
        // reshuffle the system prompt between runs and cold-write the cache for nothing.
        files.sort();

        // No working set (clean tree, or not a repo at all) leaves `changed` empty, so scoped
        // fragments simply don't match while unscoped ones still load.
        // `--untracked-files=all` matters: the default collapses an untracked directory to a bare
        // `crates/` entry, which no subsystem glob would match — so a brand-new file would silently
        // fail to pull in its own subsystem's guidance.
        let changed = match git(&self.root, &["status", "--short", "--untracked-files=all"]).await {
            Some(status) => changed_paths_from_status(&status),
            None => Vec::new(),
        };

        let mut out = String::new();
        for file in &files {
            let content = system.read_file(file).await.map_err(|error| {
                Error::Config(format!("guidance fragment `{file}` is unreadable: {error}"))
            })?;
            let doc = flux_markdown::parse_frontmatter::<FragmentMeta>(&content).map_err(|e| {
                // Loud, not silent: a fragment whose frontmatter doesn't parse would otherwise
                // vanish from the prompt, and missing guidance is invisible at the point of use.
                Error::Config(format!(
                    "guidance fragment `{file}` has malformed frontmatter: {e}"
                ))
            })?;
            let applies = doc.meta.globs.is_empty()
                || doc
                    .meta
                    .globs
                    .iter()
                    .any(|g| changed.iter().any(|c| wildcard_match(g, c)));
            if !applies {
                continue;
            }
            let body = doc.body.trim_end();
            if body.trim().is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            let name = file.rsplit('/').next().unwrap_or(file);
            out.push_str(&format!("## {name}\n{body}"));
        }
        Ok((!out.is_empty()).then_some(out))
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

    /// Build the full system prompt: `base` followed by each provider's block. Provider failures
    /// are returned to the caller so a repository-controlled guard error cannot silently look like
    /// an absent optional file.
    pub async fn try_system_prompt(&self, base: &str) -> Result<String> {
        let mut out = base.to_string();
        for p in &self.providers {
            if let Some(block) = p.render().await? {
                out.push_str(&format!(
                    "\n\n<context source=\"{}\">\n{}\n</context>",
                    p.name(),
                    block
                ));
            }
        }
        Ok(out)
    }

    /// Compatibility wrapper for callers that deliberately tolerate unavailable auxiliary
    /// context. Production agent assembly uses [`Self::try_system_prompt`] so guard failures are
    /// fail-closed.
    #[deprecated(note = "use try_system_prompt so context guard failures are surfaced")]
    pub async fn system_prompt(&self, base: &str) -> String {
        self.try_system_prompt(base)
            .await
            .unwrap_or_else(|_| base.to_string())
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

    #[cfg(unix)]
    #[tokio::test]
    async fn project_files_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let outside = temp_dir();
        std::fs::write(outside.join("secret.md"), "OUTSIDE SECRET").unwrap();
        for file in ["AGENTS.md", "CLAUDE.md", ".flux/context.md"] {
            let dir = temp_dir();
            let path = dir.join(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            symlink(outside.join("secret.md"), &path).unwrap();

            let error = ProjectFiles::new(&dir).render().await.unwrap_err();
            assert!(error.to_string().contains("outside"), "{error}");
            let projector = Projector::new().with(Box::new(ProjectFiles::new(&dir)));
            assert!(
                projector.try_system_prompt("BASE").await.is_err(),
                "projector silently discarded the guard error for {file}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        std::fs::remove_dir_all(&outside).ok();
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

    /// A-97 (failing-first): a fragment declaring `globs` contributes its body only when the git
    /// working set contains a matching path; an unscoped fragment always contributes. Drives a
    /// REAL repo because `git status --short` output is the contract being parsed.
    #[tokio::test]
    async fn fragments_load_only_when_globs_match_the_working_set() {
        let dir = temp_dir();
        git(&dir, &["init", "-q"]).await.expect("git init");
        std::fs::create_dir_all(dir.join("crates/flux-lang/src")).unwrap();
        std::fs::write(dir.join("crates/flux-lang/src/parse.rs"), "fn main() {}").unwrap();

        let frags = dir.join(".flux/context.d");
        std::fs::create_dir_all(&frags).unwrap();
        std::fs::write(
            frags.join("lang.md"),
            "---\nglobs: [\"crates/flux-lang/**\"]\n---\nLANG RULES",
        )
        .unwrap();
        std::fs::write(
            frags.join("tui.md"),
            "---\nglobs: [\"crates/flux-tui/**\"]\n---\nTUI RULES",
        )
        .unwrap();
        std::fs::write(frags.join("always.md"), "ALWAYS RULES").unwrap();

        let block = ContextFragments::new(&dir).render().await.unwrap().unwrap();
        assert!(
            block.contains("LANG RULES"),
            "matching fragment missing: {block}"
        );
        assert!(
            !block.contains("TUI RULES"),
            "non-matching fragment leaked: {block}"
        );
        assert!(
            block.contains("ALWAYS RULES"),
            "unscoped fragment missing: {block}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-206 (failing-first): the directory is flat by contract (see [`FRAGMENT_DIR`]) — one `ls`
    /// must enumerate everything that can reach the system prompt. A fragment parked in a
    /// subdirectory is therefore not a fragment at all, and must not load.
    #[tokio::test]
    async fn fragments_ignore_subdirectories() {
        let dir = temp_dir();
        let frags = dir.join(".flux/context.d");
        std::fs::create_dir_all(frags.join("sub/deep")).unwrap();
        std::fs::write(frags.join("top.md"), "TOP RULES").unwrap();
        std::fs::write(frags.join("sub/x.md"), "NESTED RULES").unwrap();
        std::fs::write(frags.join("sub/deep/y.md"), "DEEPLY NESTED RULES").unwrap();

        let block = ContextFragments::new(&dir).render().await.unwrap().unwrap();
        assert!(
            block.contains("TOP RULES"),
            "top-level fragment missing: {block}"
        );
        assert!(
            !block.contains("NESTED RULES"),
            "fragment from a subdirectory leaked into the prompt: {block}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The runaway guard survives the flat scan: past `FRAGMENT_SCAN_CAP` fragments, discovery
    /// stops rather than pouring an unbounded directory into the system prompt.
    #[tokio::test]
    async fn fragment_scan_stops_at_the_cap() {
        let dir = temp_dir();
        let frags = dir.join(".flux/context.d");
        std::fs::create_dir_all(&frags).unwrap();
        for i in 0..FRAGMENT_SCAN_CAP + 20 {
            std::fs::write(frags.join(format!("f{i:04}.md")), format!("RULE {i}")).unwrap();
        }

        let block = ContextFragments::new(&dir).render().await.unwrap().unwrap();
        assert_eq!(
            block.matches("\n\n## ").count() + 1,
            FRAGMENT_SCAN_CAP,
            "scan did not stop at the cap"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// No fragment directory at all is silence, not an error — the overwhelmingly common case.
    #[tokio::test]
    async fn fragments_none_when_directory_absent() {
        let dir = temp_dir();
        assert!(ContextFragments::new(&dir)
            .render()
            .await
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Outside a repo there is no working set, so scoped fragments cannot match — but unscoped
    /// ones must still load, otherwise guidance silently vanishes in a non-git directory.
    #[tokio::test]
    async fn unscoped_fragments_survive_outside_a_repo() {
        let dir = temp_dir();
        let frags = dir.join(".flux/context.d");
        std::fs::create_dir_all(&frags).unwrap();
        std::fs::write(frags.join("always.md"), "ALWAYS RULES").unwrap();
        std::fs::write(
            frags.join("scoped.md"),
            "---\nglobs: [\"src/**\"]\n---\nSCOPED RULES",
        )
        .unwrap();

        let block = ContextFragments::new(&dir).render().await.unwrap().unwrap();
        assert!(block.contains("ALWAYS RULES"), "{block}");
        assert!(!block.contains("SCOPED RULES"), "{block}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fragment is a repository-controlled input, so content from outside the workspace must
    /// never reach the prompt through one. The escape is structurally impossible rather than
    /// rejected: `System::walk_files` skips symlinks outright ("never follow symlinks (could escape
    /// a root)"), so a symlinked fragment is never even read — inside the workspace or out. That
    /// makes this quieter than the `ProjectFiles` symlink guard, which errors by name; the
    /// invariant worth pinning here is the leak, not the diagnostic.
    #[cfg(unix)]
    #[tokio::test]
    async fn fragments_never_read_through_a_symlink() {
        use std::os::unix::fs::symlink;

        let outside = temp_dir();
        std::fs::write(outside.join("secret.md"), "OUTSIDE SECRET").unwrap();
        let dir = temp_dir();
        let frags = dir.join(".flux/context.d");
        std::fs::create_dir_all(&frags).unwrap();
        symlink(outside.join("secret.md"), frags.join("evil.md")).unwrap();
        std::fs::write(frags.join("real.md"), "REAL RULES").unwrap();

        let rendered = ContextFragments::new(&dir).render().await.unwrap();
        let block = rendered.unwrap_or_default();
        assert!(!block.contains("OUTSIDE SECRET"), "escaped: {block}");
        // The regular sibling still loads, so the skip is scoped to the symlink itself.
        assert!(block.contains("REAL RULES"), "{block}");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn status_short_parses_renames_and_quotes() {
        let paths = changed_paths_from_status(
            " M crates/flux-lang/src/parse.rs\n\
             ?? new.rs\n\
             R  old/a.rs -> new/b.rs\n\
             A  \"quoted path.rs\"\n\
             x\n",
        );
        assert_eq!(
            paths,
            vec![
                "crates/flux-lang/src/parse.rs",
                "new.rs",
                "new/b.rs",
                "quoted path.rs",
            ]
        );
    }

    #[tokio::test]
    async fn projector_appends_context_blocks() {
        let dir = temp_dir();
        std::fs::write(dir.join("AGENTS.md"), "Project rules here.").unwrap();
        let projector = Projector::new()
            .with(Box::new(EnvContext::new(&dir)))
            .with(Box::new(ProjectFiles::new(&dir)));
        let sys = projector.try_system_prompt("BASE").await.unwrap();
        assert!(sys.starts_with("BASE"));
        assert!(sys.contains("<context source=\"environment\">"));
        assert!(sys.contains("<context source=\"project-files\">"));
        assert!(sys.contains("Project rules here."));
        std::fs::remove_dir_all(&dir).ok();
    }
}
