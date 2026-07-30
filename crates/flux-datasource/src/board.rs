//! Pure data contracts for a **write-capable** work board (A-113).
//!
//! Where [`live`](crate::live) describes a read projection of a system of record, a board describes
//! a system of record the agent *moves*: items advance through a closed state machine, and every
//! write goes through an edge check. That closedness is what makes a crashed coordinator
//! recoverable — reconciliation can re-derive where an item is, because the set of legal states and
//! the set of legal edges are both finite and known.
//!
//! The paging, filter, and weak-reference vocabulary is [`live`](crate::live)'s, reused verbatim
//! rather than duplicated: a board pages with [`PageRequest`](crate::live::PageRequest) /
//! [`Page`](crate::live::Page), filters with [`Filters`](crate::live::Filters) /
//! [`FilterKey`](crate::live::FilterKey), and cites external artifacts with
//! [`Reference`](crate::live::Reference).
//!
//! Like the rest of this crate these are **pure data**: no IO, no credentials, no live handles. The
//! host-side port and its generated operations live in `flux-capabilities` (L5).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::live::{FilterKey, Reference};

/// Where one work item sits in the board's state machine.
///
/// The variants are the closed set; the legal edges between them are [`State::allowed_next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Available to be picked up.
    Ready,
    /// Assigned to a worker, not yet started.
    Claimed,
    /// A worker is executing it.
    InProgress,
    /// Work finished, awaiting acceptance.
    Review,
    /// Accepted. Terminal.
    Done,
    /// Waiting on something outside the item — a dependency, a human, an outage.
    Blocked,
    /// The attempt failed. Retried by returning to [`Ready`](State::Ready).
    Failed,
}

/// One-line rendering of the machine, for operation descriptions and rejection messages.
///
/// The model sees this on `<domain>.transition`, so it can pick a legal target instead of guessing
/// and getting refused. Kept beside [`EDGES`] so the two cannot drift.
pub const EDGE_DIAGRAM: &str = "ready → claimed → in_progress → review → done; \
     {ready, claimed, in_progress, review} → blocked → ready; \
     {in_progress, review} → failed → ready (attempts += 1); done is terminal";

/// The board's legal edges, as a single table.
///
/// Three groups, and every edge belongs to exactly one:
///
/// * **The spine** — `Ready → Claimed → InProgress → Review → Done`, the happy path.
/// * **Blocking** — any *active* state may divert to `Blocked`, and `Blocked` returns to `Ready`.
///   An unblocked item goes back to the queue to be re-dispatched; it deliberately does **not**
///   resume mid-flight, because the worker that held it is long gone.
/// * **Failure and retry** — `InProgress` or `Review` may divert to `Failed` (a worker that dies is
///   in `InProgress`, which is exactly what the sweep journey inspects; a rejected review is
///   `Review → Failed`), and `Failed → Ready` re-opens the work with [`Item::attempts`] bumped.
///
/// `Done` is terminal.
///
/// **Every edge lives here and nowhere else.** A backend must not invent one, and a caller must not
/// assume one — [`State::allowed_next`] and [`validate_transition`] are the only readers.
///
/// > The ASCII diagram in `docs/designs/fleet-coordinator.md` §2 predates this table and draws a
/// > narrower machine (`Blocked` rejoining at `Claimed`, `Failed` reachable only from `Review`).
/// > This table is the corrected one; the design doc is being updated to match.
const EDGES: &[(State, &[State])] = &[
    (State::Ready, &[State::Claimed, State::Blocked]),
    (State::Claimed, &[State::InProgress, State::Blocked]),
    (
        State::InProgress,
        &[State::Review, State::Blocked, State::Failed],
    ),
    (State::Review, &[State::Done, State::Blocked, State::Failed]),
    (State::Blocked, &[State::Ready]),
    (State::Failed, &[State::Ready]),
    (State::Done, &[]),
];

impl State {
    /// Every state, in declaration order. Useful for schema enums and exhaustive tests.
    pub const ALL: [State; 7] = [
        State::Ready,
        State::Claimed,
        State::InProgress,
        State::Review,
        State::Done,
        State::Blocked,
        State::Failed,
    ];

