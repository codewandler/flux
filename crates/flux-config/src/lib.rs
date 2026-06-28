//! `flux-config` — layered project/user configuration for the `flux` binary.
//!
//! Two files are read and merged: `~/.flux/config.toml` (user defaults) then
//! `<cwd>/.flux/config.toml` (project, takes precedence). A missing file is not an error — it
//! contributes nothing; a malformed file is an error. CLI flags layer on top of the result (the
//! caller resolves that). The config carries the coder-style permission rules, an optional default
//! model, an optional [`AuthorizationPolicy`] (extends [`flux_policy::default_local_grants`]), and
//! the network egress toggle. Newly "always-allow"ed approval rules are persisted back to the
//! **project** file via [`persist_allow_rules`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use flux_core::{Error, Result};
use flux_policy::AuthorizationPolicy;

/// Coder-style permission rules (`read`, `Bash(git:*)`, …): deny wins, then allow, else prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// The merged flux configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Default `provider/model` spec (a CLI `--model` flag overrides this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Allow the guarded web tool to reach private/loopback addresses (off by default).
    #[serde(default)]
    pub allow_private_net: bool,
    /// Opt into the generic `bash` op (the `shell` group). Off by default — the agent works through
    /// the dedicated ops; setting this surfaces `bash` as an escape hatch. The CLI exports
    /// `FLUX_ENABLE_BASH` from this so the runtime's `shell` signal fires.
    #[serde(default)]
    pub enable_shell: bool,
    #[serde(default)]
    pub permissions: Permissions,
    /// Extra authorization grants, layered onto the built-in local defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<AuthorizationPolicy>,
}

fn home_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".flux").join("config.toml"))
}

fn project_config_path(cwd: &Path) -> PathBuf {
    cwd.join(".flux").join("config.toml")
}

/// Read a config file, returning `None` if it doesn't exist and erroring if it's malformed.
fn read_optional(path: &Path) -> Result<Option<Config>> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let cfg = toml::from_str(&s)
                .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
            Ok(Some(cfg))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Merge `project` onto `user`: lists (and policy grants) concatenate (user first), scalars prefer
/// project, `allow_private_net` is true if either enables it.
fn merge(user: Config, project: Config) -> Config {
    Config {
        model: project.model.or(user.model),
        allow_private_net: user.allow_private_net || project.allow_private_net,
        enable_shell: user.enable_shell || project.enable_shell,
        permissions: Permissions {
            allow: [user.permissions.allow, project.permissions.allow].concat(),
            deny: [user.permissions.deny, project.permissions.deny].concat(),
        },
        // Concatenate grants like permissions — a project policy refines (adds to) the user's, it
        // doesn't silently discard it. (Previously `project.policy.or(user.policy)` dropped every
        // user grant the moment a project defined any policy block.)
        policy: match (user.policy, project.policy) {
            (None, None) => None,
            (Some(u), None) => Some(u),
            (None, Some(p)) => Some(p),
            (Some(u), Some(p)) => Some(AuthorizationPolicy {
                grants: [u.grants, p.grants].concat(),
            }),
        },
    }
}

/// Load and merge `~/.flux/config.toml` (user) then `<cwd>/.flux/config.toml` (project).
pub fn load(cwd: &Path) -> Result<Config> {
    let user = match home_config_path() {
        Some(p) => read_optional(&p)?.unwrap_or_default(),
        None => Config::default(),
    };
    let project = read_optional(&project_config_path(cwd))?.unwrap_or_default();
    Ok(merge(user, project))
}

/// Persist allow rules back to the **project** config (`<cwd>/.flux/config.toml`), unioned with
/// whatever is already there (order-preserving, de-duplicated). Creates `.flux/` if needed.
pub fn persist_allow_rules(cwd: &Path, rules: &[String]) -> Result<()> {
    let path = project_config_path(cwd);
    let mut cfg = read_optional(&path)?.unwrap_or_default();

    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for r in cfg.permissions.allow.iter().chain(rules.iter()) {
        if seen.insert(r.clone()) {
            merged.push(r.clone());
        }
    }
    cfg.permissions.allow = merged;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let body = toml::to_string_pretty(&cfg).map_err(|e| Error::Config(e.to_string()))?;
    std::fs::write(&path, body).map_err(Error::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tool-group manifest (.flux/groups.toml)
// ---------------------------------------------------------------------------

/// The `.flux/groups.toml` manifest: a list of `[[group]]` entries declaring evidence-gated tool
/// groups (name, optional membership `tools`, and `surface_when` signal matches).
#[derive(Debug, Clone, Default, Deserialize)]
struct GroupsManifest {
    #[serde(default, rename = "group")]
    groups: Vec<flux_evidence::ToolGroup>,
}

fn home_groups_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".flux").join("groups.toml"))
}

fn project_groups_path(cwd: &Path) -> PathBuf {
    cwd.join(".flux").join("groups.toml")
}

