use super::*;

// Codex/Anthropic model resolution is backend knowledge owned by each provider crate
// (`flux_providers::codex::resolve_model`, `flux_providers::anthropic::resolve_model`) so every
// surface — CLI, SDK, server, TUI, the L3 sub-agent spawner — shares one owner instead of each
// carrying its own alias table. The CLI owns only the *shorthand policy*: bare `codex` (no model)
// means "use the provider default".

/// Resolve the model spec with precedence: `--model` flag > config `model` > `sonnet`.
pub(super) fn resolve_model_spec(cli_model: &Option<String>, cfg: &flux_config::Config) -> String {
    cli_model
        .clone()
        .or_else(|| cfg.model.clone())
        .unwrap_or_else(|| "sonnet".to_string())
}

/// Resolve the model spec with an enabled skill's `model` frontmatter (D-189) layered in: `--model`
/// flag > skill `model` > config `model` > `sonnet`. An explicit CLI/SDK model always wins — the
/// same precedence a role's `model` uses against its `default_model` (`Role::to_spec`), just with
/// the skill sitting one tier below the caller's own explicit choice instead of above it. When
/// several enabled skills declare a `model`, the first (highest-precedence `--skill` order) wins;
/// this is a corner case worth naming rather than silently picking one — see
/// `docs/designs/claude-interop.md`'s Risks section.
pub(super) fn resolve_model_spec_with_skill(
    cli_model: &Option<String>,
    cfg: &flux_config::Config,
    skills: &[flux_skill::Skill],
) -> String {
    let skill_model = skills.iter().find_map(|s| s.model.clone());
    cli_model
        .clone()
        .or(skill_model)
        .or_else(|| cfg.model.clone())
        .unwrap_or_else(|| "sonnet".to_string())
}

/// Persist newly "always-allow"ed permission rules back to the project config, if any changed.
pub(super) fn persist_new_rules(initial: &[String], current: &[String]) {
    if current == initial {
        return;
    }
    if let Ok(cwd) = std::env::current_dir() {
        match flux_runtime::metadata::persist_allow_rules(&cwd, current) {
            Ok(()) => eprintln!(
                "{}",
                style::dim("(saved updated permissions to .flux/config.toml)")
            ),
            Err(e) => eprintln!(
                "{}",
                style::dim(&format!("(could not save permissions: {e})"))
            ),
        }
    }
}

/// Parse a fully-qualified `provider/model` spec and build the matching provider from environment
/// credentials. Thin delegate to the shared [`flux_providers::spec::build`] (D-152 moved the
/// mapping into `flux-providers` so every embedder resolves a spec identically); the `?` folds the
/// library's `flux_core::Error` into the CLI's `anyhow` chain with the same string. Spec forms and
/// validation live in `flux_providers::spec::parse_model_spec`.
pub(super) fn build_provider(spec: &str) -> Result<(NativeProvider, String, String)> {
    Ok(flux_providers::spec::build(spec)?)
}

/// Whether a model spec selects the offline [`MockCliProvider`] — a bare `mock` or any `mock/…`
/// form. The single predicate behind both the engine's [`resolve_cli_provider`] policy and the
/// sub-agent factory [`provider_for`], so the two can't disagree on what counts as the mock.
pub(super) fn is_mock_spec(spec: &str) -> bool {
    spec == "mock" || spec.starts_with("mock/")
}

/// One boxed provider under the CLI's mock/lazy/eager policy, plus the resolved model name and the
/// canonical `provider/model` spec (what cost/subscription detection reads).
pub(super) struct ResolvedProvider {
    pub(super) provider: Box<dyn Provider>,
    pub(super) model: String,
    pub(super) canonical_spec: String,
}

/// Resolve a provider for `model_spec` under the CLI's three-way policy, shared by the engine's
/// primary provider and the cognition pack's sibling so the two can't drift:
/// - `mock` / `mock/…` → the offline [`MockCliProvider`] (canonical spec is just `mock`).
/// - lazy (`!eager`, C-11) → a [`LazyProvider`] that reads no credential until the first model call;
///   `model` is the display part and `canonical_spec` is the raw input.
/// - eager → the one provider factory ([`build_provider`]), materializing the credential chain now;
///   the resolved `provider/model` becomes the canonical spec.
///
/// The eager branch PROPAGATES its construction error; a caller that prefers to degrade (the
/// cognition sibling) catches it.
pub(super) fn resolve_cli_provider(model_spec: &str, eager: bool) -> Result<ResolvedProvider> {
    if is_mock_spec(model_spec) {
        Ok(ResolvedProvider {
            provider: Box::<MockCliProvider>::default(),
            model: "mock".to_string(),
            canonical_spec: "mock".to_string(),
        })
    } else if !eager {
        // C-11 lazy: no credential read, no chain/model-id resolution — all deferred to
        // `LazyProvider` on the first model call. The unresolved model part serves for display.
        let display_model = model_spec
            .split_once('/')
            .map(|(_, m)| m.to_string())
            .unwrap_or_else(|| model_spec.to_string());
        Ok(ResolvedProvider {
            provider: Box::new(LazyProvider::new(model_spec.to_string())),
            model: display_model,
            canonical_spec: model_spec.to_string(),
        })
    } else {
        // The one provider factory (C-11): `build_provider` owns the whole construction, including
        // the aws credential-chain materialization. The raw `model_spec` may be a bare alias
        // (`codex`, `sonnet`) that cost detection can't decode, so surface the resolved form.
        let (native, provider, m) = build_provider(model_spec)?;
        let canonical_spec = format!("{provider}/{m}");
        Ok(ResolvedProvider {
            provider: Box::new(native),
            model: m,
            canonical_spec,
        })
    }
}

/// Walk `base` (guarded, capped at 4000 entries) and collect `(path, text)` for documentation files
/// (markdown/text, by extension), stopping at `max_docs` and skipping any file whose metadata size
/// exceeds `max_bytes`. The size check reads metadata BEFORE the file so a stray 500 MB `notes.txt`
/// never costs a whole-file read just to be discarded. A walk error yields an empty vec (an empty
/// index just means "no matches"). Shared by [`build_doc_index`] and the `markdown` datasource arm.
async fn walk_docs(
    system: &System,
    base: &str,
    max_docs: usize,
    max_bytes: usize,
) -> Vec<(String, String)> {
    const DOC_EXTS: &[&str] = &[".md", ".txt", ".rst", ".adoc", ".mdx"];
    let Ok(files) = system.walk_files(base, 4000).await else {
        return Vec::new();
    };
    let mut docs: Vec<(String, String)> = Vec::new();
    for f in files {
        if docs.len() >= max_docs {
            break;
        }
        if !DOC_EXTS.iter().any(|e| f.ends_with(e)) {
            continue;
        }
        if !matches!(system.file_size(&f).await, Ok(n) if n as usize <= max_bytes) {
            continue;
        }
        if let Ok(text) = system.read_file(&f).await {
            docs.push((f, text));
        }
    }
    docs
}

/// Build the knowledge datasource from the workspace's documentation files (markdown/text), indexed as
/// `file.document` records under the `local` source. Deliberately cheap: doc extensions only, capped file
/// count and size — code search is served by `grep`, not this. Errors are swallowed (an empty index just
/// yields "no matches"). Returns the shared backend the retrieval ops dispatch against.
pub(super) async fn build_doc_index(
    system: &System,
) -> Arc<dyn flux_capabilities::DatasourceBackend> {
    // Wrap the keyword backend in the semantic (embeddings) backend *before* ingest — when built with
    // `--features embeddings` and an embeddings key resolves — so records are embedded as they're indexed.
    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        datasource_backend(Arc::new(flux_capabilities::MemoryBackend::new()));
    let docs = walk_docs(system, ".", 200, 100_000).await;
    // Index under the `local` source as `file.document` records via the markdown ingester.
    let _ = flux_capabilities::ingest_markdown(&*backend, "local", &docs);
    backend
}

/// The `kind` prefix that names a **board** declaration (A-131).
///
/// Naming this was a real decision, not a detail. `markdown` is already taken by the *knowledge*
/// ingester, and a board backed by markdown files (A-114) is a different port entirely — so board
/// kinds live in their own namespace: `board:memory` today, `board:markdown` / `board:jira` /
/// `board:gitlab` as those backends land. Two properties follow, and both are the point:
///
/// * `kind = "markdown"` keeps meaning exactly what it has always meant. A knowledge declaration is
///   never silently promoted to a board, and a board declaration is never silently ingested as
///   knowledge — the prefix, not a lookup table, decides which port is bound.
/// * A kind *under* this prefix that names no backend is a hard error, never a fall-through. The
///   failure mode this closes is a user pointing `kind = "markdown"` at a board file store and
///   getting a silently wrong, read-only knowledge index instead of a diagnostic.
const BOARD_KIND_PREFIX: &str = "board:";

/// The board backends bindable from a `datasource` declaration today, for the error message that
/// names them. `MemoryBoard` is the offline backend A-113 shipped with the port; `MarkdownBoard`
/// (A-114) is the file-backed one; the issue-tracker-backed ones arrive with their own stories.
const BOARD_BACKENDS: &str = "markdown | memory";

/// What a Program's `datasource` declarations resolve to: one shared knowledge backend, plus the
/// work boards declared alongside it.
pub(super) struct ProgramDatasources {
    /// The shared index every `markdown` / `openapi` declaration ingested into, and the backend the
    /// retrieval ops (`search`/`get`/`list`/…) dispatch against.
    pub(super) knowledge: Arc<dyn flux_capabilities::DatasourceBackend>,
    /// `(domain, backend)` per declared board, in declaration order. The caller installs each with
    /// [`flux_capabilities::try_register_work_board`], which *derives* the generated op set from the
    /// port — so an operation added to `WorkBoard` surfaces here with no change to this module.
    pub(super) boards: Vec<(String, Arc<dyn flux_capabilities::WorkBoard>)>,
}

/// Resolve one `board:<backend>` declaration into a [`WorkBoard`](flux_capabilities::WorkBoard).
///
/// An unrecognized backend names itself and the ones that exist. It is deliberately NOT tolerant:
/// falling back to the knowledge ingester here would reintroduce exactly the silent, wrong behaviour
/// [`BOARD_KIND_PREFIX`] exists to prevent.
///
/// `root` is the already-resolved, program-relative directory the file-backed backends live under;
/// `memory` ignores it. `MarkdownBoard` is built with
/// [`rooted_in`](flux_capabilities::MarkdownBoard::rooted_in) rather than `new`, so the board
/// **inherits the session's guarded `System`** instead of minting a fresh `Workspace` at an
/// arbitrary root — a board is a write surface, so it must not be able to widen the sandbox it was
/// handed.
fn build_work_board(
    name: &str,
    backend: &str,
    root: &str,
    system: &System,
) -> Result<Arc<dyn flux_capabilities::WorkBoard>> {
    match backend {
        "memory" => Ok(Arc::new(flux_capabilities::MemoryBoard::new())),
        "markdown" => Ok(Arc::new(
            flux_capabilities::MarkdownBoard::rooted_in(system, root).map_err(|e| {
                anyhow::anyhow!("datasource `{name}` (board:markdown) at `{root}`: {e}")
            })?,
        )),
        "" => Err(anyhow::anyhow!(
            "datasource `{name}` declares a board but names no backend — write \
             `kind = \"{BOARD_KIND_PREFIX}markdown\"` (available: {BOARD_BACKENDS})"
        )),
        other => Err(anyhow::anyhow!(
            "datasource `{name}`: unknown board backend `{other}` (available: {BOARD_BACKENDS})"
        )),
    }
}

/// Build a program's declared [`datasource`](flux_lang::program::DatasourceDecl)s — the
/// `flux app run` counterpart of [`build_doc_index`]'s implicit workspace index.
///
/// The `kind` dispatch is **total**, which is the whole safety property here: a knowledge kind
/// ingests under its own name by the matching ingester (`markdown` walks a docs directory; `openapi`
/// reads a JSON spec file), a `board:<backend>` kind binds a write-capable [`WorkBoard`] instead,
/// and anything else is a clean error. Nothing falls through to a default.
pub(super) async fn build_datasources(
    decls: &[flux_lang::program::DatasourceDecl],
    program_dir: &std::path::Path,
    system: &System,
) -> Result<ProgramDatasources> {
    // A datasource path is relative to the PROGRAM FILE's directory (absolute paths pass through), so
    // `path "./docs"` means "beside the .flux file" regardless of the launch cwd. `program_dir` is a
    // read-only root of `system`, so the resulting absolute path is walkable/readable.
    fn resolve_ds_path(program_dir: &std::path::Path, raw: &str) -> String {
        let p = std::path::Path::new(raw);
        if p.is_absolute() {
            raw.to_string()
        } else {
            program_dir.join(p).to_string_lossy().into_owned()
        }
    }
    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        datasource_backend(Arc::new(flux_capabilities::MemoryBackend::new()));
    let mut boards: Vec<(String, Arc<dyn flux_capabilities::WorkBoard>)> = Vec::new();
    for d in decls {
        match d.kind.as_str() {
            // A board, not a knowledge source. `board` on its own is the shape a bare
            // `datasource board { … }` lowers to (native text defaults `kind` to the decl name), so
            // it is routed here to get the "names no backend" diagnostic rather than the generic
            // unknown-kind one.
            kind if kind == "board" || kind.starts_with(BOARD_KIND_PREFIX) => {
                let backend = kind.strip_prefix(BOARD_KIND_PREFIX).unwrap_or("").trim();
                // A board's `path` is program-relative exactly like a knowledge datasource's, so
                // `path "./board"` means "beside the .flux file" whatever the launch cwd is.
                let root = resolve_ds_path(program_dir, d.path.as_deref().unwrap_or("."));
                boards.push((
                    d.name.clone(),
                    build_work_board(&d.name, backend, &root, system)?,
                ));
            }
            "markdown" => {
                let base = resolve_ds_path(program_dir, d.path.as_deref().unwrap_or("."));
                let docs = walk_docs(system, &base, 1000, 200_000).await;
                flux_capabilities::ingest_markdown(&*backend, &d.name, &docs)
                    .map_err(|e| anyhow::anyhow!("datasource `{}` (markdown): {e}", d.name))?;
            }
            "openapi" => {
                let raw = d.path.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("datasource `{}` (openapi) needs a `path`", d.name)
                })?;
                let path = resolve_ds_path(program_dir, raw);
                let text = system
                    .read_file(&path)
                    .await
                    .map_err(|e| anyhow::anyhow!("datasource `{}`: read {raw}: {e}", d.name))?;
                let spec: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                    anyhow::anyhow!("datasource `{}`: parse {raw} as OpenAPI JSON: {e}", d.name)
                })?;
                flux_capabilities::ingest_openapi(&*backend, &d.name, &spec)
                    .map_err(|e| anyhow::anyhow!("datasource `{}` (openapi): {e}", d.name))?;
            }
            other => {
                return Err(anyhow::anyhow!(
                    "datasource `{}` has unknown kind `{other}` (expected markdown | openapi | \
                     {BOARD_KIND_PREFIX}<backend>)",
                    d.name
                ))
            }
        }
    }
    Ok(ProgramDatasources {
        knowledge: backend,
        boards,
    })
}

