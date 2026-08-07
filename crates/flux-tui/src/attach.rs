//! **Attach mode** — driving the panes from an agent that lives on another machine (C-686).
//!
//! Ordinary `flux tui` renders a *local* session: a [`flux_flow::engine::FlowEngine`] runs the
//! turn, `ChannelSink` translates its `AgentSink` calls into [`crate::controller::UiEvent`]s, and
//! the whole thing is durable in the local event store. Attach mode keeps the second half of that
//! sentence and replaces the first: the turn runs on a served agent somewhere else, and this
//! module is the seam its updates arrive through.
//!
//! ## Three rules this module exists to keep
//!
//! 1. **A remote turn never pretends to be a local session event.** Attach mode installs no
//!    `AgentSink`, mints no local session and appends nothing to the local event store, so a
//!    remote agent's conversation cannot appear in `flux sessions` or be `flux replay`ed. The
//!    remote's own store is authoritative and is read back through [`AttachedAgent::history`].
//! 2. **The vocabulary is only as wide as the wire.** [`AttachUpdate`] carries text, lifecycle
//!    state, artifacts and notices — exactly what a served agent's `message/stream` emits. There is
//!    deliberately no tool-call variant, because tool activity does not cross that wire; the
//!    capability lines say so instead of leaving the tool pane silently empty.
//! 3. **An unsupported affordance is shown disabled with its reason**, never left inert.
//!    [`Availability`] and [`ApprovalReach`] carry the reason, and
//!    [`AttachCapabilities::capability_lines`] is what the surface renders.
//!
//! ## Why the protocol is not named here
//!
//! `flux-tui` is L6 and the A2A client is L1; a surface that named the protocol would be a second
//! client. [`AttachedAgent`] is therefore protocol-free, and `flux-cli` — the one crate that sees
//! both — implements it over `flux_a2a::attach`. See `docs/designs/tui-attach.md`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::controller::{send_action_event, ApprovalRequest, UiEvent};
use crate::{ChatState, Entry, Sev};
use flux_runtime::ApprovalChoice;

/// One update from the attached remote agent, in the surface's own vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachUpdate {
    /// Agent text to append. Already normalized by the driver: append unconditionally.
    Text(String),
    /// The remote turn moved to a new lifecycle state.
    State { state: String, terminal: bool },
    /// A structured artifact the remote produced.
    Artifact { name: String, text: String },
    /// A line the operator must read. `error` separates "this went wrong" from "this is how it is".
    Notice { text: String, error: bool },
}

/// One recorded turn as the **remote** holds it. The replay unit on reattach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachTurn {
    pub from_user: bool,
    pub text: String,
}

/// Whether an affordance exists on the far side, and if not, why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(String),
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Availability::Available => None,
            Availability::Unavailable(why) => Some(why),
        }
    }
}

/// Whether an approval raised by the remote agent can be answered from this terminal.
///
/// Four distinct answers, deliberately not three: "nothing is parked right now" and "nobody is ever
/// asked" are different facts about a deployment and only one of them has a human in the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalReach {
    /// The remote parks each guarded effect and this credential may answer. `caveat` states the
    /// shared-operator-token limit the served side is capped at until C-687.
    Answerable { caveat: String },
    /// The remote runs a posture that never asks a human. A statement of posture, not an error.
    NotRaised(String),
    /// The remote parks effects but this credential cannot answer them.
    Unanswerable(String),
    /// The posture could not be determined.
    Unknown(String),
}

impl ApprovalReach {
    pub fn is_answerable(&self) -> bool {
        matches!(self, ApprovalReach::Answerable { .. })
    }

    /// The half-line rendered after `approvals: `.
    pub fn describe(&self) -> String {
        match self {
            ApprovalReach::Answerable { caveat } => format!("answerable here — {caveat}"),
            ApprovalReach::NotRaised(why) => format!("never raised — {why}"),
            ApprovalReach::Unanswerable(why) => format!("not answerable here — {why}"),
            ApprovalReach::Unknown(why) => format!("unknown — {why}"),
        }
    }
}