/// Load user (`~/.flux/groups.toml`) then project (`<cwd>/.flux/groups.toml`) group definitions.
/// A project entry overrides a user entry of the same `name`. Missing files are not an error; a
/// malformed file is skipped (a warning is printed) rather than failing the session.
pub fn load_groups(cwd: &Path) -> Vec<flux_evidence::ToolGroup> {
    let mut out: Vec<flux_evidence::ToolGroup> = Vec::new();
    let paths = home_groups_path()
        .into_iter()
        .chain(std::iter::once(project_groups_path(cwd)));
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue; // missing file: fine
        };
        match toml::from_str::<GroupsManifest>(&text) {
            Ok(m) => {
                for g in m.groups {
                    // Later file (project) overrides an earlier (user) group of the same name.
                    if let Some(slot) = out.iter_mut().find(|e| e.name == g.name) {
                        *slot = g;
                    } else {
                        out.push(g);
                    }
                }
            }
            Err(e) => eprintln!("(ignoring malformed {}: {e})", p.display()),
        }
    }
    out
}

/// Merge built-in groups with config `overrides`: a config group with the same `name` replaces the
/// built-in (so a project can retune surfacing or membership); new names are appended.
pub fn merge_groups(
    base: Vec<flux_evidence::ToolGroup>,
    overrides: Vec<flux_evidence::ToolGroup>,
) -> Vec<flux_evidence::ToolGroup> {
    let mut out = base;
    for g in overrides {
        if let Some(slot) = out.iter_mut().find(|e| e.name == g.name) {
            *slot = g;
        } else {
            out.push(g);
        }
    }
    out
}

#[cfg(test)]
mod groups_tests {
    use super::*;

    #[test]
    fn load_and_merge_groups() {
        let dir = std::env::temp_dir().join(format!("flux-cfg-groups-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".flux")).unwrap();
        std::fs::write(
            dir.join(".flux").join("groups.toml"),
            r#"
[[group]]
name = "git"
surface_when = [{ signal = "git_repo" }]

[[group]]
name = "custom"
tools = ["my_op"]
surface_when = [{ kind = "project.signal", signal = "custom" }]
"#,
        )
        .unwrap();
        // HOME points elsewhere so only the project file is read.
        std::env::set_var("HOME", dir.join("nohome"));
        let cfg = load_groups(&dir);
        assert!(cfg
            .iter()
            .any(|g| g.name == "custom" && g.tools == vec!["my_op".to_string()]));
        let git = cfg.iter().find(|g| g.name == "git").unwrap();
        // `kind` defaulted to project.signal.
        assert_eq!(git.surface_when[0].kind, "project.signal");

        // merge: config "git" replaces a built-in "git"; "custom" is appended.
        let base = vec![flux_evidence::ToolGroup {
            name: "git".into(),
            description: "builtin".into(),
            ..Default::default()
        }];
        let merged = merge_groups(base, cfg);
        let git = merged.iter().find(|g| g.name == "git").unwrap();
        assert!(git.description.is_empty()); // replaced by config (no description)
        assert!(merged.iter().any(|g| g.name == "custom"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-config-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join(".flux")).unwrap();
        dir
    }

    fn write_project(cwd: &Path, body: &str) {
        std::fs::write(cwd.join(".flux").join("config.toml"), body).unwrap();
    }

    #[test]
    fn missing_files_yield_default() {
        let dir = std::env::temp_dir().join(format!("flux-config-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = load(&dir).unwrap();
        assert!(cfg.model.is_none());
        assert!(cfg.permissions.allow.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_project_config() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
model = "claude/opus"
allow_private_net = true

[permissions]
allow = ["read", "Bash(git:*)"]
deny = ["Bash(rm:*)"]
"#,
        );
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("claude/opus"));
        assert!(cfg.allow_private_net);
        assert_eq!(cfg.permissions.allow, vec!["read", "Bash(git:*)"]);
        assert_eq!(cfg.permissions.deny, vec!["Bash(rm:*)"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_config_errors() {
        let dir = temp_dir();
        write_project(&dir, "this is = = not toml");
        assert!(load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_policy_grants() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "workspace", id = "*" }]
actions = ["workspace.read"]
"#,
        );
        let cfg = load(&dir).unwrap();
        let policy = cfg.policy.expect("policy present");
        assert_eq!(policy.grants.len(), 1);
        assert_eq!(policy.grants[0].actions[0].0, "workspace.read");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn policy_grants_concatenate_across_user_and_project() {
        use flux_policy::{
            Action, AuthorizationPolicy, Grant, ResourceKind, ResourceRef, SubjectKind, SubjectRef,
            TrustLevel,
        };
        let mk = |action: &str| AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::any(ResourceKind::Workspace)],
                actions: vec![Action::from(action)],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            }],
        };
        let user = Config {
            policy: Some(mk("workspace.read")),
            ..Default::default()
        };
        let project = Config {
            policy: Some(mk("workspace.write")),
            ..Default::default()
        };
        let merged = merge(user, project);
        let grants = merged.policy.expect("policy present").grants;
        assert_eq!(
            grants.len(),
            2,
            "user + project policy grants must concatenate, not replace"
        );
    }

    #[test]
    fn persist_allow_rules_unions_and_dedups() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[permissions]
allow = ["read"]
"#,
        );
        persist_allow_rules(&dir, &["read".into(), "Bash(git:*)".into()]).unwrap();
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.permissions.allow, vec!["read", "Bash(git:*)"]);

        // A second persist with a new rule appends without duplicating.
        persist_allow_rules(&dir, &["write".into()]).unwrap();
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.permissions.allow, vec!["read", "Bash(git:*)", "write"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
