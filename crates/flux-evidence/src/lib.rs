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

    /// A group-surfacing signal observation: [`KIND_SIGNAL`] at [`Phase::Turn`] with the
    /// `{"signal": name}` payload [`SignalMatch::matches`] reads. The ONE constructor for that
    /// cross-crate shape — signal emitters (workspace probes, session-ambient injection, tests)
    /// call this instead of hand-building the JSON, so a typo'd key can't silently stop a group
    /// from surfacing.
    pub fn signal(name: &str) -> Self {
        Self::new(
            KIND_SIGNAL,
            Phase::Turn,
            serde_json::json!({ "signal": name }),
        )
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
    ActivateSkill {
        name: String,
    },
    InjectContext {
        text: String,
    },
    Escalate {
        reason: String,
    },
    /// Surface an evidence-gated tool group into the model-facing op catalog (e.g. `"git"` once a
    /// `git_repo` signal is observed). Produced by a group surfacer reaction; consumed by the
    /// runtime's catalog filter.
    SurfaceGroup {
        name: String,
    },
}

/// Turns an observation into zero or more actions.
pub trait Reaction: Send + Sync {
    fn react(&self, observation: &Observation) -> Vec<Action>;
}

/// The kind string recorded for a tool invocation that matches the destructive-command heuristic.
pub const KIND_DESTRUCTIVE: &str = "destructive_command";

/// The observation kind every workspace signal (a project marker such as a git repo or `go.mod`) is
/// recorded under. Shared by the detector that emits signals and the groups that match on them.
pub const KIND_SIGNAL: &str = "project.signal";

/// A signal inferred from the current user's wording rather than the workspace. Tool groups use
/// this for large optional catalogs (notably installed integrations): naming the integration makes
/// its group relevant for the turn without treating every installed operation as core.
pub const KIND_TURN_INTENT: &str = "turn.intent";

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

// ---------------------------------------------------------------------------
// Evidence-gated tool groups
// ---------------------------------------------------------------------------

/// A predicate over an [`Observation`]: matches when `kind` equals the observation's kind and — if
/// `signal` is set — the observation's `data["signal"]` equals it. The data-driven analogue of
/// fluxplane's evidence matcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalMatch {
    /// The observation kind to match. Defaults to [`KIND_SIGNAL`] so a config can write just
    /// `{ signal = "go" }`.
    #[serde(default = "default_signal_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

fn default_signal_kind() -> String {
    KIND_SIGNAL.to_string()
}

impl SignalMatch {
    pub fn matches(&self, obs: &Observation) -> bool {
        obs.kind == self.kind
            && match &self.signal {
                None => true,
                Some(want) => obs.data.get("signal").and_then(Value::as_str) == Some(want.as_str()),
            }
    }
}

/// An evidence-gated bundle of ops. The group **owns its membership** (`tools`): an op named here is
/// advertised to the model only when the group is *active*. An empty `surface_when` means the group
/// is always active (force-on, e.g. a user pins it on); otherwise it activates when the current
/// signals satisfy any of its matches.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolGroup {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub surface_when: Vec<SignalMatch>,
}

/// One integration family matched by explicit turn-routing evidence. `signals` contains only
/// manifest-declared [`KIND_TURN_INTENT`] values that occurred in the input; matching never invents
/// a capability from arbitrary prompt words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentGroupMatch {
    pub group: String,
    pub signals: Vec<String>,
}

/// Match explicit integration aliases, semantic capabilities, and URL-host hints declared as
/// [`KIND_TURN_INTENT`] signals. Results are name-stable and contain each group once.
pub fn matching_turn_intent_groups(groups: &[ToolGroup], input: &str) -> Vec<IntentGroupMatch> {
    let input = input.to_lowercase();
    let mut matched =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for group in groups {
        for signal in group
            .surface_when
            .iter()
            .filter(|matcher| matcher.kind == KIND_TURN_INTENT)
            .filter_map(|matcher| matcher.signal.as_deref())
        {
            if contains_bounded(&input, &signal.to_lowercase()) {
                matched
                    .entry(group.name.clone())
                    .or_default()
                    .insert(signal.to_string());
            }
        }
    }
    matched
        .into_iter()
        .map(|(group, signals)| IntentGroupMatch {
            group,
            signals: signals.into_iter().collect(),
        })
        .collect()
}

/// Infer the declared [`KIND_TURN_INTENT`] signals present in `input`. Matching is
/// case-insensitive and bounded by non-alphanumeric characters, so an integration named `slack`
/// matches `Slack` and `slack.message.send` but not `Slackware`. Only signals explicitly declared
/// by a group are considered; arbitrary prompt words never become evidence.
pub fn turn_intent_observations(groups: &[ToolGroup], input: &str) -> Vec<Observation> {
    let signals: std::collections::BTreeSet<String> = matching_turn_intent_groups(groups, input)
        .into_iter()
        .flat_map(|matched| matched.signals)
        .collect();

    signals
        .into_iter()
        .map(|signal| {
            Observation::new(
                KIND_TURN_INTENT,
                Phase::Turn,
                serde_json::json!({ "signal": signal }),
            )
        })
        .collect()
}

fn contains_bounded(haystack: &str, needle: &str) -> bool {
    !needle.is_empty()
        && haystack.match_indices(needle).any(|(start, _)| {
            let before = haystack[..start].chars().next_back();
            let after = haystack[start + needle.len()..].chars().next();
            before.is_none_or(|ch| !ch.is_alphanumeric())
                && after.is_none_or(|ch| !ch.is_alphanumeric())
        })
}

