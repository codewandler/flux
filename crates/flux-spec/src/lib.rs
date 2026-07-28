//! `flux-spec` — the inert description of a tool/operation (pure, no IO; distilled from
//! `fluxplane-operation`).
//!
//! A [`ToolSpec`] declares a tool's typed I/O (JSON Schema), its side [`Effect`]s, [`Risk`],
//! [`Idempotency`], and the [`AccessKind`]s it needs. Before execution a tool also derives an
//! [`IntentSet`] — concrete (target, behavior) pairs the runtime classifies for approval. None of
//! this performs IO; the runtime maps these specs onto policy requests and the safety envelope.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod schema;
pub use schema::{empty_schema, object_schema, opt, req, Field, FieldType};

/// Generate the provider-facing JSON Schema for a typed tool input.
///
/// Tool schemas are an API contract with models/providers, so keep them derived from Rust input
/// structs rather than hand-maintained JSON. The returned value intentionally omits the root
/// meta-schema/title noise while preserving definitions and doc-comment descriptions.
pub fn tool_input_schema<T: schemars::JsonSchema + 'static>() -> serde_json::Value {
    // A tool's input schema is a compile-time constant for its `T`, but `schema_for!` is a
    // non-trivial reflective build (a recursive type like the plan AST expands to dozens of
    // subschemas). Memoize it so the reflection runs once per type instead of on every call — this
    // matters on the model-stage hot path, where the op catalog re-derives every tool's spec and each op
    // resolution rebuilds a signature. NOTE: a `static` inside a generic fn is SHARED across all
    // monomorphizations (not one-per-`T`), so the cache must be keyed by `TypeId::of::<T>()` — a bare
    // `OnceLock<Value>` here would return the first-built type's schema for every tool.
    use std::any::TypeId;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<TypeId, serde_json::Value>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = TypeId::of::<T>();
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return v.clone();
    }
    let mut schema =
        serde_json::to_value(schemars::schema_for!(T)).expect("tool input schema serializes");
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        // The op's own `description` carries its purpose; a struct doc-comment would land here as a
        // redundant top-level description, so drop it. Field descriptions (under `properties`) stay.
        obj.remove("description");
    }
    cache.lock().unwrap().insert(key, schema.clone());
    schema
}

/// Generate an operation output schema while retaining its analyzer-visible type identity.
///
/// Provider JSON Schema has no standard keyword for the Rust/schema name once the root `title` is
/// removed. Flux stores that non-authority metadata under `x-flux-type`; providers ignore the
/// extension, while the Flux-Lang catalog can recover a named result type for typed stage
/// composition. Primitive and list schemas keep their structural type regardless of this marker.
pub fn tool_output_schema<T: schemars::JsonSchema + 'static>() -> serde_json::Value {
    let mut schema = tool_input_schema::<T>();
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "x-flux-type".into(),
            serde_json::Value::String(T::schema_name().into_owned()),
        );
    }
    schema
}

/// A side effect a tool may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
    Network,
    Process,
    Browser,
    Filesystem,
    LocalSystem,
}

/// A first-class *semantic* effect, declared on operations. Distinct from the host-resource
/// [`Effect`] (Read/Write/Network/…): a `FlowEffect` expresses execution *meaning* (this op sends
/// mail, costs money, touches a calendar) and lowers onto the host effect + a policy action via
/// [`FlowEffect::lower`]. Policy decides allow / deny / require-approval.
///
/// Lives here rather than in `flux-lang` because it is part of the **plugin wire vocabulary**
/// (`PluginManifest`'s `semantic_effects`), and a guest plugin must be able to name it without
/// compiling the language front-end (C-141). `flux_lang::ast` re-exports it, so the
/// `flux_lang::ast::FlowEffect` path used across the codebase keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowEffect {
    /// No effect — deterministic, side-effect free.
    Pure,
    /// Reads external state.
    Read,
    /// Invokes a model (non-deterministic unless cached).
    Model,
    /// General network egress.
    Network,
    /// Writes to the filesystem.
    WriteFile,
    /// Writes to a database / persistent store.
    WriteDb,
    /// Sends something externally (email, message, webhook).
    SendExternal,
    /// Irreversibly deletes.
    Delete,
    /// Moves money.
    Money,
    /// Mutates a calendar.
    Calendar,
    /// Produces output a human will see.
    HumanVisible,
}

