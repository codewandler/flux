//! `flux-evidence` — the audit/automation backbone: typed [`Observation`]s gathered at lifecycle
//! phases, recorded in an [`EvidenceLog`], and turned into actions by [`Reaction`]s.
//!
//! This is intentionally small and pure: observers produce structured observations (not log
//! lines), and reactions map observations to actions (activate a skill, escalate to approval,
//! modify context). The runtime wires observers/reactions to phases.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// When in a session's life an observation was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Startup,
    SessionOpen,
    Turn,
    ToolFollowup,
}

/// A structured observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub kind: String,
    pub phase: Phase,
    #[serde(default)]
    pub data: Value,
}

impl Observation {
    pub fn new(kind: impl Into<String>, phase: Phase, data: Value) -> Self {
        Self {
            kind: kind.into(),
            phase,
            data,
        }
    }
}

/// Produces observations at a given phase.
pub trait Observer: Send + Sync {
    fn observe(&self, phase: Phase) -> Vec<Observation>;
}

/// A described action a reaction wants the runtime to take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    ActivateSkill { name: String },
    InjectContext { text: String },
    Escalate { reason: String },
}

/// Turns an observation into zero or more actions.
pub trait Reaction: Send + Sync {
    fn react(&self, observation: &Observation) -> Vec<Action>;
}

/// The kind string recorded for a tool invocation that matches the destructive-command heuristic.
pub const KIND_DESTRUCTIVE: &str = "destructive_command";

/// A built-in reaction: a [`KIND_DESTRUCTIVE`] observation escalates the operation to human
/// approval. The runtime consults this to force an approval prompt even under a permissive
/// allow-rule.
pub struct DestructiveEscalation;

impl Reaction for DestructiveEscalation {
    fn react(&self, observation: &Observation) -> Vec<Action> {
        if observation.kind == KIND_DESTRUCTIVE {
            vec![Action::Escalate {
                reason: "destructive command requires approval".into(),
            }]
        } else {
            Vec::new()
        }
    }
}

/// An append-only record of observations, queryable by kind/phase.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EvidenceLog {
    observations: Vec<Observation>,
}

impl EvidenceLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, observation: Observation) {
        self.observations.push(observation);
    }

    pub fn extend(&mut self, observations: impl IntoIterator<Item = Observation>) {
        self.observations.extend(observations);
    }

    pub fn all(&self) -> &[Observation] {
        &self.observations
    }

    pub fn by_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Observation> + 'a {
        self.observations.iter().filter(move |o| o.kind == kind)
    }

    /// Run `reaction` over every recorded observation, collecting all actions.
    pub fn react_all(&self, reaction: &dyn Reaction) -> Vec<Action> {
        self.observations
            .iter()
            .flat_map(|o| reaction.react(o))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Escalator;
    impl Reaction for Escalator {
        fn react(&self, o: &Observation) -> Vec<Action> {
            if o.kind == "destructive_command" {
                vec![Action::Escalate {
                    reason: "destructive command observed".into(),
                }]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn log_records_and_queries() {
        let mut log = EvidenceLog::new();
        log.record(Observation::new(
            "toolchain",
            Phase::Startup,
            json!({"lang": "rust"}),
        ));
        log.record(Observation::new(
            "destructive_command",
            Phase::Turn,
            json!({"cmd": "rm -rf"}),
        ));
        assert_eq!(log.all().len(), 2);
        assert_eq!(log.by_kind("toolchain").count(), 1);
    }

    #[test]
    fn reactions_produce_actions() {
        let mut log = EvidenceLog::new();
        log.record(Observation::new(
            "destructive_command",
            Phase::Turn,
            json!({}),
        ));
        log.record(Observation::new("benign", Phase::Turn, json!({})));
        let actions = log.react_all(&Escalator);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Escalate { .. }));
    }

    #[test]
    fn destructive_escalation_reacts_only_to_destructive() {
        let r = DestructiveEscalation;
        let destructive = Observation::new(KIND_DESTRUCTIVE, Phase::Turn, json!({"tool": "bash"}));
        assert!(matches!(
            r.react(&destructive).as_slice(),
            [Action::Escalate { .. }]
        ));
        let benign = Observation::new("tool_call", Phase::Turn, json!({"tool": "read"}));
        assert!(r.react(&benign).is_empty());
    }

    #[test]
    fn observation_roundtrips() {
        let o = Observation::new("x", Phase::ToolFollowup, json!({"a": 1}));
        let s = serde_json::to_string(&o).unwrap();
        assert_eq!(serde_json::from_str::<Observation>(&s).unwrap(), o);
    }
}