/// What the attached agent actually supports, as probed at connect time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachCapabilities {
    pub streaming: Availability,
    pub cancel: Availability,
    pub history: Availability,
    pub approvals: ApprovalReach,
}

impl AttachCapabilities {
    /// The lines the surface shows when an attachment opens: what works, and for everything that
    /// does not, **why**.
    ///
    /// This is the whole of "disabled visibly rather than silently inert". A control whose reason
    /// is missing here is a control the operator presses and learns nothing from.
    pub fn capability_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(match self.streaming.reason() {
            None => "live streaming: on".to_string(),
            Some(why) => format!("live streaming: off — {why}"),
        });
        lines.push(match self.cancel.reason() {
            None => "interrupt (Ctrl-C): stops the remote turn".to_string(),
            Some(why) => format!("interrupt (Ctrl-C): disabled — {why}"),
        });
        lines.push(match self.history.reason() {
            None => "reattach replay: from the remote's own history".to_string(),
            Some(why) => format!("reattach replay: disabled — {why}"),
        });
        lines.push(format!("approvals: {}", self.approvals.describe()));
        lines.push(
            "tool calls and results: not carried by this protocol — the tool pane stays empty for \
             a remote agent"
                .to_string(),
        );
        lines.push(
            "local session: none — this conversation lives on the remote and is not in `flux \
             sessions` or `flux replay`"
                .to_string(),
        );
        lines
    }
}

/// One guarded effect the remote has parked for a human, as this surface receives it.
///
/// `fingerprint` is carried verbatim and never interpreted: echoing it exactly is what binds a
/// decision to the effect the operator was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachApproval {
    pub id: String,
    pub fingerprint: String,
    pub tool: String,
    pub subjects: Vec<String>,
    pub summary: Option<String>,
    pub destructive: bool,
    pub mutating: bool,
}

/// An agent running somewhere else that this surface drives.
///
/// Object-safe and protocol-free on purpose — see the module docs. Every fallible method returns a
/// human-readable reason rather than a typed error, because everything this trait can fail at ends
/// up as a line in the transcript.
#[async_trait]
pub trait AttachedAgent: Send + Sync {
    /// Display identity: the agent's name and where it lives. **Never a credential.**
    fn label(&self) -> String;

    /// What the far side supports, probed once when the attachment opened.
    fn capabilities(&self) -> AttachCapabilities;

    /// Send `input` into the live remote session and drive the turn to its end, emitting each
    /// update on `out`. Returning means the turn is over.
    async fn send(&self, input: String, out: mpsc::UnboundedSender<AttachUpdate>);

    /// Ask the remote to abort the live turn. The returned line is shown to the operator as-is, so
    /// an implementation that cannot cancel says so here rather than returning quietly.
    async fn cancel(&self) -> String;

    /// The conversation as the **remote** holds it — authoritative for a reattach.
    async fn history(&self) -> Result<Vec<AttachTurn>, String>;

    /// The guarded effects currently parked for a human on the remote.
    async fn pending_approvals(&self) -> Result<Vec<AttachApproval>, String>;

    /// Deliver one decision for one parked effect, echoing its `fingerprint` verbatim.
    async fn decide_approval(
        &self,
        id: &str,
        fingerprint: &str,
        allow: bool,
        reason: Option<String>,
    ) -> Result<(), String>;
}

/// The live attachment held by [`ChatState`]: the driver plus the facts the renderer needs without
/// awaiting anything.
#[derive(Clone)]
pub struct Attachment {
    pub agent: Arc<dyn AttachedAgent>,
    pub label: String,
    pub capabilities: AttachCapabilities,
}

impl Attachment {
    pub fn new(agent: Arc<dyn AttachedAgent>) -> Self {
        Attachment {
            label: agent.label(),
            capabilities: agent.capabilities(),
            agent,
        }
    }
}

