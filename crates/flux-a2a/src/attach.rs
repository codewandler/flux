//! **Attaching** to an agent that lives on a host — the protocol half of C-686.
//!
//! `flux app run --serve` serves a whole agent; `flux a2a <url>` already chats with it as a line
//! REPL. This module is the piece a *rich* surface needs instead: one long-lived
//! [`AttachedA2aAgent`] bound to one conversation, which streams a turn, cancels it, replays the
//! remote's own history, and reports honestly which of those the far side actually implements.
//!
//! It is built on the same [`A2aClient`] the REPL uses — the same discovery, the same bearer
//! handling, the same origin lock. There is one A2A client in the tree and this is not a second one.
//!
//! ## What deliberately is not here
//!
//! The event vocabulary ([`AttachEvent`]) is small because the wire is: flux's served
//! `message/stream` emits **text deltas and lifecycle status only**. The remote agent's tool calls
//! and tool results do not cross the A2A wire at all, so there is no `ToolCall` variant to fill —
//! inventing one would promise a surface something the transport never delivers. See
//! `docs/designs/tui-attach.md` for the gap and the candidate story.
//!
//! ## Layering
//!
//! This crate is L1 and knows nothing about any surface. It emits [`AttachEvent`]/[`AttachTurn`]
//! plain data; a surface translates them into whatever its view model speaks.

use std::sync::Mutex;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::client::{A2aClient, A2aError, Result};
use crate::error;
use crate::types::{new_id, Message, Role, StreamEvent, TaskState};

/// One update from an attached remote agent — everything the A2A wire actually carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachEvent {
    /// Agent text to append. Already normalized: an agent that streams cumulative snapshots
    /// instead of deltas yields only the new suffix, so a consumer appends unconditionally.
    Text(String),
    /// The remote task moved to a new lifecycle state (`working`, `completed`, `canceled`, …).
    State { state: String, terminal: bool },
    /// A structured artifact the remote produced during the turn.
    Artifact { name: String, text: String },
    /// Something the operator must read: a transport failure, a refusal, or an unsupported
    /// operation. `error` distinguishes "this went wrong" from "this is how it is".
    Notice { text: String, error: bool },
    /// No more events for this turn.
    Ended,
}

/// One recorded conversation turn as the **remote** holds it — the reattach unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachTurn {
    /// `true` when the operator authored it, `false` for the agent.
    pub from_user: bool,
    pub text: String,
}

/// Whether an affordance is available on the far side, and if not, why not. A surface renders the
/// reason rather than leaving the control silently inert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(String),
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }

    /// The reason it is unavailable, or `None` when it is available.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Availability::Available => None,
            Availability::Unavailable(why) => Some(why),
        }
    }
}

/// Whether an approval raised by the remote agent can be answered from here — probed against the
/// served agent's C-453 `/approvals` routes, and reported as itself rather than collapsed into
/// "no approvals".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalReach {
    /// The remote parks each guarded effect for a human and this credential may answer.
    ///
    /// ⚠ `caveat` carries the honest limit: the served side supports one **shared operator token**
    /// (or open loopback) only, so an answer is attributed to the deployment, not to a principal.
    /// Principal-authenticated approval is refused at router construction and is C-687's work.
    Answerable { caveat: String },
    /// The remote runs some other posture and never parks an effect for a human. This is a
    /// statement of posture, not an error.
    NotRaised(String),
    /// The remote parks effects but this credential cannot answer them.
    Unanswerable(String),
    /// The posture could not be determined (transport error, unexpected status).
    Unknown(String),
}

impl ApprovalReach {
    pub fn is_answerable(&self) -> bool {
        matches!(self, ApprovalReach::Answerable { .. })
    }

    /// One line naming the posture, suitable for a status pane.
    pub fn describe(&self) -> String {
        match self {
            ApprovalReach::Answerable { caveat } => {
                format!("answerable from here — {caveat}")
            }
            ApprovalReach::NotRaised(why) => format!("never raised — {why}"),
            ApprovalReach::Unanswerable(why) => format!("not answerable here — {why}"),
            ApprovalReach::Unknown(why) => format!("unknown — {why}"),
        }
    }
}

