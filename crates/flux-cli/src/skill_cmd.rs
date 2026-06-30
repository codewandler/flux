//! Generated Claude-format flux skills (`flux skill ...`).
//!
//! These renderers intentionally read from live command/spec catalogs instead of hand-maintained docs:
//! Clap owns the CLI surface, `flux-lang` owns the language skill body, `ToolRegistry`/`OpRegistry`
//! own host ops, and plugin manifests own plugin operations.

use std::collections::BTreeMap;

use clap::{Arg, ArgAction, Command, ValueEnum};
use flux_evidence::ToolGroup;
use flux_flow::registry::OpRegistry;
use flux_runtime::ToolRegistry;
use flux_spec::ToolSpec;
use serde::Serialize;

use crate::plugin_skill;

/// The generated section skills supported by `flux skill <type>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SkillType {
    Cli,
    Lang,
    Plugin,
    Ops,
}

impl SkillType {
    pub fn skill_name(self) -> &'static str {
        match self {
            Self::Cli => "flux-cli",
            Self::Lang => "flux-lang",
            Self::Plugin => "flux-plugin",
            Self::Ops => "flux-ops",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Cli => "CLI",
            Self::Lang => "Flux-Lang",
            Self::Plugin => "Plugins",
            Self::Ops => "Operations",
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Cli, Self::Lang, Self::Plugin, Self::Ops]
    }
}

/// A generated skill directory: `SKILL.md` plus optional `references/*.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSkill {
    pub name: String,
    pub skill_md: String,
    pub references: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct ClaudeSkillMeta {
    name: String,
    description: String,
}

fn render_claude_skill(name: &str, description: &str, body: &str) -> String {
    let meta = ClaudeSkillMeta {
        name: name.to_string(),
        description: description.to_string(),
    };
    flux_markdown::render_document(&meta, body)
        .unwrap_or_else(|e| format!("---\n# frontmatter render failed: {e}\n---\n{body}"))
}

fn rendered(name: &str, description: &str, body: String) -> RenderedSkill {
    RenderedSkill {
        name: name.to_string(),
        skill_md: render_claude_skill(name, description, &body),
        references: Vec::new(),
    }
}

/// Render the small root skill that routes agents to the generated section skills.
pub fn render_root_skill() -> RenderedSkill {
    let mut body = String::new();
    body.push_str("# Flux skill index\n\n");
    body.push_str(
        "Use this root skill to discover Flux-specific skills and install only the section needed \
         for the current task. The section skills are generated from live source-of-truth catalogs; \
         rerun the install command to refresh them after Flux changes.\n\n",
    );
    body.push_str("## Sections\n\n");
    body.push_str("| Need | Skill | Render | Install |\n");
    body.push_str("|---|---|---|---|\n");
    for section in SkillType::all() {
        body.push_str(&format!(
            "| {} | `{}` | `flux skill {}` | `flux skill {} --install` |\n",
            section.label(),
            section.skill_name(),
            section.value_variants_name(),
            section.value_variants_name()
        ));
    }
    body.push_str(
        "\nUse `flux skill --install` to install the root plus every section skill. Use \
         `flux skill <type> --install` to install the root plus one section.\n",
    );
    rendered(
        "flux",
        "Root index for generated Flux skills: CLI, Flux-Lang, plugin, and operation references.",
        body,
    )
}

trait SkillTypeName {
    fn value_variants_name(self) -> &'static str;
}

impl SkillTypeName for SkillType {
    fn value_variants_name(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Lang => "lang",
            Self::Plugin => "plugin",
            Self::Ops => "ops",
        }
    }
}

/// Render the CLI skill from the live Clap command tree.
pub fn render_cli_skill(mut cmd: Command) -> RenderedSkill {
    cmd.build();

    let mut body = String::new();
    body.push_str("# Flux CLI\n\n");
    body.push_str(
        "This skill is generated from Flux's Clap command tree. Treat `flux --help` and \
         `flux <command> --help` as the runtime source of truth when examples drift.\n\n",
    );
    body.push_str("## Top-level commands\n\n");
    body.push_str("| Command | Summary |\n");
    body.push_str("|---|---|\n");
    for sub in visible_subcommands(&cmd) {
        body.push_str(&format!(
            "| `flux {}` | {} |\n",
            cell(sub.get_name()),
            cell(&command_help(sub))
        ));
    }
    body.push_str("\n## Command reference\n");
    render_command(&cmd, "flux".to_string(), 0, &mut body);

    rendered(
        "flux-cli",
        "Use when invoking or documenting the Flux command-line interface generated from Clap.",
        body,
    )
}