/// `ChatState` derives `Debug`; a driver trait object cannot. Render the facts instead — and never
/// anything that could carry a credential.
impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("label", &self.label)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

/// Clear every piece of view state that would describe **this** machine while the operator is
/// watching another one.
///
/// One function rather than four assignments at the call site, because each of these is a claim the
/// header makes and every one of them would be false under an attachment:
///
/// - **the session id** — there is no local session; an empty id is what keeps this conversation
///   out of `flux sessions` and `flux replay`;
/// - **the model and its pricing** — the local model neither answers nor costs anything here, and
///   the A2A card does not publish the remote's;
/// - **the `auto-ok` badge** — `--yes` is a *local* posture that installs an allow-approver on this
///   machine's engine. It does not, and must not, speak for someone else's deployment: a remote
///   effect parked for a human is still shown and still waits.
pub(crate) fn apply_attached_invariants(state: &mut ChatState) {
    state.session_id.clear();
    state.model = "remote agent".to_string();
    state.model_spec = None;
    state.cost_model = None;
    state.auto_approve = false;
}

/// Fold one remote update into the view model.
///
/// The one crossing point between the remote stream and [`ChatState`]. It uses the *same* mutators
/// a local turn uses (`stream_text`, `push`), so a remote turn renders in the ordinary panes — but
/// it is a separate function, and a separate `UiEvent` arm, precisely so nothing here can be
/// mistaken for the local `AgentSink` path or start writing the local event store.
pub(crate) fn apply_attach_update(state: &mut ChatState, update: AttachUpdate) {
    match update {
        AttachUpdate::Text(text) => state.stream_text(&text),
        // Lifecycle states other than the terminal ones are the remote's spinner, not content.
        // `input-required` is the exception worth a row: the remote is waiting on the operator and
        // a silent pane would look like a stall.
        AttachUpdate::State { state: name, .. } => {
            if name == "input-required" {
                state.push(Entry::Notice {
                    text: "the remote agent is waiting for input".into(),
                    sev: Sev::Info,
                });
            }
        }
        AttachUpdate::Artifact { name, text } => {
            let label = if name.is_empty() {
                "artifact".to_string()
            } else {
                format!("artifact {name}")
            };
            state.push(Entry::Notice {
                text: if text.is_empty() {
                    label
                } else {
                    format!("{label}: {text}")
                },
                sev: Sev::Info,
            });
        }
        AttachUpdate::Notice { text, error } => state.push(Entry::Notice {
            text,
            sev: if error { Sev::Err } else { Sev::Info },
        }),
    }
}

/// Replay the remote's own history into an empty transcript.
///
/// Called when an attachment opens, before the operator types anything, so a reattach is truthful
/// about what happened while they were detached. `Err` is rendered rather than swallowed: an
/// operator who cannot see the earlier turns must know that, not infer it from a short pane.
pub(crate) fn replay_remote_history(
    state: &mut ChatState,
    history: Result<Vec<AttachTurn>, String>,
) {
    match history {
        Ok(turns) if turns.is_empty() => {}
        Ok(turns) => {
            for turn in turns {
                if turn.from_user {
                    state.push_user(turn.text);
                } else {
                    state.stream_text(&turn.text);
                    state.end_stream();
                }
            }
            state.push(Entry::Notice {
                text: "— replayed from the remote agent's own history —".into(),
                sev: Sev::Info,
            });
        }
        Err(why) => state.push(Entry::Notice {
            text: format!("history unavailable: {why}"),
            sev: Sev::Warn,
        }),
    }
}

/// Announce an attachment: where it points, and what does and does not work there.
pub(crate) fn announce_attachment(state: &mut ChatState, attachment: &Attachment) {
    state.push(Entry::Notice {
        text: format!("attached to {} — the agent runs there", attachment.label),
        sev: Sev::Info,
    });
    for line in attachment.capabilities.capability_lines() {
        state.push(Entry::Notice {
            text: line,
            sev: Sev::Info,
        });
    }
}

