//! Guarded startup-time project metadata assembly.
//!
//! Parsing stays in the L0 config/skill crates. This L2 module owns provenance: repository paths
//! are read through a workspace confined to the exact project root, while explicitly selected
//! user-global roots get their own confined [`System`] boundary.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

fn parse_skill(path: PathBuf, text: &str) -> flux_skill::Skill {
    let mut skill = flux_skill::parse(text, Some(path.clone()));
    if skill.name == "SKILL" {
        if let Some(name) = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
        {
            skill.name = name;
        }
    }
    skill
}

fn extend_skills(
    out: &mut Vec<flux_skill::Skill>,
    seen: &mut HashSet<String>,
    files: Vec<(PathBuf, String)>,
) {
    for (path, text) in files {
        let skill = parse_skill(path, &text);
        if seen.insert(skill.name.clone()) {
            out.push(skill);
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

/// Discover skills in explicit precedence order without giving L0 parsing code project filesystem
/// authority. `extra` roots come first; relative roots are repository-controlled and guarded,
/// absolute roots are explicit trusted control-plane selections. Then project defaults precede
/// user-global defaults. Skill activation remains the caller's explicit allowlist decision.
pub fn discover_skills(cwd: &Path, extra: &[PathBuf]) -> Result<Vec<flux_skill::Skill>> {
    discover_skills_from(
        cwd,
        &extra
            .iter()
            .cloned()
            .map(SkillRoot::Project)
            .collect::<Vec<_>>(),
    )
}

/// Provenance-aware skill discovery used when layered config and explicit CLI paths are combined.
pub fn discover_skills_from(cwd: &Path, extra: &[SkillRoot]) -> Result<Vec<flux_skill::Skill>> {
    let project = project_system(cwd)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();

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
        extend_skills(&mut out, &mut seen, files);
    }

    for dir in [".flux/skills", ".claude/skills"] {
        let files = guarded_skill_files(&project, cwd, dir)?;
        extend_skills(&mut out, &mut seen, files);
    }

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for dir in [
            home.join(".flux/skills"),
            home.join(".agents/skills"),
            home.join(".claude/skills"),
        ] {
            let files = trusted_skill_files(&dir)?;
            extend_skills(&mut out, &mut seen, files);
        }
    }

    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

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
}
