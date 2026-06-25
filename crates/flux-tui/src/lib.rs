//! `flux-tui` — a ratatui chat frontend for the agent.
//!
//! [`render`] draws the chat (a scrolling transcript + an input box, plus an optional approval
//! modal) into a ratatui frame and is verified headlessly with `TestBackend`. [`run`] drives the
//! real interactive loop over crossterm: type, Enter submits a turn that **streams token-by-token**
//! into the transcript, tool activity appears live, Ctrl-C interrupts the turn, and tool calls that
//! need approval raise a y/a/N modal (the TUI installs its own [`ChannelApprover`], so it no longer
//! requires `--yes`).

pub mod toolview;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use flux_agent::AgentSink;
use flux_core::Role;
use flux_flow::engine::FlowEngine;
use flux_runtime::{ApprovalChoice, Approver, ToolResult};
use flux_spec::IntentSet;

/// One rendered line of the transcript.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub role: Role,
    pub text: String,
}

/// The chat view's state.
#[derive(Debug, Default, Clone)]
pub struct ChatState {
    pub lines: Vec<ChatLine>,
    pub input: String,
    pub status: String,
    /// When set, an approval modal is shown over the transcript.
    pub modal: Option<String>,
    /// Whether the last line is the in-progress (streaming) assistant line.
    assistant_open: bool,
}

impl ChatState {
    fn push(&mut self, role: Role, text: impl Into<String>) {
        self.lines.push(ChatLine {
            role,
            text: text.into(),
        });
        self.assistant_open = false;
    }

    /// Append a streamed assistant token, extending the live assistant line (or starting one).
    fn stream_text(&mut self, delta: &str) {
        if self.assistant_open {
            if let Some(last) = self.lines.last_mut() {
                last.text.push_str(delta);
                return;
            }
        }
        self.lines.push(ChatLine {
            role: Role::Assistant,
            text: delta.to_string(),
        });
        self.assistant_open = true;
    }

    fn end_stream(&mut self) {
        if self.assistant_open {
            if let Some(last) = self.lines.last_mut() {
                let trimmed = last.text.trim_end().to_string();
                last.text = trimmed;
            }
        }
        self.assistant_open = false;
    }
}

/// A centered sub-rect `w`×`h` (clamped to `area`).
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Render the chat: transcript on top, a single-line input box at the bottom, optional modal.
pub fn render(frame: &mut Frame, state: &ChatState) {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(frame.area());

    let mut lines: Vec<Line> = Vec::new();
    for l in &state.lines {
        let (prefix, style) = match l.role {
            Role::User => ("› ", Style::default().fg(Color::Yellow)),
            Role::Assistant => ("", Style::default()),
            Role::System => ("• ", Style::default().fg(Color::DarkGray)),
        };
        lines.push(Line::styled(format!("{prefix}{}", l.text), style));
    }
    let transcript = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("flux"));
    frame.render_widget(transcript, chunks[0]);

    let input = Paragraph::new(state.input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(state.status.clone()),
    );
    frame.render_widget(input, chunks[1]);

    if let Some(modal) = &state.modal {
        let area = centered(frame.area(), 64, 7);
        frame.render_widget(Clear, area);
        let p = Paragraph::new(modal.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("approve")
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        frame.render_widget(p, area);
    }
}

/// A UI event produced by the running turn (on a background task) for the event loop to render.
enum UiEvent {
    Text(String),
    /// The planner is composing (`true`) / done (`false`) — drives the status line.
    Planning(bool),
    Tool(String),
    ToolResult(String),
    Observation(String),
    Approval {
        tool: String,
        subjects: Vec<String>,
        reply: oneshot::Sender<ApprovalChoice>,
    },
    Finished,
}

/// Forwards a turn's streamed output to the event loop over an mpsc channel.
struct ChannelSink {
    tx: mpsc::UnboundedSender<UiEvent>,
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, t: &str) {
        let _ = self.tx.send(UiEvent::Text(t.to_string()));
    }
    fn planning(&mut self, active: bool) {
        let _ = self.tx.send(UiEvent::Planning(active));
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let call = crate::toolview::format_call(name, input);
        let label = if call.arg.is_empty() {
            call.verb
        } else {
            format!("{}  {}", call.verb, call.arg)
        };
        let _ = self.tx.send(UiEvent::Tool(label));
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let tag = if result.is_error { "✗" } else { "✓" };
        let body = crate::toolview::format_result(name, &result.content, result.is_error)
            .unwrap_or_else(|| result.content.trim().chars().take(160).collect());
        let _ = self.tx.send(UiEvent::ToolResult(format!("  {tag} {body}")));
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        if o.kind == flux_evidence::KIND_DESTRUCTIVE {
            let _ = self.tx.send(UiEvent::Observation(
                "⚠ destructive operation flagged".into(),
            ));
        } else if o.kind == "skill.activated" {
            if let Some(name) = o.data.get("skill").and_then(|v| v.as_str()) {
                let _ = self
                    .tx
                    .send(UiEvent::Observation(format!("✦ skill activated: {name}")));
            }
        }
    }
}

