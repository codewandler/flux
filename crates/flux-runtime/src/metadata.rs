//! Guarded startup-time project metadata assembly.
//!
//! Parsing stays in the L0 config/skill crates. This L2 module owns provenance: repository paths
//! are read through a workspace confined to the exact project root, while explicitly selected
//! user-global roots get their own confined [`System`] boundary.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use flux_core::{Error, Result};
use flux_system::{System, Workspace};

const PROJECT_CONFIG: &str = ".flux/config.toml";
const PROJECT_GROUPS: &str = ".flux/groups.toml";

/// Build the deliberately non-widened system used for automatic repository metadata.
pub fn project_system(cwd: &Path) -> Result<System> {
    Ok(System::new(Workspace::new(cwd)?))
}

fn trusted_root(path: &Path) -> Result<Option<System>> {
    Ok(Workspace::new_optional(path)?.map(System::new))
}

fn trusted_flux_root() -> Result<Option<(PathBuf, System)>> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(None);
    };
    let root = home.join(".flux");
    Ok(trusted_root(&root)?.map(|system| (root, system)))
}

/// Load trusted user defaults and guarded project configuration, preserving the established
/// project-over-user merge order. Missing files are harmless; guard and parse failures are loud.
pub fn load_config(cwd: &Path) -> Result<flux_config::Config> {
    let project = project_system(cwd)?;
    let project_text = project.read_optional_text(PROJECT_CONFIG)?;
    let trusted = trusted_flux_root()?;
    let user_text = trusted
        .as_ref()
        .map(|(_, system)| system.read_optional_text("config.toml"))
        .transpose()?
        .flatten();

    flux_config::from_sources(
        user_text
            .as_deref()
            .map(|text| ("~/.flux/config.toml", text)),
        project_text.as_deref().map(|text| (PROJECT_CONFIG, text)),
    )
}

/// Atomically persist project allow rules through the same repository-only path identity used for
/// reads. Existing unrelated settings are round-tripped by `flux-config`'s pure serializer.
pub fn persist_allow_rules(cwd: &Path, rules: &[String]) -> Result<()> {
    let project = project_system(cwd)?;
    let current = project.read_optional_text(PROJECT_CONFIG)?;
    let body = flux_config::render_allow_rules(
        current.as_deref().map(|text| (PROJECT_CONFIG, text)),
        rules,
    )?;
    project.write_file_atomic(PROJECT_CONFIG, &body)
}

/// Atomically persist the user's TUI theme choice to `~/.flux/config.toml` (a personal
/// preference, so user-level rather than project-level), round-tripping every other setting via
/// `flux-config`'s pure serializer. Creates `~/.flux` on first use.
pub fn persist_user_theme(theme: &str) -> Result<()> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Err(Error::Config("HOME is not set".into()));
    };
    let root = home.join(".flux");
    if !root.exists() {
        std::fs::create_dir_all(&root)
            .map_err(|error| Error::Config(format!("create {}: {error}", root.display())))?;
    }
    let Some((_, system)) = trusted_flux_root()? else {
        return Err(Error::Config("~/.flux is not usable".into()));
    };
    let current = system.read_optional_text("config.toml")?;
    let body = flux_config::render_theme(
        current.as_deref().map(|text| ("~/.flux/config.toml", text)),
        theme,
    )?;
    system.write_file_atomic("config.toml", &body)
}

/// Load guarded project and trusted user-global group manifests. Callers decide how to present a
/// malformed optional manifest, but an escaping path is never converted to absence here.
pub fn load_groups(cwd: &Path) -> Result<Vec<flux_evidence::ToolGroup>> {
    let project = project_system(cwd)?;
    let project_text = project.read_optional_text(PROJECT_GROUPS)?;
    let trusted = trusted_flux_root()?;
    let user_text = trusted
        .as_ref()
        .map(|(_, system)| system.read_optional_text("groups.toml"))
        .transpose()?
        .flatten();
    flux_config::groups_from_sources(
        user_text
            .as_deref()
            .map(|text| ("~/.flux/groups.toml", text)),
        project_text.as_deref().map(|text| (PROJECT_GROUPS, text)),
    )
}