/// What an attached agent supports, probed once at connect time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSupport {
    /// `message/stream`. When unavailable the turn still runs, blocking, via `message/send`.
    pub streaming: Availability,
    /// `tasks/cancel`.
    pub cancel: Availability,
    /// `tasks/get` history for reattach.
    pub history: Availability,
    /// The C-453 approval posture.
    pub approvals: ApprovalReach,
}

/// One pending approval as the remote's `/approvals` queue publishes it. A plain wire mirror —
/// this crate is L1 and cannot see `flux_runtime::PendingApproval`, so the fields it needs are
/// deserialized by name. Unknown fields are ignored; the `fingerprint` is carried verbatim and
/// never interpreted, because echoing it exactly is what binds a decision to its effect.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AttachApproval {
    pub id: String,
    pub fingerprint: String,
    pub tool: String,
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub destructive: bool,
    #[serde(default)]
    pub mutating: bool,
}

/// The outcome of asking the remote to abort the live turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The remote fired the run's cancellation token.
    Requested,
    /// There is no live turn to cancel.
    Idle,
    /// The task had already finished — benign for an opportunistic cancel.
    AlreadyTerminal,
    /// This agent does not implement `tasks/cancel`.
    Unsupported(String),
}

/// A live attachment to one served agent's conversation.
///
/// The `context_id` is the conversation key: a flux-served agent maps one `contextId` to one
/// session, so reusing it across processes continues the *same* remote conversation. The task id is
/// learned from the first frame of a turn (task id == session id server-side) and is what
/// [`AttachedA2aAgent::cancel`], [`AttachedA2aAgent::history`] and
/// [`AttachedA2aAgent::resubscribe`] address.
pub struct AttachedA2aAgent {
    client: A2aClient,
    context_id: String,
    /// Display identity from the agent card — never a credential, never the token.
    label: String,
    support: AttachSupport,
    /// The task most recently seen on this attachment.
    task_id: Mutex<Option<String>>,
}

/// The two probe ids are deliberately impossible task ids: a `tasks/cancel` for a task that cannot
/// exist distinguishes "this agent does not implement cancel" (`-32004`) from "it does, and that id
/// is unknown" (`-32001`) without touching any real run.
const CANCEL_PROBE_TASK: &str = "@probe/attach-cancel-support";

impl AttachedA2aAgent {
    /// Connect to the served agent at `url`, authenticated with `token`, and bind to `context_id`
    /// (a fresh one is minted when `None`). Fetches the agent card, adopts its advertised endpoint,
    /// and probes what the far side supports.
    ///
    /// `token` is a *value the caller already resolved from a reference* — this function never
    /// reads the environment and never logs it; the label it builds comes from the card alone.
    pub async fn connect(
        url: &str,
        token: Option<String>,
        context_id: Option<String>,
    ) -> Result<Self> {
        let mut client = A2aClient::new(url)?.with_token(token);
        let (label, streaming) = match client.fetch_agent_card().await {
            Ok(card) => {
                let name = if card.name.is_empty() {
                    "a2a agent".to_string()
                } else {
                    card.name.clone()
                };
                let label = if card.version.is_empty() {
                    name
                } else {
                    format!("{name} v{}", card.version)
                };
                let streaming = if card.capabilities.streaming {
                    Availability::Available
                } else {
                    Availability::Unavailable(
                        "the agent card declares no streaming — turns arrive whole, at the end"
                            .to_string(),
                    )
                };
                (label, streaming)
            }
            // No card is not fatal: `message/send` is the lowest common denominator and returns a
            // clear result or a clear error, unlike a silent non-SSE response to `message/stream`.
            Err(e) => (
                "a2a agent".to_string(),
                Availability::Unavailable(format!(
                    "no agent card ({e}) — falling back to whole-turn message/send"
                )),
            ),
        };
        let cancel = probe_cancel(&client).await;
        // `tasks/get` retains the same task model `tasks/cancel` does, so one probe answers both:
        // an agent with no addressable task surface can neither cancel nor replay.
        let history = match &cancel {
            Availability::Available => Availability::Available,
            Availability::Unavailable(_) => Availability::Unavailable(
                "this agent keeps no addressable task, so it cannot replay what happened while \
                 you were detached"
                    .to_string(),
            ),
        };
        let approvals = probe_approvals(&client).await;
        Ok(AttachedA2aAgent {
            client,
            context_id: context_id.unwrap_or_else(new_id),
            label,
            support: AttachSupport {
                streaming,
                cancel,
                history,
                approvals,
            },
            task_id: Mutex::new(None),
        })
    }