    /// The wire spelling, identical to this type's serde representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Claimed => "claimed",
            State::InProgress => "in_progress",
            State::Review => "review",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Failed => "failed",
        }
    }

    /// Parse a wire spelling produced by [`State::as_str`]. `None` for anything else.
    pub fn parse(value: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// The states this one may legally advance to. Empty means terminal.
    pub fn allowed_next(&self) -> &'static [State] {
        EDGES
            .iter()
            .find(|(from, _)| from == self)
            .map(|(_, next)| *next)
            .unwrap_or(&[])
    }

    /// Whether `self → to` is a legal edge.
    pub fn can_transition_to(&self, to: State) -> bool {
        self.allowed_next().contains(&to)
    }

    /// Whether nothing leaves this state.
    pub fn is_terminal(&self) -> bool {
        self.allowed_next().is_empty()
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected [`State`] edge. Carries both endpoints so a backend can report the attempt verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    /// The item's current state.
    pub from: State,
    /// The state the caller asked for.
    pub to: State,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let allowed = self.from.allowed_next();
        if allowed.is_empty() {
            return write!(
                f,
                "illegal transition `{}` -> `{}`: `{}` is terminal",
                self.from, self.to, self.from
            );
        }
        let allowed = allowed
            .iter()
            .map(|state| format!("`{state}`"))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "illegal transition `{}` -> `{}`: `{}` may only advance to {allowed}",
            self.from, self.to, self.from
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// Check one edge before anything is written.
///
/// This is the whole rule, in one place, so the host's generated operation and every backend
/// enforce the *same* machine rather than each carrying its own copy.
pub fn validate_transition(from: State, to: State) -> Result<(), IllegalTransition> {
    if from.can_transition_to(to) {
        Ok(())
    } else {
        Err(IllegalTransition { from, to })
    }
}

/// Whether the edge `from → to` is a **retry**, and therefore obliges the backend to increment
/// [`Item::attempts`].
///
/// Only `Failed → Ready` is: it is the one edge that re-opens work already attempted, and
/// `attempts` is what lets the coordinator detect an item that keeps failing instead of retrying it
/// forever.
pub const fn is_retry(from: State, to: State) -> bool {
    matches!((from, to), (State::Failed, State::Ready))
}

/// The reserved `depends_on` list filter (C-236).
///
/// Reserved exactly like `state`: the host declares it on the structured `query` operation and a
/// backend may not redeclare it. The two values it takes are [`DependencyMatch`].
pub const DEPENDS_ON_FILTER: &str = "depends_on";

/// The values the reserved [`DEPENDS_ON_FILTER`] filter takes.
///
/// "Ready and unblocked" is the wave-selection query a coordinator runs on every sweep, so the
/// dependency rule lives here once — beside [`validate_transition`], the board's other whole-rule
/// — rather than being re-derived per backend: **an item is unblocked exactly when every id in its
/// [`depends_on`](Item::depends_on) resolves to a [`Done`](State::Done) item.** No dependencies is
/// trivially unblocked; an absent id is not `done`, so it keeps the item blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyMatch {
    /// Every dependency is `done` (vacuously true for none) — the item is unblocked.
    Satisfied,
    /// At least one dependency is absent or not yet `done` — the item is still blocked.
    Unsatisfied,
}

impl DependencyMatch {
    /// Every value, in declaration order. Useful for the filter's schema enum and exhaustive tests.
    pub const ALL: [DependencyMatch; 2] = [Self::Satisfied, Self::Unsatisfied];

    /// The wire spelling the filter accepts.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Unsatisfied => "unsatisfied",
        }
    }

    /// Parse a wire spelling produced by [`DependencyMatch::as_str`]. `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == value)
    }

    /// Whether `item` matches, resolving each dependency's current state through `state_of`.
    pub fn matches(&self, item: &Item, state_of: impl Fn(&str) -> Option<State>) -> bool {
        let satisfied = item
            .depends_on
            .iter()
            .all(|id| state_of(id) == Some(State::Done));
        match self {
            Self::Satisfied => satisfied,
            Self::Unsatisfied => !satisfied,
        }
    }
}