/// Parse one skill file and collect its D-189 load-time warnings: recognized-but-unsupported
/// frontmatter fields, unmappable `allowed-tools` entries (both from `flux_skill::parse_checked`),
/// and `flux_skill::validate` naming/description issues (wired into discovery here — it was
/// previously dead code). Directory skills (`<dir>/SKILL.md`) are expected to name themselves
/// after their directory; flat `name.md` files have no such expectation, so `validate` only checks
/// the directory match for the former.
fn parse_skill(path: PathBuf, text: &str, warnings: &mut Vec<String>) -> flux_skill::Skill {
    let (mut skill, field_warnings) = flux_skill::parse_checked(text, Some(path.clone()));
    warnings.extend(field_warnings);
    let is_skill_md = path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md");
    if skill.name == "SKILL" {
        if let Some(name) = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
        {
            skill.name = name;
        }
    }
    let expected_dir = if is_skill_md {
        path.parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
    } else {
        None
    };
    for issue in flux_skill::validate(&skill, expected_dir.as_deref()) {
        warnings.push(format!("{}: {issue}", path.display()));
    }
    skill
}

fn extend_skills(
    out: &mut Vec<flux_skill::Skill>,
    seen: &mut HashSet<String>,
    files: Vec<(PathBuf, String)>,
    warnings: &mut Vec<String>,
) {
    for (path, text) in files {
        let skill = parse_skill(path, &text, warnings);
        if seen.insert(skill.name.clone()) {
            out.push(skill);
        } else {
            // First-wins dedup (established precedence order across roots/dirs) — a namespaced
            // duplicate (e.g. `a/foo` and `b/foo` both naming `foo`) silently shadowing the earlier
            // one is a common trap with nested trees, so make the loser loud instead of silent.
            tracing::warn!(
                skill = %skill.name,
                path = ?skill.source,
                "duplicate skill name; keeping the first-discovered definition and ignoring this one"
            );
            warnings.push(format!(
                "duplicate skill `{}` (already defined; keeping the first-discovered definition, ignoring `{}`)",
                skill.name,
                skill
                    .source
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
        }
    }
}

fn guarded_skill_files(
    system: &System,
    display_root: &Path,
    dir: &str,
) -> Result<Vec<(PathBuf, String)>> {
    system
        .read_dir_text_files_with_nested(dir, "md", "SKILL.md")
        .map(|files| {
            files
                .into_iter()
                .map(|(path, text)| (display_root.join(path), text))
                .collect()
        })
}

fn trusted_skill_files(dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let Some(mut workspace) = Workspace::new_optional(dir)? else {
        return Ok(Vec::new());
    };
    // The operator selected this root (well-known home directory or explicit absolute path), so
    // links beneath it belong to the trusted control plane rather than the repository jail.
    workspace.set_unconfined(true);
    let system = System::new(workspace);
    guarded_skill_files(&system, dir, ".")
}

/// Provenance-tagged custom skill root. Repository-controlled values stay inside the project
/// workspace even when they spell an absolute path; only an operator-controlled source may select
/// a trusted host root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRoot {
    Project(PathBuf),
    Trusted(PathBuf),
}

/// Convert layered config roots without losing their trust provenance.
pub fn configured_skill_roots(config: &flux_config::Config) -> Vec<SkillRoot> {
    config
        .skill_dirs_with_origin()
        .into_iter()
        .map(|(path, origin)| match origin {
            flux_config::SkillDirOrigin::User if path.is_absolute() => SkillRoot::Trusted(path),
            flux_config::SkillDirOrigin::User | flux_config::SkillDirOrigin::Project => {
                SkillRoot::Project(path)
            }
        })
        .collect()
}

/// The result of a skill discovery pass: the precedence-deduped skills plus any load-time warnings
/// (D-189) — a recognized-but-unsupported frontmatter field, an `allowed-tools` entry with no flux
/// op equivalent, a `flux_skill::validate` naming/description issue, or a duplicate name lost to a
/// first-discovered definition. Mirrors [`CommandDiscovery`]: warnings are data, not direct stderr
/// output — the caller decides how and whether to render them.
#[derive(Debug, Clone, Default)]
pub struct SkillDiscovery {
    pub skills: Vec<flux_skill::Skill>,
    pub warnings: Vec<String>,
}

/// Discover skills in explicit precedence order without giving L0 parsing code project filesystem
/// authority. `extra` roots come first; relative roots are repository-controlled and guarded,
/// absolute roots are explicit trusted control-plane selections. Then project defaults precede
/// user-global defaults. Skill activation remains the caller's explicit allowlist decision.
///
/// This convenience wrapper discards [`SkillDiscovery::warnings`] — callers that want them (the
/// CLI's `load_skills`) should call [`discover_skills_from`] directly.
pub fn discover_skills(cwd: &Path, extra: &[PathBuf]) -> Result<Vec<flux_skill::Skill>> {
    Ok(discover_skills_from(
        cwd,
        &extra
            .iter()
            .cloned()
            .map(SkillRoot::Project)
            .collect::<Vec<_>>(),
    )?
    .skills)
}

/// Provenance-aware skill discovery used when layered config and explicit CLI paths are combined.
///
/// D-192: the five well-known directories below (project `.flux/skills`, project `.claude/skills`,
/// then user-global `~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`, in that precedence
/// order) are the **single** definition of flux's default skill-directory set — `flux-skill` no
/// longer carries a duplicate of this list; it owns parsing only.
pub fn discover_skills_from(cwd: &Path, extra: &[SkillRoot]) -> Result<SkillDiscovery> {
    let project = project_system(cwd)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut warnings = Vec::new();

    for root in extra {
        let files = match root {
            SkillRoot::Trusted(dir) => trusted_skill_files(dir)?,
            SkillRoot::Project(dir) => {
                let dir = dir.to_str().ok_or_else(|| {
                    Error::Config(format!(
                        "project skill directory {dir:?} is not valid UTF-8"
                    ))
                })?;
                guarded_skill_files(&project, cwd, dir)?
            }
        };
        extend_skills(&mut out, &mut seen, files, &mut warnings);
    }

    for dir in [".flux/skills", ".claude/skills"] {
        let files = guarded_skill_files(&project, cwd, dir)?;
        extend_skills(&mut out, &mut seen, files, &mut warnings);
    }

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for dir in [
            home.join(".flux/skills"),
            home.join(".agents/skills"),
            home.join(".claude/skills"),
        ] {
            let files = trusted_skill_files(&dir)?;
            extend_skills(&mut out, &mut seen, files, &mut warnings);
        }
    }

    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(SkillDiscovery {
        skills: out,
        warnings,
    })
}