/// Wrap a keyword backend in the semantic (embeddings) backend when built with `--features embeddings`
/// and an embeddings API key resolves from env; otherwise return it unchanged (the default).
#[cfg(feature = "embeddings")]
pub(super) fn datasource_backend(
    inner: Arc<dyn flux_capabilities::DatasourceBackend>,
) -> Arc<dyn flux_capabilities::DatasourceBackend> {
    match flux_capabilities::OpenAiEmbedder::from_env() {
        Some(embedder) => Arc::new(flux_capabilities::SemanticIndex::new(
            inner,
            Arc::new(embedder),
        )),
        None => inner,
    }
}

#[cfg(not(feature = "embeddings"))]
pub(super) fn datasource_backend(
    inner: Arc<dyn flux_capabilities::DatasourceBackend>,
) -> Arc<dyn flux_capabilities::DatasourceBackend> {
    inner
}

/// Session size (serialized chars) past which the agent summarizes old turns. Override with
/// `FLUX_COMPACT_CHARS` (`0` disables compaction).
pub(super) fn compact_threshold() -> usize {
    match std::env::var("FLUX_COMPACT_CHARS") {
        Ok(s) => s.parse().unwrap_or_else(|_| {
            // Warn instead of silently reverting: the user set the knob, so a typo'd value
            // (`48k`) falling back to the default would contradict the documented 0-disables
            // contract without a trace.
            eprintln!(
                "{} FLUX_COMPACT_CHARS is not a number ({s:?}); using the default 48000",
                style::yellow("warning:")
            );
            48_000
        }),
        Err(_) => 48_000,
    }
}

/// Walk `.flux/skills` and `.claude/skills` plus the user/global dirs (`~/.flux/skills`,
/// `~/.agents/skills`, `~/.claude/skills`), with `cli_dirs` layered above `[skills] dirs` from the
/// layered config (project before user) above the well-known set — earlier dirs win a name clash
/// (L-02). Metadata only (no bodies read). Shared by [`load_skills`] and
/// [`load_model_invoked_skill_catalog`] so the two callers can't drift on directory precedence.
fn discover_skills(
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    cli_dirs: &[std::path::PathBuf],
) -> Result<flux_runtime::metadata::SkillDiscovery> {
    let mut extra = cli_dirs
        .iter()
        .cloned()
        .map(|path| {
            if path.is_absolute() {
                flux_runtime::metadata::SkillRoot::Trusted(path)
            } else {
                flux_runtime::metadata::SkillRoot::Project(path)
            }
        })
        .collect::<Vec<_>>();
    extra.extend(flux_runtime::metadata::configured_skill_roots(cfg));
    flux_runtime::metadata::discover_skills_from(cwd, &extra)
        .map_err(|error| anyhow::anyhow!("discover skills: {error}"))
}

/// D-189: load-time frontmatter honesty — a recognized-but-unsupported field, an unmappable
/// `allowed-tools` entry, or a `flux_skill::validate` naming/description issue. Data, not a
/// silent drop; printed the same way `load_command_files` reports its own warnings.
fn print_skill_warnings(discovery: &flux_runtime::metadata::SkillDiscovery) {
    for warning in &discovery.warnings {
        eprintln!("{} {warning}", style::yellow("warning:"));
    }
}

/// Explicitly enable named skills. `enabled` is the `--skill NAME` allowlist; prompt text never
/// activates a skill automatically.
pub(super) fn load_skills(
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    cli_dirs: &[std::path::PathBuf],
    enabled: &[String],
) -> Result<Vec<flux_skill::Skill>> {
    // Manual-only means more than "discover everything, then select nothing": an ordinary turn
    // must not pay to walk every project and global skill directory. Discovery is only useful once
    // the caller has explicitly named at least one skill.
    if enabled.is_empty() {
        return Ok(Vec::new());
    }
    let discovery = discover_skills(cwd, cfg, cli_dirs)?;
    print_skill_warnings(&discovery);
    let discovered = discovery.skills;
    let mut selected = Vec::new();
    for name in enabled {
        let skill = discovered
            .iter()
            .find(|skill| skill.name == *name)
            .ok_or_else(|| {
                let mut available: Vec<&str> =
                    discovered.iter().map(|skill| skill.name.as_str()).collect();
                available.sort_unstable();
                anyhow::anyhow!(
                    "unknown skill `{name}` (discovered: {})",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )
            })?;
        if !selected
            .iter()
            .any(|selected: &flux_skill::Skill| selected.name == skill.name)
        {
            selected.push(skill.clone());
        }
    }
    Ok(selected)
}

/// D-188: discover the opt-in model-invoked skill catalog — every discovered skill except those
/// declaring `disable-model-invocation: true`. Unlike [`load_skills`], this runs the directory walk
/// unconditionally when `model_invoked` is set, since the whole point of the opt-in is to surface
/// what's discoverable rather than wait for an explicit `--skill` name.
pub(super) fn load_model_invoked_skill_catalog(
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    cli_dirs: &[std::path::PathBuf],
    model_invoked: bool,
) -> Result<Vec<flux_skill::Skill>> {
    if !model_invoked {
        return Ok(Vec::new());
    }
    let discovery = discover_skills(cwd, cfg, cli_dirs)?;
    print_skill_warnings(&discovery);
    Ok(discovery
        .skills
        .into_iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect())
}

/// Discover command files (D-186) eagerly — unlike skills, they populate `/help` and the slash
/// menu on every run rather than only once explicitly enabled. Built-ins always win a name clash:
/// a shadowed file command is dropped (not dispatched) with a load-time warning naming both the
/// clashing file and the built-in it lost to, rather than silently masking either one.
pub(super) fn load_command_files(
    cwd: &std::path::Path,
    builtin_names: &[&str],
) -> Vec<flux_runtime::metadata::CommandFile> {
    let discovery = match flux_runtime::metadata::discover_commands(cwd) {
        Ok(discovery) => discovery,
        Err(error) => {
            eprintln!("{} discover commands: {error}", style::yellow("warning:"));
            return Vec::new();
        }
    };
    for warning in &discovery.warnings {
        eprintln!("{} {warning}", style::yellow("warning:"));
    }
    discovery
        .commands
        .into_iter()
        .filter(|command| {
            let shadowed = builtin_names.contains(&command.name.as_str());
            if shadowed {
                eprintln!(
                    "{} command file `{}` names the built-in `/{}` — the built-in wins; rename \
                     the file to use it",
                    style::yellow("warning:"),
                    command.source.display(),
                    command.name
                );
            }
            !shadowed
        })
        .collect()
}

#[cfg(test)]
mod command_file_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "flux-cli-cmdfiles-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_builtin_named_file_command_is_dropped_not_dispatched() {
        let root = temp_dir("shadow");
        std::fs::create_dir_all(root.join(".flux/commands")).unwrap();
        std::fs::write(root.join(".flux/commands/help.md"), "not the real help").unwrap();
        std::fs::write(
            root.join(".flux/commands/review.md"),
            "---\ndescription: Review a PR\n---\nReview $1",
        )
        .unwrap();

        let loaded = load_command_files(&root, &["help", "clear"]);
        assert!(
            !loaded.iter().any(|c| c.name == "help"),
            "a command file named `help` must not shadow the built-in: {loaded:?}"
        );
        assert!(loaded.iter().any(|c| c.name == "review"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn no_command_dirs_yields_an_empty_list() {
        let root = temp_dir("empty");
        assert!(load_command_files(&root, &["help"]).is_empty());
        std::fs::remove_dir_all(root).ok();
    }
}

/// The plugin descriptor directory `~/.flux/plugins` (None if `HOME` is unset).
pub(super) fn plugins_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".flux").join("plugins"))
}

pub(super) const PLUGIN_LOAD_CONCURRENCY: usize = 16;

pub(super) async fn collect_bounded<F, T>(futures: Vec<F>, limit: usize) -> Result<Vec<T>>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let permits = Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    let tasks = futures.into_iter().map(|future| {
        let permits = permits.clone();
        tokio::spawn(async move {
            let _permit = permits.acquire_owned().await.map_err(|error| {
                anyhow::anyhow!("plugin-load concurrency limiter closed: {error}")
            })?;
            Ok::<T, anyhow::Error>(future.await)
        })
    });
    let joined: Vec<_> = futures::stream::iter(tasks)
        .buffer_unordered(limit.max(1))
        .collect()
        .await;
    let mut values = Vec::with_capacity(joined.len());
    for result in joined {
        values.push(result.context("plugin-load task failed")??);
    }
    Ok(values)
}

/// Open the unified event store under `~/.flux/events.db` (conversation + run trace + turn telemetry).
pub(super) fn open_event_store() -> Result<EventStore> {
    let dir = flux_store_dir()?;
    std::fs::create_dir_all(&dir)?;
    EventStore::open(dir.join("events.db")).context("open event store")
}

/// Where this invocation's sessions live: `--store <DIR>` (exported as `FLUX_STORE_DIR` in
/// `dispatch::run`, so subprocess paths inherit it) if given, else the default `~/.flux`.
///
/// D-179: a scenario fixture written by `flux record` is an ordinary `Storage::dir` store, so
/// pointing `--store` at one makes the session tools (`replay`, `fork`, `diff`, `sessions`) work
/// against a committed fixture with no fixture-specific code path. (`usage` reads the global
/// `FLUX_HOME` events store and does not consult `--store`.)
pub(super) fn flux_store_dir() -> Result<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("FLUX_STORE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Ok(dir);
        }
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    Ok(home.join(".flux"))
}

/// Open flux-flow's own store under `~/.flux/flow.db` (values, symbols, suspensions). Run-trace
/// events are forwarded to the shared `events` log.
pub(super) fn open_flow_store(events: Arc<EventStore>) -> Result<FlowStore> {
    let dir = flux_store_dir()?;
    std::fs::create_dir_all(&dir)?;
    FlowStore::open(dir.join("flow.db"), events).context("open flow store")
}

/// The `flux-events`-backed [`EgressAudit`](flux_plugin::EgressAudit) impl: appends a
/// [`EventKind::PrivateNetAdmit`] to the session's stream whenever the plugin host admits a request
/// to a private/internal address under a scoped grant. This is the L6 binding of the L4 trait seam —
/// flux-plugin stays free of an event-store dependency. An append failure is logged, never fatal
/// (auditing must not break a live tool call).
pub(super) struct EventStoreEgressAudit {
    pub(super) store: Arc<EventStore>,
    pub(super) stream: String,
}

impl flux_plugin::EgressAudit for EventStoreEgressAudit {
    fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
        let ev = flux_events::NewEvent::new(flux_events::EventKind::PrivateNetAdmit {
            caller: caller.to_string(),
            host: host.to_string(),
            grant_source: grant_source.to_string(),
        });
        if let Err(e) = self.store.append(&self.stream, ev) {
            eprintln!(
                "{}",
                style::dim(&format!("(audit: failed to record private-net admit: {e})"))
            );
        }
    }
}

/// L6 binding of the L5 [`flux_web::RecordSink`] seam: contributes the `web.page` records `web.fetch`
/// produces to the workspace datasource backend, so a fetched page is searchable afterwards. Errors
/// are swallowed — contribution is best-effort enrichment, never load-bearing for the fetch.
pub(super) struct BackendRecordSink {
    pub(super) backend: Arc<dyn flux_capabilities::DatasourceBackend>,
}

impl flux_web::RecordSink for BackendRecordSink {
    fn contribute(&self, records: &[flux_datasource::Record]) {
        let _ = self.backend.upsert(records);
    }
}

/// Seed `redactor` from the credential-bearing env vars: the provider keys
/// (`flux_credentials::provider_env_keys()` — the single source, covering the API-key providers and
/// the AWS secret material the Bedrock chain materializes into env) plus flux's own `FLUX_SECRET`.
/// Credential-shaped tokens are also caught by the redactor's heuristics; this makes the known ones
/// exact. The redactor shares its value store across clones, so seeding any clone seeds them all.
pub(super) fn seed_provider_env_secrets(redactor: &flux_secret::Redactor) {
    let secret_refs: Vec<flux_secret::Ref> = flux_credentials::provider_env_keys()
        .iter()
        .chain(["FLUX_SECRET"].iter())
        .map(|k| flux_secret::Ref::env(*k))
        .collect();
    flux_runtime::SecretResolver::new().seed_redactor(&mut redactor.clone(), &secret_refs);
}

