//! Render installed plugin manifests into a generated `flux-plugins` skill (story D-13).
//!
//! The flux analogue of fluxplane's `fluxplane-plugin skill`: it turns the `PluginManifest`s of the
//! installed plugins into a trigger-activated `SKILL.md` plus one `references/<plugin>.md` per plugin,
//! so the agent's view of "which integration ops exist, their inputs, and their auth" stays in sync
//! with what is installed — no hand-maintained catalog. The render is a pure function over
//! `(install-name, manifest)` pairs so it is unit-testable without spawning a subprocess.

use flux_markdown::render_document;
use flux_plugin::PluginManifest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Frontmatter for the generated `flux-plugins` skill (flux-native format: explicit `triggers`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
}

/// The render result: the `SKILL.md` contents and one `(plugin-name, reference-markdown)` per plugin.
pub struct RenderedSkill {
    pub skill_md: String,
    pub references: Vec<(String, String)>,
}

/// Render the `flux-plugins` skill + per-plugin references from installed `(name, manifest)` pairs.
///
/// `name` is the *install* name (what `flux plugin call <name> <op>` uses), which may differ from
/// `manifest.name`. Pure — no IO.
pub fn render_plugin_skill(plugins: &[(String, PluginManifest)]) -> RenderedSkill {
    let meta = SkillMeta {
        name: "flux-plugins".into(),
        description: skill_description(plugins),
        triggers: triggers_for(plugins),
    };
    let body = skill_body(plugins);
    let skill_md = render_document(&meta, &body)
        .unwrap_or_else(|e| format!("---\n# frontmatter render failed: {e}\n---\n{body}"));
    let references = plugins
        .iter()
        .map(|(name, m)| (name.clone(), reference_md(name, m)))
        .collect();
    RenderedSkill {
        skill_md,
        references,
    }
}

/// The skill description line — names the installed plugins so description-led activation also works.
fn skill_description(plugins: &[(String, PluginManifest)]) -> String {
    if plugins.is_empty() {
        return "Call installed flux integration plugins via `flux plugin call` (none installed yet)."
            .into();
    }
    let names: Vec<&str> = plugins.iter().map(|(n, _)| n.as_str()).collect();
    let head = names.iter().take(6).copied().collect::<Vec<_>>().join(", ");
    let more = if names.len() > 6 { ", …" } else { "" };
    format!("Call installed flux integration plugins ({head}{more}) via `flux plugin call`.")
}

/// Deterministic substring triggers: each install name, each operation's leading segment, plus the
/// literal `plugin`. A turn mentioning e.g. `gitlab` or `prometheus` then activates the skill.
fn triggers_for(plugins: &[(String, PluginManifest)]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    set.insert("plugin".into());
    for (name, m) in plugins {
        set.insert(name.clone());
        for op in &m.operations {
            if let Some(prefix) = op.name.split(['.', '_']).next() {
                if !prefix.is_empty() {
                    set.insert(prefix.to_string());
                }
            }
        }
    }
    set.into_iter().collect()
}

/// The compact always-injected `SKILL.md` body (per-op detail lives in `references/`).
fn skill_body(plugins: &[(String, PluginManifest)]) -> String {
    let mut s = String::new();
    s.push_str("# Installed integration plugins\n\n");
    s.push_str("Call any installed plugin operation with:\n\n");
    s.push_str("    flux plugin call <plugin> <operation> '<json-input>'\n\n");
    s.push_str(
        "Each plugin resolves its secrets by purpose from environment variables (listed per \
         reference); set them before calling. Per-operation inputs and auth are in \
         `references/<plugin>.md`.\n\n",
    );
    s.push_str("## Installed\n\n");
    if plugins.is_empty() {
        s.push_str(
            "_No plugins installed. Build the pack (`cd plugins && cargo build --release`) then \
             `flux plugin install`._\n",
        );
        return s;
    }
    for (name, m) in plugins {
        let n = m.operations.len();
        let sample: Vec<&str> = m
            .operations
            .iter()
            .take(3)
            .map(|o| o.name.as_str())
            .collect();
        let sample = sample.join(", ");
        let tail = if n > 3 { ", …" } else { "" };
        s.push_str(&format!(
            "- **{name}** — {n} op(s) ({sample}{tail}). → `references/{name}.md`\n"
        ));
    }
    s
}