impl FlowEffect {
    /// The stable lowercase tag for this semantic effect — matches the serde `snake_case` wire tag
    /// and the `!tag` bind/memo syntax (see [`FlowEffect::from_tag`] for the inverse). The single
    /// source of truth for the tag vocabulary: `format`/`render`'s pretty-printers and `parse`'s
    /// `@effect`/`!tag` readers all resolve through this pair rather than keeping their own tables.
    pub fn tag(self) -> &'static str {
        match self {
            FlowEffect::Pure => "pure",
            FlowEffect::Read => "read",
            FlowEffect::Model => "model",
            FlowEffect::Network => "network",
            FlowEffect::WriteFile => "write_file",
            FlowEffect::WriteDb => "write_db",
            FlowEffect::SendExternal => "send_external",
            FlowEffect::Delete => "delete",
            FlowEffect::Money => "money",
            FlowEffect::Calendar => "calendar",
            FlowEffect::HumanVisible => "human_visible",
        }
    }

    /// Parse a semantic-effect tag (the inverse of [`FlowEffect::tag`]), or `None` for an unknown
    /// tag. Used both by the text-syntax reader and by anything reconstructing a [`FlowEffect`] from
    /// a catalog's serialized tag strings (e.g. a plugin-manifest-declared op, D-138).
    pub fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "pure" => FlowEffect::Pure,
            "read" => FlowEffect::Read,
            "model" => FlowEffect::Model,
            "network" => FlowEffect::Network,
            "write_file" => FlowEffect::WriteFile,
            "write_db" => FlowEffect::WriteDb,
            "send_external" => FlowEffect::SendExternal,
            "delete" => FlowEffect::Delete,
            "money" => FlowEffect::Money,
            "calendar" => FlowEffect::Calendar,
            "human_visible" => FlowEffect::HumanVisible,
            _ => return None,
        })
    }

    /// Lower this semantic effect to the host-resource [`Effect`] it implies (if any) and the
    /// additional policy [`Action`](flux_policy::Action) it requires (if any).
    ///
    /// Host effects reuse the existing `effect_requests` bridge in `flux-runtime`; semantic-only
    /// effects (`SendExternal`, `Money`, `Calendar`, …) carry a dedicated `flow.*` action a policy
    /// `Grant` can allow / deny / require-approval (`flow.delete` / `flow.money` are denied by
    /// default in policy).
    pub fn lower(self) -> (Option<Effect>, Option<flux_policy::Action>) {
        use flux_policy::Action;
        use FlowEffect::*;
        match self {
            Pure => (None, None),
            Read => (Some(Effect::Read), None),
            Model => (None, Some(Action::from("model.invoke"))),
            Network => (Some(Effect::Network), None),
            WriteFile => (Some(Effect::Write), None),
            WriteDb => (Some(Effect::Network), Some(Action::from("flow.write_db"))),
            SendExternal => (
                Some(Effect::Network),
                Some(Action::from("flow.send_external")),
            ),
            Delete => (Some(Effect::Write), Some(Action::from("flow.delete"))),
            Money => (None, Some(Action::from("flow.money"))),
            Calendar => (None, Some(Action::from("flow.calendar"))),
            HumanVisible => (None, None),
        }
    }
}

/// Coarse risk classification, driving approval thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
    Destructive,
}

/// Whether repeating the operation is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    Idempotent,
    NonIdempotent,
    Conditional,
}

