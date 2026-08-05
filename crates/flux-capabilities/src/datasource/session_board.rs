//! Event-backed boards whose lifetime is exactly one Flux session.
//!
//! Every successful mutation appends a complete, versioned snapshot as a `Custom` fact in the
//! owning session stream. Reads are folds over that stream; optimistic writes use the event store's
//! atomic stream-head compare-and-append, so a second process cannot overwrite a newer projection.

use std::collections::BTreeMap;
use std::sync::Arc;

use flux_core::{Error, Result};
use flux_datasource::board::{
    BoardBackend, BoardContract, BoardId, BoardProfile, BoardScope, ItemId,
};
use flux_events::{EventKind, EventStore, NewEvent};
use serde::{Deserialize, Serialize};

const EVENT_NAME: &str = "board.session.snapshot.v1";

/// One profile-neutral item persisted by a session board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionBoardItem {
    /// Stable identity within the board.
    pub id: ItemId,
    /// Human title.
    pub title: String,
    /// Profile-specific state, validated on every transition.
    pub state: String,
    /// Optional detail/body.
    #[serde(default)]
    pub description: String,
    /// Planning priority; absent for general/execution boards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Explicit dependencies, expressed as stable board refs.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Current holder, shared by every profile and used by execution claim/reassignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Execution runner address recorded for durable resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Execution task handle recorded for durable resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Re-open count; execution retry edges are the only mutations that increment it.
    #[serde(default)]
    pub attempts: u64,
    /// Durable chronological comments.
    #[serde(default)]
    pub comments: Vec<String>,
    /// Durable structured evidence values.
    #[serde(default)]
    pub evidence: Vec<serde_json::Value>,
}

/// Current event-folded state of one session board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionBoardSnapshot {
    /// Payload schema.
    pub schema: String,
    /// Board binding.
    pub board: BoardId,
    /// Profile whose state machine applies.
    pub profile: BoardProfile,
    /// Monotonic board-local revision.
    pub revision: u64,
    /// Stable item ordering.
    pub items: BTreeMap<ItemId, SessionBoardItem>,
}

