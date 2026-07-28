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
    /// One completed model call, delivered live from the engine's `model.call` observation
    /// (C-139 for the usage, C-180 for the latency). The turn-end [`UiEvent::Usage`] carries
    /// `Usage::accumulate`'s occupancy snapshot — the last round only — so cache accounting reads
    /// this instead.
    CallUsage {
        model: String,
        stage: String,
        round: usize,
        operations: usize,
        usage: Usage,
        timing: ModelCallTiming,
    },
    /// A provider connect retry reported *while the wait is still ahead of us* (C-181) — a backed-off
    /// 429/5xx, a forced OAuth refresh, or a transport→HTTP fallback.
    Retry {
        attempt: u32,
        max_attempts: u32,
        delay: Duration,
        reason: String,
    },
    Notice {
        text: String,
        sev: Sev,
    },
    Approval {
        request: ApprovalRequest,
        reply: oneshot::Sender<ApprovalChoice>,
    },
    /// Queued steering messages the engine consumed and injected into the running turn (A-94).
    Steered(Vec<String>),
    Finished,
}

/// What one model call cost in wall clock, off the engine's own `model.call` metrics (C-180).
///
/// The TUI attributed execution time to operations and nothing else, so the model wait — usually
/// the bulk of a turn — was invisible. `duration` is the whole provider call including any
/// connect-phase recovery; `ttft` is time to the first streamed chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ModelCallTiming {
    pub(super) duration: Duration,
    pub(super) ttft: Option<Duration>,
    /// Backed-off connect retries this call paid for (C-181).
    pub(super) retries: u32,
}

pub(super) type PendingApproval = (ApprovalRequest, oneshot::Sender<ApprovalChoice>);

/// What the surface is being asked to authorize — one op, or a whole plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApprovalRequest {
    /// The op name, or `run plan` for whole-plan approval.
    pub tool: String,
    /// The lines describing what will be touched: permission subjects for an op, or (for a plan)
    /// its op names and concrete statically-visible targets.
    pub subjects: Vec<String>,
    /// The plan's risk summary (`low · mutating`), rendered in risk color. `None` for a per-op
    /// approval, which has no aggregate risk of its own.
    pub summary: Option<String>,
    /// Whether the request discloses a destructive operation.
    pub destructive: bool,
}

/// Display state of the pending approval sheet (the reply channel stays in the event loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalView {
    pub request: ApprovalRequest,
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
    if let Some((request, reply)) = queued.pop_front() {
        let tool = request.tool.clone();
        state.approval = Some(ApprovalView {
            request,
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
        if observation.kind == "model.call" {
            // C-139: the engine emits one of these per model call with the full `Usage`. It is the
            // live twin of the `CallUsage` event `flux usage` reads offline, and the only per-call
            // usage a surface can see — `turn_end` hands over the accumulated occupancy snapshot.
            // C-180: the same observation carries the call's latency; the surface used to drop it.
            if let Some(usage) = observation
                .data
                .get("usage")
                .and_then(|value| serde_json::from_value::<Usage>(value.clone()).ok())
            {
                let text = |key: &str| {
                    observation
                        .data
                        .get(key)
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let number = |key: &str| observation.data.get(key).and_then(|value| value.as_u64());
                self.send(UiEvent::CallUsage {
                    model: text("model"),
                    stage: text("stage"),
                    round: number("round").unwrap_or_default() as usize,
                    operations: number("operations").unwrap_or_default() as usize,
                    usage,
                    timing: ModelCallTiming {
                        duration: Duration::from_micros(number("duration_us").unwrap_or_default()),
                        ttft: number("ttft_us").map(Duration::from_micros),
                        retries: number("retries").unwrap_or_default() as u32,
                    },
                });
            }
        } else if observation.kind == "model.retry" {
            // C-181: reported before the backoff sleep, so the footer can name the wait while the
            // user is still in it rather than explaining it afterwards.
            let number = |key: &str| observation.data.get(key).and_then(|value| value.as_u64());
            self.send(UiEvent::Retry {
                attempt: number("attempt").unwrap_or(1) as u32,
                max_attempts: number("max_attempts").unwrap_or_default() as u32,
                delay: Duration::from_millis(number("delay_ms").unwrap_or_default()),
                reason: observation
                    .data
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .unwrap_or("provider")
                    .to_string(),
            });
        } else if observation.kind == "flow.plan" {
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
        } else if observation.kind == "turn.steering" {
            let messages = observation
                .data
                .get("messages")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !messages.is_empty() {
                self.send(UiEvent::Steered(messages));
            }
        }
    }
}

/// An [`Approver`] that raises an approval request to the event loop and awaits its reply.
pub(super) struct ChannelApprover {
    pub(super) tx: mpsc::UnboundedSender<UiEvent>,
}

impl ChannelApprover {
    /// Raise `request` to the event loop and await the user's answer. A closed channel (the UI is
    /// gone) denies — approval is never granted by default.
    async fn ask(&self, request: ApprovalRequest) -> ApprovalChoice {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(UiEvent::Approval { request, reply }).is_err() {
            return ApprovalChoice::Deny;
        }
        rx.await.unwrap_or(ApprovalChoice::Deny)
    }
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        self.ask(ApprovalRequest {
            tool: tool.to_string(),
            subjects: subjects.to_vec(),
            ..ApprovalRequest::default()
        })
        .await
    }

    /// C-182: whole-plan approval. Without this the default `request_plan` collapses the entire
    /// batch to one line reading `3 op(s) · low · mutating` — the user is asked to authorize three
    /// operations without being told which three, or against what. The request already carries the
    /// op names and typed requirements (the plain CLI's `plan_prompt` renders both); this spends
    /// them.
    async fn request_plan(&self, plan: &flux_runtime::PlanApprovalRequest) -> ApprovalChoice {
        self.ask(ApprovalRequest {
            tool: "run plan".to_string(),
            subjects: plan_detail_lines(plan),
            summary: Some(plan.summary.clone()),
            destructive: plan.destructive,
        })
        .await
    }
}

/// The sheet's body for a whole-plan approval: the op names, then the concrete resources and
/// commands statically visible at approval time. Mirrors the plain CLI's `plan_prompt` so the two
/// surfaces disclose the same facts.
///
/// Only literal arguments contribute — a command assembled from `$symbols` at runtime is invisible
/// here, which is exactly why dispatch re-fires the per-op gate for an undisclosed destructive op.
/// Nothing here changes that; it is a display.
pub(super) fn plan_detail_lines(plan: &flux_runtime::PlanApprovalRequest) -> Vec<String> {
    let mut lines = Vec::new();
    if !plan.ops.is_empty() {
        lines.push(format!("ops: {}", plan.ops.join(", ")));
    }
    for requirement in &plan.requirements {
        // Operation-kind requirements only restate the ops line above.
        if requirement.resource.kind == flux_policy::ResourceKind::Operation {
            continue;
        }
        let subject = requirement
            .resource
            .path
            .as_deref()
            .or(requirement.resource.name.as_deref())
            .unwrap_or(&requirement.resource.id);
        if subject == "*" {
            continue;
        }
        lines.push(format!("{} → {subject}", requirement.action.0));
    }
    for intent in &plan.intents.intents {
        if let flux_spec::IntentTarget::Process { command } = &intent.target {
            lines.push(format!("process.exec → $ {command}"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    lines.retain(|line| seen.insert(line.clone()));
    lines
}
