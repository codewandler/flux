//! `flux-skill` — markdown knowledge packs discovered from skill directories.
//!
//! A skill is a `.md` file (or a directory containing `SKILL.md`) with YAML frontmatter. flux reads
//! **multiple formats**:
//!
//! - **flux-native** — carries optional `triggers` (routing hints retained for compatibility, but
//!   inert: the shipping activation paths are name-based, not keyword-matched — see below).
//! - **Agent Skills** (agentskills.io) / **Claude** — `name` + `description`, optional `license`,
//!   `compatibility`, `metadata`, `allowed-tools`; **no `triggers`**.
//!
//! ```text
//! ---
//! name: rust-style
//! description: How this project writes Rust. Use when editing Rust or running clippy.
//! triggers: [rust, clippy, cargo]   # optional (flux extension)
//! ---
//! <markdown body>
//! ```
//!
//! This crate owns pure parsing only ([`parse`]/[`parse_checked`]) plus the non-fatal naming lint
//! [`validate`]; filesystem discovery is not implemented here. The production discovery path lives
//! in `flux_runtime::metadata` (`discover_skills`/`discover_skills_from`), which reads project and
//! user-global skill directories through a guarded `flux_system::System`/`flux_system::Workspace`
//! boundary and hands this crate's [`parse_checked`] the full file bytes — eagerly, not
//! progressively: there is no lazy/partial body loader here. The five-directory precedence order
//! (project `.flux/skills`, project `.claude/skills`, then user-global `~/.flux/skills`,
//! `~/.agents/skills`, `~/.claude/skills`) lives exactly once, in `flux_runtime::metadata`.
//!
//! Skill *selection* — which discovered skills actually activate for a turn — is the caller's
//! explicit choice (`--skill` / `AgentSpec.skills`, or D-188's opt-in model-invoked catalog); this
//! crate has no keyword-ranking activation helper. [`validate`] is wired into discovery as a
//! load-time lint (D-189), not a gate — an invalid name/description warns but still loads.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which skill-format family a skill was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SkillFormat {
    /// flux-native: carries explicit `triggers`.
    #[default]
    Flux,
    /// The cross-agent Agent Skills spec (agentskills.io); also Claude/opencode. Description-driven.
    AgentSkills,
}

/// A discovered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub body: SkillBody,
    #[serde(default)]
    pub format: SkillFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    /// Claude `allowed-tools`, translated to flux op names via [`translate_allowed_tool`] (D-189).
    /// Empty means the skill declared no `allowed-tools` (or every entry was unmappable) and
    /// therefore imposes no op-surface narrowing. Unmappable entries are dropped here; the
    /// discovery layer that produced this skill reports them as warnings.
    #[serde(default)]
    pub allowed_ops: Vec<String>,
    /// Claude `model` override for turns where this skill is active (D-189). `None` inherits the
    /// caller's resolved model; an explicit CLI/SDK model selection still wins over this (the
    /// caller applies that precedence — this field only carries the skill's request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Claude `disable-model-invocation`: opts this skill out of D-188's model-invoked
    /// (progressive-disclosure) activation. Parsed here so D-188 has somewhere to read it from;
    /// this field has no effect outside that consumer.
    #[serde(default)]
    pub disable_model_invocation: bool,
    /// Claude `argument-hint`: a display hint for the skill's expected arguments, consumed by
    /// D-186/D-187's invocation surfaces. Empty when absent.
    #[serde(default)]
    pub argument_hint: String,
    /// D-187: explicit frontmatter opt-in (`agent-triggerable: true`, default `false`) that lets
    /// the agent invoke this skill itself via `command.invoke` mid-turn. Distinct from
    /// `disable-model-invocation` (D-188's opt-in progressive-disclosure surfacing) — this flag
    /// governs explicit invocation, not passive surfacing. Parsed silently, no warning either way.
    #[serde(default)]
    pub agent_triggerable: bool,
}

/// A skill's markdown body. Discovery in `flux_runtime::metadata` reads the whole file up front, so
/// this is always in-memory text — no lazy/partial loading. Wrapping it (rather than using `String`
/// directly on [`Skill`]) keeps `Skill`'s field type stable if a future disclosure mode needs
/// something richer than a plain string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillBody(String);

impl SkillBody {
    /// An in-memory body.
    pub fn inline(text: impl Into<String>) -> Self {
        SkillBody(text.into())
    }