/// How an operation participates in an adaptive agent turn before approval.
///
/// This is a routing hint, not authority. The runtime still derives concrete intents and refuses
/// to gather any mutating, destructive, risky, or non-idempotent invocation even when an operation
/// declares [`Gather`](Self::Gather).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingDisposition {
    /// Infer from effects, risk, idempotency, and the invocation's concrete intents.
    #[default]
    Infer,
    /// Prefer immediate exploration when the ordinary safety contract also permits it.
    Gather,
    /// Capture the invocation into the proposed action batch; never run it during exploration.
    Capture,
}

/// A host capability the tool needs access to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Auth,
    Secret,
    Network,
    Connection,
    Datasource,
    Provider,
    Process,
    Browser,
    Filesystem,
    LocalSystem,
}

/// The inert specification of a tool/operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's input object.
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    pub risk: Risk,
    pub idempotency: Idempotency,
    #[serde(default)]
    pub access: Vec<AccessKind>,
    /// The evidence-gated tool group this op belongs to. `None` means *core* — always advertised to
    /// the model. A named group is only surfaced into the model-facing catalog when the workspace
    /// shows evidence the group is relevant (see `flux-runtime`'s group resolver).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl ToolSpec {
    /// A minimal read-only, idempotent spec — the safe default for query tools.
    pub fn read_only(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            output_schema: None,
            effects: vec![Effect::Read],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: Vec::new(),
            group: None,
        }
    }

    /// A read-only spec whose input schema is derived from the Rust input type.
    pub fn read_only_typed<T: schemars::JsonSchema + 'static>(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::read_only(name, description, tool_input_schema::<T>())
    }

    pub fn with_risk(mut self, risk: Risk) -> Self {
        self.risk = risk;
        self
    }

    pub fn with_effects(mut self, effects: Vec<Effect>) -> Self {
        self.effects = effects;
        self
    }

    pub fn with_access(mut self, access: Vec<AccessKind>) -> Self {
        self.access = access;
        self
    }

    /// Declare the operation's result schema for typed Flux-Lang analysis and SDK stages.
    pub fn with_output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Assign this op to an evidence-gated tool group (e.g. `"git"`). Ungrouped ops are *core* and
    /// always advertised; a grouped op only appears in the catalog when its group is surfaced.
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn has_effect(&self, e: Effect) -> bool {
        self.effects.contains(&e)
    }
}

// ---------------------------------------------------------------------------
// Intent (pre-execution risk signal)
// ---------------------------------------------------------------------------

/// What a derived intent will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentBehavior {
    CommandExecution,
    FilesystemRead,
    FilesystemWrite,
    NetworkFetch,
    NetworkConnect,
    BrowserNavigate,
}

/// The concrete target of an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IntentTarget {
    Path { path: String },
    Url { url: String },
    Process { command: String },
    Browser { url: String },
}

/// The role the target plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentRole {
    ReadTarget,
    WriteTarget,
    ProcessCommand,
}

/// How sure we are the intent will actually occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentCertainty {
    Certain,
    Potential,
}

/// A single declared intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub behavior: IntentBehavior,
    pub target: IntentTarget,
    pub role: IntentRole,
    pub certainty: IntentCertainty,
}

/// The set of intents a tool invocation will (or may) perform — the runtime's pre-execution
/// risk signal for approval gating.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentSet {
    pub intents: Vec<Intent>,
}

impl IntentSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, intent: Intent) {
        self.intents.push(intent);
    }

    /// True if any intent writes to the filesystem, executes a command, or is destructive-shaped.
    pub fn is_mutating(&self) -> bool {
        self.intents.iter().any(|i| {
            matches!(
                i.behavior,
                IntentBehavior::FilesystemWrite
                    | IntentBehavior::CommandExecution
                    | IntentBehavior::NetworkConnect
            )
        })
    }

    /// True if any process-execution intent targets a command matching a destructive heuristic
    /// (see [`is_destructive_command`]). Such operations are forced to human approval even when a
    /// permissive allow-rule would otherwise cover them.
    pub fn is_destructive(&self) -> bool {
        self.intents.iter().any(|i| match &i.target {
            IntentTarget::Process { command } => is_destructive_command(command),
            _ => false,
        })
    }
}

