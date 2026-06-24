//! Agent roles loaded from markdown (`.flux/agents/<name>.md`): frontmatter `description`/`model`/
//! `tools` plus a body used as the role's system prompt.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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

fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}

fn parse_list(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(unquote)
        .filter(|p| !p.is_empty())
        .collect()
}

fn split_frontmatter(content: &str) -> (String, String) {
    let t = content.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let fm = rest[..end].trim_start_matches(['\r', '\n']).to_string();
            let body = rest[end + 4..]
                .split_once('\n')
                .map(|x| x.1)
                .unwrap_or("")
                .to_string();
            return (fm, body);
        }
    }
    (String::new(), content.to_string())
}

/// Parse role markdown. `name_fallback` is used when frontmatter omits `name`.
pub fn parse_role(content: &str, name_fallback: &str) -> Role {
    let (fm, body) = split_frontmatter(content);
    let mut name = String::new();
    let mut description = String::new();
    let mut model = None;
    let mut tools = None;
    for line in fm.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "name" => name = unquote(v),
                "description" => description = unquote(v),
                "model" => {
                    let m = unquote(v);
                    model = (!m.is_empty()).then_some(m);
                }
                "tools" => tools = Some(parse_list(v)),
                _ => {}
            }
        }
    }
    if name.is_empty() {
        name = name_fallback.to_string();
    }
    Role {
        name,
        description,
        model,
        tools,
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