/// A discovered Markdown slash-command file (D-186): project `.flux/commands` + `.claude/commands`
/// and their user-global `~` twins, with the same first-wins precedence and symlink jail as skill
/// discovery. `name` is always the file stem — frontmatter carries only display metadata, never
/// identity, so two commands can never disagree with their own filename about who they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFile {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    /// The body after the frontmatter block, trimmed. Claude's `!`-prefixed inline-bash and
    /// `@file` body syntax are NOT interpreted — they pass through as literal text (out of scope
    /// for D-186; see `website/docs/agent/claude-compat.md`).
    pub body: String,
    pub source: PathBuf,
    /// D-187: explicit frontmatter opt-in (`agent-triggerable: true`, default `false`) that lets
    /// the agent invoke this command itself via `command.invoke`, distinct from human REPL/TUI
    /// dispatch which is unaffected by this flag. Parsed silently — no lint warning either way.
    pub agent_triggerable: bool,
}

/// The result of a command discovery pass: the precedence-deduped commands plus any load-time
/// warnings (an unrecognized frontmatter field). Warnings are data rather than direct stderr
/// output — human surfaces (REPL/TUI) decide how and whether to render them.
#[derive(Debug, Clone, Default)]
pub struct CommandDiscovery {
    pub commands: Vec<CommandFile>,
    pub warnings: Vec<String>,
}

/// Frontmatter recognized on a command file. Anything else collected into `rest` is reported (not
/// silently dropped) via `CommandDiscovery::warnings` — a trivial shared warn hook, not the full
/// frontmatter lint system D-189 owns.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CommandFrontmatter {
    description: String,
    #[serde(rename = "argument-hint")]
    argument_hint: String,
    /// D-187: explicit agent-invocation opt-in, default `false` (human-only). A known, honored
    /// field — never reported through `rest`/`warnings`.
    #[serde(rename = "agent-triggerable")]
    agent_triggerable: bool,
    #[serde(flatten)]
    rest: BTreeMap<String, serde_norway::Value>,
}

fn parse_command(path: PathBuf, text: &str, warnings: &mut Vec<String>) -> CommandFile {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Lenient like skill parsing: malformed frontmatter degrades to an empty-metadata command
    // rather than failing discovery over one bad file.
    let doc = flux_markdown::parse_frontmatter::<CommandFrontmatter>(text).unwrap_or_else(|_| {
        flux_markdown::Document {
            meta: CommandFrontmatter::default(),
            body: text.to_string(),
        }
    });
    if !doc.meta.rest.is_empty() {
        let mut fields: Vec<&str> = doc.meta.rest.keys().map(String::as_str).collect();
        fields.sort_unstable();
        warnings.push(format!(
            "{}: ignoring unrecognized command frontmatter field(s): {}",
            path.display(),
            fields.join(", ")
        ));
    }
    CommandFile {
        name,
        description: doc.meta.description,
        argument_hint: doc.meta.argument_hint,
        body: doc.body.trim().to_string(),
        source: path,
        agent_triggerable: doc.meta.agent_triggerable,
    }
}