impl SessionBoardSnapshot {
    fn empty(board: BoardId, profile: BoardProfile) -> Self {
        Self {
            schema: "flux.session-board/v1".into(),
            board,
            profile,
            revision: 0,
            items: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionBoardEvent {
    schema: String,
    operation: String,
    snapshot: SessionBoardSnapshot,
}

/// A handle onto one board projection in one registered session.
#[derive(Clone)]
pub struct SessionBoard {
    events: Arc<EventStore>,
    session: String,
    contract: BoardContract,
}

impl std::fmt::Debug for SessionBoard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionBoard")
            .field("session", &self.session)
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

impl SessionBoard {
    /// Whether one state edge belongs to the selected profile.
    pub fn transition_allowed(profile: BoardProfile, from: &str, to: &str) -> bool {
        valid_transition(profile, &normalize_state(from), &normalize_state(to))
    }

    /// Bind a validated session contract to an existing session stream.
    pub fn open(
        events: Arc<EventStore>,
        session: impl Into<String>,
        contract: BoardContract,
    ) -> Result<Self> {
        contract
            .validate()
            .map_err(|error| Error::Config(error.to_string()))?;
        let session = session.into();
        if contract.backend != BoardBackend::Session {
            return Err(Error::Config(format!(
                "session board `{}` requires the session backend",
                contract.id
            )));
        }
        match &contract.scope {
            BoardScope::Session { session_id } if session_id == &session => {}
            BoardScope::Session { session_id } => {
                return Err(Error::Config(format!(
                    "session board `{}` owns {session_id}, not {session}",
                    contract.id
                )));
            }
            other => {
                return Err(Error::Config(format!(
                    "session board `{}` cannot use scope {other:?}",
                    contract.id
                )));
            }
        }
        events
            .info(&session)
            .map_err(|error| Error::Other(format!("unknown session `{session}`: {error}")))?;
        Ok(Self {
            events,
            session,
            contract,
        })
    }

    /// Owning session id.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Fold the latest matching snapshot from the session event stream.
    pub fn snapshot(&self) -> Result<SessionBoardSnapshot> {
        self.snapshot_with_head().map(|(snapshot, _)| snapshot)
    }

    /// Create one item using the profile's initial state.
    pub fn create(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: ItemId,
        title: String,
        description: String,
        priority: Option<i64>,
    ) -> Result<SessionBoardSnapshot> {
        if title.trim().is_empty() {
            return Err(Error::Config("session board item title is empty".into()));
        }
        self.mutate(expected_revision, request_id, "create", move |snapshot| {
            if snapshot.items.contains_key(&id) {
                return Err(Error::Config(format!("board item `{id}` already exists")));
            }
            let state = initial_state(snapshot.profile).to_string();
            snapshot.items.insert(
                id.clone(),
                SessionBoardItem {
                    id,
                    title,
                    state,
                    description,
                    priority,
                    dependencies: Vec::new(),
                    assignee: None,
                    runner: None,
                    task_id: None,
                    attempts: 0,
                    comments: Vec::new(),
                    evidence: Vec::new(),
                },
            );
            Ok(())
        })
    }

    /// Update fields offered by the planning profile.
    pub fn update(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        title: Option<String>,
        priority: Option<i64>,
        description: Option<String>,
    ) -> Result<SessionBoardSnapshot> {
        if self.contract.profile != BoardProfile::Planning {
            return Err(Error::Config(format!(
                "profile {:?} does not support update",
                self.contract.profile
            )));
        }
        let id = id.clone();
        self.mutate(expected_revision, request_id, "update", move |snapshot| {
            let item = snapshot
                .items
                .get_mut(&id)
                .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?;
            if let Some(title) = title {
                if title.trim().is_empty() {
                    return Err(Error::Config("session board item title is empty".into()));
                }
                item.title = title;
            }
            if let Some(priority) = priority {
                item.priority = Some(priority);
            }
            if let Some(description) = description {
                item.description = description;
            }
            Ok(())
        })
    }

    /// Move one item through the selected profile's closed state machine.
    pub fn transition(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        to: &str,
    ) -> Result<SessionBoardSnapshot> {
        let id = id.clone();
        let to = normalize_state(to);
        self.mutate(
            expected_revision,
            request_id,
            "transition",
            move |snapshot| {
                let item = snapshot
                    .items
                    .get_mut(&id)
                    .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?;
                if !valid_transition(snapshot.profile, &item.state, &to) {
                    return Err(Error::Config(format!(
                        "profile {:?} refuses transition {} -> {to}",
                        snapshot.profile, item.state
                    )));
                }
                if snapshot.profile == BoardProfile::Execution
                    && matches!(
                        (item.state.as_str(), to.as_str()),
                        ("failed" | "blocked", "ready")
                    )
                {
                    item.attempts += 1;
                    item.runner = None;
                    item.task_id = None;
                }
                item.state = to;
                Ok(())
            },
        )
    }

    /// Claim an execution item. Repeating the current holder is idempotent; another holder
    /// conflicts and cannot change the event projection.
    pub fn claim(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        assignee: String,
    ) -> Result<SessionBoardSnapshot> {
        if self.contract.profile != BoardProfile::Execution {
            return Err(Error::Config(format!(
                "profile {:?} does not support claim",
                self.contract.profile
            )));
        }
        if assignee.trim().is_empty() {
            return Err(Error::Config("session board assignee is empty".into()));
        }
        let id = id.clone();
        self.mutate(expected_revision, request_id, "claim", move |snapshot| {
            let item = snapshot
                .items
                .get_mut(&id)
                .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?;
            if item.state == "claimed" {
                return match item.assignee.as_deref() {
                    Some(holder) if holder == assignee => Ok(()),
                    Some(holder) => Err(Error::Config(format!(
                        "board item `{id}` is already claimed by `{holder}`"
                    ))),
                    None => {
                        item.assignee = Some(assignee);
                        Ok(())
                    }
                };
            }
            if !valid_transition(BoardProfile::Execution, &item.state, "claimed") {
                return Err(Error::Config(format!(
                    "execution profile refuses transition {} -> claimed",
                    item.state
                )));
            }
            item.state = "claimed".into();
            item.assignee = Some(assignee);
            Ok(())
        })
    }

    /// Record the concrete runner/task pair without moving the state machine.
    pub fn record_dispatch(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        runner: String,
        task_id: String,
    ) -> Result<SessionBoardSnapshot> {
        if self.contract.profile != BoardProfile::Execution {
            return Err(Error::Config(format!(
                "profile {:?} does not support record_dispatch",
                self.contract.profile
            )));
        }
        if runner.trim().is_empty() || task_id.trim().is_empty() {
            return Err(Error::Config(
                "session board dispatch runner/task_id is empty".into(),
            ));
        }
        let id = id.clone();
        self.mutate(
            expected_revision,
            request_id,
            "record_dispatch",
            move |snapshot| {
                let item = snapshot
                    .items
                    .get_mut(&id)
                    .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?;
                item.runner = Some(runner);
                item.task_id = Some(task_id);
                Ok(())
            },
        )
    }

    /// Forcibly move an execution item to another holder and clear the old run identity.
    pub fn reassign(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        assignee: String,
    ) -> Result<SessionBoardSnapshot> {
        if self.contract.profile != BoardProfile::Execution {
            return Err(Error::Config(format!(
                "profile {:?} does not support reassign",
                self.contract.profile
            )));
        }
        if assignee.trim().is_empty() {
            return Err(Error::Config("session board assignee is empty".into()));
        }
        let id = id.clone();
        self.mutate(expected_revision, request_id, "reassign", move |snapshot| {
            let item = snapshot
                .items
                .get_mut(&id)
                .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?;
            item.assignee = Some(assignee);
            item.runner = None;
            item.task_id = None;
            Ok(())
        })
    }

    /// Append one comment.
    pub fn comment(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        comment: String,
    ) -> Result<SessionBoardSnapshot> {
        if comment.trim().is_empty() {
            return Err(Error::Config("session board comment is empty".into()));
        }
        let id = id.clone();
        self.mutate(expected_revision, request_id, "comment", move |snapshot| {
            snapshot
                .items
                .get_mut(&id)
                .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?
                .comments
                .push(comment);
            Ok(())
        })
    }

    /// Append one structured evidence value.
    pub fn record_evidence(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        id: &ItemId,
        evidence: serde_json::Value,
    ) -> Result<SessionBoardSnapshot> {
        let id = id.clone();
        self.mutate(
            expected_revision,
            request_id,
            "record_evidence",
            move |snapshot| {
                let item = snapshot
                    .items
                    .get_mut(&id)
                    .ok_or_else(|| Error::Config(format!("unknown board item `{id}`")))?;
                if !item.evidence.contains(&evidence) {
                    item.evidence.push(evidence);
                }
                Ok(())
            },
        )
    }

    fn mutate(
        &self,
        expected_revision: u64,
        request_id: Option<&str>,
        operation: &str,
        change: impl FnOnce(&mut SessionBoardSnapshot) -> Result<()>,
    ) -> Result<SessionBoardSnapshot> {
        if let Some(request_id) = request_id {
            if let Some(snapshot) = self.idempotent_result(request_id)? {
                return Ok(snapshot);
            }
        }
        let (mut snapshot, stream_head) = self.snapshot_with_head()?;
        if snapshot.revision != expected_revision {
            return Err(Error::Config(format!(
                "stale board revision {expected_revision}; current revision is {}",
                snapshot.revision
            )));
        }
        change(&mut snapshot)?;
        snapshot.revision += 1;
        let payload = serde_json::to_value(SessionBoardEvent {
            schema: "flux.session-board-event/v1".into(),
            operation: operation.into(),
            snapshot: snapshot.clone(),
        })
        .map_err(|error| Error::Other(error.to_string()))?;
        let mut event = NewEvent::new(EventKind::Custom {
            name: EVENT_NAME.into(),
            payload,
        });
        if let Some(request_id) = request_id {
            event = event.with_id(self.event_id(request_id));
        }
        let stored = self
            .events
            .append_if_stream_head(&self.session, event, stream_head)
            .map_err(|error| Error::Other(error.to_string()))?;
        if stored.is_none() {
            return Err(Error::Config(
                "stale session stream; reload the board revision and retry".into(),
            ));
        }
        // Idempotent retries may have returned the earlier event rather than the proposed value.
        self.snapshot()
    }

    fn event_id(&self, request_id: &str) -> String {
        format!(
            "session-board:{}:{}:{request_id}",
            self.session, self.contract.id
        )
    }

    fn idempotent_result(&self, request_id: &str) -> Result<Option<SessionBoardSnapshot>> {
        let id = self.event_id(request_id);
        for event in self
            .events
            .load_stream(&self.session, None)
            .map_err(|error| Error::Other(error.to_string()))?
        {
            if event.id != id {
                continue;
            }
            let EventKind::Custom { name, payload } = event.kind else {
                return Err(Error::Config(format!(
                    "idempotency id {id} belongs to a non-board event"
                )));
            };
            if name != EVENT_NAME {
                return Err(Error::Config(format!(
                    "idempotency id {id} belongs to another custom event"
                )));
            }
            let event: SessionBoardEvent = serde_json::from_value(payload)
                .map_err(|error| Error::Other(format!("invalid session board event: {error}")))?;
            if event.snapshot.board != self.contract.id {
                return Err(Error::Config(format!(
                    "idempotency id {id} belongs to board {}",
                    event.snapshot.board
                )));
            }
            return Ok(Some(event.snapshot));
        }
        Ok(None)
    }

    fn snapshot_with_head(&self) -> Result<(SessionBoardSnapshot, i64)> {
        let events = self
            .events
            .load_stream(&self.session, None)
            .map_err(|error| Error::Other(error.to_string()))?;
        let head = events.last().map_or(-1, |event| event.stream_seq);
        let mut snapshot =
            SessionBoardSnapshot::empty(self.contract.id.clone(), self.contract.profile);
        for event in events {
            let EventKind::Custom { name, payload } = event.kind else {
                continue;
            };
            if name != EVENT_NAME {
                continue;
            }
            let event: SessionBoardEvent = serde_json::from_value(payload)
                .map_err(|error| Error::Other(format!("invalid session board event: {error}")))?;
            if event.snapshot.board == self.contract.id {
                if event.snapshot.profile != self.contract.profile {
                    return Err(Error::Config(format!(
                        "session board `{}` changed profile in its event history",
                        self.contract.id
                    )));
                }
                snapshot = event.snapshot;
            }
        }
        Ok((snapshot, head))
    }
}

fn normalize_state(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '_'], "-")
}

fn initial_state(profile: BoardProfile) -> &'static str {
    match profile {
        BoardProfile::General => "open",
        BoardProfile::Planning => "backlog",
        BoardProfile::Execution => "ready",
    }
}