    /// Where this attachment points and what it found there — safe to render; carries no secret.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The JSON-RPC endpoint in use (after agent-card adoption).
    pub fn endpoint(&self) -> &str {
        self.client.rpc_url()
    }

    /// The conversation key. Reusing it on a later attach continues the same remote session.
    pub fn context_id(&self) -> &str {
        &self.context_id
    }

    pub fn support(&self) -> &AttachSupport {
        &self.support
    }

    /// The task this attachment last saw, if any.
    pub fn task_id(&self) -> Option<String> {
        self.task_id.lock().ok().and_then(|t| t.clone())
    }

    fn remember_task(&self, id: &str) {
        if let Ok(mut slot) = self.task_id.lock() {
            *slot = Some(id.to_string());
        }
    }

    /// Send `text` into the live remote session and drive the turn to its end, emitting every
    /// update through `emit`.
    ///
    /// Streams when the agent card declares streaming; otherwise runs one blocking `message/send`
    /// and emits its answer as a single [`AttachEvent::Text`] — the difference is reported through
    /// [`AttachSupport::streaming`] rather than hidden.
    pub async fn send(&self, text: &str, emit: &mut (dyn FnMut(AttachEvent) + Send)) {
        let message = Message::user_text(text, Some(self.context_id.clone()));
        if self.support.streaming.is_available() {
            match self.client.stream(message).await {
                Ok(stream) => self.pump(stream, emit).await,
                Err(e) => emit(AttachEvent::Notice {
                    text: format!("the remote agent refused the stream: {e}"),
                    error: true,
                }),
            }
        } else {
            match self.client.send(message, true).await {
                Ok(outcome) => {
                    if let Some(task) = outcome.as_task() {
                        self.remember_task(&task.id);
                    }
                    let reply = outcome.final_text();
                    if !reply.is_empty() {
                        emit(AttachEvent::Text(reply));
                    }
                }
                Err(e) => emit(AttachEvent::Notice {
                    text: format!("the remote agent refused the turn: {e}"),
                    error: true,
                }),
            }
        }
        emit(AttachEvent::Ended);
    }

    /// Re-attach to the task this attachment last saw and follow it to its terminal frame, without
    /// starting a turn. A dropped stream is a transport event, not a cancellation: the remote run
    /// keeps going, and this picks it back up.
    pub async fn resubscribe(&self, emit: &mut (dyn FnMut(AttachEvent) + Send)) {
        let Some(task) = self.task_id() else {
            emit(AttachEvent::Notice {
                text: "nothing to re-attach to — this attachment has not seen a task yet"
                    .to_string(),
                error: false,
            });
            emit(AttachEvent::Ended);
            return;
        };
        match self.client.resubscribe(&task).await {
            Ok(stream) => self.pump(stream, emit).await,
            Err(e) => emit(AttachEvent::Notice {
                text: format!("could not re-attach to task {task}: {e}"),
                error: true,
            }),
        }
        emit(AttachEvent::Ended);
    }

    /// Decode one SSE stream into [`AttachEvent`]s, normalizing delta- vs snapshot-style agents so
    /// a consumer can append every [`AttachEvent::Text`] unconditionally.
    async fn pump(
        &self,
        mut stream: crate::client::EventStream,
        emit: &mut (dyn FnMut(AttachEvent) + Send),
    ) {
        // Everything rendered so far this stream, so a cumulative-snapshot agent contributes only
        // its new suffix instead of re-emitting the whole answer on every frame.
        let mut rendered = String::new();
        let push =
            |emit: &mut (dyn FnMut(AttachEvent) + Send), rendered: &mut String, text: &str| {
                let suffix = text.strip_prefix(rendered.as_str()).unwrap_or(text);
                if suffix.is_empty() {
                    return;
                }
                rendered.push_str(suffix);
                emit(AttachEvent::Text(suffix.to_string()));
            };
        while let Some(next) = stream.next().await {
            match next {
                Ok(StreamEvent::StatusUpdate(update)) => {
                    if let Some(message) = &update.status.message {
                        push(emit, &mut rendered, &message.text());
                    }
                    let terminal = update.status.state.is_terminal();
                    emit(AttachEvent::State {
                        state: state_name(update.status.state),
                        terminal,
                    });
                    self.remember_task(&update.task_id);
                    if update.is_final || terminal {
                        return;
                    }
                }
                Ok(StreamEvent::Message(message)) => push(emit, &mut rendered, &message.text()),
                Ok(StreamEvent::Task(task)) => {
                    self.remember_task(&task.id);
                    push(emit, &mut rendered, &task.final_text());
                    emit(AttachEvent::State {
                        state: state_name(task.status.state),
                        terminal: task.status.state.is_terminal(),
                    });
                    if task.status.state.is_terminal() {
                        return;
                    }
                }
                Ok(StreamEvent::ArtifactUpdate(update)) => {
                    self.remember_task(&update.task_id);
                    let text: String = update
                        .artifact
                        .parts
                        .iter()
                        .filter_map(|p| p.as_text())
                        .collect();
                    emit(AttachEvent::Artifact {
                        name: update.artifact.name.clone().unwrap_or_default(),
                        text,
                    });
                }
                Err(e) => {
                    emit(AttachEvent::Notice {
                        text: format!("the stream from the remote agent broke: {e}"),
                        error: true,
                    });
                    return;
                }
            }
        }
    }