/// An [`Approver`] that raises an approval request to the event loop and awaits its reply.
struct ChannelApprover {
    tx: mpsc::UnboundedSender<UiEvent>,
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

fn idle_status(model: &str) -> String {
    format!("flux · {model} · Enter sends · Ctrl-C interrupts · Esc quits")
}

type Tui = Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Run the interactive TUI against `agent`/`session_id`. Requires a real terminal. Installs a modal
/// approver, then always restores the terminal (raw mode + alternate screen) even on error.
pub async fn run(mut agent: FlowEngine, session_id: String) -> anyhow::Result<()> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    let (tx, rx) = mpsc::unbounded_channel::<UiEvent>();
    agent
        .executor
        .set_approver(Arc::new(ChannelApprover { tx: tx.clone() }));
    let agent = Arc::new(agent);

    enable_raw_mode()?;
    let mut out = std::io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(out))?;

    let mut state = ChatState {
        status: idle_status(&agent.model),
        ..Default::default()
    };
    let result = event_loop(&mut terminal, agent, &session_id, &mut state, tx, rx).await;

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

async fn event_loop(
    terminal: &mut Tui,
    agent: Arc<FlowEngine>,
    session_id: &str,
    state: &mut ChatState,
    tx: mpsc::UnboundedSender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<UiEvent>,
) -> anyhow::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    let mut running = false;
    let mut cancel = CancellationToken::new();
    let mut pending_reply: Option<(String, oneshot::Sender<ApprovalChoice>)> = None;

    loop {
        // Drain everything the running turn has produced.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                UiEvent::Text(t) => state.stream_text(&t),
                UiEvent::Planning(active) => {
                    state.status = if active {
                        "composing plan…".to_string()
                    } else {
                        "thinking…".to_string()
                    };
                }
                UiEvent::Tool(t) => state.push(Role::System, format!("→ {t}")),
                UiEvent::ToolResult(t) => state.push(Role::System, t),
                UiEvent::Observation(t) => state.push(Role::System, t),
                UiEvent::Approval {
                    tool,
                    subjects,
                    reply,
                } => {
                    state.modal = Some(format!(
                        "approve `{tool}` {subjects:?}\n\n[y]es   [a]lways   [N]o"
                    ));
                    pending_reply = Some((tool, reply));
                }
                UiEvent::Finished => {
                    running = false;
                    state.end_stream();
                    state.status = idle_status(&agent.model);
                }
            }
        }

        terminal.draw(|f| render(f, state))?;

        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // Modal mode: the next key answers the pending approval.
        if state.modal.is_some() {
            if let Some((tool, reply)) = pending_reply.take() {
                let choice = match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => ApprovalChoice::Allow,
                    KeyCode::Char('a') | KeyCode::Char('A') => ApprovalChoice::AllowAlways(tool),
                    _ => ApprovalChoice::Deny,
                };
                let _ = reply.send(choice);
            }
            state.modal = None;
            continue;
        }

        match key.code {
            KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if running {
                    cancel.cancel();
                    state.push(Role::System, "(interrupting…)");
                } else {
                    break;
                }
            }
            KeyCode::Enter if !running => {
                let input = std::mem::take(&mut state.input).trim().to_string();
                if input.is_empty() {
                    continue;
                }
                state.push(Role::User, input.clone());
                state.status = "thinking…".to_string();

                running = true;
                cancel = CancellationToken::new();
                let task_agent = agent.clone();
                let task_sid = session_id.to_string();
                let task_tx = tx.clone();
                let task_cancel = cancel.clone();
                tokio::spawn(async move {
                    let mut sink = ChannelSink {
                        tx: task_tx.clone(),
                    };
                    if let Err(e) = task_agent
                        .run_turn_cancellable(&task_sid, &input, &mut sink, &task_cancel)
                        .await
                    {
                        let _ = task_tx.send(UiEvent::Observation(format!("error: {e}")));
                    }
                    let _ = task_tx.send(UiEvent::Finished);
                });
            }
            KeyCode::Char(c) if !running => state.input.push(c),
            KeyCode::Backspace if !running => {
                state.input.pop();
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn renders_transcript_and_input() {
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        let mut state = ChatState {
            status: "ready".to_string(),
            ..Default::default()
        };
        state.push(Role::User, "hello flux");
        state.push(Role::Assistant, "hi there");
        state.input = "next message".to_string();

        terminal.draw(|f| render(f, &state)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(content.contains("hello flux"));
        assert!(content.contains("hi there"));
        assert!(content.contains("next message"));
        assert!(content.contains("flux")); // border title
    }

    #[test]
    fn streams_text_into_one_assistant_line_and_renders_modal() {
        let mut state = ChatState::default();
        state.stream_text("Hel");
        state.stream_text("lo");
        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0].text, "Hello");
        // a discrete line closes the stream; the next delta starts a fresh assistant line
        state.push(Role::System, "→ bash");
        state.stream_text("done");
        assert_eq!(state.lines.len(), 3);
        assert_eq!(state.lines[2].text, "done");

        // modal renders over the transcript
        state.modal = Some("approve `bash`\n[y]es [a]lways [N]o".to_string());
        let mut terminal = Terminal::new(TestBackend::new(70, 16)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("approve"));
    }
}