/// L6 binding of the L4 [`flux_plugin::SecretSink`] seam: registers a credential the host materialized
/// on the `credential` capability path with the executor's [`Redactor`](flux_secret::Redactor), so it
/// is scrubbed from any model-visible output. The redactor shares its value store across clones, so a
/// secret registered here is redacted by the clone the executor uses.
pub(super) struct RedactorSecretSink {
    pub(super) redactor: flux_secret::Redactor,
}

impl flux_plugin::SecretSink for RedactorSecretSink {
    fn register_secret(&self, value: &str) {
        self.redactor.add_secret(value);
    }
}

/// L6 binding of the L5 [`flux_capabilities::CrossPluginAudit`] seam: appends a `CrossPluginResolve`
/// event recording which consumer resolved which provider's credential, by *location* (the
/// `credential_ref` string) — never the value (D-27); and (D-30) an `EndpointDiscovered` event per
/// provider whose discovery returned candidates — count only, no URL, no secret. An append failure is
/// logged, never fatal.
pub(super) struct EventStoreCrossPluginAudit {
    pub(super) store: Arc<EventStore>,
    pub(super) stream: String,
}

impl flux_capabilities::CrossPluginAudit for EventStoreCrossPluginAudit {
    fn record_cross_plugin_resolve(
        &self,
        consumer: &str,
        provider: &str,
        reference_location: &str,
    ) {
        let ev = flux_events::NewEvent::new(flux_events::EventKind::CrossPluginResolve {
            consumer: consumer.to_string(),
            provider: provider.to_string(),
            reference_location: reference_location.to_string(),
        });
        if let Err(e) = self.store.append(&self.stream, ev) {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "(audit: failed to record cross-plugin resolve: {e})"
                ))
            );
        }
    }

    fn record_discovery(&self, product: &str, provider: &str, count: usize) {
        let ev = flux_events::NewEvent::new(flux_events::EventKind::EndpointDiscovered {
            product: product.to_string(),
            provider: provider.to_string(),
            count,
        });
        if let Err(e) = self.store.append(&self.stream, ev) {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "(audit: failed to record endpoint discovery: {e})"
                ))
            );
        }
    }
}

/// Build a fresh boxed provider for a model spec (used by the sub-agent factory).
pub(super) fn provider_for(spec: &str) -> Result<Box<dyn Provider>> {
    if is_mock_spec(spec) {
        Ok(Box::<MockCliProvider>::default())
    } else {
        let (native, _provider, _model) = build_provider(spec).map_err(|e| {
            anyhow::anyhow!(
            "sub-agent provider: {e} (hint: the parent --model spec is forwarded to sub-agents)"
        )
        })?;
        Ok(Box::new(native))
    }
}

/// A provider constructed on FIRST use (C-11). The deterministic execution paths (`flux flow run`,
/// `flux preset --run`) replay pre-authored plans that often contain no model op at all — demanding
/// a credential up front broke credential-less replay (CI boxes re-running a saved plan). The
/// construction error, when the flow DOES reach a model op, is the same one the eager path raises.
pub(super) struct LazyProvider {
    pub(super) spec: String,
    /// The provider prefix of `spec`, for `Provider::name` (a `&str` getter needs owned storage).
    pub(super) display: String,
    /// Unresolved provider-local default model carried by the engine until first construction.
    pub(super) default_model: String,
    pub(super) cell: tokio::sync::OnceCell<(Box<dyn Provider>, String)>,
}