fn render_command(cmd: &Command, path: String, depth: usize, out: &mut String) {
    if depth > 0 {
        out.push_str(&format!("\n### `{}`\n\n", cell(&path)));
        let help = command_help(cmd);
        if !help.is_empty() {
            out.push_str(&help);
            out.push_str("\n\n");
        }
        render_args(cmd, out);
    }

    for sub in visible_subcommands(cmd) {
        render_command(sub, format!("{path} {}", sub.get_name()), depth + 1, out);
    }
}

fn render_args(cmd: &Command, out: &mut String) {
    let args: Vec<&Arg> = cmd.get_arguments().filter(|a| !a.is_hide_set()).collect();
    if args.is_empty() {
        return;
    }
    out.push_str("| Argument | Required | Help |\n");
    out.push_str("|---|---:|---|\n");
    for arg in args {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            cell(&arg_label(arg)),
            if arg.is_required_set() { "yes" } else { "no" },
            cell(&arg_help(arg))
        ));
    }
    out.push('\n');
}

fn visible_subcommands(cmd: &Command) -> Vec<&Command> {
    cmd.get_subcommands().filter(|s| !s.is_hide_set()).collect()
}

fn command_help(cmd: &Command) -> String {
    cmd.get_long_about()
        .or_else(|| cmd.get_about())
        .map(|s| s.to_string())
        .unwrap_or_default()
        .replace('\n', " ")
}

fn arg_help(arg: &Arg) -> String {
    arg.get_long_help()
        .or_else(|| arg.get_help())
        .map(|s| s.to_string())
        .unwrap_or_default()
        .replace('\n', " ")
}

fn arg_label(arg: &Arg) -> String {
    let mut parts = Vec::new();
    if let Some(short) = arg.get_short() {
        parts.push(format!("-{short}"));
    }
    if let Some(long) = arg.get_long() {
        parts.push(format!("--{long}"));
    }
    if parts.is_empty() {
        return format!("<{}>", arg.get_id());
    }
    let mut label = parts.join(", ");
    if let Some(values) = arg.get_value_names() {
        let names = values
            .iter()
            .map(|v| format!("<{v}>"))
            .collect::<Vec<_>>()
            .join(" ");
        if !names.is_empty() {
            label.push(' ');
            label.push_str(&names);
        }
    } else if takes_value(arg) {
        label.push_str(&format!(" <{}>", arg.get_id()));
    }
    label
}

fn takes_value(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
}

/// Render the language skill from the `flux-lang` generated skill body.
pub fn render_lang_skill() -> RenderedSkill {
    let generated = flux_lang::skill::render();
    let (_, body) = flux_markdown::split_frontmatter(&generated);
    rendered(
        "flux-lang",
        "Use when authoring Flux-Lang plans, text syntax, AST nodes, and deterministic execution graphs.",
        body.trim().to_string(),
    )
}

/// Render the operation skill from the live registry adapter used by the planner/analyzer.
pub fn render_ops_skill(registry: &ToolRegistry, groups: &[ToolGroup]) -> RenderedSkill {
    let specs_by_name: BTreeMap<String, ToolSpec> = registry
        .specs()
        .into_iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect();
    let ops = OpRegistry::new(registry);
    let mut signatures = ops.signatures();
    signatures.sort_by(|a, b| a.name.cmp(&b.name));

    let mut body = String::new();
    body.push_str("# Flux operations\n\n");
    body.push_str(
        "This skill is generated from the registered `ToolRegistry` via `OpRegistry`. It is the \
         operation catalog Flux-Lang plans can call; do not copy operation tables from docs when this \
         command is available.\n\n",
    );
    body.push_str("## Operation catalog\n\n");
    body.push_str(
        "| Operation | Description | Effects | Risk | Idempotency | Access | Surfacing |\n",
    );
    body.push_str("|---|---|---|---|---|---|---|\n");
    for sig in signatures {
        let Some(spec) = specs_by_name.get(&sig.name) else {
            continue;
        };
        body.push_str(&format!(
            "| `{}` | {} | {} | {:?} | {:?} | {} | {} |\n",
            cell(&format!("{}({})", sig.name, sig.param_signature())),
            cell(&sig.description),
            cell(&debug_list(&spec.effects)),
            spec.risk,
            spec.idempotency,
            cell(&debug_list(&spec.access)),
            cell(&surfacing(spec, groups))
        ));
    }
    body.push_str(
        "\nGrouped ops are registered but advertised to the model only when their evidence signal is \
         active. Core ops have no group and are always available to authored flows.\n",
    );

    rendered(
        "flux-ops",
        "Use when selecting Flux-Lang operations from the live registered operation catalog.",
        body,
    )
}