    /// Ask the remote to abort the live turn (`tasks/cancel`).
    ///
    /// This fires the token the remote run observes between plan rounds, so it stops work that is
    /// genuinely still in flight rather than merely detaching this client from it.
    pub async fn cancel(&self) -> CancelOutcome {
        if let Availability::Unavailable(why) = &self.support.cancel {
            return CancelOutcome::Unsupported(why.clone());
        }
        let Some(task) = self.task_id() else {
            return CancelOutcome::Idle;
        };
        match self.client.cancel_task(&task).await {
            Ok(_) => CancelOutcome::Requested,
            Err(A2aError::Rpc { code, .. }) if code == i64::from(error::TASK_NOT_CANCELABLE) => {
                CancelOutcome::AlreadyTerminal
            }
            Err(A2aError::Rpc { code, message })
                if code == i64::from(error::UNSUPPORTED_OPERATION) =>
            {
                CancelOutcome::Unsupported(message)
            }
            Err(e) => CancelOutcome::Unsupported(e.to_string()),
        }
    }

    /// The conversation as the **remote** holds it — the authoritative history for a reattach.
    ///
    /// Read from `tasks/get`, whose `Task.history` is projected from the served agent's own event
    /// store. Nothing local is consulted, because nothing local was written.
    pub async fn history(&self) -> std::result::Result<Vec<AttachTurn>, String> {
        if let Availability::Unavailable(why) = &self.support.history {
            return Err(why.clone());
        }
        let Some(task) = self.task_id() else {
            // The honest shape of gap 2 in docs/designs/tui-attach.md: a `contextId` cannot be
            // resolved to its task id without running a turn, so a fresh process attaching to an
            // existing conversation has nothing to address yet.
            return Err(
                "this attachment has not seen a task yet — the served agent offers no \
                        read-only route from a contextId to its task, so history becomes \
                        available after the first turn"
                    .to_string(),
            );
        };
        let task = self
            .client
            .get_task(&task)
            .await
            .map_err(|e| format!("{e}"))?;
        Ok(task
            .history
            .iter()
            .filter_map(|m| {
                let text = m.text();
                (!text.trim().is_empty()).then_some(AttachTurn {
                    from_user: matches!(m.role, Role::User),
                    text,
                })
            })
            .collect())
    }

    /// The guarded effects the remote currently has parked for a human (C-453 `GET /approvals`).
    pub async fn pending_approvals(&self) -> std::result::Result<Vec<AttachApproval>, String> {
        if !self.support.approvals.is_answerable() {
            return Err(self.support.approvals.describe());
        }
        let (status, body) = self
            .client
            .origin_get("/approvals")
            .await
            .map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("GET /approvals: HTTP {status}"));
        }
        Ok(
            serde_json::from_value(body.get("approvals").cloned().unwrap_or(Value::Null))
                .unwrap_or_default(),
        )
    }

    /// Deliver one decision for one parked effect (C-453 `POST /approvals/{id}`).
    ///
    /// `fingerprint` must be the parked request's own, echoed verbatim: that equality is what binds
    /// this answer to the effect the operator was actually shown, and the server refuses a mismatch
    /// with `409` rather than approving something else.
    pub async fn decide_approval(
        &self,
        id: &str,
        fingerprint: &str,
        allow: bool,
        reason: Option<&str>,
    ) -> std::result::Result<(), String> {
        let mut body = json!({
            "fingerprint": fingerprint,
            "decision": if allow { "allow" } else { "deny" },
        });
        if let Some(reason) = reason.filter(|r| !r.is_empty()) {
            body["reason"] = Value::String(reason.to_string());
        }
        let (status, answer) = self
            .client
            .origin_post(&format!("/approvals/{id}"), &body)
            .await
            .map_err(|e| e.to_string())?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        // Every one of these means *nothing was approved*; say which, because an operator responds
        // to "already gone" and "you answered the wrong effect" differently.
        let detail = answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("no detail");
        Err(format!("POST /approvals/{id}: HTTP {status} — {detail}"))
    }
}