/// One plugin's reference page: operations table, auth, endpoints, datasources.
fn reference_md(name: &str, m: &PluginManifest) -> String {
    let mut s = String::new();
    let version = if m.version.is_empty() {
        String::new()
    } else {
        format!(" v{}", m.version)
    };
    s.push_str(&format!("# {name}\n\n"));
    s.push_str(&format!(
        "Plugin `{}`{version}. Call: `flux plugin call {name} <operation> '<json-input>'`.\n\n",
        m.name
    ));

    s.push_str("## Operations\n\n");
    if m.operations.is_empty() {
        s.push_str("_none declared_\n\n");
    } else {
        s.push_str("| Operation | Description | Required input | Risk |\n");
        s.push_str("|---|---|---|---|\n");
        for op in &m.operations {
            let req = required_fields(&op.input_schema);
            let req = if req.is_empty() {
                "—".into()
            } else {
                req.join(", ")
            };
            let risk = op
                .risk
                .map(|r| format!("{r:?}"))
                .unwrap_or_else(|| "Medium*".into());
            s.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                cell(&op.name),
                cell(&op.description),
                cell(&req),
                risk
            ));
        }
        s.push('\n');
    }

    if !m.auth.is_empty() {
        s.push_str("## Auth\n\n");
        for a in &m.auth {
            let envs = a
                .env
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let desc = if a.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", a.description)
            };
            s.push_str(&format!("- **{}** (env: {envs}){desc}\n", a.purpose));
        }
        s.push('\n');
    }

    if !m.endpoints.is_empty() {
        s.push_str("## Endpoints\n\n");
        for e in &m.endpoints {
            let envs = e
                .env
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&format!("- **{}** (env: {envs})\n", e.name));
        }
        s.push('\n');
    }

    if !m.datasources.is_empty() {
        s.push_str("## Datasources\n\n");
        for d in &m.datasources {
            let caps = if d.capabilities.is_empty() {
                String::new()
            } else {
                format!(" — {}", d.capabilities.join(", "))
            };
            s.push_str(&format!("- **{}** (entity `{}`){caps}\n", d.name, d.entity));
        }
        s.push('\n');
    }

    s
}

/// The `required` field names declared in an op's JSON input schema (empty when none/absent).
fn required_fields(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| format!("`{s}`")))
                .collect()
        })
        .unwrap_or_default()
}

/// Escape a markdown table cell: collapse newlines and escape pipes.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_markdown::{parse_frontmatter, Document};
    use flux_plugin::{AuthMethod, EndpointSpec, OperationSpec, PluginManifest};
    use serde_json::json;

    fn fixture() -> Vec<(String, PluginManifest)> {
        let gitlab = PluginManifest {
            name: "gitlab".into(),
            version: "0.1.0".into(),
            operations: vec![OperationSpec {
                name: "gitlab.project.list".into(),
                description: "List projects".into(),
                input_schema: json!({"type":"object","properties":{"q":{"type":"string"}}}),
                ..Default::default()
            }],
            auth: vec![AuthMethod {
                purpose: "personal_token".into(),
                env: vec!["GITLAB_PERSONAL_TOKEN".into()],
                description: "GitLab PAT".into(),
                ..Default::default()
            }],
            endpoints: vec![EndpointSpec {
                name: "gitlab.endpoint".into(),
                env: vec!["GITLAB_URL".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let prometheus = PluginManifest {
            name: "prometheus".into(),
            version: "0.1.0".into(),
            operations: vec![OperationSpec {
                name: "prometheus.query".into(),
                description: "Instant PromQL query".into(),
                input_schema: json!({
                    "type":"object",
                    "properties":{"query":{"type":"string"}},
                    "required":["query"]
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        vec![("gitlab".into(), gitlab), ("prometheus".into(), prometheus)]
    }

    #[test]
    fn frontmatter_round_trips_with_expected_triggers() {
        let r = render_plugin_skill(&fixture());
        let doc: Document<SkillMeta> = parse_frontmatter(&r.skill_md).unwrap();
        assert_eq!(doc.meta.name, "flux-plugins");
        // install names + op prefixes + literal "plugin"
        for t in ["plugin", "gitlab", "prometheus"] {
            assert!(
                doc.meta.triggers.contains(&t.to_string()),
                "missing trigger {t}"
            );
        }
        assert!(doc.body.contains("flux plugin call"));
    }

    #[test]
    fn one_reference_per_plugin_with_ops_and_required_input() {
        let r = render_plugin_skill(&fixture());
        assert_eq!(r.references.len(), 2);
        let names: Vec<&str> = r.references.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"gitlab") && names.contains(&"prometheus"));
        let (_, prom_md) = r
            .references
            .iter()
            .find(|(n, _)| n == "prometheus")
            .unwrap();
        assert!(prom_md.contains("prometheus.query"));
        assert!(prom_md.contains("`query`")); // the required input is surfaced
        let (_, gl_md) = r.references.iter().find(|(n, _)| n == "gitlab").unwrap();
        assert!(gl_md.contains("personal_token") && gl_md.contains("GITLAB_PERSONAL_TOKEN"));
        assert!(gl_md.contains("gitlab.endpoint"));
    }

    #[test]
    fn empty_install_is_handled() {
        let r = render_plugin_skill(&[]);
        let doc: Document<SkillMeta> = parse_frontmatter(&r.skill_md).unwrap();
        assert_eq!(doc.meta.triggers, vec!["plugin".to_string()]);
        assert!(r.references.is_empty());
    }
}
