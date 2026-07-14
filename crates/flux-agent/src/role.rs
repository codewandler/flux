//! Agent roles loaded from markdown (`.flux/agents/<name>.md`): frontmatter `description`/`model`/
//! `tools` plus a body used as the role's system prompt. A [`Role`] is a file-defined agent
//! definition; [`Role::to_spec`] turns it into an [`AgentSpec`](crate::AgentSpec).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use flux_core::{Error, Result};
use flux_provider::Effort;
use serde::{Deserialize, Serialize};

use crate::AgentSpec;

/// A sub-agent role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Model override; `None` inherits the parent's model.
    #[serde(default)]
    pub model: Option<String>,
    /// Adaptive-thinking override; `None` inherits the parent's setting for spawned roles.
    #[serde(default)]
    pub thinking: Option<bool>,
    /// Reasoning-effort override; `None` inherits the parent's setting for spawned roles.
    #[serde(default)]
    pub effort: Option<Effort>,
    /// `adaptive` or inline Flux-Lang source for this role's outer loop.
    #[serde(default, rename = "loop")]
    pub agent_loop: Option<String>,
    /// Tool allowlist. `None` (no `tools` key) inherits all tools available to the parent;
    /// `Some([])` (an explicit empty list) grants none.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// The role's system prompt (markdown body).
    pub prompt: String,
}

impl Role {
    /// An [`AgentSpec`] for this role: the role body becomes the system prompt, `tools` becomes the
    /// tool selection, and the model falls back to `default_model` when the role doesn't override it.
    /// Turn settings (`max_tokens`, `max_iterations`, …) take spec defaults; the caller can override.
    pub fn to_spec(&self, default_model: &str) -> Result<AgentSpec> {
        let agent_loop = match self.agent_loop.as_deref() {
            Some(source) => crate::AgentLoopSpec::parse(source).map_err(|error| {
                Error::Other(format!(
                    "role `{}` has an invalid agent loop: {error}",
                    self.name
                ))
            })?,
            None => crate::AgentLoopSpec::default(),
        };
        Ok(AgentSpec {
            model: self
                .model
                .clone()
                .unwrap_or_else(|| default_model.to_string()),
            system_prompt: self.prompt.clone(),
            tools: self.tools.clone(),
            thinking: self.thinking.unwrap_or(false),
            effort: self.effort,
            agent_loop,
            ..AgentSpec::default()
        })
    }
}

/// Role frontmatter. `tools` distinguishes a missing key
/// (`None` → inherit all) from an explicit `tools: []` (`Some([])` → grant none).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RoleFrontmatter {
    name: String,
    description: String,
    model: Option<String>,
    thinking: Option<bool>,
    effort: Option<Effort>,
    #[serde(rename = "loop")]
    agent_loop: Option<String>,
    tools: Option<Vec<String>>,
}

/// Strictly parse role markdown. `name_fallback` is used when frontmatter omits `name`.
/// Malformed metadata is rejected rather than defaulted because a missing `tools` field means
/// "inherit the parent's tools" and is therefore security-relevant.
pub fn try_parse_role(content: &str, name_fallback: &str) -> Result<Role> {
    try_parse_role_at(content, name_fallback, name_fallback)
}

fn try_parse_role_at(content: &str, name_fallback: &str, source: &str) -> Result<Role> {
    let (fm, body) = flux_markdown::split_frontmatter(content);
    let meta: RoleFrontmatter = match fm {
        Some(yaml) => serde_norway::from_str(yaml).map_err(|error| {
            Error::Config(format!("role `{source}` has invalid frontmatter: {error}"))
        })?,
        None => RoleFrontmatter::default(),
    };

    if let Some(agent_loop) = meta.agent_loop.as_deref() {
        crate::AgentLoopSpec::parse(agent_loop).map_err(|error| {
            Error::Config(format!(
                "role `{source}` has an invalid agent loop: {error}"
            ))
        })?;
    }

    let name = if meta.name.is_empty() {
        name_fallback.to_string()
    } else {
        meta.name
    };
    Ok(Role {
        name,
        description: meta.description,
        // an empty `model:` (null/blank) inherits the parent's model
        model: meta.model.filter(|m| !m.is_empty()),
        thinking: meta.thinking,
        effort: meta.effort,
        agent_loop: meta.agent_loop,
        tools: meta.tools,
        prompt: body.trim().to_string(),
    })
}

/// Lenient compatibility parser. Production discovery uses [`try_parse_role`] and fails closed.
#[deprecated(note = "use try_parse_role; malformed role metadata must not inherit parent tools")]
pub fn parse_role(content: &str, name_fallback: &str) -> Role {
    try_parse_role(content, name_fallback).unwrap_or_else(|_| Role {
        name: name_fallback.to_string(),
        description: String::new(),
        model: None,
        thinking: None,
        effort: None,
        agent_loop: None,
        tools: None,
        prompt: flux_markdown::split_frontmatter(content)
            .1
            .trim()
            .to_string(),
    })
}

/// A set of roles keyed by name.
#[derive(Debug, Default, Clone)]
pub struct RoleRegistry {
    roles: HashMap<String, Role>,
}

