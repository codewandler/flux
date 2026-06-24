//! `flux-spec` — the inert description of a tool/operation (pure, no IO; distilled from
//! `fluxplane-operation`).
//!
//! A [`ToolSpec`] declares a tool's typed I/O (JSON Schema), its side [`Effect`]s, [`Risk`],
//! [`Idempotency`], and the [`AccessKind`]s it needs. Before execution a tool also derives an
//! [`IntentSet`] — concrete (target, behavior) pairs the runtime classifies for approval. None of
//! this performs IO; the runtime maps these specs onto policy requests and the safety envelope.

use serde::{Deserialize, Serialize};

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

/// A host capability the tool needs access to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Auth,
    Secret,
    Network,
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
        }
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