/// Run one turn against the attached agent, forwarding its updates as tagged [`UiEvent`]s.
///
/// Mirrors `start_turn`'s local spawn exactly — inner task so a panic surfaces, a `Finished` on
/// every exit path — because the event loop's turn bookkeeping (the spinner, `active_action_id`,
/// the interrupt seal) must not learn which kind of turn it is watching.
pub(crate) fn spawn_attached_turn(
    attachment: &Attachment,
    tx: &mpsc::UnboundedSender<UiEvent>,
    action_id: u64,
    input: String,
) {
    let agent = attachment.agent.clone();
    let task_tx = tx.clone();
    tokio::spawn(async move {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<AttachUpdate>();
        let forward_tx = task_tx.clone();
        let forward = tokio::spawn(async move {
            while let Some(update) = out_rx.recv().await {
                send_action_event(&forward_tx, action_id, UiEvent::Attached(Box::new(update)));
            }
        });
        let run = tokio::spawn(async move { agent.send(input, out_tx).await });
        let note = match run.await {
            Ok(()) => None,
            Err(join) if join.is_cancelled() => None,
            Err(join) => Some(format!("the remote turn crashed locally: {join}")),
        };
        // The forwarder ends when the driver drops its sender, so this cannot outlive the turn.
        let _ = forward.await;
        if let Some(text) = note {
            send_action_event(
                &task_tx,
                action_id,
                UiEvent::Notice {
                    text,
                    sev: Sev::Err,
                },
            );
        }
        send_action_event(&task_tx, action_id, UiEvent::Finished);
    });
}

/// Ask the remote to abort its live turn, reporting the outcome into the transcript.
///
/// Fired alongside the local cancellation token: the token stops *this* client pumping, and this
/// stops the *remote* working. An implementation that cannot cancel returns a line saying so, which
/// is why an interrupt in attach mode is never silently a no-op.
pub(crate) fn spawn_attached_cancel(
    attachment: &Attachment,
    tx: &mpsc::UnboundedSender<UiEvent>,
    action_id: u64,
) {
    let agent = attachment.agent.clone();
    let task_tx = tx.clone();
    tokio::spawn(async move {
        let outcome = agent.cancel().await;
        send_action_event(
            &task_tx,
            action_id,
            UiEvent::Notice {
                text: outcome,
                sev: Sev::Info,
            },
        );
    });
}