/// The lowercase wire name of a task state (matching the serde encoding), for display.
fn state_name(state: TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Probe `tasks/cancel` without touching a real run: an id that cannot exist separates "this agent
/// does not implement cancel" (`-32004`) from "it does, and that id is unknown" (`-32001`).
async fn probe_cancel(client: &A2aClient) -> Availability {
    match client.cancel_task(CANCEL_PROBE_TASK).await {
        // A server that answers *about the task* implements the method.
        Ok(_) => Availability::Available,
        Err(A2aError::Rpc { code, .. })
            if code == i64::from(error::TASK_NOT_FOUND)
                || code == i64::from(error::TASK_NOT_CANCELABLE) =>
        {
            Availability::Available
        }
        Err(A2aError::Rpc { code, message }) if code == i64::from(error::UNSUPPORTED_OPERATION) => {
            Availability::Unavailable(format!(
                "this agent does not implement tasks/cancel ({message}) — an interrupt detaches \
                 you, it does not stop the remote turn"
            ))
        }
        Err(e) => Availability::Unavailable(format!("tasks/cancel could not be probed: {e}")),
    }
}

/// Probe the C-453 approval posture. Each status is a different answer and none of them is
/// "no approvals": an empty list under a headless posture and a live queue with nothing parked
/// look identical, and only one of them has a human in the loop.
async fn probe_approvals(client: &A2aClient) -> ApprovalReach {
    match client.origin_get("/approvals").await {
        Ok((200, _)) => ApprovalReach::Answerable {
            caveat: "answers are attributed to this deployment's shared operator token, not to \
                     you — per-principal approval authorization is not available yet (C-687)"
                .to_string(),
        },
        Ok((501, body)) => ApprovalReach::NotRaised(
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("this server is not running the remote-approval posture")
                .to_string(),
        ),
        Ok((401 | 403, _)) => ApprovalReach::Unanswerable(
            "this credential is not admitted to the approval routes".to_string(),
        ),
        Ok((status, _)) => ApprovalReach::Unknown(format!("GET /approvals answered HTTP {status}")),
        Err(e) => ApprovalReach::Unknown(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unavailable_affordance_carries_its_reason() {
        let cancel = Availability::Unavailable("no task surface".to_string());
        assert!(!cancel.is_available());
        assert_eq!(cancel.reason(), Some("no task surface"));
        assert_eq!(Availability::Available.reason(), None);
    }

    #[test]
    fn every_approval_posture_describes_itself_rather_than_reading_as_empty() {
        // The C-453 rule, restated on the client: "nothing is parked" and "nobody is ever asked"
        // must never render the same way.
        let answerable = ApprovalReach::Answerable {
            caveat: "shared operator token".to_string(),
        };
        assert!(answerable.is_answerable());
        assert!(answerable.describe().contains("shared operator token"));

        let headless = ApprovalReach::NotRaised("headless approver".to_string());
        assert!(!headless.is_answerable());
        assert!(headless.describe().contains("never raised"));
        assert!(headless.describe().contains("headless approver"));

        assert!(!ApprovalReach::Unanswerable("wrong credential".into()).is_answerable());
        assert!(ApprovalReach::Unknown("timeout".into())
            .describe()
            .contains("unknown"));
    }

    #[test]
    fn task_state_names_match_the_wire() {
        assert_eq!(state_name(TaskState::Working), "working");
        assert_eq!(state_name(TaskState::InputRequired), "input-required");
        assert_eq!(state_name(TaskState::Canceled), "canceled");
    }
}