impl fmt::Display for DependencyMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One work item — the unit the coordinator reasons about.
///
/// Deliberately typed rather than an opaque row: dependency waves come from
/// [`depends_on`](Item::depends_on), stuck detection from [`state`](Item::state) +
/// [`attempts`](Item::attempts). A generic record store would push both into prompt text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Stable identifier assigned by the backend.
    pub id: String,
    /// Short human-facing title.
    #[serde(default)]
    pub title: String,
    /// Position in the state machine.
    pub state: State,
    /// Who holds the item, when claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Where the assigned worker is reachable (an A2A endpoint URL), when dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// The remote task handle returned by a dispatch, when dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Ids of items that must reach [`State::Done`] first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// The repository the work belongs to, for a cross-repo board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// How many times this item has been opened for work. Incremented on every retry edge
    /// (see [`is_retry`]).
    #[serde(default)]
    pub attempts: u32,
    /// Weak locators for artifacts produced against this item. Plain data, never a credential.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Reference>,
}

/// The caller-supplied half of a new [`Item`].
///
/// Everything the *board* owns is absent by construction: [`Item::id`] is the backend's to assign,
/// [`Item::state`] always starts at [`State::Ready`], [`Item::attempts`] at zero, and
/// [`Item::runner`] / [`Item::task_id`] / [`Item::evidence`] are written later by dispatch and
/// execution. A draft therefore cannot smuggle an item into a state it did not transition into.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ItemDraft {
    /// Short human-facing title.
    pub title: String,
    /// Optional initial owner. An item may be created already assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Ids of items that must reach [`State::Done`] first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// The repository the work belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Complete model-facing schema for one registered board domain.