fn extend_commands(
    out: &mut Vec<CommandFile>,
    seen: &mut HashSet<String>,
    files: Vec<(PathBuf, String)>,
    warnings: &mut Vec<String>,
) {
    for (path, text) in files {
        let command = parse_command(path, &text, warnings);
        if seen.insert(command.name.clone()) {
            out.push(command);
        }
    }
}

fn guarded_command_files(
    system: &System,
    display_root: &Path,
    dir: &str,
) -> Result<Vec<(PathBuf, String)>> {
    // Commands are flat (name = file stem) — the plain reader is enough, unlike skills' nested
    // `<name>/SKILL.md` shape.
    system.read_dir_text_files(dir, "md").map(|files| {
        files
            .into_iter()
            .map(|(path, text)| (display_root.join(path), text))
            .collect()
    })
}

fn trusted_command_files(dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let Some(mut workspace) = Workspace::new_optional(dir)? else {
        return Ok(Vec::new());
    };
    // Operator-selected root (well-known home directory): trusted control plane, not the
    // repository jail — mirrors `trusted_skill_files`.
    workspace.set_unconfined(true);
    let system = System::new(workspace);
    guarded_command_files(&system, dir, ".")
}

/// Discover command files in the fixed precedence order: project `.flux/commands` then
/// `.claude/commands` (symlink-jailed to `cwd`), then user-global `~/.flux/commands` then
/// `~/.claude/commands` (the operator's own trusted control plane). First-wins by name across all
/// four roots. Dispatch-time built-in shadowing is the caller's decision (built-ins live in the
/// REPL/TUI surfaces, not here).
pub fn discover_commands(cwd: &Path) -> Result<CommandDiscovery> {
    let project = project_system(cwd)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut warnings = Vec::new();

    for dir in [".flux/commands", ".claude/commands"] {
        let files = guarded_command_files(&project, cwd, dir)?;
        extend_commands(&mut out, &mut seen, files, &mut warnings);
    }

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for dir in [home.join(".flux/commands"), home.join(".claude/commands")] {
            let files = trusted_command_files(&dir)?;
            extend_commands(&mut out, &mut seen, files, &mut warnings);
        }
    }

    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(CommandDiscovery {
        commands: out,
        warnings,
    })
}