impl RoleRegistry {
    pub fn insert(&mut self, role: Role) {
        self.roles.insert(role.name.clone(), role);
    }

    pub fn get(&self, name: &str) -> Option<&Role> {
        self.roles.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.roles.keys().cloned().collect();
        n.sort();
        n
    }

    /// Build a registry from in-memory roles — for a programmatic consumer (e.g. a multi-tenant
    /// service) that registers roles in code rather than from a shared `.flux/agents` directory.
    pub fn from_roles(roles: impl IntoIterator<Item = Role>) -> Self {
        roles.into_iter().collect()
    }

    fn insert_loaded(
        &mut self,
        role: Role,
        source: &str,
        sources: &mut HashMap<String, String>,
    ) -> Result<()> {
        if let Some(previous) = sources.get(&role.name) {
            return Err(Error::Config(format!(
                "duplicate role `{}` in `{source}` (already defined by `{previous}`)",
                role.name
            )));
        }
        sources.insert(role.name.clone(), source.to_string());
        self.insert(role);
        Ok(())
    }

    /// Strictly load repository roles through the guarded workspace. Missing directories are
    /// harmless; unreadable files, symlink escapes, malformed metadata, and duplicate role names
    /// are errors with their source path attached.
    pub fn try_load_project(system: &flux_system::System, dir: &str) -> Result<Self> {
        let files = system
            .read_dir_text_files(dir, "md")
            .map_err(|error| Error::Config(format!("load project roles from `{dir}`: {error}")))?;
        let mut reg = RoleRegistry::default();
        let mut sources = HashMap::new();
        for (path, content) in files {
            let stem = Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "role".to_string());
            let role = try_parse_role_at(&content, &stem, &path)?;
            reg.insert_loaded(role, &path, &mut sources)?;
        }
        Ok(reg)
    }

    /// Strictly load roles from trusted user-global control-plane directories. The first
    /// directory has precedence on a name clash; duplicates inside one directory are rejected.
    /// Repository-local paths must use [`Self::try_load_project`] instead.
    pub fn try_load(dirs: &[PathBuf]) -> Result<Self> {
        let mut reg = RoleRegistry::default();
        for dir in dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(Error::Config(format!(
                        "read trusted role directory `{}`: {error}",
                        dir.display()
                    )))
                }
            };
            let mut paths = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|error| {
                    Error::Config(format!(
                        "read trusted role directory `{}`: {error}",
                        dir.display()
                    ))
                })?;
                let path = entry.path();
                if path.extension().is_some_and(|extension| extension == "md") {
                    paths.push(path);
                }
            }
            paths.sort();
            let mut directory_sources = HashMap::new();
            for path in paths {
                let content = std::fs::read_to_string(&path).map_err(|error| {
                    Error::Config(format!("read trusted role `{}`: {error}", path.display()))
                })?;
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "role".to_string());
                let source = path.display().to_string();
                let role = try_parse_role_at(&content, &stem, &source)?;
                if let Some(previous) = directory_sources.get(&role.name) {
                    return Err(Error::Config(format!(
                        "duplicate role `{}` in `{source}` (already defined by `{previous}`)",
                        role.name
                    )));
                }
                if reg.get(&role.name).is_some() {
                    continue;
                }
                reg.insert_loaded(role, &source, &mut directory_sources)?;
            }
        }
        Ok(reg)
    }

    /// Add roles that are not already defined, without allowing a lower-precedence source to
    /// replace an existing definition.
    pub fn extend_missing(&mut self, roles: Self) {
        for role in roles.roles.into_values() {
            if self.get(&role.name).is_none() {
                self.insert(role);
            }
        }
    }

    /// Load roles from `*.md` files under each directory (filename stem = default role name).
    #[deprecated(note = "use try_load or try_load_project; production discovery must fail closed")]
    pub fn load(dirs: &[PathBuf]) -> Self {
        Self::try_load(dirs).unwrap_or_default()
    }
}

impl FromIterator<Role> for RoleRegistry {
    fn from_iter<I: IntoIterator<Item = Role>>(iter: I) -> Self {
        let mut reg = RoleRegistry::default();
        for role in iter {
            reg.insert(role);
        }
        reg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn parse_role(content: &str, name_fallback: &str) -> Role {
        try_parse_role(content, name_fallback).unwrap()
    }

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-roles-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_role_frontmatter() {
        let r = parse_role(
            "---\ndescription: fast recon\nmodel: haiku\nthinking: true\neffort: high\ntools: [read, grep, ls]\n---\nYou are a scout.",
            "scout",
        );
        assert_eq!(r.name, "scout");
        assert_eq!(r.description, "fast recon");
        assert_eq!(r.model.as_deref(), Some("haiku"));
        assert_eq!(r.thinking, Some(true));
        assert_eq!(r.effort, Some(flux_provider::Effort::High));
        assert_eq!(
            r.tools.as_deref(),
            Some(&["read".into(), "grep".into(), "ls".into()][..])
        );
        assert_eq!(r.prompt, "You are a scout.");
    }

    #[test]
    fn empty_model_inherits() {
        let r = parse_role("---\nmodel:\n---\nbody", "worker");
        assert_eq!(r.name, "worker");
        assert_eq!(r.model, None);
        assert!(
            r.tools.is_none(),
            "no tools key → inherit all (None), not an empty allowlist"
        );
    }

    #[test]
    fn explicit_empty_tools_is_some_empty() {
        // `tools: []` is the most-restrictive declaration and must parse to Some([]), not None.
        let r = parse_role("---\ntools: []\n---\nbody", "locked");
        assert_eq!(r.tools, Some(Vec::new()));
    }

    #[test]
    fn malformed_tools_metadata_cannot_inherit_parent_tools() {
        let error = try_parse_role("---\ntools: read\n---\nbody", "unsafe").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("unsafe"), "{message}");
        assert!(message.contains("tools"), "{message}");
    }