/// Poll the remote's parked approvals once and raise anything new through the **ordinary** approval
/// sheet.
///
/// Reusing `UiEvent::Approval` is the point: a remote effect is answered with the same `y`/`a`/`n`/`d`
/// keys, the same modal and the same silence-denies rule as a local one. `seen` is the set of ids
/// already raised, so a request that stays parked between polls is not re-asked.
///
/// The oneshot the sheet answers is awaited here and POSTed back with the request's own
/// `fingerprint`. A refused decision (gone, mismatched, unauthenticated) is reported: every one of
/// those means **nothing was approved**, and an operator responds to them differently.
pub(crate) fn spawn_attached_approval_poll(
    attachment: &Attachment,
    tx: &mpsc::UnboundedSender<UiEvent>,
    seen: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
) {
    if !attachment.capabilities.approvals.is_answerable() {
        return;
    }
    let agent = attachment.agent.clone();
    let task_tx = tx.clone();
    tokio::spawn(async move {
        let pending = match agent.pending_approvals().await {
            Ok(pending) => pending,
            // A poll failure is not an approval outcome; report once per occurrence and keep the
            // sheet out of it. Nothing is approved and nothing is denied by this path.
            Err(why) => {
                let _ = task_tx.send(UiEvent::Notice {
                    text: format!("could not read the remote's parked approvals: {why}"),
                    sev: Sev::Warn,
                });
                return;
            }
        };
        for approval in pending {
            {
                let mut seen = match seen.lock() {
                    Ok(seen) => seen,
                    Err(_) => return,
                };
                if !seen.insert(approval.id.clone()) {
                    continue;
                }
            }
            let (reply, answer) = tokio::sync::oneshot::channel();
            if task_tx
                .send(UiEvent::Approval {
                    request: ApprovalRequest {
                        tool: approval.tool.clone(),
                        subjects: approval.subjects.clone(),
                        summary: approval.summary.clone(),
                        destructive: approval.destructive,
                        mutating: approval.mutating,
                    },
                    reply,
                })
                .is_err()
            {
                return;
            }
            let agent = agent.clone();
            let deliver_tx = task_tx.clone();
            tokio::spawn(async move {
                // A dropped sheet (the TUI quit, the turn ended) delivers nothing. The remote's own
                // approval timeout then denies it, which is the fail-closed behaviour C-453 pins.
                let Ok(choice) = answer.await else { return };
                // ⚠ `AllowAlways` narrows to a one-shot allow on purpose. A standing grant is a
                // *local* rule this surface persists for its own engine; there is no remote
                // vocabulary for one, and accumulating one click by click on someone else's
                // deployment is a posture nobody chose (the same reason C-453 declined to ship a
                // remote `AllowAlways`).
                let (allow, reason) = match choice {
                    ApprovalChoice::Allow | ApprovalChoice::AllowAlways(_) => (true, None),
                    ApprovalChoice::Deny => (false, None),
                    ApprovalChoice::DenyWithReason(why) => (false, Some(why)),
                };
                if let Err(why) = agent
                    .decide_approval(&approval.id, &approval.fingerprint, allow, reason)
                    .await
                {
                    let _ = deliver_tx.send(UiEvent::Notice {
                        text: format!(
                            "the remote refused that decision — nothing was approved: {why}"
                        ),
                        sev: Sev::Err,
                    });
                }
            });
        }
    });
}

/// Slash commands that operate on the **local** engine or the **local** event store, and would
/// therefore lie in attach mode: the operator believes they are steering the remote agent.
///
/// Refusing by name is the honest answer. Silently applying them to a local engine that is not
/// producing any of the visible output is the failure this list exists to prevent.
pub(crate) const LOCAL_ONLY_COMMANDS: &[&str] = &[
    "model", "compact", "new", "clear", "evidence", "sessions", "fork", "insights",
];