/// A [`Reaction`] that surfaces any group whose `surface_when` matches an observation — keeping op
/// surfacing inside the evidence backbone (reused via [`EvidenceLog::react_all`]). Force-on groups
/// (empty `surface_when`) match no specific observation and are added by [`resolve_active_groups`].
pub struct GroupSurfacer<'a>(pub &'a [ToolGroup]);

impl Reaction for GroupSurfacer<'_> {
    fn react(&self, observation: &Observation) -> Vec<Action> {
        self.0
            .iter()
            .filter(|g| g.surface_when.iter().any(|m| m.matches(observation)))
            .map(|g| Action::SurfaceGroup {
                name: g.name.clone(),
            })
            .collect()
    }
}

/// Resolve the set of *active* group names from the **current** turn's signal observations: a group
/// is active when any of its `surface_when` matches, or when it is force-on (empty `surface_when`).
///
/// Evaluated against current signals (not the append-only historical log) so a group can both
/// *surface* when evidence arrives and *un-surface* when it's gone — mirroring fluxplane's `Dynamic`
/// per-turn re-derivation. Reuses [`GroupSurfacer`] + [`EvidenceLog::react_all`].
pub fn resolve_active_groups(
    groups: &[ToolGroup],
    current: &[Observation],
) -> std::collections::HashSet<String> {
    let mut log = EvidenceLog::new();
    log.extend(current.iter().cloned());
    let mut active: std::collections::HashSet<String> = log
        .react_all(&GroupSurfacer(groups))
        .into_iter()
        .filter_map(|a| match a {
            Action::SurfaceGroup { name } => Some(name),
            _ => None,
        })
        .collect();
    // Force-on groups match no observation; add them explicitly.
    for g in groups.iter().filter(|g| g.surface_when.is_empty()) {
        active.insert(g.name.clone());
    }
    active
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

    fn signal(name: &str) -> Observation {
        Observation::signal(name)
    }

    #[test]
    fn surface_when_signal_gates_a_group() {
        let groups = vec![ToolGroup {
            name: "git".into(),
            tools: vec!["git_status".into()],
            surface_when: vec![SignalMatch {
                kind: "project.signal".into(),
                signal: Some("git_repo".into()),
            }],
            ..Default::default()
        }];
        // No signal → not active.
        assert!(resolve_active_groups(&groups, &[]).is_empty());
        // Matching signal → active.
        let active = resolve_active_groups(&groups, &[signal("git_repo")]);
        assert!(active.contains("git"));
        // Different signal → not active (proves un-surfacing when evidence changes).
        assert!(resolve_active_groups(&groups, &[signal("go")]).is_empty());
    }

    #[test]
    fn empty_surface_when_is_force_on() {
        let groups = vec![ToolGroup {
            name: "pinned".into(),
            tools: vec!["x".into()],
            surface_when: vec![],
            ..Default::default()
        }];
        assert!(resolve_active_groups(&groups, &[]).contains("pinned"));
    }

    #[test]
    fn turn_intent_signals_match_integration_names_without_substring_collisions() {
        let groups = vec![ToolGroup {
            name: "plugin.slack".into(),
            tools: vec!["slack.message.send".into()],
            surface_when: vec![SignalMatch {
                kind: KIND_TURN_INTENT.into(),
                signal: Some("slack".into()),
            }],
            ..Default::default()
        }];

        for input in ["Post this in Slack", "call slack.message.send"] {
            let observations = turn_intent_observations(&groups, input);
            let active = resolve_active_groups(&groups, &observations);
            assert!(active.contains("plugin.slack"), "{input:?}");
        }
        for input in ["Install Slackware", "summarize the notebook"] {
            assert!(
                turn_intent_observations(&groups, input).is_empty(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn routing_matches_semantic_aliases_and_url_hosts_per_group() {
        let groups = vec![
            ToolGroup {
                name: "plugin.slack".into(),
                tools: vec!["slack.message.send".into()],
                surface_when: ["slack", "company chat", "chat", "slack.com"]
                    .into_iter()
                    .map(|signal| SignalMatch {
                        kind: KIND_TURN_INTENT.into(),
                        signal: Some(signal.into()),
                    })
                    .collect(),
                ..Default::default()
            },
            ToolGroup {
                name: "plugin.teams".into(),
                tools: vec!["teams.message.send".into()],
                surface_when: vec![SignalMatch {
                    kind: KIND_TURN_INTENT.into(),
                    signal: Some("chat".into()),
                }],
                ..Default::default()
            },
        ];

        let url = matching_turn_intent_groups(
            &groups,
            "summarize https://acme.slack.com/archives/C123/p456",
        );
        assert_eq!(url.len(), 1);
        assert_eq!(url[0].group, "plugin.slack");
        assert_eq!(url[0].signals, vec!["slack", "slack.com"]);

        let ambiguous = matching_turn_intent_groups(&groups, "post this to company chat");
        assert_eq!(
            ambiguous
                .iter()
                .map(|matched| matched.group.as_str())
                .collect::<Vec<_>>(),
            vec!["plugin.slack", "plugin.teams"]
        );
    }

    #[test]
    fn signal_match_requires_kind_and_value() {
        let m = SignalMatch {
            kind: "project.signal".into(),
            signal: Some("go".into()),
        };
        assert!(m.matches(&signal("go")));
        assert!(!m.matches(&signal("rust")));
        assert!(!m.matches(&Observation::new(
            "other",
            Phase::Turn,
            json!({"signal": "go"})
        )));
    }

    #[test]
    fn tool_group_roundtrips() {
        let g = ToolGroup {
            name: "git".into(),
            description: "git ops".into(),
            tools: vec!["git_status".into()],
            surface_when: vec![SignalMatch {
                kind: "project.signal".into(),
                signal: Some("git_repo".into()),
            }],
        };
        let s = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<ToolGroup>(&s).unwrap(), g);
    }
}
