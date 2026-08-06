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

    /// Whether this observation's `data` payload was elided by a retained-payload ceiling
    /// ([`EvidenceLog::set_max_payload_bytes`]) rather than being what the observer actually
    /// recorded.
    ///
    /// This is the seam that keeps a bounded log from lying: the observation itself — its `kind`,
    /// its `phase`, its position in the record — is exactly what it always was, and only the
    /// payload is gone. A reader that cares about payloads (a human reading `/evidence`, a model
    /// reading the `evidence` op, an offline auditor reading the durable event-store mirror) can
    /// tell an elided payload apart from an observation that genuinely carried none.
    pub fn is_payload_elided(&self) -> bool {
        self.data.get(ELIDED_PAYLOAD_KEY).is_some()
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

/// C-542: the observation kind carrying the live budget projection — spent versus declared for the
/// enforced envelope, plus any crossed target or hard limit. The enforcing ledger publishes it and
/// every surface renders that exact payload, so the kind is shared here rather than spelled out
/// independently at each end.
pub const KIND_BUDGET_PROJECTION: &str = "budget.projection";

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

/// The key an elided payload is marked with, and the whole of what replaces the original `data`:
/// `{"evidence_payload_elided": {"original_bytes": N, "ceiling_bytes": M, "knob": "…"}}`.
/// See [`Observation::is_payload_elided`] and [`EvidenceLog::set_max_payload_bytes`].
pub const ELIDED_PAYLOAD_KEY: &str = "evidence_payload_elided";

/// The host-facing name of the ceiling that elides payloads, quoted in the elision marker and in
/// [`EvidenceLog::compaction_notice`] so a reader is told what to change. Spelled here — in the crate
/// that does the eliding — so the knob's name cannot drift from what the marker claims it is.
pub const MAX_PAYLOAD_BYTES_KNOB: &str = "max_evidence_payload_bytes";

/// The `data` an elided observation carries in place of its real payload.
fn elided_payload(original_bytes: usize, ceiling_bytes: usize) -> Value {
    serde_json::json!({
        ELIDED_PAYLOAD_KEY: {
            "original_bytes": original_bytes,
            "ceiling_bytes": ceiling_bytes,
            "knob": MAX_PAYLOAD_BYTES_KNOB,
        }
    })
}

/// The encoded size of an observation's payload, computed without allocating. This is the quantity
/// [`EvidenceLog::set_max_payload_bytes`] bounds: it is what a reader would see and what the durable
/// event-store mirror stores, and — unlike a process RSS figure — a library can actually measure it.
fn payload_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        // Digits, near enough: this is a size signal for a ceiling, not an exact byte count.
        Value::Number(_) => 8,
        // `+2` for the quotes; escaping is not modelled.
        Value::String(s) => s.len() + 2,
        // `[…]` plus a comma between items.
        Value::Array(items) => {
            2 + items.len().saturating_sub(1) + items.iter().map(payload_bytes).sum::<usize>()
        }
        // `{…}` plus `"key":` and a comma between entries.
        Value::Object(entries) => {
            2 + entries.len().saturating_sub(1)
                + entries
                    .iter()
                    .map(|(k, v)| k.len() + 4 + payload_bytes(v))
                    .sum::<usize>()
        }
    }
}

/// An append-only record of observations, queryable by kind/phase.
///
/// # Retention (C-298)
///
/// The log is **append-only and never drops an observation**, because it is an audit record and
/// three separate readers depend on that: `flux-flow`'s durable event-store flush addresses its
/// unwritten tail by *absolute index* into [`all`](Self::all), and both `flux-tools`' `metrics` op
/// and `flux-flow`'s per-turn `turn.iteration` / `subagent.usage` baselines are cumulative
/// [`by_kind`](Self::by_kind) counts. Evicting the oldest entries to fit a ceiling would silently
/// corrupt all three *and* truncate the audit trail — so it is not what this type does.
///
/// What a host can bound instead is the arbitrary-size part: the `data` payloads. See
/// [`set_max_payload_bytes`](Self::set_max_payload_bytes). Off by default.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EvidenceLog {
    observations: Vec<Observation>,
    /// The host's retained-payload ceiling, and the accounting that enforces it. Runtime state a
    /// host installs — never part of the serialized record, which is a snapshot of observations.
    #[serde(skip)]
    retention: Retention,
}

