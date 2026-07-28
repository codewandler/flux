//! Events and adapters connecting background agent work to the UI controller loop.

use super::*;
use crate::projection::staged_intent_entry;
use crossterm::event::KeyCode;

/// A UI event produced by the running turn (on a background task) for the event loop to render.
pub(super) enum UiEvent {
    Tagged {
        action_id: u64,
        event: Box<UiEvent>,
    },
    Text(String),
    Thinking(String),
    Planning(bool),
    Plan(serde_json::Value),
    Phase(String),
    Brief {
        goal: String,
        needs: Vec<String>,
    },
    Intent(IntentEntry),
    ToolCall {
        name: String,
        input: serde_json::Value,
    },
    ToolTiming {
        name: String,
        timing: flux_core::OperationTiming,
    },
    ToolResult {
        name: String,
        content: String,
        is_error: bool,
    },
    Usage(Usage),
    Notice {
        text: String,
        sev: Sev,
    },
    Approval {
        tool: String,
        subjects: Vec<String>,
        reply: oneshot::Sender<ApprovalChoice>,
    },
    Finished,
}

pub(super) type PendingApproval = (String, Vec<String>, oneshot::Sender<ApprovalChoice>);

/// Display state of the pending approval sheet (the reply channel stays in the event loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalView {
    pub tool: String,
    pub subjects: Vec<String>,
    /// First visible row of the subject list when it overflows the sheet.
    pub scroll: usize,
    /// `Some` = the `d` reason-input line is active with this draft; Enter resolves the approval
    /// as a denial carrying the reason, Esc returns to the sheet with the approval still
    /// pending (C-113).
    pub reason: Option<String>,
}

/// What a key press means while the approval sheet is open. Only explicit keys act — anything
/// else is `Ignore` (the sheet stays and the reply is NOT consumed), so a stray keystroke can't
/// silently deny (C-103).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalAction {
    Allow,
    AllowAlways,
    Deny,
    /// Open the one-line reason input; the denial is only resolved on Enter (C-113).
    DenyWithReason,
    Scroll(isize),
    Ignore,
}

pub(super) fn approval_key(code: KeyCode) -> ApprovalAction {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => ApprovalAction::Allow,
        KeyCode::Char('a') | KeyCode::Char('A') => ApprovalAction::AllowAlways,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ApprovalAction::Deny,
        KeyCode::Char('d') | KeyCode::Char('D') => ApprovalAction::DenyWithReason,
        KeyCode::Up => ApprovalAction::Scroll(-1),
        KeyCode::Down => ApprovalAction::Scroll(1),
        _ => ApprovalAction::Ignore,
    }
}

pub(super) fn show_next_approval(
    state: &mut ChatState,
    current: &mut Option<(String, oneshot::Sender<ApprovalChoice>)>,
    queued: &mut VecDeque<PendingApproval>,
) {
    if current.is_some() {
        return;
    }
    if let Some((tool, subjects, reply)) = queued.pop_front() {
        state.approval = Some(ApprovalView {
            tool: tool.clone(),
            subjects,
            scroll: 0,
            reason: None,
        });
        *current = Some((tool, reply));
    }
}

/// Forwards a turn's streamed output to the event loop over an mpsc channel.
pub(super) struct ChannelSink {
    pub(super) tx: mpsc::UnboundedSender<UiEvent>,
    pub(super) action_id: u64,
}

impl ChannelSink {
    fn send(&self, event: UiEvent) {
        let _ = self.tx.send(UiEvent::Tagged {
            action_id: self.action_id,
            event: Box::new(event),
        });
    }
}

pub(super) fn send_action_event(
    tx: &mpsc::UnboundedSender<UiEvent>,
    action_id: u64,
    event: UiEvent,
) {
    let _ = tx.send(UiEvent::Tagged {
        action_id,
        event: Box::new(event),
    });
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, text: &str) {
        self.send(UiEvent::Text(text.to_string()));
    }

    fn thinking_delta(&mut self, text: &str) {
        self.send(UiEvent::Thinking(text.to_string()));
    }

    fn planning(&mut self, active: bool) {
        self.send(UiEvent::Planning(active));
    }

    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.send(UiEvent::ToolCall {
            name: name.to_string(),
            input: input.clone(),
        });
    }

    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.send(UiEvent::ToolResult {
            name: name.to_string(),
            content: result.content.clone(),
            is_error: result.is_error,
        });
    }

    fn tool_timing(&mut self, name: &str, timing: &flux_core::OperationTiming) {
        self.send(UiEvent::ToolTiming {
            name: name.to_string(),
            timing: *timing,
        });
    }

    fn turn_end(&mut self, usage: Option<Usage>) {
        if let Some(usage) = usage {
            self.send(UiEvent::Usage(usage));
        }
    }

    fn observation(&mut self, observation: &flux_evidence::Observation) {
        if observation.kind == "flow.plan" {
            self.send(UiEvent::Plan(observation.data.clone()));
        } else if observation.kind == "loop.phase" {
            if let Some(phase) = observation
                .data
                .get("phase")
                .and_then(|value| value.as_str())
            {
                self.send(UiEvent::Phase(phase.to_string()));
            }
        } else if observation.kind == flux_evidence::KIND_TURN_INTENT {
            if let Some(intent) = staged_intent_entry(&observation.data) {
                self.send(UiEvent::Intent(intent));
            }
        } else if observation.kind == "flow.brief" {
            let goal = observation
                .data
                .get("goal")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let needs = observation
                .data
                .get("needs")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            self.send(UiEvent::Brief { goal, needs });
        } else if observation.kind == flux_evidence::KIND_DESTRUCTIVE {
            self.send(UiEvent::Notice {
                text: "⚠ destructive operation flagged".into(),
                sev: Sev::Warn,
            });
        } else if observation.kind == "skill.activated" {
            if let Some(name) = observation
                .data
                .get("skill")
                .and_then(|value| value.as_str())
            {
                self.send(UiEvent::Notice {
                    text: format!("✦ skill activated: {name}"),
                    sev: Sev::Info,
                });
            }
        } else if observation.kind == "flow.halt" {
            self.send(UiEvent::Notice {
                text: halt_line(&observation.data),
                sev: Sev::Err,
            });
        }
    }
}

/// An [`Approver`] that raises an approval request to the event loop and awaits its reply.
pub(super) struct ChannelApprover {
    pub(super) tx: mpsc::UnboundedSender<UiEvent>,
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(UiEvent::Approval {
                tool: tool.to_string(),
                subjects: subjects.to_vec(),
                reply,
            })
            .is_err()
        {
            return ApprovalChoice::Deny;
        }
        rx.await.unwrap_or(ApprovalChoice::Deny)
    }
}
