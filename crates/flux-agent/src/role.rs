//! Agent roles loaded from markdown (`.flux/agents/<name>.md`): frontmatter `description`/`model`/
//! `tools` plus a body used as the role's system prompt. A [`Role`] is a file-defined agent
//! definition; [`Role::to_spec`] turns it into an [`AgentSpec`](crate::AgentSpec).

use std::collections::HashMap;
use std::path::PathBuf;

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
    pub fn to_spec(&self, default_model: &str) -> AgentSpec {
        AgentSpec {
            model: self
                .model
                .clone()
                .unwrap_or_else(|| default_model.to_string()),
            system_prompt: self.prompt.clone(),
            tools: self.tools.clone(),
            ..AgentSpec::default()
        }
    }
}

/// Role frontmatter (all fields optional → lenient parsing). `tools` distinguishes a missing key
/// (`None` → inherit all) from an explicit `tools: []` (`Some([])` → grant none).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RoleFrontmatter {
    name: String,
    description: String,
    model: Option<String>,
    tools: Option<Vec<String>>,
}

/// Parse role markdown. `name_fallback` is used when frontmatter omits `name`.
pub fn parse_role(content: &str, name_fallback: &str) -> Role {
    let (fm, body) = flux_markdown::split_frontmatter(content);
    let meta: RoleFrontmatter = fm
        .map(|y| serde_norway::from_str(y).unwrap_or_default())
        .unwrap_or_default();

    let name = if meta.name.is_empty() {
        name_fallback.to_string()
    } else {
        meta.name
    };
    Role {
        name,
        description: meta.description,
        // an empty `model:` (null/blank) inherits the parent's model
        model: meta.model.filter(|m| !m.is_empty()),
        tools: meta.tools,
        prompt: body.trim().to_string(),
    }
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

    /// Load roles from `*.md` files under each directory (filename stem = default role name).
    pub fn load(dirs: &[PathBuf]) -> Self {
        let mut reg = RoleRegistry::default();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "md").unwrap_or(false) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let stem = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "role".to_string());
                        reg.insert(parse_role(&content, &stem));
                    }
                }
            }
        }
        reg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_role_frontmatter() {
        let r = parse_role(
            "---\ndescription: fast recon\nmodel: haiku\ntools: [read, grep, ls]\n---\nYou are a scout.",
            "scout",
        );
        assert_eq!(r.name, "scout");
        assert_eq!(r.description, "fast recon");
        assert_eq!(r.model.as_deref(), Some("haiku"));
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
    fn to_spec_inherits_model_and_carries_tools() {
        let r = parse_role("---\ntools: [read, grep]\n---\nBe terse.", "scout");
        let spec = r.to_spec("default-model");
        assert_eq!(spec.model, "default-model"); // role omitted model → inherit
        assert_eq!(spec.system_prompt, "Be terse.");
        assert_eq!(
            spec.tools.as_deref(),
            Some(&["read".into(), "grep".into()][..])
        );

        let r2 = parse_role("---\nmodel: haiku\n---\nx", "s");
        assert_eq!(r2.to_spec("default-model").model, "haiku"); // role overrides
    }

    #[test]
    fn registry_load_from_dir() {
        let dir = std::env::temp_dir().join(format!("flux-roles-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("planner.md"),
            "---\ndescription: plans\n---\nPlan well.",
        )
        .unwrap();
        let reg = RoleRegistry::load(std::slice::from_ref(&dir));
        assert_eq!(reg.names(), vec!["planner"]);
        assert_eq!(reg.get("planner").unwrap().description, "plans");
        std::fs::remove_dir_all(&dir).ok();
    }
}