fn valid_transition(profile: BoardProfile, from: &str, to: &str) -> bool {
    from == to
        || match profile {
            BoardProfile::General => matches!(
                (from, to),
                ("open", "in-progress")
                    | ("open", "blocked")
                    | ("in-progress", "blocked")
                    | ("in-progress", "done")
                    | ("blocked", "open")
                    | ("blocked", "in-progress")
                    | ("blocked", "done")
            ),
            BoardProfile::Planning => matches!(
                (from, to),
                ("backlog", "ready")
                    | ("ready", "in-progress")
                    | ("ready", "blocked")
                    | ("in-progress", "blocked")
                    | ("in-progress", "done")
                    | ("blocked", "ready")
                    | ("blocked", "in-progress")
                    | ("blocked", "done")
            ),
            BoardProfile::Execution => matches!(
                (from, to),
                ("ready", "claimed")
                    | ("ready", "blocked")
                    | ("claimed", "in-progress")
                    | ("claimed", "ready")
                    | ("claimed", "blocked")
                    | ("in-progress", "review")
                    | ("in-progress", "failed")
                    | ("in-progress", "blocked")
                    | ("review", "done")
                    | ("review", "in-progress")
                    | ("review", "failed")
                    | ("failed", "ready")
                    | ("failed", "blocked")
                    | ("blocked", "ready")
            ),
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(session: &str) -> BoardContract {
        BoardContract {
            id: BoardId::new("scratch").unwrap(),
            scope: BoardScope::Session {
                session_id: session.into(),
            },
            profile: BoardProfile::Planning,
            backend: BoardBackend::Session,
            source: "test".into(),
        }
    }

    #[test]
    fn reopen_retry_conflict_and_fork_are_event_native() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let parent = events.create_session("test/model").unwrap();
        let board = SessionBoard::open(events.clone(), &parent, contract(&parent)).unwrap();
        let created = board
            .create(
                0,
                Some("create-one"),
                ItemId::new("S-1").unwrap(),
                "One".into(),
                "durable".into(),
                Some(1),
            )
            .unwrap();
        assert_eq!(created.revision, 1);

        let reopened = SessionBoard::open(events.clone(), &parent, contract(&parent)).unwrap();
        assert_eq!(reopened.snapshot().unwrap(), created);
        assert_eq!(
            reopened
                .create(
                    0,
                    Some("create-one"),
                    ItemId::new("S-1").unwrap(),
                    "ignored retry".into(),
                    String::new(),
                    None,
                )
                .unwrap(),
            created,
            "an idempotent retry returns its original snapshot"
        );
        assert!(reopened
            .create(
                0,
                Some("stale-new-request"),
                ItemId::new("S-2").unwrap(),
                "Stale".into(),
                String::new(),
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("stale board revision"));

        let child = events.copy_session_to(&parent, &events).unwrap();
        let child_board = SessionBoard::open(events.clone(), &child, contract(&child)).unwrap();
        child_board
            .comment(
                1,
                Some("child-comment"),
                &ItemId::new("S-1").unwrap(),
                "child only".into(),
            )
            .unwrap();
        assert!(
            reopened.snapshot().unwrap().items[&ItemId::new("S-1").unwrap()]
                .comments
                .is_empty()
        );
        assert_eq!(
            child_board.snapshot().unwrap().items[&ItemId::new("S-1").unwrap()].comments,
            ["child only"]
        );
    }

    #[test]
    fn every_profile_uses_its_closed_machine_and_session_retention_owns_the_board() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        for (profile, path) in [
            (BoardProfile::General, vec!["in-progress", "done"]),
            (BoardProfile::Planning, vec!["ready", "in-progress", "done"]),
            (
                BoardProfile::Execution,
                vec!["in-progress", "review", "done"],
            ),
        ] {
            let session = events.create_session("test/model").unwrap();
            let mut contract = contract(&session);
            contract.id =
                BoardId::new(format!("profile-{profile:?}").to_ascii_lowercase()).unwrap();
            contract.profile = profile;
            let board = SessionBoard::open(events.clone(), &session, contract.clone()).unwrap();
            let id = ItemId::new("S-1").unwrap();
            let mut snapshot = board
                .create(
                    0,
                    Some("create"),
                    id.clone(),
                    "One".into(),
                    String::new(),
                    None,
                )
                .unwrap();
            if profile == BoardProfile::Execution {
                snapshot = board
                    .claim(snapshot.revision, Some("claim"), &id, "worker-one".into())
                    .unwrap();
                assert_eq!(snapshot.items[&id].state, "claimed");
                assert_eq!(snapshot.items[&id].assignee.as_deref(), Some("worker-one"));
                assert!(board
                    .claim(
                        snapshot.revision,
                        Some("claim-other"),
                        &id,
                        "worker-two".into(),
                    )
                    .is_err());
                snapshot = board
                    .record_dispatch(
                        snapshot.revision,
                        Some("dispatch"),
                        &id,
                        "local://worker-one".into(),
                        "task-1".into(),
                    )
                    .unwrap();
                snapshot = board
                    .reassign(
                        snapshot.revision,
                        Some("reassign"),
                        &id,
                        "worker-two".into(),
                    )
                    .unwrap();
                assert_eq!(snapshot.items[&id].assignee.as_deref(), Some("worker-two"));
                assert_eq!(snapshot.items[&id].runner, None);
                assert_eq!(snapshot.items[&id].task_id, None);
            }
            for state in path {
                snapshot = board
                    .transition(snapshot.revision, Some(&format!("to-{state}")), &id, state)
                    .unwrap();
            }
            assert_eq!(snapshot.items[&id].state, "done");
            assert!(
                board
                    .transition(snapshot.revision, Some("reopen"), &id, "ready")
                    .is_err(),
                "done must be terminal for {profile:?}"
            );
            if profile == BoardProfile::Planning {
                // Planning's ninth operation is available and persists through the same stream.
                let second = ItemId::new("S-2").unwrap();
                let created = board
                    .create(
                        snapshot.revision,
                        Some("create-two"),
                        second.clone(),
                        "Two".into(),
                        String::new(),
                        Some(2),
                    )
                    .unwrap();
                let updated = board
                    .update(
                        created.revision,
                        Some("update-two"),
                        &second,
                        Some("Two updated".into()),
                        Some(1),
                        None,
                    )
                    .unwrap();
                assert_eq!(updated.items[&second].priority, Some(1));
            } else {
                assert!(board
                    .update(
                        snapshot.revision,
                        Some("invalid-update"),
                        &id,
                        None,
                        None,
                        None,
                    )
                    .is_err());
            }

            assert_eq!(events.prune_older_than(0).unwrap(), 0);
            assert!(SessionBoard::open(events.clone(), &session, contract).is_ok());
        }

        let count = events.list(100).unwrap().len();
        assert!(count >= 3);
        assert_eq!(events.prune_older_than(i64::MAX).unwrap(), count);
        assert!(events.list(100).unwrap().is_empty());
    }
}