/// Heuristic match for shell commands that are irreversible or system-altering and should always
/// require explicit approval (e.g. recursive deletes, disk/format ops, force-pushes, fork bombs).
/// Conservative by design — a false negative degrades to the normal approval path, never silent.
pub fn is_destructive_command(command: &str) -> bool {
    let c = command.to_lowercase();
    const PATTERNS: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "rm -r ",
        "rm --recursive",
        ":(){", // fork bomb
        "mkfs",
        "dd if=",
        "dd of=",
        "> /dev/sd",
        "shutdown",
        "reboot",
        "chmod -r 777",
        "git push --force",
        "git push -f",
        "truncate -s 0",
    ];
    PATTERNS.iter().any(|p| c.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct SampleInput {
        /// Path to inspect.
        path: String,
    }

    #[test]
    fn tool_input_schema_preserves_struct_shape_without_root_noise() {
        let schema = tool_input_schema::<SampleInput>();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("$schema").is_none());
        assert!(schema.get("title").is_none());
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(
            schema["properties"]["path"]["description"],
            "Path to inspect."
        );
    }

    #[test]
    fn tool_output_schema_retains_type_identity_without_restoring_root_noise() {
        let schema = tool_output_schema::<SampleInput>();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["x-flux-type"], "SampleInput");
        assert!(schema.get("title").is_none());
    }

    #[test]
    fn read_only_typed_uses_derived_schema() {
        let spec = ToolSpec::read_only_typed::<SampleInput>("sample", "sample op");
        assert_eq!(spec.name, "sample");
        assert_eq!(spec.input_schema["required"], json!(["path"]));
        assert!(spec.has_effect(Effect::Read));
    }

    #[test]
    fn read_only_spec_defaults() {
        let s = ToolSpec::read_only("read", "read a file", json!({"type": "object"}));
        assert_eq!(s.risk, Risk::Low);
        assert_eq!(s.idempotency, Idempotency::Idempotent);
        assert!(s.has_effect(Effect::Read));
        assert!(!s.has_effect(Effect::Write));
    }

    #[test]
    fn builders_compose() {
        let s = ToolSpec::read_only("bash", "run a command", json!({"type": "object"}))
            .with_risk(Risk::High)
            .with_effects(vec![Effect::Process, Effect::LocalSystem])
            .with_access(vec![AccessKind::Process]);
        assert_eq!(s.risk, Risk::High);
        assert!(s.has_effect(Effect::Process));
        assert_eq!(s.access, vec![AccessKind::Process]);
    }

    #[test]
    fn intent_set_detects_mutation_and_roundtrips() {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::FilesystemWrite,
            target: IntentTarget::Path {
                path: "src/main.rs".into(),
            },
            role: IntentRole::WriteTarget,
            certainty: IntentCertainty::Certain,
        });
        assert!(set.is_mutating());

        let s = serde_json::to_string(&set).unwrap();
        let back: IntentSet = serde_json::from_str(&s).unwrap();
        assert_eq!(set, back);
    }

    #[test]
    fn detects_destructive_commands() {
        assert!(is_destructive_command("rm -rf /"));
        assert!(is_destructive_command("sudo  rm -rf  node_modules"));
        assert!(is_destructive_command("git push --force origin main"));
        assert!(is_destructive_command("mkfs.ext4 /dev/sda1"));
        assert!(!is_destructive_command("ls -la"));
        assert!(!is_destructive_command("git status"));
        assert!(!is_destructive_command("rm file.txt"));
    }

    #[test]
    fn intent_set_flags_destructive_process() {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "rm -rf build".into(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        assert!(set.is_destructive());
        assert!(set.is_mutating());

        let mut safe = IntentSet::new();
        safe.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "cargo build".into(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        assert!(!safe.is_destructive());
    }

    #[test]
    fn read_intent_is_not_mutating() {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::FilesystemRead,
            target: IntentTarget::Path {
                path: "README.md".into(),
            },
            role: IntentRole::ReadTarget,
            certainty: IntentCertainty::Certain,
        });
        assert!(!set.is_mutating());
    }
}