///
/// Mirrors [`LiveSchema`](crate::live::LiveSchema) with the entity dimension collapsed: a board has
/// exactly one entity — the item — so its filters and page bounds sit directly on the schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSchema {
    /// Filters accepted by `list`, beyond the always-available `state` filter the host declares.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FilterKey>,
    /// Page size used when the caller omits a limit.
    pub default_page: usize,
    /// Hard ceiling applied to caller-supplied limits.
    pub max_page: usize,
    /// Optional model-facing explanation of what this board tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Default for BoardSchema {
    fn default() -> Self {
        Self {
            filters: Vec::new(),
            default_page: 20,
            max_page: 100,
            description: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-236: the dependency rule the whole board shares — an item is unblocked exactly when every
    /// dependency is `done`; none is trivially unblocked; an absent id never resolves.
    #[test]
    fn an_item_is_blocked_until_every_dependency_is_done() {
        let mut item = Item {
            id: "child".into(),
            title: String::new(),
            state: State::Ready,
            assignee: None,
            runner: None,
            task_id: None,
            depends_on: vec!["a".into(), "b".into()],
            repo: None,
            attempts: 0,
            evidence: Vec::new(),
        };
        /// A `state_of` resolver where exactly `done` is `done` and everything else is absent.
        fn states<'a>(done: &'a [&'a str]) -> impl Fn(&str) -> Option<State> + 'a {
            move |id: &str| done.contains(&id).then_some(State::Done)
        }

        for spelling in ["satisfied", "unsatisfied"] {
            assert_eq!(
                DependencyMatch::parse(spelling).map(|m| m.as_str()),
                Some(spelling)
            );
        }
        assert_eq!(DependencyMatch::parse("maybe"), None);

        // Half-done is blocked; all-done unblocks; an absent id is not `done`.
        assert!(!DependencyMatch::Satisfied.matches(&item, states(&["a"])));
        assert!(DependencyMatch::Unsatisfied.matches(&item, states(&["a"])));
        assert!(DependencyMatch::Satisfied.matches(&item, states(&["a", "b"])));
        assert!(!DependencyMatch::Satisfied.matches(&item, states(&["a", "c"])));

        // No dependencies is trivially satisfied.
        item.depends_on.clear();
        assert!(DependencyMatch::Satisfied.matches(&item, states(&[])));
        assert!(!DependencyMatch::Unsatisfied.matches(&item, states(&[])));
    }

    #[test]
    fn the_wire_spelling_round_trips_through_parse_and_serde() {
        for state in State::ALL {
            assert_eq!(State::parse(state.as_str()), Some(state));
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            assert_eq!(serde_json::from_str::<State>(&json).unwrap(), state);
        }
        assert_eq!(State::parse("nope"), None);
        assert_eq!(State::parse("InProgress"), None);
    }

    #[test]
    fn the_edge_table_is_closed_and_covers_every_state_exactly_once() {
        for state in State::ALL {
            assert_eq!(
                EDGES.iter().filter(|(from, _)| *from == state).count(),
                1,
                "{state} must appear exactly once in the edge table"
            );
        }
        assert_eq!(EDGES.len(), State::ALL.len());
        // No edge may point at a state that does not exist, and none may be a self-loop.
        for (from, next) in EDGES {
            for to in *next {
                assert!(State::ALL.contains(to), "{from} -> {to} leaves the machine");
                assert_ne!(from, to, "{from} -> {to} is a self-loop");
            }
        }
    }

    #[test]
    fn done_is_the_only_terminal_state() {
        let terminal: Vec<State> = State::ALL.into_iter().filter(State::is_terminal).collect();
        assert_eq!(terminal, vec![State::Done]);
    }

    #[test]
    fn the_spine_blocking_and_retry_are_the_only_legal_edges() {
        let legal = [
            // The spine.
            (State::Ready, State::Claimed),
            (State::Claimed, State::InProgress),
            (State::InProgress, State::Review),
            (State::Review, State::Done),
            // Blocking: any active state diverts, and an unblocked item requeues.
            (State::Ready, State::Blocked),
            (State::Claimed, State::Blocked),
            (State::InProgress, State::Blocked),
            (State::Review, State::Blocked),
            (State::Blocked, State::Ready),
            // Failure and retry.
            (State::InProgress, State::Failed),
            (State::Review, State::Failed),
            (State::Failed, State::Ready),
        ];
        for (from, to) in legal {
            validate_transition(from, to)
                .unwrap_or_else(|error| panic!("{from} -> {to} should be legal: {error}"));
        }
        // Everything else is illegal — including plausible-looking shortcuts.
        for from in State::ALL {
            for to in State::ALL {
                let expected = legal.contains(&(from, to));
                assert_eq!(
                    validate_transition(from, to).is_ok(),
                    expected,
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn only_the_failed_to_ready_edge_counts_as_a_retry() {
        for from in State::ALL {
            for to in State::ALL {
                assert_eq!(
                    is_retry(from, to),
                    from == State::Failed && to == State::Ready,
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn an_illegal_transition_names_both_endpoints_and_the_legal_alternatives() {
        let error = validate_transition(State::Ready, State::Done).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("`ready` -> `done`"), "{message}");
        assert!(message.contains("`claimed`"), "{message}");
        assert!(message.contains("`blocked`"), "{message}");

        let terminal = validate_transition(State::Done, State::Ready).unwrap_err();
        assert!(
            terminal.to_string().contains("`done` is terminal"),
            "{terminal}"
        );
    }

    /// The rendered diagram is what the model reads on `transition`, so it has to describe the
    /// table rather than a stale drawing of it.
    #[test]
    fn the_rendered_diagram_mentions_every_state() {
        for state in State::ALL {
            assert!(
                EDGE_DIAGRAM.contains(state.as_str()),
                "{state} is missing from EDGE_DIAGRAM"
            );
        }
    }

    #[test]
    fn a_draft_carries_only_caller_owned_fields() {
        let draft = ItemDraft {
            title: "port the board".into(),
            ..ItemDraft::default()
        };
        let json = serde_json::to_value(&draft).unwrap();
        assert_eq!(json, serde_json::json!({"title": "port the board"}));
        // The board-owned fields are simply not expressible on a draft.
        assert!(json.get("id").is_none());
        assert!(json.get("state").is_none());
        assert!(json.get("attempts").is_none());
    }
}