/// Render the installed-plugin skill from manifest data.
pub fn render_plugin_skill(plugins: &[(String, flux_plugin::PluginManifest)]) -> RenderedSkill {
    let generated = plugin_skill::render_plugin_skill(plugins);
    RenderedSkill {
        name: "flux-plugin".to_string(),
        skill_md: generated.skill_md,
        references: generated.references,
    }
}

fn surfacing(spec: &ToolSpec, groups: &[ToolGroup]) -> String {
    if let Some(group) = groups
        .iter()
        .find(|g| g.tools.iter().any(|tool| tool == &spec.name))
    {
        return group_surface(group);
    }
    if let Some(name) = spec.group.as_deref() {
        if let Some(group) = groups.iter().find(|g| g.name == name) {
            return group_surface(group);
        }
        return format!("group `{name}` (not advertised by built-in signals)");
    }
    "core".to_string()
}

fn group_surface(group: &ToolGroup) -> String {
    if group.surface_when.is_empty() {
        return format!("group `{}` (always)", group.name);
    }
    let signals = group
        .surface_when
        .iter()
        .map(|m| {
            m.signal
                .as_deref()
                .map(|s| format!("signal `{s}`"))
                .unwrap_or_else(|| format!("kind `{}`", m.kind))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("group `{}` when {signals}", group.name)
}

fn debug_list<T: std::fmt::Debug>(items: &[T]) -> String {
    if items.is_empty() {
        return "-".to_string();
    }
    items
        .iter()
        .map(|item| format!("{item:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use clap::{Arg, Command};
    use flux_markdown::{parse_frontmatter, Document};
    use flux_runtime::Tool;
    use flux_spec::{AccessKind, Effect, Risk, ToolSpec};
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Meta {
        name: String,
        description: String,
        #[serde(default)]
        triggers: Vec<String>,
    }

    struct FixtureTool;

    #[async_trait::async_trait]
    impl Tool for FixtureTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "fixture_op",
                "Read fixture data",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "limit": {"type": "integer"}
                    },
                    "required": ["path"]
                }),
            )
            .with_effects(vec![Effect::Read])
            .with_access(vec![AccessKind::Filesystem])
            .with_risk(Risk::Low)
        }

        async fn execute(
            &self,
            _ctx: &flux_runtime::ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<flux_runtime::ToolResult> {
            Ok(flux_runtime::ToolResult::ok("ok"))
        }
    }

    #[test]
    fn root_skill_is_claude_format_and_routes_sections() {
        let skill = render_root_skill();
        let doc: Document<Meta> = parse_frontmatter(&skill.skill_md).unwrap();
        assert_eq!(doc.meta.name, "flux");
        assert!(doc.meta.description.contains("generated Flux skills"));
        assert!(
            doc.meta.triggers.is_empty(),
            "Claude format has no triggers"
        );
        assert!(doc.body.contains("flux skill cli --install"));
        assert!(doc.body.contains("flux-ops"));
    }

    #[test]
    fn cli_skill_reflects_clap_command_tree() {
        let cmd = Command::new("flux").about("fixture").subcommand(
            Command::new("demo").about("Run demo").arg(
                Arg::new("name")
                    .long("name")
                    .help("Demo name")
                    .required(true),
            ),
        );
        let skill = render_cli_skill(cmd);
        let doc: Document<Meta> = parse_frontmatter(&skill.skill_md).unwrap();
        assert_eq!(doc.meta.name, "flux-cli");
        assert!(doc.body.contains("`flux demo`"));
        assert!(doc.body.contains("`--name <name>`"));
        assert!(doc.body.contains("Demo name"));
    }

    #[test]
    fn lang_skill_rewraps_generated_language_body_as_claude_format() {
        let skill = render_lang_skill();
        let doc: Document<Meta> = parse_frontmatter(&skill.skill_md).unwrap();
        assert_eq!(doc.meta.name, "flux-lang");
        assert!(doc.meta.triggers.is_empty());
        assert!(doc.body.contains("<!-- BEGIN generated:node-kinds -->"));
        assert!(doc.body.contains("# Flux-Lang"));
    }

    #[test]
    fn ops_skill_uses_registered_specs_and_group_metadata() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FixtureTool));
        let groups = vec![ToolGroup {
            name: "fixture".into(),
            description: "Fixture ops".into(),
            tools: vec!["fixture_op".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_SIGNAL.into(),
                signal: Some("fixture".into()),
            }],
        }];
        let skill = render_ops_skill(&registry, &groups);
        let doc: Document<Meta> = parse_frontmatter(&skill.skill_md).unwrap();
        assert_eq!(doc.meta.name, "flux-ops");
        assert!(doc.body.contains("`fixture_op(path[, limit])`"));
        assert!(doc.body.contains("Filesystem"));
        assert!(doc.body.contains("group `fixture` when signal `fixture`"));
    }
}