/// The retained-payload ceiling's state. Separate from the observations so the record's serialized
/// shape is unchanged by C-298.
#[derive(Debug, Default, Clone)]
struct Retention {
    /// The ceiling, or `None` for unbounded — the default.
    max_payload_bytes: Option<usize>,
    /// Encoded payload bytes currently retained. Maintained incrementally, and only while a ceiling
    /// is set: an unconfigured log must not pay a payload measurement per dispatch.
    retained: usize,
    /// How far the oldest-first elision cursor has advanced. Monotonic — elision is irreversible, so
    /// re-examining an earlier index could only ever find an already-elided payload. This is what
    /// keeps enforcement amortized O(1) per record instead of rescanning the log each time.
    cursor: usize,
    /// How many payloads have been elided, and how many bytes they held.
    elided: usize,
    elided_bytes: usize,
}

impl EvidenceLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bound the encoded `data` payload bytes this log retains, or `None` for unbounded (the
    /// default — an unconfigured runtime behaves exactly as it did before C-298).
    ///
    /// **This never drops an observation.** When the ceiling is exceeded, the *oldest* payloads are
    /// replaced, one at a time, by a self-describing elision marker
    /// ([`Observation::is_payload_elided`]) until the log is back under it. Count, order, `kind` and
    /// `phase` are untouched, so every consumer that reads the log by absolute index or counts it by
    /// kind keeps reading exactly what it read before — see this type's Retention notes.
    ///
    /// **What is bounded and what is not.** The ceiling governs payloads, which are the
    /// arbitrary-size and therefore dominant term: a `tool_call` observation carries the call's
    /// permission subjects, a flow-emitted `observe(…)` carries whatever the flow passed. It does
    /// **not** bound the log's entry count, and no honest ceiling here could: an entry ceiling means
    /// dropping entries, and dropping entries is precisely the silent truncation the three readers
    /// above forbid. A long-lived runtime therefore still retains a fixed-size header
    /// (`kind` + `phase` + marker) per observation; what it no longer retains is unbounded payload.
    ///
    /// **The payload is not necessarily lost.** `flux-flow` flushes each completed turn's
    /// observations verbatim to the session event store (C-14), so a payload elided after its turn
    /// closed is still readable there in full. A payload elided *within* an unusually long single
    /// turn is flushed already-elided — the marker says so, with the original size, rather than
    /// leaving a reader to guess.
    ///
    /// Installing a ceiling re-derives the accounting from the observations already present, so it
    /// is correct to call on a log that is not empty. Re-installing the ceiling a log already has is
    /// a no-op — `Executor::with_resource_limits` runs for *every* executor an environment derives
    /// over one shared log, and re-deriving the accounting each time would make a long-lived session
    /// quadratic.
    pub fn set_max_payload_bytes(&mut self, bytes: Option<usize>) {
        if self.retention.max_payload_bytes == bytes {
            return;
        }
        self.retention.max_payload_bytes = bytes;
        if bytes.is_some() {
            self.retention.retained = self.retained_payload_bytes();
            self.retention.cursor = 0;
        }
        self.enforce_payload_ceiling();
    }

    /// [`set_max_payload_bytes`](Self::set_max_payload_bytes) as a builder.
    pub fn with_max_payload_bytes(mut self, bytes: Option<usize>) -> Self {
        self.set_max_payload_bytes(bytes);
        self
    }

    /// The configured retained-payload ceiling, if any.
    pub fn max_payload_bytes(&self) -> Option<usize> {
        self.retention.max_payload_bytes
    }

    /// Encoded payload bytes this log currently retains — what the ceiling bounds. Walks the log, so
    /// it is exact whether or not a ceiling is installed; an elided payload contributes nothing.
    /// Introspection, not a hot path.
    pub fn retained_payload_bytes(&self) -> usize {
        self.observations
            .iter()
            .filter(|o| !o.is_payload_elided())
            .map(|o| payload_bytes(&o.data))
            .sum()
    }

    /// How many observation payloads the ceiling has elided. The observations themselves are all
    /// still in [`all`](Self::all).
    pub fn elided_payloads(&self) -> usize {
        self.retention.elided
    }

    /// How many payload bytes those elisions reclaimed.
    pub fn elided_payload_bytes(&self) -> usize {
        self.retention.elided_bytes
    }

    /// An actionable report of what the ceiling did, or `None` if it never bound. Held to C-290's
    /// bar: it says what was elided, where the full record still is, and which knob to raise.
    pub fn compaction_notice(&self) -> Option<String> {
        let ceiling = self.retention.max_payload_bytes?;
        if self.retention.elided == 0 {
            return None;
        }
        Some(format!(
            "evidence log: {} observation payload(s) totalling {} bytes were elided to stay under \
             the runtime's {MAX_PAYLOAD_BYTES_KNOB} ceiling of {ceiling} bytes. No observation was \
             dropped — each elided one is marked `{ELIDED_PAYLOAD_KEY}` with its original size, and \
             payloads from turns that already completed remain in full in the session event store. \
             Raise {MAX_PAYLOAD_BYTES_KNOB} to retain them in memory.",
            self.retention.elided, self.retention.elided_bytes,
        ))
    }

    pub fn record(&mut self, observation: Observation) {
        if self.retention.max_payload_bytes.is_some() {
            self.retention.retained = self
                .retention
                .retained
                .saturating_add(payload_bytes(&observation.data));
        }
        self.observations.push(observation);
        self.enforce_payload_ceiling();
    }

    pub fn extend(&mut self, observations: impl IntoIterator<Item = Observation>) {
        if self.retention.max_payload_bytes.is_some() {
            // Sum as we go rather than measuring the whole log again afterwards.
            for observation in observations {
                self.retention.retained = self
                    .retention
                    .retained
                    .saturating_add(payload_bytes(&observation.data));
                self.observations.push(observation);
            }
        } else {
            self.observations.extend(observations);
        }
        self.enforce_payload_ceiling();
    }

    /// Elide oldest-first until the retained payload fits the ceiling. A no-op when unbounded, and
    /// when already under it — which is the common case even for a configured log.
    fn enforce_payload_ceiling(&mut self) {
        let Some(ceiling) = self.retention.max_payload_bytes else {
            return;
        };
        if self.retention.retained <= ceiling {
            return;
        }
        // A single payload larger than the entire ceiling can never be retained without breaching
        // it. Elide that one on arrival rather than eliding the whole history to make room for it
        // and breaching anyway — the same call `OpCache` makes for an oversized tool result.
        if let Some(last) = self.observations.len().checked_sub(1) {
            if payload_bytes(&self.observations[last].data) > ceiling {
                self.elide(last, ceiling);
            }
        }
        while self.retention.retained > ceiling && self.retention.cursor < self.observations.len() {
            let index = self.retention.cursor;
            self.retention.cursor += 1;
            self.elide(index, ceiling);
        }
    }

    /// Replace observation `index`'s payload with the elision marker, and account for it. Idempotent:
    /// an already-elided payload is left alone.
    fn elide(&mut self, index: usize, ceiling: usize) {
        let observation = &mut self.observations[index];
        if observation.is_payload_elided() {
            return;
        }
        let was = payload_bytes(&observation.data);
        observation.data = elided_payload(was, ceiling);
        self.retention.retained = self.retention.retained.saturating_sub(was);
        self.retention.elided += 1;
        self.retention.elided_bytes = self.retention.elided_bytes.saturating_add(was);
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

    // -----------------------------------------------------------------------
    // C-298 — the retained-payload ceiling
    // -----------------------------------------------------------------------

    fn bulky(kind: &str, n: usize) -> Observation {
        Observation::new(kind, Phase::Turn, json!({ "blob": "x".repeat(n) }))
    }

    /// The default is unbounded, and that is deliberate: C-290's rule is that an unconfigured
    /// runtime behaves exactly as it did before.
    #[test]
    fn a_log_is_unbounded_until_a_host_sets_a_ceiling() {
        let mut log = EvidenceLog::new();
        for i in 0..50 {
            log.record(bulky("tool_call", 500 + i));
        }
        assert_eq!(log.max_payload_bytes(), None);
        assert_eq!(log.elided_payloads(), 0);
        assert!(
            log.retained_payload_bytes() > 25_000,
            "retained {}",
            log.retained_payload_bytes()
        );
        assert!(log.all().iter().all(|o| !o.is_payload_elided()));
    }

    /// The ceiling binds, and it binds by eliding the OLDEST payloads — the newest observation, the
    /// one a reaction or a progress check is most likely to care about, keeps its payload.
    #[test]
    fn the_ceiling_elides_oldest_first_and_keeps_the_newest_payload() {
        const CEILING: usize = 4_096;
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(CEILING));
        for _ in 0..40 {
            log.record(bulky("tool_call", 500));
        }
        assert!(
            log.retained_payload_bytes() <= CEILING,
            "retained {} over a {CEILING}-byte ceiling",
            log.retained_payload_bytes()
        );
        assert!(log.elided_payloads() > 0);
        assert!(
            log.all()[0].is_payload_elided(),
            "the oldest payload must go first"
        );
        assert!(
            !log.all().last().unwrap().is_payload_elided(),
            "the newest payload must survive"
        );
    }

    /// **The invariant this whole design exists for.** Three readers outside this crate depend on the
    /// log's shape rather than its payloads: `flux-flow`'s durable event-store flush slices the
    /// unflushed tail by *absolute index* into `all()`, and both `flux-tools`' `metrics` op and
    /// `flux-flow`'s per-turn iteration baselines are cumulative `by_kind` counts. A ceiling that
    /// dropped entries would silently corrupt all three. This proves it drops none.
    #[test]
    fn a_bound_ceiling_changes_no_index_no_count_no_kind_and_no_phase() {
        const CEILING: usize = 2_048;
        const N: usize = 60;
        let mut unbounded = EvidenceLog::new();
        let mut bounded = EvidenceLog::new().with_max_payload_bytes(Some(CEILING));
        for i in 0..N {
            let kind = if i % 3 == 0 {
                "tool_call"
            } else {
                "tool_error"
            };
            unbounded.record(bulky(kind, 400));
            bounded.record(bulky(kind, 400));
        }

        assert!(bounded.elided_payloads() > 0, "the ceiling must have bound");
        assert_eq!(
            bounded.all().len(),
            unbounded.all().len(),
            "absolute indices must be preserved — flux-flow's flush watermark is one"
        );
        assert_eq!(
            bounded.by_kind("tool_call").count(),
            unbounded.by_kind("tool_call").count(),
            "cumulative per-kind counts must be preserved — `metrics()` is one"
        );
        assert_eq!(
            bounded.by_kind("tool_error").count(),
            unbounded.by_kind("tool_error").count()
        );
        let kinds = |log: &EvidenceLog| -> Vec<(String, Phase)> {
            log.all()
                .iter()
                .map(|o| (o.kind.clone(), o.phase))
                .collect()
        };
        assert_eq!(
            kinds(&bounded),
            kinds(&unbounded),
            "kind and phase, in order, must be identical"
        );
    }

    /// An elided payload is legible as such and carries its original size, so the loss is visible
    /// rather than looking like an observation that genuinely had no payload.
    #[test]
    fn an_elided_payload_is_distinguishable_from_an_empty_one() {
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(512));
        log.record(bulky("tool_call", 4_000));
        log.record(Observation::new("empty", Phase::Turn, json!({})));

        let elided = &log.all()[0];
        assert!(elided.is_payload_elided());
        let marker = &elided.data[ELIDED_PAYLOAD_KEY];
        assert!(
            marker["original_bytes"].as_u64().unwrap() > 4_000,
            "the marker must record what was there: {marker}"
        );
        assert_eq!(marker["ceiling_bytes"], 512);
        assert_eq!(marker["knob"], MAX_PAYLOAD_BYTES_KNOB);

        let empty = &log.all()[1];
        assert!(
            !empty.is_payload_elided(),
            "a genuinely empty payload must not read as elided"
        );
    }

    /// A payload larger than the whole ceiling is elided on arrival, rather than the log eliding its
    /// entire history to make room and breaching the ceiling anyway. Mirrors `OpCache`'s call for an
    /// oversized tool result (C-290).
    #[test]
    fn a_payload_larger_than_the_whole_ceiling_does_not_elide_the_history() {
        const CEILING: usize = 2_048;
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(CEILING));
        log.record(bulky("small", 200));
        log.record(bulky("huge", 10_000));
        assert!(
            log.all()[1].is_payload_elided(),
            "the oversized payload must be the one elided"
        );
        assert!(
            !log.all()[0].is_payload_elided(),
            "it must not cost the history its payloads"
        );
        assert!(log.retained_payload_bytes() <= CEILING);
    }

    /// Installing a ceiling on a log that already has observations re-derives the accounting and
    /// enforces immediately — the runtime installs limits after startup observations exist.
    #[test]
    fn installing_a_ceiling_on_a_non_empty_log_enforces_it_at_once() {
        let mut log = EvidenceLog::new();
        for _ in 0..30 {
            log.record(bulky("startup", 400));
        }
        assert_eq!(log.elided_payloads(), 0);
        log.set_max_payload_bytes(Some(1_024));
        assert!(
            log.retained_payload_bytes() <= 1_024,
            "retained {}",
            log.retained_payload_bytes()
        );
        assert_eq!(log.all().len(), 30, "still every observation");

        // Re-installing the same ceiling must not re-derive the accounting or re-count the elisions
        // — every executor an environment derives calls through this seam over one shared log.
        let elided = log.elided_payloads();
        let retained = log.retained_payload_bytes();
        log.set_max_payload_bytes(Some(1_024));
        assert_eq!(log.elided_payloads(), elided);
        assert_eq!(log.retained_payload_bytes(), retained);
    }

    /// The notice is the actionable half of "never silent": it names the knob and says where the full
    /// payloads still are. Absent until the ceiling actually binds.
    #[test]
    fn the_compaction_notice_names_the_knob_only_once_the_ceiling_binds() {
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(8_192));
        log.record(bulky("tool_call", 100));
        assert!(
            log.compaction_notice().is_none(),
            "nothing elided yet — no notice"
        );
        for _ in 0..40 {
            log.record(bulky("tool_call", 500));
        }
        let notice = log.compaction_notice().expect("the ceiling bound");
        assert!(notice.contains(MAX_PAYLOAD_BYTES_KNOB), "{notice}");
        assert!(notice.contains("event store"), "{notice}");
        assert!(notice.contains("No observation was dropped"), "{notice}");
        assert_eq!(
            EvidenceLog::new().compaction_notice(),
            None,
            "an unbounded log has nothing to report"
        );
    }

    /// `extend` is the dispatcher's batch path (`tool_call` + a destructive marker together) and must
    /// honor the ceiling exactly like `record`.
    #[test]
    fn extend_honors_the_ceiling_too() {
        const CEILING: usize = 1_024;
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(CEILING));
        for _ in 0..20 {
            log.extend([bulky("tool_call", 300), bulky(KIND_DESTRUCTIVE, 300)]);
        }
        assert_eq!(log.all().len(), 40);
        assert!(
            log.retained_payload_bytes() <= CEILING,
            "retained {}",
            log.retained_payload_bytes()
        );
        assert_eq!(log.by_kind(KIND_DESTRUCTIVE).count(), 20);
    }

    /// Reactions read a single observation's kind (`DestructiveEscalation`) or the *current* turn's
    /// signals (`resolve_active_groups` builds a throwaway log), never the historical payload — which
    /// is why eliding old payloads cannot change what a reaction decides. Pinned here because the
    /// story turns on it.
    #[test]
    fn eliding_old_payloads_does_not_change_what_reactions_decide() {
        const CEILING: usize = 512;
        let groups = vec![ToolGroup {
            name: "git".into(),
            tools: vec!["git_status".into()],
            surface_when: vec![SignalMatch {
                kind: KIND_SIGNAL.into(),
                signal: Some("git_repo".into()),
            }],
            ..Default::default()
        }];
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(CEILING));
        // Bury the history under enough payload to force elision, then present this turn's signals.
        for _ in 0..40 {
            log.record(bulky("tool_call", 300));
        }
        assert!(log.elided_payloads() > 0);

        let current = [Observation::signal("git_repo")];
        assert!(
            resolve_active_groups(&groups, &current).contains("git"),
            "group surfacing reads the CURRENT turn's signals, not the elided history"
        );
        // And the escalation reaction keys on kind, which elision never touches.
        log.record(Observation::new(KIND_DESTRUCTIVE, Phase::Turn, json!({})));
        assert_eq!(
            log.react_all(&DestructiveEscalation).len(),
            1,
            "one destructive marker, one escalation — regardless of elided payloads"
        );
    }

    /// The serialized record is unchanged by C-298: the ceiling and its accounting are host-installed
    /// runtime state, not part of the wire shape (`flux-evidence` is on the 1.x protocol line).
    #[test]
    fn the_ceiling_is_not_part_of_the_serialized_record() {
        let mut log = EvidenceLog::new().with_max_payload_bytes(Some(256));
        log.record(bulky("tool_call", 2_000));
        let encoded = serde_json::to_value(&log).unwrap();
        assert_eq!(
            encoded.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["observations"],
            "only `observations` may appear on the wire: {encoded}"
        );
        let back: EvidenceLog = serde_json::from_value(encoded).unwrap();
        assert_eq!(back.all().len(), 1);
        assert_eq!(
            back.max_payload_bytes(),
            None,
            "a deserialized record carries no ceiling"
        );
        assert!(
            back.all()[0].is_payload_elided(),
            "but the elision marker travels with the observation, so an offline reader still sees it"
        );
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