    #[test]
    fn invalid_effort_and_loop_are_rejected_during_parse() {
        let effort = try_parse_role("---\neffort: turbo\n---\nbody", "worker").unwrap_err();
        assert!(effort.to_string().contains("worker"), "{effort}");

        let agent_loop =
            try_parse_role("---\nloop: 'flow main:'\n---\nbody", "worker").unwrap_err();
        assert!(
            agent_loop.to_string().contains("agent loop"),
            "{agent_loop}"
        );
    }

    #[test]
    fn strict_loader_reports_source_and_duplicate_role_names() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.md"), "---\nname: shared\n---\na").unwrap();
        std::fs::write(dir.join("b.md"), "---\nname: shared\n---\nb").unwrap();

        let error = RoleRegistry::try_load(std::slice::from_ref(&dir)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("duplicate role `shared`"), "{message}");
        assert!(message.contains("a.md"), "{message}");
        assert!(message.contains("b.md"), "{message}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strict_project_loader_reports_unreadable_role_path() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join(".flux/agents")).unwrap();
        std::fs::write(root.join(".flux/agents/binary.md"), [0xff, 0xfe]).unwrap();

        let system = flux_system::System::new(flux_system::Workspace::new(&root).unwrap());
        let error = RoleRegistry::try_load_project(&system, ".flux/agents").unwrap_err();
        let message = error.to_string();
        assert!(message.contains(".flux/agents/binary.md"), "{message}");
        assert!(message.contains("UTF-8"), "{message}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extend_missing_preserves_higher_precedence_roles() {
        let mut trusted =
            RoleRegistry::from_roles([
                try_parse_role("---\ntools: []\n---\ntrusted", "worker").unwrap()
            ]);
        let project = RoleRegistry::from_roles([try_parse_role(
            "---\ntools: [read]\n---\nproject",
            "worker",
        )
        .unwrap()]);

        trusted.extend_missing(project);
        let role = trusted.get("worker").unwrap();
        assert_eq!(role.prompt, "trusted");
        assert_eq!(role.tools, Some(Vec::new()));
    }

    #[cfg(unix)]
    #[test]
    fn project_role_loader_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_dir();
        let outside = temp_dir();
        std::fs::create_dir_all(root.join(".flux/agents")).unwrap();
        std::fs::write(
            outside.join("worker.md"),
            "---\ntools: [read]\n---\nOUTSIDE",
        )
        .unwrap();
        symlink(
            outside.join("worker.md"),
            root.join(".flux/agents/worker.md"),
        )
        .unwrap();

        let system = flux_system::System::new(flux_system::Workspace::new(&root).unwrap());
        let error = RoleRegistry::try_load_project(&system, ".flux/agents").unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn to_spec_inherits_model_and_carries_tools() {
        let r = parse_role("---\ntools: [read, grep]\n---\nBe terse.", "scout");
        let spec = r.to_spec("default-model").unwrap();
        assert_eq!(spec.model, "default-model"); // role omitted model → inherit
        assert_eq!(spec.system_prompt, "Be terse.");
        assert_eq!(
            spec.tools.as_deref(),
            Some(&["read".into(), "grep".into()][..])
        );

        let r2 = parse_role("---\nmodel: haiku\n---\nx", "s");
        assert_eq!(r2.to_spec("default-model").unwrap().model, "haiku"); // role overrides
    }

    #[test]
    fn to_spec_carries_explicit_reasoning_settings() {
        let r = parse_role(
            "---\nthinking: true\neffort: xhigh\n---\nInvestigate deeply.",
            "investigator",
        );
        let spec = r.to_spec("parent-model").unwrap();
        assert!(spec.thinking);
        assert_eq!(spec.effort, Some(flux_provider::Effort::Xhigh));
    }

    #[test]
    fn registry_load_from_dir() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("planner.md"),
            "---\ndescription: plans\n---\nPlan well.",
        )
        .unwrap();
        let reg = RoleRegistry::try_load(std::slice::from_ref(&dir)).unwrap();
        assert_eq!(reg.names(), vec!["planner"]);
        assert_eq!(reg.get("planner").unwrap().description, "plans");
        std::fs::remove_dir_all(&dir).ok();
    }
}