    /// The body text.
    pub fn text(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for SkillBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for SkillBody {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}
impl PartialEq<&str> for SkillBody {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl From<String> for SkillBody {
    fn from(s: String) -> Self {
        SkillBody(s)
    }
}
impl From<&str> for SkillBody {
    fn from(s: &str) -> Self {
        SkillBody(s.to_string())
    }
}

impl Serialize for SkillBody {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SkillBody {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(SkillBody(String::deserialize(d)?))
    }
}

/// Frontmatter superset covering flux-native + Agent-Skills/Claude. All fields optional so parsing is
/// lenient; `metadata` keeps raw YAML values so a third-party scalar (number/bool) can't fail it.
///
/// Fields fall into three honesty tiers (D-189):
/// - **Honored**: `name`, `description`, `triggers`, `metadata`, `allowed-tools` (translated),
///   `model`, `disable-model-invocation`, `argument-hint`, `agent-triggerable` (D-187) — parsed
///   and acted on.
/// - **Recognized but unsupported**: `context`, `agent`, `hooks`, `license`, `compatibility` —
///   parsed only far enough to detect presence, so [`assemble`] can emit one warning per field
///   naming the skill instead of silently dropping it.
/// - **Truly unknown**: anything else. Lenient parsing already ignores it; there is nothing to
///   name, so it stays silent (as documented in `website/docs/agent/claude-compat.md`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(deserialize_with = "de_string_list")]
    triggers: Vec<String>,
    metadata: BTreeMap<String, serde_norway::Value>,
    #[serde(rename = "allowed-tools", deserialize_with = "de_string_list")]
    allowed_tools: Vec<String>,
    model: Option<String>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: bool,
    #[serde(rename = "argument-hint")]
    argument_hint: String,
    #[serde(rename = "agent-triggerable")]
    agent_triggerable: bool,
    // Recognized-but-unsupported Claude fields (presence-only; value is never used).
    context: Option<serde_norway::Value>,
    agent: Option<serde_norway::Value>,
    hooks: Option<serde_norway::Value>,
    license: Option<serde_norway::Value>,
    compatibility: Option<serde_norway::Value>,
}

/// Accept a YAML list **or** a comma-separated string — shared by `triggers` and `allowed-tools`,
/// both of which Claude and flux authors write either way in practice.
fn de_string_list<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let raw = match Option::<OneOrMany>::deserialize(d)? {
        None => return Ok(Vec::new()),
        Some(OneOrMany::One(s)) => s.split(',').map(str::to_string).collect(),
        Some(OneOrMany::Many(v)) => v,
    };
    Ok(raw
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Explicit Claude tool name → flux op name table for `allowed-tools` (D-189). Only tools with an
/// unambiguous flux equivalent are mapped (see `crates/flux-flow/docs/ops-reference.md` for the op
/// catalog); anything else is unmappable and the caller warns + ignores it rather than guessing.
const ALLOWED_TOOLS_MAP: &[(&str, &str)] = &[
    ("Bash", "bash"),
    ("Edit", "edit"),
    ("Read", "read"),
    ("Grep", "grep"),
    ("Glob", "glob"),
    ("Write", "write"),
    ("WebFetch", "web.fetch"),
    ("WebSearch", "web.search"),
    ("Task", "task"),
];

/// Translate one Claude `allowed-tools` entry to its flux op name, if flux has an unambiguous
/// equivalent (D-189). `None` means the entry is unmappable — the caller should warn and ignore it.
pub fn translate_allowed_tool(claude_tool: &str) -> Option<&'static str> {
    ALLOWED_TOOLS_MAP
        .iter()
        .find(|(name, _)| *name == claude_tool)
        .map(|(_, op)| *op)
}

/// Parse skill content (lenient — malformed frontmatter degrades to a bodyless-frontmatter skill).
/// Frontmatter-honesty warnings (D-189) are discarded; use [`parse_checked`] to see them.
pub fn parse(content: &str, source: Option<PathBuf>) -> Skill {
    parse_checked(content, source).0
}

/// Parse skill content, also returning load-time warnings (D-189): one entry per
/// recognized-but-unsupported frontmatter field present, and one per `allowed-tools` entry with no
/// flux op equivalent. Truly unknown frontmatter keys never produce a warning — there is nothing
/// meaningful to name.
pub fn parse_checked(content: &str, source: Option<PathBuf>) -> (Skill, Vec<String>) {
    let (fm, body) = flux_markdown::split_frontmatter(content);
    let meta: SkillFrontmatter = fm
        .map(|y| serde_norway::from_str(y).unwrap_or_default())
        .unwrap_or_default();
    assemble(meta, SkillBody::inline(body.trim().to_string()), source)
}

/// Build a [`Skill`] (+ D-189 warnings) from parsed frontmatter + a body: name falls back to the
/// source file stem, `metadata.triggers` comma-strings are honored, and the format follows from
/// having triggers.
fn assemble(
    meta: SkillFrontmatter,
    body: SkillBody,
    source: Option<PathBuf>,
) -> (Skill, Vec<String>) {
    let name = if meta.name.is_empty() {
        source
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_string())
    } else {
        meta.name
    };

    // Triggers can also live under `metadata.triggers` as a comma-string (e.g. golang-pro).
    let mut triggers = meta.triggers;
    if triggers.is_empty() {
        if let Some(s) = meta.metadata.get("triggers").and_then(|v| v.as_str()) {
            triggers = s
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }

    let format = if triggers.is_empty() {
        SkillFormat::AgentSkills
    } else {
        SkillFormat::Flux
    };

    let mut warnings = Vec::new();
    for (present, field) in [
        (meta.context.is_some(), "context"),
        (meta.agent.is_some(), "agent"),
        (meta.hooks.is_some(), "hooks"),
        (meta.license.is_some(), "license"),
        (meta.compatibility.is_some(), "compatibility"),
    ] {
        if present {
            warnings.push(format!(
                "skill `{name}`: ignoring unsupported frontmatter field `{field}`"
            ));
        }
    }

    let mut allowed_ops = Vec::new();
    for raw in &meta.allowed_tools {
        match translate_allowed_tool(raw) {
            Some(op) => {
                let op = op.to_string();
                if !allowed_ops.contains(&op) {
                    allowed_ops.push(op);
                }
            }
            None => warnings.push(format!(
                "skill `{name}`: allowed-tools entry `{raw}` has no flux op equivalent; ignoring"
            )),
        }
    }

    let skill = Skill {
        name,
        description: meta.description,
        triggers,
        body,
        format,
        source,
        allowed_ops,
        model: meta.model.filter(|m| !m.is_empty()),
        disable_model_invocation: meta.disable_model_invocation,
        argument_hint: meta.argument_hint,
        agent_triggerable: meta.agent_triggerable,
    };
    (skill, warnings)
}

/// Validate a skill's `name`/`description` against the Agent Skills naming rules. Returns
/// human-readable issues (empty = valid). **Non-fatal**: this crate never calls it itself, but
/// `flux_runtime::metadata`'s production discovery wires it in as a load-time lint (D-189) — an
/// invalid name/description warns, it does not fail discovery.
pub fn validate(skill: &Skill, expected_dir: Option<&str>) -> Vec<String> {
    let mut issues = Vec::new();
    let n = &skill.name;
    if n.is_empty() || n.chars().count() > 64 {
        issues.push(format!(
            "name must be 1-64 characters (got {})",
            n.chars().count()
        ));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        issues.push("name may only contain lowercase letters, digits, and hyphens".into());
    }
    if n.starts_with('-') || n.ends_with('-') {
        issues.push("name must not start or end with a hyphen".into());
    }
    if n.contains("--") {
        issues.push("name must not contain consecutive hyphens".into());
    }
    if let Some(dir) = expected_dir {
        if dir != n {
            issues.push(format!("name `{n}` must match its directory `{dir}`"));
        }
    }
    if skill.description.is_empty() {
        issues.push("description must be non-empty".into());
    }
    if skill.description.chars().count() > 1024 {
        issues.push("description must be at most 1024 characters".into());
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let content = "---\nname: rust-style\ndescription: How we write Rust\ntriggers: [rust, clippy]\n---\nUse tabs.\n";
        let s = parse(content, None);
        assert_eq!(s.name, "rust-style");
        assert_eq!(s.description, "How we write Rust");
        assert_eq!(s.triggers, vec!["rust", "clippy"]);
        assert_eq!(s.body, "Use tabs.");
        assert_eq!(s.format, SkillFormat::Flux);
    }

    #[test]
    fn lenient_without_frontmatter() {
        let s = parse("just a body", Some(PathBuf::from("/x/notes.md")));
        assert_eq!(s.name, "notes");
        assert_eq!(s.body, "just a body");
        assert!(s.triggers.is_empty());
    }

    #[test]
    fn claude_format_without_triggers_activates_via_description() {
        // An Agent-Skills/Claude skill: name + description, no triggers.
        let content = "---\nname: axon\ndescription: Use Axon CLI to index directories and query graph data with AQL\nlicense: MIT\ncompatibility: opencode\n---\nIndex a directory.";
        let s = parse(content, None);
        assert_eq!(s.format, SkillFormat::AgentSkills);
        assert!(s.triggers.is_empty());
    }

    #[test]
    fn nested_metadata_triggers_are_picked_up() {
        // golang-pro stuffs triggers under `metadata:` as a comma string.
        let content = "---\nname: golang-pro\ndescription: Go specialist\nmetadata:\n  version: \"1.0.0\"\n  triggers: Go, Golang, goroutines, gRPC\n---\nbody";
        let s = parse(content, None);
        assert!(s.triggers.contains(&"Golang".to_string()));
        assert!(s.triggers.contains(&"goroutines".to_string()));
        assert_eq!(s.format, SkillFormat::Flux);
    }

    #[test]
    fn validate_flags_bad_name() {
        let s = parse("---\nname: Bad--Name\ndescription: x\n---\nb", None);
        let issues = validate(&s, None);
        assert!(
            !issues.is_empty(),
            "uppercase + consecutive hyphens should fail"
        );
    }

    #[test]
    fn validate_flags_oversized_description() {
        let long_desc = "x".repeat(1025);
        let s = parse(
            &format!("---\nname: ok-name\ndescription: {long_desc}\n---\nb"),
            None,
        );
        let issues = validate(&s, None);
        assert!(
            issues.iter().any(|i| i.contains("1024")),
            "oversized description should fail: {issues:?}"
        );
    }

    /// D-189: a recognized-but-unsupported Claude field (`context`, `hooks`, `license`,
    /// `compatibility`) produces exactly one warning naming the skill and the field, instead of
    /// vanishing silently in serde.
    #[test]
    fn unsupported_frontmatter_fields_warn_by_name() {
        let content = "---\nname: rust-style\ndescription: d\ncontext: something\nhooks:\n  pre: x\nlicense: MIT\ncompatibility: opencode\n---\nbody";
        let (skill, warnings) = parse_checked(content, None);
        assert_eq!(skill.name, "rust-style");
        for field in ["context", "hooks", "license", "compatibility"] {
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains("rust-style") && w.contains(field)),
                "expected a warning naming `{field}`: {warnings:?}"
            );
        }
    }