impl LazyProvider {
    pub(super) fn new(spec: String) -> Self {
        let display = spec.split('/').next().unwrap_or("model").to_string();
        let default_model = spec
            .split_once('/')
            .map(|(_, model)| model.to_string())
            .unwrap_or_else(|| spec.clone());
        Self {
            spec,
            display,
            default_model,
            cell: tokio::sync::OnceCell::new(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for LazyProvider {
    fn name(&self) -> &str {
        &self.display
    }

    async fn stream(
        &self,
        mut req: flux_provider::Request,
    ) -> flux_core::Result<flux_provider::ChunkStream> {
        let (provider, resolved_model) = self
            .cell
            .get_or_try_init(|| async {
                let (native, _provider, model) = build_provider(&self.spec)
                    .map_err(|e| flux_core::Error::Other(e.to_string()))?;
                Ok::<_, flux_core::Error>((Box::new(native) as Box<dyn Provider>, model))
            })
            .await?;
        // The engine's inherited model is unresolved on this lazy path, so replace only that exact
        // default. An explicitly configured same-provider stage model is already provider-local and
        // must survive instead of being silently overwritten by the parent default.
        if req.model == self.default_model {
            req.model = resolved_model.clone();
        }
        provider.stream(req).await
    }
}

/// Built-in sub-agent roles (used when `.flux/agents/*.md` doesn't define them).
pub(super) const DEFAULT_ROLES: &[(&str, &str, &str)] = &[
    (
        "scout",
        "Fast read-only codebase reconnaissance",
        "You are a scout. Quickly investigate the codebase with read-only tools and return a \
         compressed summary of relevant findings. Do not modify anything.",
    ),
    (
        "planner",
        "Produce a structured implementation plan",
        "You are a planner. Analyze the task and return a concise, ordered list of concrete \
         subtasks with any open questions. Do not modify files.",
    ),
    (
        "worker",
        "Execute a single well-scoped subtask",
        "You are a worker. Execute the given subtask precisely using the available tools, then \
         report what you changed.",
    ),
    (
        "reviewer",
        "Review changes for correctness",
        "You are a reviewer. Inspect the described changes for bugs and issues and report your \
         findings. Read-only.",
    ),
    (
        "evaluator",
        "Judge whether a goal is satisfied",
        "You are a strict evaluator. Given a goal and the latest result, reply with exactly \
         `SATISFIED` if the goal is fully met, otherwise `CONTINUE: <one concrete next \
         instruction>`. Do not do the work yourself.",
    ),
    (
        "summarizer",
        "Condense a transcript",
        "You are a summarizer. Condense the conversation so far into a compact set of durable \
         facts, decisions, and open threads. Preserve file paths, names, and numbers. Be terse.",
    ),
];

/// Load repository roles through a confined workspace, while preserving the existing precedence:
/// trusted user-global control-plane roles override repository definitions, and built-ins fill only
/// missing names. Malformed/unreadable repository roles are still fatal: silently skipping one
/// could replace an explicit tool allowlist with a broader built-in role.
pub(super) fn load_roles(cwd: &std::path::Path) -> Result<RoleRegistry> {
    let role_system = System::new(Workspace::new(cwd).context("role workspace")?);
    let project = RoleRegistry::try_load_project(&role_system, ".flux/agents")
        .context("load project roles")?;
    let mut reg = if let Some(home) = std::env::var_os("HOME") {
        let dirs = [std::path::PathBuf::from(home).join(".flux").join("agents")];
        RoleRegistry::try_load(&dirs).context("load user-global roles")?
    } else {
        RoleRegistry::default()
    };
    reg.extend_missing(project);
    for (name, desc, prompt) in DEFAULT_ROLES {
        if reg.get(name).is_none() {
            reg.insert(Role {
                name: (*name).to_string(),
                description: (*desc).to_string(),
                model: None,
                thinking: None,
                effort: None,
                agent_loop: None,
                tools: None, // built-in roles inherit the parent's full toolset
                prompt: (*prompt).to_string(),
            });
        }
    }
    // The strict-review reviewer roles ship in the binary (L-14) — `flux review` and the
    // `review_code` journey must work in ANY repo, not just one carrying `.flux/agents/review-*.md`
    // (a project's own files, loaded above, still win).
    for role in flux_app::review::builtin_review_roles() {
        if reg.get(&role.name).is_none() {
            reg.insert(role);
        }
    }
    Ok(reg)
}

/// The session-ambient group-surfacing signals known to the host at startup (D-115): `endpoint`
/// when the loaded endpoints store has records — so an operator who registered a Postgres
/// endpoint sees the endpoint ops without a kubeconfig. Computed from the startup-loaded registry
/// (an in-memory emptiness check), never by re-reading `~/.flux/endpoints.toml` per turn;
/// sticky-monotonic surfacing makes a startup-static answer sufficient.
pub(super) fn session_ambient_signals(
    endpoints: &flux_capabilities::EndpointRegistry,
) -> Vec<String> {
    if endpoints.is_empty() {
        Vec::new()
    } else {
        vec!["endpoint".to_string()]
    }
}

/// Put a plugin's otherwise-ungrouped visible operations behind one turn-intent group. Explicit
/// manifest membership and per-op group tags remain authoritative; this only changes the legacy
/// `group = None` case that would otherwise classify hundreds of installed integration ops as core
/// and inject them into every adaptive model-stage request.
pub(super) fn implicit_plugin_group(
    manifest: &flux_plugin::PluginManifest,
    specs: &[flux_spec::ToolSpec],
) -> Option<flux_evidence::ToolGroup> {
    let explicitly_grouped: std::collections::HashSet<&str> = manifest
        .groups
        .iter()
        .flat_map(|group| group.tools.iter().map(String::as_str))
        .collect();
    let mut tools: Vec<String> = specs
        .iter()
        .filter(|spec| spec.group.is_none() && !explicitly_grouped.contains(spec.name.as_str()))
        .map(|spec| spec.name.clone())
        .collect();
    tools.sort();
    tools.dedup();
    if tools.is_empty() {
        return None;
    }

    let intent = manifest.name.to_lowercase();
    let mut routing = std::collections::BTreeSet::from([intent.clone()]);
    routing.extend(
        manifest
            .capabilities
            .http_hosts
            .iter()
            .chain(
                manifest
                    .endpoints
                    .iter()
                    .flat_map(|endpoint| endpoint.http_hosts.iter()),
            )
            .map(|host| host.trim().trim_start_matches("*.").to_lowercase())
            .filter(|host| !host.is_empty()),
    );
    Some(flux_evidence::ToolGroup {
        name: format!("plugin.{intent}"),
        description: format!(
            "Operations from the live `{}` integration. Routing hints: {}.",
            manifest.name,
            routing.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        tools,
        surface_when: routing
            .into_iter()
            .map(|signal| flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_TURN_INTENT.into(),
                signal: Some(signal),
            })
            .collect(),
    })
}

/// Read-only ops pre-allowed by default when no `[permissions].allow` is configured, so the common
/// case needs no config. `read`/`glob`/`grep`/`search` are the workspace reads; `now`/`cwd`/`home_dir`/
/// `sys_info` are zero-arg ambient reads (no IO, no permission subjects) that carry no approval-worthy
/// effect — gating them only adds friction (e.g. a `now()` in a stored flow would otherwise prompt, and
/// auto-deny on a non-TTY). A configured allow-list replaces this default entirely.
pub(super) const DEFAULT_ALLOW: &[&str] = &[
    "read", "glob", "grep", "search", "now", "cwd", "home_dir", "sys_info",
];

/// Turn the operator's `[limits]` table into runtime ceilings (C-299) — the **one** place the CLI
/// does so.
///
/// Build this **once** per agent assembly and clone it into every consumer. `ResourceLimits` clones
/// share the concurrency semaphore (that sharing is what makes the ceiling bound the *runtime*
/// rather than one executor), so calling this twice would silently fork one configured ceiling of N
/// into two independent ceilings of N — the top-level agent and its sub-agents would then each get
/// their own budget instead of counting against the operator's single number.
pub(super) fn cli_resource_limits(cfg: &flux_config::Config) -> flux_runtime::ResourceLimits {
    flux_runtime::ResourceLimits::from_config(&cfg.limits)
}

/// Assemble the CLI's shared runtime envelope from already-resolved surface decisions.
///
/// `build_agent_with` remains responsible for choosing the catalog, permissions, approver, and
/// integrations. This helper is the mechanical C-67 seam: tests and production both prove those
/// choices enter one explicitly rooted [`ExecutionEnvironment`] without another cwd lookup.
///
/// `resource_limits` (C-299) rides through here rather than being read from config inside, so the
/// caller can hand the **same** ceiling to the sub-agent spawner — see [`cli_resource_limits`].
#[allow(clippy::too_many_arguments)]
pub(super) fn assemble_cli_execution_environment(
    system: Arc<System>,
    registry: ToolRegistry,
    permissions: PermissionManager,
    approver: Arc<dyn Approver>,
    authorization: ExecutionAuthorization,
    redactor: flux_secret::Redactor,
    spawner: Option<Arc<dyn flux_runtime::Spawner>>,
    hooks: Vec<Arc<dyn flux_runtime::PreToolHook>>,
    resource_limits: flux_runtime::ResourceLimits,
) -> ExecutionEnvironment {
    let mut environment =
        ExecutionEnvironment::new(system, registry, permissions, approver, authorization)
            .with_redactor(redactor)
            .with_hooks(hooks)
            .with_resource_limits(resource_limits);
    if let Some(spawner) = spawner {
        environment = environment.with_spawner(spawner);
    }
    environment
}

/// Agentic mode: run a tool-enabled, policy-gated, session-persisted turn.
/// Build a tool-enabled agent (provider + safety envelope + session) for agentic mode / the REPL.
/// Eager provider construction: an agentic turn always calls the model, so a credential problem
/// should fail fast here. Deterministic execution paths use [`build_agent_lazy`].
pub(super) async fn build_agent(
    flags: &AgentFlags,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
    build_agent_with(flags, true, None).await
}

/// [`build_agent`] with a LAZY provider (C-11): `flux flow run` / `flux preset --run` replay
/// pre-authored plans that may contain no model op — they must not demand a credential up front.
/// The provider constructs on the first actual model call (same error, surfaced only if needed).
/// `session_override`, when given (L-25's `flux flow run --resume`), is used as the run's session id
/// verbatim instead of minting a fresh one — so a corrected re-run lands in the SAME session whose
/// halt latch it is folding.
pub(super) async fn build_agent_lazy(
    flags: &AgentFlags,
    session_override: Option<String>,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
    build_agent_with(flags, false, session_override).await
}

/// Build the workspace view used by every saved-flow consumer. Agent construction creates the two
/// global homes (preserving its existing behavior); read-only CLI listing/resolution merely
/// registers homes that already exist, so `flux flow list` has no session/provider side effects.
pub(super) fn workspace_with_flow_roots(
    cwd: &std::path::Path,
    create_global: bool,
) -> Result<Workspace> {
    let mut workspace = Workspace::from_env(cwd).context("workspace")?;
    if let Some(home) = std::env::var_os("HOME") {
        let flux_dir = std::path::PathBuf::from(home).join(".flux");
        for (name, sub) in [("global_flows", "flows"), ("global_ops", "ops")] {
            let dir = flux_dir.join(sub);
            if create_global {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("create {}", dir.display()))?;
            }
            if dir.is_dir() {
                workspace
                    .add_named_root(name, &dir)
                    .with_context(|| format!("register {}", dir.display()))?;
            }
        }
    }
    Ok(workspace)
}

/// The D-130 sandbox posture resolved from the environment — the counterpart to
/// `workspace_with_flow_roots`'s custom [`Workspace`] construction. Call sites that build a
/// `System` from a hand-assembled workspace (rather than `System::from_env`) attach this via
/// `System::with_sandbox` so they still pick up `FLUX_SANDBOX`/`[sandbox]` like every other
/// production entry point.
pub(super) fn resolved_sandbox() -> flux_system::sandbox::Sandbox {
    flux_system::sandbox::Sandbox::resolve(flux_system::sandbox::SandboxSettings::from_env())
}

/// Resolve an explicit outer-loop selector. The built-in preset needs no IO; a file is read through
/// the guarded workspace rather than by the engine probing a magic path behind the caller's back.
pub(super) async fn resolve_agent_loop(
    selection: Option<&str>,
    system: &System,
) -> Result<AgentLoopSpec> {
    let Some(selection) = selection.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(AgentLoopSpec::default());
    };
    if selection.eq_ignore_ascii_case("adaptive") {
        return Ok(AgentLoopSpec::default());
    }
    let source = system
        .read_file(selection)
        .await
        .with_context(|| format!("read explicit agent loop `{selection}`"))?;
    AgentLoopSpec::parse(&source).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Register the model-facing operation packs onto the top-level registry, in the order that lets the
/// name-collision checks fire deterministically: the cognition pack (when a sibling provider was
/// built), the second-opinion `consult` op (when a target is configured), then eval/self-improvement,
/// the reflect stages, flow discovery/run, and render. These are registered on the top-level registry
/// ONLY — never on a worker sub-agent's scoped toolset, so a child can't run eval/git ops or the
/// model-facing cognition/consult ops.
#[allow(clippy::too_many_arguments)]
/// The private-network egress grant the `fleet.*` ops run under.
///
/// Deliberately **not** the `[private_net] web` scope. That grant names the native web family
/// (`http.request`, `web.fetch`, `browser.*`); quietly extending it to cover outbound dispatch to a
/// remote worker would widen a grant an operator already made, for a family they did not grant it
/// to. Only the blanket `--allow-private-net` / `FLUX_ALLOW_PRIVATE_NET` override — already
/// documented as a private-net `*` grant for native ops — admits a private worker; otherwise the
/// full SSRF guard applies and a `worker-1.internal` endpoint is refused until an operator says
/// otherwise.
///
/// This is a bound on the *grant*, never a substitute for the guard: every `fleet.*` op resolves its
/// caller-supplied endpoint through `guard_url_scoped` before any request either way (A-116), and
/// each reports the worker's origin as its permission subject.
fn fleet_private_net() -> flux_system::net::PrivateNetAllow {
    if private_net_cli_override() {
        flux_system::net::PrivateNetAllow::Any
    } else {
        flux_system::net::PrivateNetAllow::None
    }
}

/// The whole fleet surface, in one place: worker **lifecycle** (C-243) plus outbound A2A **dispatch**
/// (A-116).
///
/// A-116 landed the dispatch ops; nothing constructed them, so a Program could not call one. This is
/// the one construction site, shared by the agent assembly below and the `flux app run` path, so the
/// two surfaces cannot offer different fleet catalogs — and a further fleet op is added here alone.
///
/// C-243 adds `fleet.start` / `fleet.worker_status` / `fleet.stop` over a [`ProcessRuntime`], because
/// the dispatch half could only talk to a worker that somehow already existed: `flux` never spawned
/// `flux`, and `FlowEngine`'s turn gate serves one concurrent turn per worker, so a wave was a wave of
/// one. `ProcessRuntime` is constructed here rather than passed in for the same reason the dispatch
/// ops are — one site, one catalog — and it owns the live child handles, so its lifetime is the
/// process's.
///
/// **Cleanup is best-effort, and the gap is real.** `ManagedChild` is `kill_on_drop`, so an orderly
/// exit — and any panic that unwinds — stops every worker this coordinator started. `SIGKILL` and
/// `std::process::exit` run no destructor, so neither does. bubblewrap's `--die-with-parent` closes
/// that hole, but only when the coordinator's own sandbox is active; an unsandboxed coordinator that
/// is `SIGKILL`ed therefore leaves its workers running — auto-approving (`--yes`) daemons on loopback
/// ports. Until a worker registry survives a coordinator restart (the board write C-243 left owed),
/// recovery is `fleet.stop` from the next coordinator, or by hand.
///
/// `ExternalRuntime` is deliberately **not** wired: naming already-running workers is operator
/// configuration that does not exist yet, and inventing a config key for it here would add public
/// surface this story does not own. The port has both implementations; only the spawning one is
/// reachable from the CLI today.
///
/// The worker bearer token is `None`: a token for an authenticated worker is operator configuration
/// that does not exist yet, and inventing an env var for it here would add public surface this
/// story does not own. A worker behind `flux serve`'s required auth is therefore not yet reachable;
/// an unauthenticated one is.
pub(super) fn try_register_fleet(
    registry: &mut ToolRegistry,
    ledger: Option<Arc<dyn flux_runtime::DispatchLedger>>,
) -> Result<()> {
    let runtime: Arc<dyn flux_runtime::AgentRuntime> =
        Arc::new(flux_orchestrate::ProcessRuntime::new()?);
    let lifecycle: Vec<Arc<dyn flux_runtime::Tool>> = vec![
        Arc::new(flux_orchestrate::FleetStartTool::new(runtime.clone())),
        Arc::new(flux_orchestrate::FleetWorkerStatusTool::new(
            runtime.clone(),
        )),
        Arc::new(flux_orchestrate::FleetStopTool::new(runtime)),
    ];
    registry.try_register_all_from("flux-cli fleet lifecycle", lifecycle)?;

    let private_net = fleet_private_net();
    let dispatch = flux_orchestrate::FleetDispatchTool::new(private_net.clone(), None);
    // With a ledger, `fleet.dispatch` records `runner` + `task_id` onto the board item it was given,
    // which is what makes design §5's "the board IS the run registry" true rather than aspirational
    // — and it is why crash recovery can be "restart, sweep, re-derive" with no second store.
    // Without one, the op refuses any call that names an `item` rather than dispatching work that
    // nothing could later sweep. Dispatch with no `item` stays legal either way, so a Program that
    // declares no board is unaffected.
    let dispatch = match ledger {
        Some(ledger) => dispatch.with_ledger(ledger),
        None => dispatch,
    };
    let tools: Vec<Arc<dyn flux_runtime::Tool>> = vec![
        Arc::new(dispatch),
        Arc::new(flux_orchestrate::FleetStatusTool::new(
            private_net.clone(),
            None,
        )),
        Arc::new(flux_orchestrate::FleetCancelTool::new(private_net, None)),
    ];
    registry.try_register_all_from("flux-cli fleet dispatch", tools)?;
    Ok(())
}

pub(super) fn register_tool_packs(
    registry: &mut ToolRegistry,
    cog_provider: Option<Box<dyn Provider>>,
    model: &str,
    flags: &AgentFlags,
    cfg: &flux_config::Config,
    canonical_model_spec: &str,
    events: &Arc<EventStore>,
) -> Result<()> {
    if let Some(cog_provider) = cog_provider {
        flux_cognition::CognitionPack::new(Arc::from(cog_provider), model.to_string())
            .with_reasoning(flags.think, flags.effort.map(Into::into))
            .try_register_from("flux-cli cognition pack", registry)?;
    }
    // A-96: `consult` — ask a DIFFERENT model for a second opinion; pure advice, no tools. Only
    // registered when an operator has named a default target (`[consult] model`): the op's group
    // gates its ADVERTISEMENT on the same setting (see the `consult` ambient signal above), but
    // registering it at all only when opted-in keeps an unconfigured workspace from carrying a
    // dormant model-invoking op in its catalog machinery for no reason. `resolve_cli_provider` is
    // reused verbatim — the exact provider/model routing `-m`/`--model` uses, subscription
    // providers included — resolved fresh (eager) on every call, never cached, since a consult
    // reply is by definition a cold prompt for whichever model answers it.
    if let Some(consult_target) = cfg.consult.model.clone() {
        let factory: flux_cognition::ConsultFactory = Arc::new(move |spec: &str| {
            let resolved = resolve_cli_provider(spec, true)
                .map_err(|e| flux_core::Error::Other(e.to_string()))?;
            Ok((resolved.provider, resolved.model))
        });
        let max_calls = cfg
            .consult
            .max_calls
            .unwrap_or(flux_cognition::DEFAULT_CONSULT_MAX_CALLS);
        flux_cognition::ConsultTool::new(
            factory,
            Some(consult_target),
            canonical_model_spec.to_string(),
            max_calls,
        )
        .try_register(registry)?;
    }
    // A-98: `schedule_wakeup` — let a live turn register a durable future continuation of its own
    // session. Off by default (`[wakeup] enabled`), mirroring `enable_shell`'s posture: an
    // unconfigured workspace carries no dormant host-write-authority op. Registering one still
    // needs approval (the op's default-derived `host.write` authority requirement) — this switch
    // only controls whether the op is offered at all.
    if cfg.wakeup.enabled {
        flux_flow::wakeup::WakeupTool::new(
            events.clone(),
            cfg.wakeup
                .max_horizon_secs
                .unwrap_or(flux_flow::wakeup::DEFAULT_MAX_HORIZON_SECS),
            cfg.wakeup
                .max_pending_per_session
                .unwrap_or(flux_flow::wakeup::DEFAULT_MAX_PENDING_PER_SESSION),
        )
        .try_register(registry)?;
    }
    // Outbound A2A dispatch to remote flux workers (A-116, wired by A-131). Unconditional: the
    // worker endpoint is a per-call argument, not configuration, so there is nothing to switch on —
    // and the ops carry no authority of their own beyond the egress guard they each run first.
    //
    // No ledger on this path: `register_tool_packs` builds the agent catalog, which has no Program
    // and therefore no declared board to record a dispatch onto. Naming an `item` here refuses
    // rather than dispatching untrackable work; the `flux app run` path wires the ledger from the
    // Program's own board.
    try_register_fleet(registry, None)?;
    // Eval / self-improvement ops (the ones the improve flows orchestrate).
    flux_eval::try_register_eval_ops(registry)?;
    // Authored-loop stages are registered for `agent-loop.flux` but tagged to the never-surfaced
    // `reflect` group, so they stay OUT of native model catalogs. `op.register` remains model-facing
    // and delegates to the engine-installed composite registrar.
    flux_tools::try_register_reflect(registry)?;
    // Flow discovery/run: `flow_list` (enumerate .flux/flows + ~/.flux/flows) and `flow_run`
    // (run a stored flow by name in the current session). Model-facing.
    flux_tools::try_register_flows(registry)?;
    // `flow_render`: Flux-Lang source/plan → syntax-highlighted SVG (source + tree views), for
    // surfaces that can't highlight .flux themselves (READMEs, Slack, docs, chat panels).
    flux_tools::try_register_render(registry)?;
    Ok(())
}

/// The permission floor, approver, and JS pre-tool hooks for the CLI executor.
struct ResolvedPermissions {
    perms: PermissionManager,
    approver: Arc<dyn Approver>,
    hooks: Vec<Arc<dyn flux_runtime::PreToolHook>>,
}

/// Resolve the permission floor, approver, and pre-tool hooks. Read-only tools are pre-allowed by
/// default (empty allow-list) so the common case needs no config; network/mutating tools still gate.
/// A configured allow-list replaces the [`DEFAULT_ALLOW`] default entirely. `--yes` swaps the
/// interactive approver for auto-allow. Hooks are the observe/modify/deny JS scripts under the
/// project and user `.flux/hooks/*.js`.
fn resolve_permissions(
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    flags: &AgentFlags,
) -> ResolvedPermissions {
    let mut allow = cfg.permissions.allow.clone();
    if allow.is_empty() {
        allow.extend(DEFAULT_ALLOW.iter().map(|s| s.to_string()));
    }
    let perms = PermissionManager::from_rules(&allow, &cfg.permissions.deny);
    let approver: Arc<dyn Approver> = if flags.yes {
        Arc::new(AllowApprover)
    } else {
        Arc::new(StdinApprover)
    };
    let mut hook_dirs = vec![cwd.join(".flux").join("hooks")];
    if let Some(home) = std::env::var_os("HOME") {
        hook_dirs.push(std::path::PathBuf::from(home).join(".flux").join("hooks"));
    }
    let js_hooks = flux_plugin::hooks::JsHookEngine::load(&hook_dirs);
    let mut hooks: Vec<Arc<dyn flux_runtime::PreToolHook>> = Vec::new();
    if !js_hooks.is_empty() {
        hooks.push(Arc::new(js_hooks));
    }
    ResolvedPermissions {
        perms,
        approver,
        hooks,
    }
}

/// The engine-assembly inputs produced by the middle of [`build_agent_with`]: the provider +
/// executor + event store, plus the resolved model, prompt, evidence-gated groups, ambient signals,
/// config-authored model stages, and the validated iteration cap.
struct EngineParts {
    provider: Box<dyn Provider>,
    executor: flux_runtime::Executor,
    events: Arc<EventStore>,
    model: String,
    system_prompt: String,
    groups: Vec<flux_evidence::ToolGroup>,
    ambient_signals: Vec<String>,
    model_stages: std::collections::BTreeMap<String, flux_flow::ModelStageDefinition>,
    max_iterations: usize,
    /// Explicitly enabled skills (D-189: loaded up front, before model resolution, so a skill's
    /// `model` frontmatter can take part in the precedence chain) — carried through rather than
    /// re-discovered here.
    skills: Vec<flux_skill::Skill>,
    /// D-188: the opt-in model-invoked skill catalog, empty unless `--skills-model-invoked` /
    /// `[skills] model_invoked` is set — carried through rather than re-discovered here.
    model_invoked_skills: Vec<flux_skill::Skill>,
}

/// Assemble the [`FlowEngine`] from the resolved parts: install the authored-loop host, load the
/// selected Flux-Lang outer loop, register the config-authored model stages, and apply the per-turn
/// token ceiling (A-10, default OFF; precedence `--turn-budget` > `FLUX_TURN_TOKEN_BUDGET` >
/// `[limits] turn_token_budget`). A malformed env budget is a hard error, not a silent fall-through —
/// this is a spend/safety ceiling and running unbounded is exactly the failure it prevents.
async fn assemble_engine(
    parts: EngineParts,
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    flags: &AgentFlags,
    system: &System,
) -> Result<FlowEngine> {
    let EngineParts {
        provider,
        executor,
        events,
        model,
        system_prompt,
        groups,
        ambient_signals,
        model_stages,
        max_iterations,
        skills,
        model_invoked_skills,
    } = parts;
    let flow = open_flow_store(events.clone())?;
    let spec = AgentSpec {
        model,
        system_prompt,
        skills,
        model_invoked_skills,
        max_tokens: flags.max_tokens,
        max_iterations,
        thinking: flags.think,
        effort: flags.effort.map(Into::into),
        agent_loop: resolve_agent_loop(
            flags
                .agent_loop
                .as_deref()
                .or(cfg.agent.loop_spec.as_deref()),
            system,
        )
        .await?,
        groups,
        adaptive_policy: adaptive_loop_policy(flags, &cfg.agent)?,
        ambient_signals,
        compact_threshold_chars: compact_threshold(),
        cwd: cwd.to_path_buf(),
        // The CLI builds its own richly-configured executor (perms/approver/hooks/policy/identity)
        // above, so `tools`/`permissions` are already applied there — `into_engine` consumes only the
        // engine-identity fields.
        ..AgentSpec::default()
    };
    let agent = spec
        .into_engine(Arc::from(provider), executor, events, flow)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    agent.loop_host.set_model_stages(model_stages);
    let env_budget = match std::env::var("FLUX_TURN_TOKEN_BUDGET") {
        Ok(v) => Some(v.trim().parse::<u64>().map_err(|e| {
            anyhow::anyhow!("FLUX_TURN_TOKEN_BUDGET is not a token count ({v:?}): {e}")
        })?),
        Err(_) => None,
    };
    let turn_budget = flags
        .turn_budget
        .or(env_budget)
        .or(cfg.limits.turn_token_budget);
    agent.loop_host.set_token_budget(turn_budget);
    Ok(agent)
}

pub(super) async fn build_agent_with(
    flags: &AgentFlags,
    eager_provider: bool,
    session_override: Option<String>,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
    // Guarded system rooted at the current directory; layered config loaded from it.
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    // Validate this input-driven expansion bound before provider, plugin, or agent assembly work.
    let max_iterations = agent_max_iterations(flags, &cfg.agent)?;
    // Role metadata controls the child tool ceiling (omitting tools inherits the parent's catalog),
    // so strict project/user discovery must win over every eager provider failure. If a malformed
    // role were discovered later, a credential/provider error could mask the actionable file path
    // and leave the least-privilege failure dependent on provider availability.
    let roles = load_roles(&cwd)?;
    // Opt into the generic `bash` op when config enables it — via the runtime's in-process
    // override, NOT `set_var` (we're on a live multi-threaded runtime here). A user who set
    // `FLUX_ENABLE_BASH` directly is honored too (we only ever turn it on here, never off).
    if cfg.enable_shell {
        flux_runtime::set_shell_opt_in(true);
    }
    // Skills load before model resolution (D-189): an enabled skill's `model` frontmatter is a
    // precedence tier between the explicit `--model` flag and config/default, so it must be known
    // before the primary provider is resolved. Loaded once here and threaded through
    // `EngineParts` into `assemble_engine` so the directory walk doesn't happen twice.
    let skills = load_skills(&cwd, &cfg, &flags.skill_dirs, &flags.skills)?;
    let model_spec = resolve_model_spec_with_skill(&flags.model, &cfg, &skills);
    // D-188: opt-in model-invoked skill catalog, `--skills-model-invoked` flag OR `[skills]
    // model_invoked` config. Loaded here (not in `assemble_engine`) for the same reason `skills`
    // is: one directory walk, threaded through `EngineParts`.
    let model_invoked_skills = load_model_invoked_skill_catalog(
        &cwd,
        &cfg,
        &flags.skill_dirs,
        flags.skills_model_invoked || cfg.skills.model_invoked,
    )?;

    // The engine's primary provider under the mock/lazy/eager policy (the `mock` provider lets the
    // full agentic loop be exercised offline via the CLI).
    let ResolvedProvider {
        provider,
        model,
        canonical_spec,
    } = resolve_cli_provider(&model_spec, eager_provider)?;

    // Global roots for agent-reusable definitions: `~/.flux/flows` is the home for flows +
    // composite ops (discovered by `flow_list`, run by `flow_run`, ops auto-loaded); `~/.flux/ops`
    // is the legacy location, still read during the ops→flows unification.
    let system = Arc::new(
        System::new(workspace_with_flow_roots(&cwd, true)?).with_sandbox(resolved_sandbox()),
    );
    // C-122: the session's workspace handle, created BEFORE plugin loading so the plugin host
    // capabilities and the executor's context share one view of worktree transitions.
    let session_workspace = flux_runtime::WorkspaceContext::new(system.clone());

    // Project context folded into the system prompt: environment, git working-tree state, repo
    // shape/stack, and project conventions (CLAUDE.md/AGENTS.md) — so the agent isn't cold-starting.
    // `ContextFragments` comes last so the always-loaded conventions land before the guidance that
    // only applies to what the working set is actually touching (A-97).
    let system_prompt = Projector::new()
        .with(Box::new(EnvContext::new(cwd.clone())))
        .with(Box::new(GitContext::new(cwd.clone())))
        .with(Box::new(RepoSignal::new(cwd.clone())))
        .with(Box::new(ProjectFiles::new(cwd.clone())))
        .with(Box::new(ContextFragments::new(cwd.clone())))
        .try_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .await
        .context("load guarded project context")?;

    // Authorization policy floor (built-in local grants + any config grants) and resolved
    // identity — shared by the top-level agent and the sub-agents it spawns.
    let mut policy = flux_policy::default_local_grants();
    if let Some(extra) = cfg.policy.clone() {
        policy.grants.extend(extra.grants);
    }
    let (caller, trust) =
        flux_auth::IdentityProvider::resolve(&flux_auth::LocalIdentity::current());
    // ONE immutable fallback identity backs the top-level executor AND sub-agent spawner. A
    // per-request server turn carries its principal lexically; supervised children inherit that
    // turn snapshot instead of mutating this assembly-time default.
    let identity = flux_runtime::IdentityCell::new(caller, trust);

    // The unified event store, opened BEFORE the sub-agent spawner (A-08: child runs audit into
    // this same store by default) and before plugins (the egress-audit hook appends
    // `PrivateNetAdmit` events to this stream).
    let events = Arc::new(open_event_store()?);

    // C-299: the operator's `[limits]` ceilings, resolved ONCE here and handed to both the
    // sub-agent spawner just below and the top-level environment further down. Resolved once
    // deliberately: a second `cli_resource_limits` call would mint a second semaphore, so the
    // top-level agent's own executors would stop sharing one budget with each other.
    let resource_limits = cli_resource_limits(&cfg);

    // Sub-agent spawner (multi-agent orchestration): the `task` tool delegates to roles, each run
    // as an isolated sub-agent — bounded by the same authorization policy (no blanket allow).
    let mut child_base = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut child_base)?;
    let factory: ProviderFactory = {
        let spec = model_spec.clone();
        Arc::new(move || provider_for(&spec).map_err(|e| flux_core::Error::Other(e.to_string())))
    };
    // One construction path for sub-agents (shared with the SDK's `FlowClient::with_sub_agents`):
    // `SubAgents::into_spawner` builds the spawner; we register `TaskTool` into the top-level registry
    // below. Sub-agents inherit the same authorization floor as the top-level agent, and audit into
    // the shared event store by default (A-08) — each child gets its own correlated session stream.
    let spawner: Arc<dyn flux_runtime::Spawner> =
        SubAgents::new(roles, child_base, factory, model.clone(), flags.max_tokens)
            .with_reasoning(flags.think, flags.effort.map(Into::into))
            .with_authorization_cell(policy.clone(), identity.clone())
            .with_audit(events.clone())
            // C-299: `[limits]` binds `task`-delegated work too, not just the top-level agent —
            // before this, a sub-agent ran on an unbounded executor. Each child gets an independent
            // copy of these ceilings (same numbers, own concurrency budget); sharing one budget
            // across the delegation boundary deadlocks, see `ResourceLimits::independent_copy`.
            .with_resource_limits(resource_limits.clone())
            .into_spawner(system.clone());

    // Tools + permissions: from config (deny/allow rules); if no allow rules are configured,
    // reads are pre-allowed by default so the common case needs no config. Mutating tools prompt
    // (unless --yes) and "always-allow" choices are persisted back by the caller.
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry)?;
    if flags.dev {
        flux_tools::try_register_dev_builtins(&mut registry)?;
    }
    registry.try_register_from("flux-cli sub-agent task operation", Arc::new(TaskTool))?;

    // Model-backed cognition ops (ai.extract/rank/judge/reason, synth, ai.rewrite): the L3
    // CognitionPack, advertised on the real CLI path so a plan can call the model as a typed op.
    // `CognitionPack` needs its own `Arc<dyn Provider>` (the engine's `provider` is moved below), so
    // build a sibling instance under the SAME mock/lazy/eager policy. Only the eager path can fail;
    // when it does we skip the pack rather than fail startup — the rest of the agent is unaffected.
    let cog_provider: Option<Box<dyn Provider>> =
        match resolve_cli_provider(&model_spec, eager_provider) {
            Ok(resolved) => Some(resolved.provider),
            Err(e) => {
                eprintln!(
                    "{}",
                    style::dim(&format!("(cognition pack not wired: {e})"))
                );
                None
            }
        };
    register_tool_packs(
        &mut registry,
        cog_provider,
        &model,
        flags,
        &cfg,
        &canonical_spec,
        &events,
    )?;

    // Auto-index workspace docs (markdown/text, capped & cheap) into the knowledge datasource, and
    // register the retrieval ops (`search`/`get`/`list`/`relation`/`batch_get`/`sources`). The
    // backend is also the sink `web.fetch` contributes `web.page` records to (below), so read pages
    // are groundable.
    let backend = build_doc_index(&system).await;
    flux_capabilities::try_register_datasource_ops(&mut registry, backend.clone())?;

    // This run's session on the store opened above. `session_override` (L-25's `flow run --resume`)
    // wins outright — it names an already-halted session to continue, distinct from the REPL's own
    // `--continue`/`--resume` (latest session) semantics.
    let session_id = if let Some(id) = session_override {
        id
    } else if flags.continue_ || flags.resume {
        events
            .latest_session()
            .context("latest session")?
            .ok_or_else(|| anyhow::anyhow!("no session to resume"))?
    } else {
        events.create_session(&model).context("create session")?
    };

    // Seed the secret redactor from known credential env vars so their values are scrubbed from
    // tool output and logs. (Credential-shaped tokens are also caught by the redactor's heuristics.)
    // Built BEFORE the plugin block so the `credential`-capability secret sink can register
    // host-materialized credentials with the SAME redactor the executor later redacts with — the
    // redactor shares its value store across clones, so a credential resolved mid-run is scrubbed.
    let redactor = flux_secret::Redactor::new();
    seed_provider_env_secrets(&redactor);

    // Native web capabilities (flux-web): `http.request` (tier 1), `web.fetch` + `html_to_markdown`
    // (tier 2), all under the family-wide `[private_net] web` egress scope. Registered here — after
    // the session is resolved — because the `PrivateNetAdmit` audit sink needs the event store +
    // session id, and `web.fetch` contributes `web.page` records to the datasource backend.
    {
        let web_audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
            store: events.clone(),
            stream: session_id.clone(),
        });
        flux_web::try_register_web(
            &mut registry,
            &flux_web::WebOptions {
                private_net: flux_system::net::PrivateNetAllow::from_hosts(
                    effective_web_private_hosts(&cfg),
                ),
                audit: Some(web_audit),
                grant_source: Some(web_grant_source()),
                records: Some(Arc::new(BackendRecordSink {
                    backend: backend.clone(),
                })),
                browser_bin: cfg.browser_bin.clone(),
                allowed_secrets: None,
            },
        )?;
    }

    let integrations = assemble_integrations(
        system.clone(),
        Arc::new(super::app_cmd::WorkspaceSystemSource(
            session_workspace.clone(),
        )),
        backend.clone(),
        false,
        &cfg,
        events.clone(),
        &session_id,
        &redactor,
    )
    .await?;
    for (source, tool) in integrations.tools {
        registry.try_register_from(source, tool)?;
    }
    let plugin_groups = integrations.groups;
    let mut ambient_signals = integrations.ambient_signals;
    // A-96: the `consult` op's group surfaces only when a target is configured, exactly like the
    // `endpoint` ambient signal above — computed once here from config, never re-probed per turn,
    // so surfacing can't churn the prompt prefix mid-session (A-95).
    if cfg.consult.model.is_some() {
        ambient_signals.push("consult".to_string());
    }

    // Config-authored model stages are ordinary typed operations. Register them only after every
    // built-in/plugin operation is known so name collisions and missing gather-tool wiring fail at
    // startup instead of silently shadowing a live capability.
    let mut model_stages = std::collections::BTreeMap::new();
    for (name, stage) in &cfg.agent.stages {
        if name.trim().is_empty() || registry.get(name).is_some() {
            anyhow::bail!(
                "[agent.stages.{name}] must have a non-empty operation name that does not collide with a registered tool"
            );
        }
        if stage.max_tokens == 0 {
            anyhow::bail!("[agent.stages.{name}] max_tokens must be greater than zero");
        }
        for tool in &stage.tools {
            let registered = registry.get(tool).ok_or_else(|| {
                anyhow::anyhow!(
                    "[agent.stages.{name}] tool `{tool}` is not registered and wired on this CLI path"
                )
            })?;
            if !flux_flow::statically_gather_safe(registered.as_ref()) {
                anyhow::bail!(
                    "[agent.stages.{name}] tool `{tool}` is not statically gather-safe (it must be low-risk, side-effect-free, non-mutating, and not capture-only; freshness/non-cacheability is allowed)"
                );
            }
        }
        let effort = stage
            .effort
            .as_deref()
            .map(parse_effort)
            .transpose()
            .with_context(|| format!("[agent.stages.{name}] effort"))?;
        flux_tools::reflect::try_register_model_stage(
            &mut registry,
            name.clone(),
            format!("Run the configured `{name}` model stage."),
            stage.input_schema.clone(),
            stage.output_schema.clone(),
        )?;
        model_stages.insert(
            name.clone(),
            flux_flow::ModelStageDefinition {
                prompt: stage.prompt.clone(),
                input_schema: stage.input_schema.clone(),
                output_schema: stage.output_schema.clone(),
                model: stage.model.clone(),
                tools: stage.tools.clone(),
                max_tokens: stage.max_tokens,
                effort,
            },
        );
    }

    let ResolvedPermissions {
        perms,
        approver,
        hooks,
    } = resolve_permissions(&cwd, &cfg, flags);

    let executor = assemble_cli_execution_environment(
        system.clone(),
        registry,
        perms,
        approver,
        ExecutionAuthorization::with_identity_cell(policy, identity),
        redactor,
        Some(spawner.clone()),
        hooks,
        // C-299: the operator's `[limits]` table finally binds for the shipped binary. This is the
        // SAME value already handed to the sub-agent spawner above, not a second `from_config` —
        // one configured ceiling, one semaphore, counted across the agent and its children.
        resource_limits,
    )
    .with_workspace(session_workspace)
    .into_executor();
    // C-162: resolve `[tools] disable` against the now-final registry (exact op names or
    // `family.*` globs) and install it on the executor — surface-only + defense-in-depth (the
    // engine narrows the per-turn advertised set by it, and `Executor::gate` separately refuses a
    // dispatch that names one). Resolved exactly once here, before any turn runs, so the set can
    // never churn mid-session (A-95). An entry matching no known op is a loud startup warning, not
    // a silent no-op — it's very likely a typo or a stale entry naming a retired op.
    let disabled_tools = executor.registry().resolve_disabled(&cfg.tools.disable);
    for pattern in &disabled_tools.unmatched {
        eprintln!(
            "{}",
            style::dim(&format!(
                "(warning: [tools] disable entry `{pattern}` matches no known op)"
            ))
        );
    }
    let executor = executor.with_disabled_ops(disabled_tools.disabled);
    // Record the available toolchain as a startup observation (audit backbone).
    executor.observe(flux_evidence::Observation::new(
        "toolchain",
        flux_evidence::Phase::Startup,
        serde_json::json!({ "tools": executor.registry().names() }),
    ));
    if !executor.disabled_ops().is_empty() {
        let mut disabled: Vec<&str> = executor.disabled_ops().iter().map(String::as_str).collect();
        disabled.sort_unstable();
        executor.observe(flux_evidence::Observation::new(
            "tools.disabled",
            flux_evidence::Phase::Startup,
            serde_json::json!({ "ops": disabled }),
        ));
    }

    // Evidence-gated tool groups: built-ins (git + language scaffolds) + the eval group, with
    // `.flux/groups.toml` overrides merged on top. The engine re-probes signals each turn and
    // advertises only the surfaced groups' ops; an empty manifest would disable gating.
    let mut groups = flux_tools::groups::builtin_groups();
    groups.push(flux_eval::eval_group());
    groups.push(flux_web::browser_group());
    groups.extend(plugin_groups);
    let groups = flux_config::merge_groups(
        groups,
        flux_runtime::metadata::load_groups(&cwd).context("load .flux/groups.toml")?,
    );
    // Record the current workspace signals as a startup observation (audit; per-turn resolution
    // re-probes these live so groups can surface/un-surface as the workspace changes).
    let signals: Vec<String> = flux_runtime::detect_signals(&cwd)
        .iter()
        .filter_map(|o| {
            o.data
                .get("signal")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    // The audit record must show EVERY gating input: the workspace-probed signals AND the
    // session-ambient ones (D-115) the engine appends each turn — otherwise "why did this group
    // surface?" is unanswerable from startup evidence.
    executor.observe(flux_evidence::Observation::new(
        "project.signals",
        flux_evidence::Phase::Startup,
        serde_json::json!({ "signals": signals, "ambient": &ambient_signals }),
    ));

    // Assemble the engine from the resolved parts: this installs the authored-loop host, loads the
    // selected Flux-Lang outer loop (the turn loop is Flux-Lang, not Rust), registers the
    // config-authored model stages, and applies the per-turn token ceiling.
    let agent = assemble_engine(
        EngineParts {
            provider,
            executor,
            events,
            model,
            system_prompt,
            groups,
            ambient_signals,
            model_stages,
            max_iterations,
            skills,
            model_invoked_skills,
        },
        &cwd,
        &cfg,
        flags,
        system.as_ref(),
    )
    .await?;
    Ok((agent, session_id, canonical_spec, spawner))
}

/// D-178/D-179/D-183 resurrect-on-open, plus A-98 wake-up servicing: if this session was killed
/// mid-turn (a crash, OOM, or `kill -9` left a `TurnStarted` with no `TurnEnded`), finish that turn
/// from its crash point BEFORE the new input runs — the plan is durable source, so no model call is
/// made and no op with a recorded cassette cell re-fires. THEN fire any of this session's own
/// wake-ups (`schedule_wakeup`) whose `fire_at_ms` has already elapsed — the "fires on next open"
/// answer to "who services it" (see `docs/designs/agent-set-wakeup.md`): a plain `flux` session has
/// no live host watching a timer, so a due wake-up fires here instead, each as its own ordinary
/// turn, before the caller's new input runs. Always reported on stderr; never silent.
///
/// The two shared steps every turn-entry point runs (D-183: this one-shot `flux run`, the REPL,
/// and the TUI, plus the SDK's `send`/`send_with`/`stream`/`start_flow`, for resurrect; A-98's
/// wake-up step is wired into the plain `flux` CLI only so far — see the design doc's scope note)
/// — this is the CLI's thin reporter over both, coloring [`flux_flow::resurrect::OnOpenLine`] /
/// [`flux_flow::wakeup::WakeupLine`] the way the CLI's other status lines are colored.
///
/// Never fatal. A session with nothing to resurrect or fire is the overwhelmingly common case and
/// stays quiet, and a session that CAN'T be resurrected (a crash during planning, before any plan
/// was accepted — there is no durable plan to finish) must not stop the user from working: it is
/// reported and the new turn proceeds. `FLUX_AUTO_RESURRECT=0` turns the resurrect step off (the
/// wake-up step is independent and always runs).
pub(super) async fn resurrect_on_open(
    agent: &FlowEngine,
    session_id: &str,
    sink: &mut dyn AgentSink,
) {
    flux_flow::resurrect::resurrect_on_open(
        &agent.events,
        &agent.flow,
        &agent.executor,
        session_id,
        &agent.composites.active_for_session(session_id),
        sink,
        |line| match line {
            flux_flow::resurrect::OnOpenLine::Info(msg) => eprintln!("{}", style::dim(&msg)),
            flux_flow::resurrect::OnOpenLine::Error(msg) => eprintln!("{}", style::red(&msg)),
            // `OnOpenLine` is `#[non_exhaustive]`; a future variant still gets printed, just
            // without a color pairing yet.
            other => eprintln!("resurrect: {other:?}"),
        },
    )
    .await;
    flux_flow::wakeup::service_due_wakeups_on_open(agent, session_id, sink, |line| match line {
        flux_flow::wakeup::WakeupLine::Info(msg) => eprintln!("{}", style::dim(&msg)),
        flux_flow::wakeup::WakeupLine::Error(msg) => eprintln!("{}", style::red(&msg)),
        other => eprintln!("wakeup: {other:?}"),
    })
    .await;
}

/// One-shot agentic turn.
pub(super) async fn run_agentic(flags: &AgentFlags, prompt: String) -> Result<()> {
    let (agent, session_id, model_spec, _spawner) = build_agent(flags).await?;
    eprintln!(
        "{}",
        style::dim(&format!("{} · session {session_id}", agent.model))
    );
    let initial_rules = agent.executor.allow_rules();
    let pricing = flux_credentials::load_pricing_table();
    let mut sink = CliSink::new(agent.max_iterations).with_cost(model_spec, pricing);
    resurrect_on_open(&agent, &session_id, &mut sink).await;
    let outcome = agent.run_turn(&session_id, &prompt, &mut sink).await;
    // Persist "always allow" choices made DURING the turn even when the turn itself later fails —
    // the user answered the prompt either way, and losing the choice means re-prompting next run.
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    outcome.context("agent turn")?;
    Ok(())
}

/// A built-in offline provider (`-m mock`) that speaks the same adaptive native-tool protocol as a
/// live model: declare intent, capture one literal operation, finalize its action batch, then answer
/// from the guarded execution report. This keeps the offline gate on the product-default loop rather
/// than preserving a second mock-only agent-loop path.
#[derive(Default)]
pub(super) struct MockCliProvider {
    pub(super) calls: AtomicUsize,
}

#[async_trait]
impl Provider for MockCliProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: Request) -> flux_core::Result<ChunkStream> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);

        // Test hook: `FLUX_MOCK_HANG=1` streams one delta then never completes (only cancellation
        // can end the turn) — used to exercise Ctrl-C interruption in the REPL.
        if std::env::var("FLUX_MOCK_HANG").is_ok() {
            let s = futures::stream::once(async { Ok(Chunk::TextDelta("thinking…".into())) })
                .chain(futures::stream::pending::<flux_core::Result<Chunk>>());
            return Ok(Box::pin(s));
        }

        // Deterministic provider-failure hook for black-box CLI tests. Unlike a canned response,
        // this exercises the real adaptive loop's stage-error path and its terminal machine
        // outcome without requiring credentials or network access.
        if let Ok(message) = std::env::var("FLUX_MOCK_ERROR") {
            return Err(flux_core::Error::Provider(message));
        }

        // Test hook for direct model-backed cognition ops (not the adaptive outer loop): return a canned
        // text completion. L-79 uses this to exercise `ai.extract` input mapping through the real
        // binary without provider credentials or a network stub.
        if let Ok(text) = std::env::var("FLUX_MOCK_RESPONSE") {
            let chunks = vec![
                Chunk::TextDelta(text),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        let target = std::env::var("FLUX_MOCK_TOOL")
            .ok()
            .or_else(|| std::env::var("FLUX_MOCK_BASH").ok().map(|_| "bash".into()))
            .unwrap_or_else(|| "write".into());

        // Intent routing sees only the family index. Select the one whose stable index line names
        // the target operation; this works for grouped/plugin tools too without hard-coding their
        // family names into the mock.
        if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
            let family = req
                .system_segments
                .iter()
                .flat_map(|segment| segment.text.lines())
                .filter_map(|line| {
                    let line = line.strip_prefix("- ")?;
                    let (family, details) = line.split_once(" (")?;
                    let members = details.split_once("; ")?.1.split_once("):")?.0;
                    let examples = members
                        .strip_prefix("e.g. ")
                        .or_else(|| members.strip_prefix("operations "))?;
                    let contains_target = family == target
                        || examples
                            .split(',')
                            .any(|operation| operation.trim() == target);
                    contains_target.then(|| family.to_string())
                })
                .next()
                .into_iter()
                .collect::<Vec<_>>();
            let chunks = vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent1".into(),
                    name: "declare_intent".into(),
                    input: serde_json::json!({
                        "intent": "complete the offline mock turn",
                        "capability_families": family
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        if req.tools.iter().any(|tool| tool.name == "finalize_plan") {
            // Tool-result text is deliberately not part of `Message::text()`. Serialize the mock
            // ledger so this offline provider observes the same structured result blocks a real
            // wire codec sends back to the model.
            let transcript = serde_json::to_string(&req.messages).unwrap_or_default();

            // The finalize call's matching tool result carries the actual ExecutionReport. Only now
            // may the model claim completion.
            if transcript.contains("Execution report (actual guarded results)") {
                let chunks = vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "Finished.".into(),
                    }),
                    Chunk::Usage(Usage {
                        input_tokens: 180,
                        output_tokens: 12,
                        cache_read_input_tokens: 1_240,
                        ..Default::default()
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ];
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }

            // Once the operation was captured, freeze the host-built batch.
            if transcript.contains("captured as proposed action") {
                let chunks = vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "finalize1".into(),
                        name: "finalize_plan".into(),
                        input: serde_json::json!({
                            "instructions": "Report the actual guarded operation result."
                        }),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ];
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }

            // A gather-safe operation (for example `read`) executes immediately instead of being
            // captured into an action batch. Once its successful result is in the native ledger,
            // finish the offline turn; repeatedly proposing the same gather call only used to look
            // successful because stage-budget exhaustion was laundered into prose before C-226.
            let gathered = req
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .any(|block| {
                    matches!(
                        block,
                        ContentBlock::ToolResult {
                            tool_use_id,
                            is_error: false,
                            ..
                        } if tool_use_id == "action1"
                    )
                });
            if gathered {
                let chunks = vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "Finished.".into(),
                    }),
                    Chunk::Usage(Usage {
                        input_tokens: 180,
                        output_tokens: 12,
                        cache_read_input_tokens: 1_240,
                        ..Default::default()
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ];
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }

            let input = if target == "write" && std::env::var("FLUX_MOCK_TOOL").is_err() {
                serde_json::json!({
                    "path": "flux-mock.txt",
                    "content": "created by flux mock\n"
                })
            } else if target == "bash" {
                serde_json::json!({
                    "command": std::env::var("FLUX_MOCK_BASH").unwrap_or_default()
                })
            } else {
                std::env::var("FLUX_MOCK_TOOL_INPUT")
                    .ok()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_else(|| serde_json::json!({}))
            };
            let native = req
                .tools
                .iter()
                .find(|tool| {
                    tool.name == target || tool.description.contains(&format!("`{target}`"))
                })
                .map(|tool| tool.name.clone());
            if let Some(native) = native {
                let chunks = vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "action1".into(),
                        name: native,
                        input,
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ];
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }
        }

        // A target that cannot be surfaced ends honestly in prose instead of inventing an operation.
        if n > 0 {
            let chunks = vec![
                Chunk::Block(ContentBlock::Text {
                    text: format!("The mock target `{target}` is not available in this agent."),
                }),
                Chunk::Usage(Usage {
                    input_tokens: 180,
                    output_tokens: 12,
                    cache_read_input_tokens: 1_240,
                    ..Default::default()
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        unreachable!("the first mock provider call is always intent detection")
    }
}

#[cfg(test)]
mod execution_environment_conformance {
    use super::*;

    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use flux_app::{App, HostPermissionRules};
    use flux_lang::program::{JourneyDecl, PermissionDecl, Program, TriggerDecl};
    use flux_policy::{
        Action, AuthorizationPolicy, Caller, Grant, ResourceKind, ResourceRef, SubjectKind,
        SubjectRef, Trust, TrustLevel,
    };
    use flux_runtime::{AuthorityRequirement, Executor, Tool, ToolContext};
    use flux_secret::Redactor;
    use serde_json::{json, Value};

    const PROBE: &str = "c67_environment_probe";
    const PROBE_ACTION: &str = "c67.environment.probe";
    const MARKER: &str = "marker.txt";
    const SECRET: &str = "c67-cross-surface-secret";
    const CALLER: &str = "c67-cross-surface-caller";

    struct SurfaceProbe {
        roots: Arc<Mutex<Vec<PathBuf>>>,
    }

    #[async_trait]
    impl Tool for SurfaceProbe {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                PROBE,
                "read the C-67 workspace marker through the guarded system",
                json!({"type": "object", "additionalProperties": false}),
            )
            .with_access(vec![flux_spec::AccessKind::Filesystem])
        }

        fn permission_subjects(&self, _params: &Value) -> Vec<String> {
            vec![MARKER.to_string()]
        }

        fn authority_requirements(
            &self,
            _params: &Value,
            _subjects: &[String],
        ) -> flux_core::Result<Vec<AuthorityRequirement>> {
            Ok(expected_requirements())
        }

        async fn execute(
            &self,
            ctx: &ToolContext,
            _params: Value,
        ) -> flux_core::Result<ToolResult> {
            self.roots
                .lock()
                .expect("surface-probe roots lock")
                .push(ctx.system().workspace().root().to_path_buf());
            ctx.system().read_file(MARKER).await.map(ToolResult::ok)
        }
    }

    fn expected_requirements() -> Vec<AuthorityRequirement> {
        vec![
            AuthorityRequirement::workspace_read(MARKER),
            AuthorityRequirement::operation(PROBE_ACTION, PROBE),
        ]
    }

    fn requested_registry(probe: Arc<SurfaceProbe>) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry
            .try_register_from("C-67 requested parity tool", probe)
            .expect("register parity probe");
        registry
    }

    fn parity_authorization() -> (AuthorizationPolicy, Caller, Trust) {
        let (caller, mut trust) = flux_policy::local_identity(CALLER);
        trust.level = TrustLevel::System;
        let subject = SubjectRef {
            kind: SubjectKind::User,
            id: CALLER.to_string(),
        };
        let grant = |action: &str, resource: ResourceRef| Grant {
            subjects: vec![subject.clone()],
            resources: vec![resource],
            actions: vec![Action::from(action)],
            required_trust: TrustLevel::System,
            required_scopes: Vec::new(),
            requires_approval: false,
        };
        (
            AuthorizationPolicy {
                grants: vec![
                    grant("workspace.read", ResourceRef::path(MARKER)),
                    grant(
                        PROBE_ACTION,
                        ResourceRef::named(ResourceKind::Operation, PROBE),
                    ),
                ],
            },
            caller,
            trust,
        )
    }

    fn redactor() -> Redactor {
        let redactor = Redactor::new();
        redactor.add_secret(SECRET);
        redactor
    }

    fn assert_probe_contract(registry: &ToolRegistry, expected_spec: &Value, surface: &str) {
        let tool = registry
            .get(PROBE)
            .unwrap_or_else(|| panic!("{surface}: requested probe missing from registry"));
        assert_eq!(
            serde_json::to_value(tool.spec()).unwrap(),
            *expected_spec,
            "{surface}: probe catalog contract"
        );
        let params = json!({});
        let subjects = tool.permission_subjects(&params);
        assert_eq!(subjects, vec![MARKER.to_string()], "{surface}: subjects");
        assert_eq!(
            tool.authority_requirements(&params, &subjects).unwrap(),
            expected_requirements(),
            "{surface}: typed authority requirements"
        );
        assert_eq!(
            registry
                .names()
                .iter()
                .filter(|name| name.as_str() == PROBE)
                .count(),
            1,
            "{surface}: probe must be registered exactly once"
        );
    }

    fn assert_executor_identity(executor: &Executor, root: &Path, surface: &str) {
        assert_eq!(
            executor.context().system().workspace().root(),
            root,
            "{surface}: guarded root"
        );
        let context: Value =
            serde_json::from_str(&executor.approval_context()).expect("approval context JSON");
        assert_eq!(
            context["caller"]["principal"]["id"], CALLER,
            "{surface}: caller identity"
        );
        assert_eq!(
            context["trust"]["level"], "system",
            "{surface}: trust identity"
        );
        assert!(
            context["policy"].to_string().contains(PROBE_ACTION),
            "{surface}: custom policy missing from approval context: {context}"
        );
    }

    fn app_program(flow: flux_lang::ast::DraftAst) -> Program {
        Program {
            permissions: Some(PermissionDecl {
                allow: Some(vec![PROBE.to_string()]),
                deny: Vec::new(),
            }),
            triggers: vec![TriggerDecl {
                name: "c67_probe_trigger".into(),
                on: "c67_probe".into(),
                run: "c67_probe_journey".into(),
                agent: None,
            }],
            journeys: vec![JourneyDecl {
                name: "c67_probe_journey".into(),
                agent: None,
                flow,
            }],
            ..Program::default()
        }
    }

    /// C-67: exercise the actual CLI helper, App constructor, and both SDK builders. A narrow
    /// caller+trust policy and a custom typed requirement make a defaulted policy or identity fail;
    /// the marker read and secret-bearing result prove each surface retained the guarded root and
    /// shared redactor selected during assembly.
    #[tokio::test]
    async fn cli_app_and_sdk_builders_share_the_execution_environment_contract() {
        let base = std::env::temp_dir().join(format!(
            "flux-c67-surface-conformance-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(MARKER), format!("from-explicit-root {SECRET}")).unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();

        let roots = Arc::new(Mutex::new(Vec::new()));
        let probe = Arc::new(SurfaceProbe {
            roots: roots.clone(),
        });
        let expected_spec = serde_json::to_value(probe.spec()).unwrap();
        let (policy, caller, trust) = parity_authorization();

        let cli_system = Arc::new(System::new(Workspace::new(&root).unwrap()));
        let cli = assemble_cli_execution_environment(
            cli_system,
            requested_registry(probe.clone()),
            PermissionManager::from_rules(&[PROBE.to_string()], &[]),
            Arc::new(AllowApprover),
            ExecutionAuthorization::new(policy.clone(), caller.clone(), trust.clone()),
            redactor(),
            None,
            Vec::new(),
            flux_runtime::ResourceLimits::new(),
        )
        .into_executor();
        assert_probe_contract(cli.registry(), &expected_spec, "CLI");
        assert_executor_identity(&cli, &canonical_root, "CLI");
        let cli_result = cli.dispatch(PROBE, json!({})).await;
        assert!(!cli_result.is_error, "CLI: {}", cli_result.content);
        assert_eq!(cli_result.content, "from-explicit-root [redacted]");

        let app_environment = ExecutionEnvironment::new(
            Arc::new(System::new(Workspace::new(&root).unwrap())),
            requested_registry(probe.clone()),
            PermissionManager::from_rules(&[PROBE.to_string()], &[]),
            Arc::new(AllowApprover),
            ExecutionAuthorization::new(policy.clone(), caller.clone(), trust.clone()),
        )
        .with_redactor(redactor());
        let app = App::try_with_execution_environment(
            app_program(
                flux_lang::parse::parse(&format!("flow c67_probe\n  return {PROBE}({{}})"))
                    .expect("parse App probe flow"),
            ),
            None,
            "mock",
            app_environment,
            None,
            Arc::new(EventStore::in_memory().unwrap()),
            HostPermissionRules::default(),
            Vec::new(),
        )
        .expect("assemble App through explicit environment");
        assert_probe_contract(app.registry(), &expected_spec, "App");
        let app_runs = app.deliver("c67_probe", json!({})).await.unwrap();
        assert_eq!(app_runs.len(), 1);
        assert_eq!(app_runs[0].result, "from-explicit-root [redacted]");

        let sdk_client = flux_sdk::Client::builder()
            .model("mock")
            .tools([PROBE])
            .allow(PROBE)
            .auto_approve(true)
            .with_authorization(policy.clone(), caller.clone(), trust.clone())
            .with_redactor(redactor())
            .register_op_from("C-67 requested parity tool", probe.clone())
            .build(Box::<MockCliProvider>::default(), &root)
            .expect("build SDK Client");
        let sdk_executor = sdk_client.engine().executor.as_ref();
        assert_probe_contract(sdk_executor.registry(), &expected_spec, "SDK Client");
        assert_executor_identity(sdk_executor, &canonical_root, "SDK Client");
        let sdk_result = sdk_executor.dispatch(PROBE, json!({})).await;
        assert!(!sdk_result.is_error, "SDK Client: {}", sdk_result.content);
        assert_eq!(sdk_result.content, "from-explicit-root [redacted]");

        let mut flow_client = flux_sdk::FlowClient::builder()
            .model("mock")
            .allow(PROBE)
            .auto_approve(true)
            .with_authorization(policy, caller, trust)
            .with_redactor(redactor())
            .build(Arc::new(MockCliProvider::default()), &root)
            .expect("build SDK FlowClient");
        flow_client
            .try_register_op(probe)
            .expect("register FlowClient parity probe");
        assert_probe_contract(flow_client.registry(), &expected_spec, "SDK FlowClient");
        let flow_result = flow_client
            .execute(
                &flux_lang::parse::parse(&format!("flow c67_probe\n  return {PROBE}({{}})"))
                    .expect("parse SDK probe flow"),
            )
            .await
            .expect("execute SDK FlowClient probe");
        assert_eq!(flow_result.result, "from-explicit-root [redacted]");

        let observed_roots = roots.lock().unwrap().clone();
        assert_eq!(observed_roots.len(), 4, "one probe execution per surface");
        assert!(
            observed_roots
                .iter()
                .all(|observed| observed == &canonical_root),
            "all probes must use the explicit guarded root: {observed_roots:?}"
        );

        std::fs::remove_dir_all(base).ok();
    }
}

#[cfg(test)]
mod fleet_and_board_wiring {
    use super::*;

    use flux_lang::program::DatasourceDecl;
    use flux_system::Workspace;

    /// A throwaway workspace root. `build_datasources` needs a guarded [`System`] even for a
    /// declaration that touches no file, so every case here gets its own root.
    fn temp_root(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "flux-cli-a131-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(&path).unwrap()
    }

    fn decl(name: &str, kind: &str) -> Vec<DatasourceDecl> {
        vec![DatasourceDecl {
            name: name.into(),
            kind: kind.into(),
            path: None,
            settings: serde_json::Value::Null,
        }]
    }

    /// A-131 Acceptance 4, the named failing-first test: a Program declaring a board must resolve to
    /// a `WorkBoard`-backed set of generated ops. Before this story `build_datasources` knew only
    /// `markdown` and `openapi`, so a board declaration errored as an unknown kind and a board could
    /// not be named from configuration at all.
    #[tokio::test]
    async fn a_declared_board_binds_a_work_board_instead_of_erroring() {
        let root = temp_root("board-binds");
        let system = System::new(Workspace::new(&root).unwrap());

        let bound = build_datasources(&decl("board", "board:memory"), &root, &system).await;
        assert!(
            bound.is_ok(),
            "a `board:memory` datasource must bind a WorkBoard, got: {:?}",
            bound.as_ref().err().map(ToString::to_string)
        );
        let bound = bound.unwrap();

        // Not ingested as knowledge — the failure mode the board kind namespace exists to prevent.
        assert!(
            bound.knowledge.is_empty(),
            "a board declaration must not be ingested into the knowledge index"
        );

        // ...and it resolves to a `WorkBoard`-backed set of GENERATED ops, under the decl's name as
        // the domain. The expected set is read off the port's own registration rather than listed
        // here, so a board operation added to `WorkBoard` (A-130) is covered without editing this.
        let [(domain, board)] = <[_; 1]>::try_from(bound.boards).ok().expect("one board");
        assert_eq!(domain, "board");
        let mut registry = ToolRegistry::new();
        let surface = flux_capabilities::try_register_work_board(&mut registry, &domain, board)
            .expect("the board registers");
        assert!(
            !surface.group.tools.is_empty(),
            "the port generated no operations"
        );
        for op in &surface.group.tools {
            assert!(
                registry.get(op).is_some(),
                "the declared board did not generate `{op}`"
            );
        }
        // The write half is what distinguishes a board from a read-only knowledge source.
        assert!(
            registry.get("board.claim").is_some(),
            "a bound board must offer its mutating ops: {:?}",
            registry.names()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A-131 Acceptance 3: wiring the fleet must not widen A-116's egress posture. The registered
    /// ops still report the worker's **origin** as their permission subject — never `*` — and report
    /// **no** subject for an endpoint they cannot name, which forces approval instead of matching a
    /// broad grant. Asserted against the ops as the CLI actually registers them, not against
    /// freshly-constructed ones, so a registration that handed them a different posture fails here.
    #[test]
    fn registration_preserves_the_fleet_egress_posture() {
        let mut registry = ToolRegistry::new();
        try_register_fleet(&mut registry, None).expect("the fleet ops register");

        let dispatch = registry.get("fleet.dispatch").expect("fleet.dispatch");
        assert_eq!(
            dispatch.permission_subjects(&serde_json::json!({
                "worker": "https://worker-1.internal:8787/a2a",
                "task": "build",
            })),
            vec!["https://worker-1.internal:8787".to_string()],
        );
        for op in ["fleet.dispatch", "fleet.status", "fleet.cancel"] {
            let tool = registry.get(op).expect("a registered fleet op");
            let subjects = tool.permission_subjects(&serde_json::json!({ "worker": "not a url" }));
            assert!(
                subjects.is_empty(),
                "`{op}` must report no subject for an unnameable worker, got {subjects:?}"
            );
            assert!(!subjects.iter().any(|s| s == "*"), "`{op}` reported `*`");
        }

        // The default grant is the full SSRF guard: a private worker is refused until an operator
        // opts in. Registration must not hand the ops a blanket `Any`.
        assert_eq!(
            fleet_private_net(),
            flux_system::net::PrivateNetAllow::None,
            "the fleet ops must not be registered with a standing private-network grant"
        );
    }

    /// C-243 failing-first: **nothing starts a worker**. A-116 wired the dispatch half of the fleet
    /// — hand a task to a worker that is somehow already running — but the fleet had no verb that
    /// *makes* a worker exist, so a "wave" was a wave of one (`FlowEngine`'s `turn_gate` gives one
    /// worker one concurrent turn). The worker-lifecycle ops must be registered at the same single
    /// construction site as the dispatch ops, so the two halves cannot drift into different
    /// catalogs, and each must name a **worker** rather than a network origin as its subject: these
    /// ops spawn and signal a local OS process, they perform no egress at all.
    #[test]
    fn the_fleet_worker_lifecycle_ops_are_registered_and_named_by_worker() {
        let mut registry = ToolRegistry::new();
        try_register_fleet(&mut registry, None).expect("the fleet ops register");

        for op in ["fleet.start", "fleet.worker_status", "fleet.stop"] {
            let tool = registry
                .get(op)
                .unwrap_or_else(|| panic!("`{op}` is not registered — nothing can start a worker"));
            let spec = tool.spec();
            assert!(
                spec.access.contains(&flux_spec::AccessKind::Process),
                "`{op}` spawns/signals an OS process, so it must declare Process access: {:?}",
                spec.access
            );
            assert!(
                !spec.access.contains(&flux_spec::AccessKind::Network),
                "`{op}` performs no egress, so it must not demand network access: {:?}",
                spec.access
            );
        }

        let start = registry.get("fleet.start").expect("fleet.start");
        assert_eq!(
            start.permission_subjects(&serde_json::json!({ "item": "C-243" })),
            vec!["fleet-worker:C-243".to_string()],
            "a start must be grantable per worker, never blanket"
        );
    }

    /// A-131 Acceptance 5: the wrong board kind fails **loudly**. `markdown` stays the knowledge
    /// ingester (it is not silently promoted to a board), a bare `board` names no backend, and a
    /// board backend that does not exist yet is an error rather than a fall-through to knowledge
    /// ingestion — which is the silent, wrong behaviour this story closes.
    #[tokio::test]
    async fn an_unnameable_board_backend_is_a_loud_error() {
        let root = temp_root("board-loud");
        let system = System::new(Workspace::new(&root).unwrap());

        for kind in ["board", "board:", "board:nope"] {
            let bound = build_datasources(&decl("board", kind), &root, &system).await;
            assert!(
                bound.is_err(),
                "a board kind naming no backend (`{kind}`) must error, not fall through to \
                 knowledge ingestion"
            );
            let text = bound.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(
                text.contains("board"),
                "the error must name the board kind, got: {text}"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// `board:markdown` must bind the **durable** backend (A-114), not fall back to the in-process
    /// one. Failing-first before `build_work_board` gained its `markdown` arm: the declaration was an
    /// "unknown board backend" error, so no durable board could be named from a Program at all — and
    /// without one, the design's "restart, sweep, re-derive" claim is untestable, because
    /// `MemoryBoard`'s storage *is* the process it is supposed to survive.
    ///
    /// Durability is asserted the only way that actually proves it: write through the bound board,
    /// then read the bytes back off the filesystem.
    #[tokio::test]
    async fn a_markdown_board_binds_a_durable_backend_rooted_under_the_program() {
        let root = temp_root("markdown-board");
        let board_dir = root.join("board");
        std::fs::create_dir_all(&board_dir).unwrap();
        let system = System::new(Workspace::new(&root).unwrap());

        let mut decls = decl("board", "board:markdown");
        decls[0].path = Some("./board".into());
        let bound = build_datasources(&decls, &root, &system)
            .await
            .expect("`board:markdown` must bind a durable board");
        assert_eq!(bound.boards.len(), 1, "one declared board binds one board");

        let (domain, board) = &bound.boards[0];
        assert_eq!(domain, "board");

        // The board is program-relative and rooted under the declared `path`, so a write lands
        // beside the .flux file rather than wherever the process happened to be launched.
        let ctx =
            flux_runtime::ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())));
        let item = board
            .create(
                &ctx,
                flux_datasource::board::ItemDraft {
                    title: "durable across a restart".into(),
                    ..Default::default()
                },
            )
            .await
            .expect("the board accepts a create");

        let on_disk = std::fs::read_dir(board_dir.join("items"))
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        assert!(
            on_disk > 0,
            "a durable board must leave the item on disk under the program-relative root; \
             `board:memory` would leave nothing and the recovery story would be unprovable"
        );
        assert!(!item.id.is_empty(), "the created item has an id");

        std::fs::remove_dir_all(&root).ok();
    }

    /// The ledger must actually reach `fleet.dispatch`, because that is what makes design §5's "the
    /// board IS the run registry" true. Failing-first before this story wired one:
    /// `try_register_fleet` took no ledger, so the op was always built with `None` and *refused* any
    /// call naming an `item` — "refusing to dispatch work nothing could sweep". The write path
    /// existed and production could not take it.
    ///
    /// Asserted on the declared surface rather than on registration succeeding, because a
    /// registration-only check passes with no ledger attached at all.
    #[test]
    fn a_wired_ledger_makes_fleet_dispatch_declare_the_write_it_performs() {
        fn dispatch_spec(registry: &ToolRegistry) -> flux_spec::ToolSpec {
            registry
                .get("fleet.dispatch")
                .expect("fleet.dispatch is registered")
                .spec()
        }

        let mut without = ToolRegistry::new();
        try_register_fleet(&mut without, None).expect("registers with no ledger");
        assert!(
            !dispatch_spec(&without)
                .effects
                .contains(&flux_spec::Effect::Write),
            "with no ledger the op records nothing, so it must not claim a write"
        );

        let board: Arc<dyn flux_capabilities::WorkBoard> =
            Arc::new(flux_capabilities::MemoryBoard::new());
        let ledger: Arc<dyn flux_runtime::DispatchLedger> =
            Arc::new(flux_capabilities::BoardLedger::new("board", board));
        let mut with = ToolRegistry::new();
        try_register_fleet(&mut with, Some(ledger)).expect("registers with a ledger");
        assert!(
            dispatch_spec(&with)
                .effects
                .contains(&flux_spec::Effect::Write),
            "a wired ledger means the op genuinely writes the board, so it must declare it — \
             otherwise the write escapes the gate that reads the declaration"
        );

        // And the item it would write is named concretely, so a grant scoped to one item cannot be
        // widened into another. Never `*`, never empty.
        let subjects = with
            .get("fleet.dispatch")
            .expect("registered")
            .permission_subjects(&serde_json::json!({
                "worker": "https://worker-1.internal:8787",
                "task": "do the thing",
                "item": "item-0001",
            }));
        assert!(
            subjects.iter().any(|s| s == "board/item/item-0001"),
            "the board item must be a concrete subject, got: {subjects:?}"
        );
        assert!(
            !subjects.iter().any(|s| s == "*" || s.is_empty()),
            "no subject may be a wildcard or empty, got: {subjects:?}"
        );
    }
}

#[cfg(test)]
mod cli_resource_ceiling_wiring {
    //! C-299: a configured `[limits]` table must bind for the **shipped binary**, not only for an
    //! embedding host that calls `ResourceLimits::from_config` itself.
    //!
    //! C-290 built the ceiling and enforced it in `Executor::dispatch`, but `flux-cli` was fenced
    //! for that story, so `[limits] max_concurrent_tool_calls` did nothing for anyone running
    //! `flux`. The assertion here is deliberately on **observed occupancy** — how many ops are
    //! inside `Tool::execute` at once — rather than on what the assembled executor reports it was
    //! configured with: a wiring story is only done when the wire carries current.

    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use flux_runtime::{Tool, ToolContext, ToolResult};
    use serde_json::{json, Value};

    const BLOCKER: &str = "c299_blocker";

    /// Live and peak occupancy of `Tool::execute`, sampled by [`Blocker`] itself.
    #[derive(Default)]
    struct Meter {
        in_flight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Meter {
        fn enter(&self) {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
        }

        fn leave(&self) {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }

        fn in_flight(&self) -> usize {
            self.in_flight.load(Ordering::SeqCst)
        }

        fn peak(&self) -> usize {
            self.peak.load(Ordering::SeqCst)
        }
    }

    /// A read-only op that parks inside `execute` until the test hands out a release permit, so
    /// "in flight" means "inside `Tool::execute`" and not "recently returned".
    struct Blocker {
        meter: Arc<Meter>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait]
    impl Tool for Blocker {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                BLOCKER,
                "blocks until the test releases it",
                json!({ "type": "object", "additionalProperties": false }),
            )
        }

        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: Value,
        ) -> flux_core::Result<ToolResult> {
            self.meter.enter();
            // `forget` so the permit is consumed: each `add_permits` releases exactly one execution.
            self.release
                .acquire()
                .await
                .expect("release gate closed")
                .forget();
            self.meter.leave();
            Ok(ToolResult::ok("released"))
        }
    }

    /// **The C-299 CLI acceptance test.** A `[limits] max_concurrent_tool_calls = 1` in the config
    /// file bounds the executor the CLI assembles: three concurrent dispatches, one execution in
    /// flight. Before this story the same config produced three, because `flux-cli` never read the
    /// table at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_configured_limits_table_binds_for_the_cli_executor() {
        let cfg: flux_config::Config = toml::from_str(
            "[limits]\nmax_concurrent_tool_calls = 1\ntool_call_queue_timeout_ms = 30000\n",
        )
        .expect("the `[limits]` concurrency keys must parse");

        let root = std::env::temp_dir().join(format!(
            "flux-c299-cli-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let meter = Arc::new(Meter::default());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(Blocker {
            meter: meter.clone(),
            release: release.clone(),
        }));

        let executor = Arc::new(
            assemble_cli_execution_environment(
                Arc::new(System::new(Workspace::new(&root).unwrap())),
                registry,
                PermissionManager::from_rules(&[BLOCKER.to_string()], &[]),
                Arc::new(AllowApprover),
                ExecutionAuthorization::local(),
                flux_secret::Redactor::new(),
                None,
                Vec::new(),
                // The one C-299 seam: the CLI turns `[limits]` into runtime ceilings here.
                cli_resource_limits(&cfg),
            )
            .into_executor(),
        );

        let handles: Vec<_> = (0..3)
            .map(|_| {
                let executor = executor.clone();
                tokio::spawn(async move { executor.dispatch(BLOCKER, json!({})).await })
            })
            .collect();
        // Long enough for all three to have reached the envelope; only one may be inside `execute`.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            meter.in_flight(),
            1,
            "`[limits] max_concurrent_tool_calls = 1` did not bind for the CLI executor: \
             {} tool calls were in flight at once",
            meter.in_flight()
        );

        release.add_permits(3);
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(!result.is_error, "dispatch failed: {}", result.content);
        }
        assert_eq!(
            meter.peak(),
            1,
            "peak occupancy must equal the configured ceiling"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