/// Substitute Claude-compatible `$ARGUMENTS` (the full trimmed tail) and `$1`..`$9` (whitespace-
/// split positional arguments; a missing positional substitutes empty) into a command body. One
/// left-to-right pass over `body`: a substituted value is never rescanned, so an argument that
/// happens to contain `$1` is not recursively expanded. Claude's `!`-prefixed inline-bash and
/// `@file` body syntax are NOT interpreted here — they pass through untouched as literal text.
pub fn expand_command_arguments(body: &str, raw_args: &str) -> String {
    let positional: Vec<&str> = raw_args.split_whitespace().collect();
    let trimmed_args = raw_args.trim();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if let Some(after) = rest.strip_prefix("$ARGUMENTS") {
            out.push_str(trimmed_args);
            i = body.len() - after.len();
            continue;
        }
        if let Some(after_dollar) = rest.strip_prefix('$') {
            if let Some(digit) = after_dollar.chars().next().filter(|c| c.is_ascii_digit()) {
                if digit != '0' {
                    let idx = digit.to_digit(10).unwrap() as usize;
                    out.push_str(positional.get(idx - 1).copied().unwrap_or(""));
                    i += 1 + digit.len_utf8();
                    continue;
                }
            }
        }
        let ch = rest.chars().next().expect("i < body.len()");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Serializes tests (in this module AND `flux_runtime::tests`) that repoint `HOME` — the process
/// env is shared across parallel test threads (mirrors `flux_config`'s `HOME_LOCK`).
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// Minimal `tracing::Subscriber` that records event messages, just enough to assert
    /// `extend_skills` warns on a namespaced duplicate without pulling in `tracing-subscriber`.
    struct RecordingSubscriber {
        messages: Arc<Mutex<Vec<String>>>,
    }

    struct MessageVisitor<'a>(&'a mut String);

    impl tracing::field::Visit for MessageVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                *self.0 = format!("{value:?}");
            }
        }
    }

    impl tracing::Subscriber for RecordingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut message = String::new();
            event.record(&mut MessageVisitor(&mut message));
            self.messages.lock().unwrap().push(message);
        }
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "flux-metadata-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn project_config_and_skills_reject_external_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("root");
        let outside = temp_dir("outside");
        std::fs::create_dir_all(root.join(".flux/skills")).unwrap();
        std::fs::write(outside.join("secret"), "OUTSIDE SECRET").unwrap();

        symlink(outside.join("secret"), root.join(".flux/config.toml")).unwrap();
        assert!(load_config(&root).is_err());
        std::fs::remove_file(root.join(".flux/config.toml")).unwrap();

        symlink(outside.join("secret"), root.join(".flux/skills/escaped.md")).unwrap();
        let error = discover_skills(&root, &[]).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn nested_namespaced_skill_symlink_escape_is_rejected() {
        // Extends the confinement coverage above one level deeper: a namespaced tree
        // (`.claude/skills/<ns>/<name>`) escaping the project root via a symlink two levels down
        // must be rejected the same as a top-level escape.
        use std::os::unix::fs::symlink;

        let root = temp_dir("nested-root");
        let outside = temp_dir("nested-outside");
        std::fs::create_dir_all(root.join(".claude/skills/ns")).unwrap();
        std::fs::write(outside.join("SKILL.md"), "---\nname: escaped\n---\nOUTSIDE").unwrap();

        symlink(&outside, root.join(".claude/skills/ns/escaped")).unwrap();
        let error = discover_skills(&root, &[]).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn a_skill_directory_with_references_is_not_double_discovered() {
        // A skill's own `references/` subdirectory (with its own `.md` files) must not surface as
        // a second skill — the parent already claimed the subtree by containing SKILL.md. Repoint
        // HOME to an empty dir so the host's real `~/.claude/skills` (if any) can't leak in.
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("refs-root");
        let home = temp_dir("refs-home");
        std::fs::create_dir_all(root.join(".claude/skills/foo/references")).unwrap();
        std::fs::write(
            root.join(".claude/skills/foo/SKILL.md"),
            "---\nname: foo\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/skills/foo/references/notes.md"),
            "---\nname: notes\n---\ninternal notes, not a skill",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let skills = discover_skills(&root, &[]).unwrap();
        std::env::remove_var("HOME");

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["foo"],
            "references/ must not surface as its own skill"
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn namespaced_duplicate_skill_names_dedup_first_wins_and_warn() {
        // Two namespaced trees defining the same skill name (`a/foo` vs `b/foo`): first-wins
        // precedence keeps the earlier one and the loser is loud (a warning), not silent. Repoint
        // HOME to an empty dir so the host's real user-global skills can't collide with `foo`.
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("dup-root");
        let home = temp_dir("dup-home");
        std::fs::create_dir_all(root.join(".claude/skills/a/foo")).unwrap();
        std::fs::create_dir_all(root.join(".claude/skills/b/foo")).unwrap();
        std::fs::write(
            root.join(".claude/skills/a/foo/SKILL.md"),
            "---\nname: foo\n---\nfrom a",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/skills/b/foo/SKILL.md"),
            "---\nname: foo\n---\nfrom b",
        )
        .unwrap();

        let messages = Arc::new(Mutex::new(Vec::new()));
        let subscriber = RecordingSubscriber {
            messages: messages.clone(),
        };
        std::env::set_var("HOME", &home);
        let skills =
            tracing::subscriber::with_default(subscriber, || discover_skills(&root, &[]).unwrap());
        std::env::remove_var("HOME");

        let foo: Vec<_> = skills.iter().filter(|s| s.name == "foo").collect();
        assert_eq!(
            foo.len(),
            1,
            "namespaced duplicate must dedup to a single entry"
        );
        assert_eq!(foo[0].body, "from a", "first-discovered definition wins");
        let captured = messages.lock().unwrap();
        assert!(
            captured.iter().any(|m| m.contains("duplicate skill")),
            "expected a warning about the shadowed duplicate, got: {captured:?}"
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    /// D-189: `validate()` (previously dead code) is now wired into discovery as a warn-level lint
    /// — a skill with an invalid name reports it through `SkillDiscovery::warnings`, and discovery
    /// still succeeds (non-fatal).
    #[test]
    fn discovery_lints_an_invalid_skill_name_but_still_loads_it() {
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("lint-name-root");
        let home = temp_dir("lint-name-home");
        std::fs::create_dir_all(root.join(".flux/skills")).unwrap();
        std::fs::write(
            root.join(".flux/skills/bad.md"),
            "---\nname: Bad--Name\ndescription: x\n---\nbody",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let discovery =
            discover_skills_from(&root, &[]).expect("discovery succeeds despite a lint issue");
        std::env::remove_var("HOME");

        assert!(
            discovery.skills.iter().any(|s| s.name == "Bad--Name"),
            "an invalid name is a lint warning, not a load failure"
        );
        assert!(
            discovery
                .warnings
                .iter()
                .any(|w| w.contains("Bad--Name") || w.contains("hyphen")),
            "expected a validate() warning: {:?}",
            discovery.warnings
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    /// D-189: a recognized-but-unsupported frontmatter field surfaces through discovery's
    /// `warnings`, not silently — the field the CLI's `load_skills` will print at load time.
    #[test]
    fn discovery_warns_on_unsupported_frontmatter_field() {
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("lint-field-root");
        let home = temp_dir("lint-field-home");
        std::fs::create_dir_all(root.join(".flux/skills")).unwrap();
        std::fs::write(
            root.join(".flux/skills/x.md"),
            "---\nname: x\ndescription: d\nlicense: MIT\n---\nbody",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let discovery = discover_skills_from(&root, &[]).unwrap();
        std::env::remove_var("HOME");

        assert!(
            discovery
                .warnings
                .iter()
                .any(|w| w.contains("x") && w.contains("license")),
            "{:?}",
            discovery.warnings
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    /// D-189: `allowed-tools` on a discovered skill translates to flux op names, and an unmappable
    /// entry surfaces as a discovery warning instead of silently vanishing.
    #[test]
    fn discovery_translates_allowed_tools_and_warns_on_unmappable() {
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("lint-tools-root");
        let home = temp_dir("lint-tools-home");
        std::fs::create_dir_all(root.join(".flux/skills")).unwrap();
        std::fs::write(
            root.join(".flux/skills/reviewer.md"),
            "---\nname: reviewer\ndescription: d\nallowed-tools: Bash, Read, NotARealTool\n---\nbody",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let discovery = discover_skills_from(&root, &[]).unwrap();
        std::env::remove_var("HOME");

        let skill = discovery
            .skills
            .iter()
            .find(|s| s.name == "reviewer")
            .unwrap();
        assert_eq!(
            skill.allowed_ops,
            vec!["bash".to_string(), "read".to_string()]
        );
        assert!(
            discovery
                .warnings
                .iter()
                .any(|w| w.contains("NotARealTool")),
            "{:?}",
            discovery.warnings
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    /// D-192: the five-directory skill precedence (project `.flux/skills` > project
    /// `.claude/skills` > user `~/.flux/skills` > user `~/.agents/skills` > user
    /// `~/.claude/skills`) lives exactly once, in [`discover_skills_from`]. Ported from
    /// `flux-skill`'s retired `discover_merged_precedence_project_wins` /
    /// `default_dirs_include_project_claude_after_project_flux` legacy-path tests, mirroring
    /// [`discover_commands_across_four_dirs_with_precedence_and_dedup`] for skills.
    #[test]
    fn discover_skills_across_five_dirs_with_precedence_and_dedup() {
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("skill-prec-root");
        let home = temp_dir("skill-prec-home");

        std::fs::create_dir_all(root.join(".flux/skills")).unwrap();
        std::fs::create_dir_all(root.join(".claude/skills")).unwrap();
        std::fs::write(
            root.join(".flux/skills/shared.md"),
            "---\nname: shared\ndescription: d\n---\nfrom project flux",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/skills/shared.md"),
            "---\nname: shared\ndescription: d\n---\nfrom project claude",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/skills/claude-only.md"),
            "---\nname: claude-only\ndescription: d\n---\nfrom project claude",
        )
        .unwrap();

        std::fs::create_dir_all(home.join(".flux/skills")).unwrap();
        std::fs::create_dir_all(home.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(home.join(".claude/skills")).unwrap();
        std::fs::write(
            home.join(".flux/skills/shared.md"),
            "---\nname: shared\ndescription: d\n---\nfrom user flux",
        )
        .unwrap();
        std::fs::write(
            home.join(".agents/skills/shared.md"),
            "---\nname: shared\ndescription: d\n---\nfrom user agents",
        )
        .unwrap();
        std::fs::write(
            home.join(".agents/skills/agents-only.md"),
            "---\nname: agents-only\ndescription: d\n---\nfrom user agents",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/skills/shared.md"),
            "---\nname: shared\ndescription: d\n---\nfrom user claude",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/skills/user-claude-only.md"),
            "---\nname: user-claude-only\ndescription: d\n---\nfrom user claude",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let discovery = discover_skills_from(&root, &[]).unwrap();
        std::env::remove_var("HOME");

        let shared = discovery
            .skills
            .iter()
            .find(|s| s.name == "shared")
            .unwrap();
        assert_eq!(
            shared.body, "from project flux",
            "project .flux/skills wins the five-way clash"
        );
        assert_eq!(
            discovery
                .skills
                .iter()
                .filter(|s| s.name == "shared")
                .count(),
            1,
            "exactly one `shared` survives the five-directory dedup"
        );

        for name in ["claude-only", "agents-only", "user-claude-only"] {
            assert!(
                discovery.skills.iter().any(|s| s.name == name),
                "expected `{name}` from every directory to be discovered: {:?}",
                discovery.skills.iter().map(|s| &s.name).collect::<Vec<_>>()
            );
        }

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    /// D-192: ported from `flux-skill`'s retired `discovers_md_files_and_skill_dirs` legacy-path
    /// test — a directory skill (`<dir>/SKILL.md`) with no `name` in its frontmatter takes the
    /// directory's name instead (the `skill.name == "SKILL"` fallback wired in [`parse_skill`]).
    #[test]
    fn skill_directory_with_no_frontmatter_name_takes_directory_name() {
        let root = temp_dir("skill-dirname-root");
        std::fs::create_dir_all(root.join(".flux/skills/beta")).unwrap();
        std::fs::write(
            root.join(".flux/skills/beta/SKILL.md"),
            "---\ndescription: B skill\n---\nB body",
        )
        .unwrap();

        let skills = discover_skills(&root, &[]).unwrap();
        assert!(
            skills.iter().any(|s| s.name == "beta"),
            "directory skill with no frontmatter name should take the directory name: {:?}",
            skills.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_project_config_preserves_settings_and_rejects_parent_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("config-root");
        std::fs::create_dir_all(root.join(".flux")).unwrap();
        std::fs::write(
            root.join(PROJECT_CONFIG),
            "model = \"mock\"\n[permissions]\nallow = [\"read\"]\n",
        )
        .unwrap();
        persist_allow_rules(&root, &["write".into()]).unwrap();
        let config = load_config(&root).unwrap();
        assert_eq!(config.model.as_deref(), Some("mock"));
        assert_eq!(config.permissions.allow, ["read", "write"]);

        let outside = temp_dir("config-outside");
        std::fs::remove_dir_all(root.join(".flux")).unwrap();
        symlink(&outside, root.join(".flux")).unwrap();
        assert!(persist_allow_rules(&root, &["bash".into()]).is_err());
        assert!(!outside.join("config.toml").exists());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn project_config_cannot_promote_an_absolute_skill_root_to_trusted_io() {
        let root = temp_dir("skill-config-root");
        let outside = temp_dir("skill-config-outside");
        std::fs::create_dir_all(root.join(".flux")).unwrap();
        std::fs::write(
            outside.join("escaped.md"),
            "---\nname: escaped\n---\nOUTSIDE",
        )
        .unwrap();
        std::fs::write(
            root.join(PROJECT_CONFIG),
            format!("[skills]\ndirs = [{:?}]\n", outside.display().to_string()),
        )
        .unwrap();

        let config = load_config(&root).unwrap();
        let roots = configured_skill_roots(&config);
        assert!(matches!(roots.as_slice(), [SkillRoot::Project(path)] if path == &outside));
        assert!(discover_skills_from(&root, &roots).is_err());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn missing_optional_metadata_is_harmless_on_every_platform() {
        let root = temp_dir("missing");
        assert!(load_config(&root).is_ok());
        assert!(load_groups(&root).unwrap().is_empty());
        assert!(discover_skills(&root, &[]).is_ok());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discover_commands_across_four_dirs_with_precedence_and_dedup() {
        let _home_guard = HOME_LOCK.lock().unwrap();
        let root = temp_dir("cmd-root");
        let home = temp_dir("cmd-home");

        // Project `.flux/commands` wins over project `.claude/commands`...
        std::fs::create_dir_all(root.join(".flux/commands")).unwrap();
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::write(
            root.join(".flux/commands/shared.md"),
            "---\ndescription: from project flux\n---\nproject-flux body",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/commands/shared.md"),
            "---\ndescription: from project claude\n---\nproject-claude body",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/commands/claude-only.md"),
            "claude-only body",
        )
        .unwrap();

        // ...and both project dirs win over the user-global twins.
        std::fs::create_dir_all(home.join(".flux/commands")).unwrap();
        std::fs::create_dir_all(home.join(".claude/commands")).unwrap();
        std::fs::write(
            home.join(".flux/commands/shared.md"),
            "---\ndescription: from user flux\n---\nuser-flux body",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/commands/shared.md"),
            "---\ndescription: from user claude\n---\nuser-claude body",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/commands/user-only.md"),
            "---\nargument-hint: <thing>\n---\nuser-only body",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let discovery = discover_commands(&root).unwrap();
        std::env::remove_var("HOME");

        let shared = discovery
            .commands
            .iter()
            .find(|c| c.name == "shared")
            .unwrap();
        assert_eq!(shared.description, "from project flux");
        assert_eq!(shared.body, "project-flux body");

        assert!(discovery.commands.iter().any(|c| c.name == "claude-only"));
        let user_only = discovery
            .commands
            .iter()
            .find(|c| c.name == "user-only")
            .unwrap();
        assert_eq!(user_only.argument_hint, "<thing>");

        // Exactly one `shared` entry survived the four-directory dedup.
        assert_eq!(
            discovery
                .commands
                .iter()
                .filter(|c| c.name == "shared")
                .count(),
            1
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(home).ok();
    }

    #[cfg(unix)]
    #[test]
    fn discover_commands_rejects_project_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("cmd-escape-root");
        let outside = temp_dir("cmd-escape-outside");
        std::fs::create_dir_all(root.join(".flux/commands")).unwrap();
        std::fs::write(outside.join("secret.md"), "OUTSIDE SECRET").unwrap();
        symlink(
            outside.join("secret.md"),
            root.join(".flux/commands/escaped.md"),
        )
        .unwrap();

        let error = discover_commands(&root).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn command_frontmatter_parses_known_fields_and_warns_on_unknown_ones() {
        let root = temp_dir("cmd-frontmatter");
        std::fs::create_dir_all(root.join(".flux/commands")).unwrap();
        std::fs::write(
            root.join(".flux/commands/review.md"),
            "---\ndescription: Review a PR\nargument-hint: <pr-number>\nmodel: opus\n---\nReview PR $1.",
        )
        .unwrap();

        let discovery = discover_commands(&root).unwrap();
        let cmd = discovery.commands.first().unwrap();
        assert_eq!(cmd.name, "review");
        assert_eq!(cmd.description, "Review a PR");
        assert_eq!(cmd.argument_hint, "<pr-number>");
        assert_eq!(cmd.body, "Review PR $1.");
        assert_eq!(discovery.warnings.len(), 1);
        assert!(
            discovery.warnings[0].contains("model"),
            "{:?}",
            discovery.warnings
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// D-187: `agent-triggerable` is a known, honored field — parsed silently (never a
    /// `discovery.warnings` entry) and defaults to `false` when absent.
    #[test]
    fn command_agent_triggerable_flag_parses_silently_and_defaults_false() {
        let root = temp_dir("cmd-agent-triggerable");
        std::fs::create_dir_all(root.join(".flux/commands")).unwrap();
        std::fs::write(
            root.join(".flux/commands/human.md"),
            "---\ndescription: human only\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            root.join(".flux/commands/agentic.md"),
            "---\ndescription: agent ok\nagent-triggerable: true\n---\nbody",
        )
        .unwrap();

        let discovery = discover_commands(&root).unwrap();
        let human = discovery
            .commands
            .iter()
            .find(|c| c.name == "human")
            .unwrap();
        let agentic = discovery
            .commands
            .iter()
            .find(|c| c.name == "agentic")
            .unwrap();
        assert!(!human.agent_triggerable);
        assert!(agentic.agent_triggerable);
        assert!(
            discovery.warnings.is_empty(),
            "agent-triggerable must never warn: {:?}",
            discovery.warnings
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn expand_command_arguments_substitutes_full_tail_and_positionals() {
        let body = "Do $1 then $2, given $ARGUMENTS overall.";
        let out = expand_command_arguments(body, "first second third");
        assert_eq!(
            out,
            "Do first then second, given first second third overall."
        );
    }

    #[test]
    fn expand_command_arguments_missing_positional_substitutes_empty() {
        let body = "arg1=[$1] arg2=[$2] arg9=[$9]";
        let out = expand_command_arguments(body, "only-one");
        assert_eq!(out, "arg1=[only-one] arg2=[] arg9=[]");
    }

    #[test]
    fn expand_command_arguments_leaves_claude_inline_bash_and_file_refs_literal() {
        // D-186 scope: `!`-prefixed inline bash and `@file` refs pass through untouched.
        let body = "!`git status` and @README.md with $1";
        let out = expand_command_arguments(body, "x");
        assert_eq!(out, "!`git status` and @README.md with x");
    }
}