    /// Truly unknown frontmatter keys (no Claude/flux meaning at all) stay silent — there's nothing
    /// meaningful to name.
    #[test]
    fn truly_unknown_frontmatter_field_stays_silent() {
        let content = "---\nname: x\ndescription: d\ntotally-made-up-field: 1\n---\nbody";
        let (_, warnings) = parse_checked(content, None);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// D-189: `allowed-tools` translates Claude tool names to flux op names; unmappable entries
    /// warn and are dropped rather than silently kept or silently failing the whole list.
    #[test]
    fn allowed_tools_translates_known_and_warns_on_unmappable() {
        let content = "---\nname: reviewer\ndescription: d\nallowed-tools: Bash, Read, NotARealTool\n---\nbody";
        let (skill, warnings) = parse_checked(content, None);
        assert_eq!(
            skill.allowed_ops,
            vec!["bash".to_string(), "read".to_string()]
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("NotARealTool") && w.contains("reviewer")),
            "{warnings:?}"
        );
    }

    /// D-189: `model` parses onto the skill with no warning (it's honored, not unsupported).
    #[test]
    fn model_field_parses_without_warning() {
        let content = "---\nname: fast-triage\ndescription: d\nmodel: haiku\n---\nbody";
        let (skill, warnings) = parse_checked(content, None);
        assert_eq!(skill.model.as_deref(), Some("haiku"));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// D-189: `disable-model-invocation` and `argument-hint` parse onto the skill silently —
    /// consumed by D-188/D-186, not warned about here.
    #[test]
    fn disable_model_invocation_and_argument_hint_parse_silently() {
        let content = "---\nname: manual-only\ndescription: d\ndisable-model-invocation: true\nargument-hint: \"<file>\"\n---\nbody";
        let (skill, warnings) = parse_checked(content, None);
        assert!(skill.disable_model_invocation);
        assert_eq!(skill.argument_hint, "<file>");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// D-187: `agent-triggerable` is a known, honored field — parsed silently and defaults to
    /// `false` when absent, distinct from `disable-model-invocation` (D-188's surfacing opt-out).
    #[test]
    fn agent_triggerable_flag_parses_silently_and_defaults_false() {
        let (human_only, warnings) =
            parse_checked("---\nname: human\ndescription: d\n---\nbody", None);
        assert!(!human_only.agent_triggerable);
        assert!(warnings.is_empty(), "{warnings:?}");

        let (agentic, warnings) = parse_checked(
            "---\nname: agentic\ndescription: d\nagent-triggerable: true\n---\nbody",
            None,
        );
        assert!(agentic.agent_triggerable);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