/// The refusal line for a local-only command under an attachment, or `None` when it is fine to run.
pub(crate) fn refuse_local_only(state: &ChatState, command: &str) -> Option<String> {
    let attachment = state.attachment.as_ref()?;
    if !LOCAL_ONLY_COMMANDS.contains(&command) {
        return None;
    }
    Some(format!(
        "/{command} acts on this machine's engine and session store, and you are attached to {} — \
         the agent, its session and its evidence all live there. Run it on that host.",
        attachment.label
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(cancel: Availability, approvals: ApprovalReach) -> AttachCapabilities {
        AttachCapabilities {
            streaming: Availability::Available,
            cancel,
            history: Availability::Available,
            approvals,
        }
    }

    /// One decision the fake driver was asked to deliver: `(id, fingerprint, allow, reason)`.
    type RecordedDecision = (String, String, bool, Option<String>);

    /// A driver that records what it was asked to do and replays a scripted turn.
    struct FakeAgent {
        capabilities: AttachCapabilities,
        script: Vec<AttachUpdate>,
        history: Result<Vec<AttachTurn>, String>,
        approvals: Vec<AttachApproval>,
        sent: std::sync::Mutex<Vec<String>>,
        cancels: std::sync::Mutex<usize>,
        decisions: std::sync::Mutex<Vec<RecordedDecision>>,
    }

    impl FakeAgent {
        fn new(capabilities: AttachCapabilities) -> Self {
            FakeAgent {
                capabilities,
                script: Vec::new(),
                history: Ok(Vec::new()),
                approvals: Vec::new(),
                sent: std::sync::Mutex::new(Vec::new()),
                cancels: std::sync::Mutex::new(0),
                decisions: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AttachedAgent for FakeAgent {
        fn label(&self) -> String {
            "fixture-agent · https://agent.example/a2a".to_string()
        }
        fn capabilities(&self) -> AttachCapabilities {
            self.capabilities.clone()
        }
        async fn send(&self, input: String, out: mpsc::UnboundedSender<AttachUpdate>) {
            self.sent.lock().unwrap().push(input);
            for update in &self.script {
                let _ = out.send(update.clone());
            }
        }
        async fn cancel(&self) -> String {
            *self.cancels.lock().unwrap() += 1;
            match self.capabilities.cancel.reason() {
                None => "cancel requested — the remote turn is stopping".to_string(),
                Some(why) => format!("cancel is unavailable: {why}"),
            }
        }
        async fn history(&self) -> Result<Vec<AttachTurn>, String> {
            self.history.clone()
        }
        async fn pending_approvals(&self) -> Result<Vec<AttachApproval>, String> {
            Ok(self.approvals.clone())
        }
        async fn decide_approval(
            &self,
            id: &str,
            fingerprint: &str,
            allow: bool,
            reason: Option<String>,
        ) -> Result<(), String> {
            self.decisions.lock().unwrap().push((
                id.to_string(),
                fingerprint.to_string(),
                allow,
                reason,
            ));
            Ok(())
        }
    }

    fn attached_state(agent: Arc<FakeAgent>) -> ChatState {
        let attachment = Attachment::new(agent);
        let mut state = ChatState::attached("remote-model".into(), attachment);
        // Same order the surface uses: announce, then replay.
        let attachment = state.attachment.clone().unwrap();
        announce_attachment(&mut state, &attachment);
        state
    }

    fn drawn(state: &ChatState) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| crate::render(f, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Acceptance 1: a remote agent's streamed turn renders in the ordinary transcript pane.
    #[tokio::test]
    async fn a_streamed_remote_turn_reaches_the_ordinary_transcript() {
        let mut agent = FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised("headless approver".into()),
        ));
        agent.script = vec![
            AttachUpdate::Text("reading ".into()),
            AttachUpdate::Text("the deployment manifest".into()),
            AttachUpdate::State {
                state: "completed".into(),
                terminal: true,
            },
        ];
        let agent = Arc::new(agent);
        let mut state = attached_state(agent.clone());

        let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();
        let attachment = state.attachment.clone().unwrap();
        state.begin_action();
        let action_id = state.active_action_id.unwrap();
        state.push_user("what is deployed?");
        spawn_attached_turn(&attachment, &tx, action_id, "what is deployed?".into());

        let mut finished = false;
        while let Some(event) = rx.recv().await {
            match state.accept_ui_event(event) {
                Some(UiEvent::Attached(update)) => apply_attach_update(&mut state, *update),
                Some(UiEvent::Finished) => {
                    finished = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(finished, "the attached turn must always end with Finished");
        assert_eq!(
            agent.sent.lock().unwrap().as_slice(),
            ["what is deployed?"],
            "acceptance 2: the message must reach the live remote session"
        );
        let screen = drawn(&state);
        assert!(
            screen.contains("reading the deployment manifest"),
            "the remote turn must render in the ordinary transcript: {screen}"
        );
    }

    /// Acceptance 2: an interrupt asks the remote to stop, and says so when it cannot.
    #[tokio::test]
    async fn an_interrupt_delivers_a_cancel_and_reports_when_it_cannot() {
        let agent = Arc::new(FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised("headless approver".into()),
        )));
        let attachment = Attachment::new(agent.clone());
        let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();
        spawn_attached_cancel(&attachment, &tx, 1);
        let event = rx.recv().await.expect("a cancel always reports an outcome");
        assert_eq!(*agent.cancels.lock().unwrap(), 1);
        match event {
            UiEvent::Tagged { event, .. } => match *event {
                UiEvent::Notice { text, .. } => {
                    assert!(text.contains("cancel requested"), "{text}")
                }
                _ => panic!("expected a notice"),
            },
            _ => panic!("expected a tagged event"),
        }

        // The same keypress against an agent that cannot cancel must say so, not do nothing.
        let mute = Arc::new(FakeAgent::new(caps(
            Availability::Unavailable("this agent does not implement tasks/cancel".into()),
            ApprovalReach::NotRaised("headless approver".into()),
        )));
        let attachment = Attachment::new(mute);
        let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();
        spawn_attached_cancel(&attachment, &tx, 1);
        let event = rx.recv().await.expect("an outcome is always reported");
        let text = match event {
            UiEvent::Tagged { event, .. } => match *event {
                UiEvent::Notice { text, .. } => text,
                _ => panic!("expected a notice"),
            },
            _ => panic!("expected a tagged event"),
        };
        assert!(text.contains("unavailable"), "{text}");
        assert!(text.contains("tasks/cancel"), "{text}");
    }

    /// Acceptance 2: an unsupported capability is rendered as disabled **with its reason**.
    #[test]
    fn an_unsupported_capability_renders_disabled_rather_than_inert() {
        let agent = Arc::new(FakeAgent::new(caps(
            Availability::Unavailable("this agent does not implement tasks/cancel".into()),
            ApprovalReach::NotRaised("headless approver".into()),
        )));
        let screen = drawn(&attached_state(agent));
        assert!(
            screen.contains("interrupt (Ctrl-C): disabled"),
            "a missing affordance must be visibly disabled: {screen}"
        );
        assert!(
            screen.contains("tasks/cancel"),
            "the reason must be visible, not just the fact: {screen}"
        );
    }

    /// Acceptance 3: each approval posture is reported as itself.
    #[test]
    fn the_approval_posture_is_reported_plainly_in_every_case() {
        let headless = Arc::new(FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised(
                "this server is not running the remote-approval posture".into(),
            ),
        )));
        let screen = drawn(&attached_state(headless));
        assert!(screen.contains("approvals: never raised"), "{screen}");

        let answerable = Arc::new(FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::Answerable {
                caveat: "answers are attributed to this deployment's shared operator token (C-687)"
                    .into(),
            },
        )));
        let screen = drawn(&attached_state(answerable));
        assert!(screen.contains("approvals: answerable here"), "{screen}");
        assert!(
            screen.contains("shared operator token"),
            "the C-687 limit must be stated where approvals are offered: {screen}"
        );
    }

    /// Acceptance 3: a parked remote effect is answered through the ordinary sheet, and the
    /// decision carries the request's own fingerprint.
    #[tokio::test]
    async fn a_remote_approval_is_answered_through_the_ordinary_sheet() {
        let mut agent = FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::Answerable {
                caveat: "shared operator token".into(),
            },
        ));
        agent.approvals = vec![AttachApproval {
            id: "a_1".into(),
            fingerprint: "the-canonical-effect".into(),
            tool: "bash".into(),
            subjects: vec!["rm -rf /srv/tmp".into()],
            summary: Some("delete a scratch directory".into()),
            destructive: true,
            mutating: true,
        }];
        let agent = Arc::new(agent);
        let attachment = Attachment::new(agent.clone());
        let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();
        spawn_attached_approval_poll(
            &attachment,
            &tx,
            Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        );

        let (request, reply) = match rx.recv().await.expect("a parked effect must be raised") {
            UiEvent::Approval { request, reply } => (request, reply),
            _ => panic!("expected an approval as the first event on the channel"),
        };
        assert_eq!(request.tool, "bash");
        assert_eq!(request.subjects, ["rm -rf /srv/tmp"]);
        assert!(request.destructive);

        reply
            .send(ApprovalChoice::DenyWithReason("not on this host".into()))
            .expect("the sheet answers the parked effect");
        for _ in 0..200 {
            if !agent.decisions.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let decisions = agent.decisions.lock().unwrap().clone();
        assert_eq!(
            decisions,
            vec![(
                "a_1".to_string(),
                "the-canonical-effect".to_string(),
                false,
                Some("not on this host".to_string())
            )],
            "the decision must echo the request's own fingerprint verbatim"
        );
    }

    /// Acceptance 4: reattaching replays the remote's history, and says so when it cannot.
    #[test]
    fn reattaching_replays_the_remotes_own_history() {
        let mut agent = FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised("headless".into()),
        ));
        agent.history = Ok(vec![
            AttachTurn {
                from_user: true,
                text: "roll the deployment".into(),
            },
            AttachTurn {
                from_user: false,
                text: "rolled it while you were away".into(),
            },
        ]);
        let agent = Arc::new(agent);
        let mut state = attached_state(agent.clone());
        replay_remote_history(
            &mut state,
            Ok(vec![
                AttachTurn {
                    from_user: true,
                    text: "roll the deployment".into(),
                },
                AttachTurn {
                    from_user: false,
                    text: "rolled it while you were away".into(),
                },
            ]),
        );
        let screen = drawn(&state);
        assert!(screen.contains("roll the deployment"), "{screen}");
        assert!(
            screen.contains("rolled it while you were away"),
            "what happened while detached must be in the pane: {screen}"
        );
        assert!(
            screen.contains("replayed from the remote agent"),
            "the pane must say the history is the remote's, not local: {screen}"
        );
    }

    #[test]
    fn an_unreplayable_history_says_so_instead_of_showing_a_short_pane() {
        let agent = Arc::new(FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised("headless".into()),
        )));
        let mut state = attached_state(agent);
        replay_remote_history(&mut state, Err("no task addressable yet".into()));
        let screen = drawn(&state);
        assert!(screen.contains("history unavailable"), "{screen}");
        assert!(screen.contains("no task addressable yet"), "{screen}");
    }

    /// Acceptance 5: an attached conversation is not a local session, and the surface says so.
    #[test]
    fn an_attached_conversation_is_not_a_local_session() {
        let agent = Arc::new(FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised("headless".into()),
        )));
        let state = attached_state(agent);
        assert!(
            state.session_id.is_empty(),
            "attach mode must mint no local session id"
        );
        let screen = drawn(&state);
        assert!(
            screen.contains("attached to fixture-agent"),
            "remoteness must be unmissable: {screen}"
        );
        assert!(
            screen.contains("not in `flux sessions`"),
            "the local/remote split must be stated where the operator reads it: {screen}"
        );
    }

    /// `--yes` is a local posture and cannot grant autonomy on someone else's deployment, so the
    /// header must not badge `auto-ok` while remote effects still park for a human.
    #[test]
    fn local_auto_approve_does_not_follow_the_operator_onto_a_remote_agent() {
        let mut state = ChatState::new("local-model".into());
        state.auto_approve = true;
        state.model_spec = Some("anthropic/claude-sonnet-5".into());
        apply_attached_invariants(&mut state);
        assert!(
            !state.auto_approve,
            "--yes must not claim to auto-approve a remote agent's effects"
        );
        assert!(state.session_id.is_empty());
        assert_eq!(state.model, "remote agent");
        assert!(
            state.model_spec.is_none(),
            "the local model neither answers nor costs anything under an attachment"
        );
    }

    #[test]
    fn local_only_commands_are_refused_by_name_under_an_attachment() {
        let agent = Arc::new(FakeAgent::new(caps(
            Availability::Available,
            ApprovalReach::NotRaised("headless".into()),
        )));
        let state = attached_state(agent);
        let refusal = refuse_local_only(&state, "evidence").expect("/evidence is local-only");
        assert!(refusal.contains("live there"), "{refusal}");
        assert!(
            refuse_local_only(&state, "help").is_none(),
            "/help is not local-only"
        );

        let unattached = ChatState::new("m".into());
        assert!(
            refuse_local_only(&unattached, "evidence").is_none(),
            "nothing is refused without an attachment"
        );
    }
}
