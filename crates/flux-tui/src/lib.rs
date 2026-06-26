//! `flux-tui` — a ratatui chat frontend for the agent.
//!
//! [`render`] draws the chat — a **scrollable** transcript, a one-line status/spinner row, and an
//! input box, plus an optional approval modal — into a ratatui frame and is verified headlessly with
//! `TestBackend`. [`run`] drives the real interactive loop over crossterm: type, Enter submits a turn
//! that **streams token-by-token** into the transcript (assistant replies render as **Markdown**),
//! tool activity appears live, the planner's **DAG plan** is shown inline, an **animated spinner**
//! tracks the running turn, PgUp/PgDn/wheel scroll the history, Ctrl-C interrupts, and tool calls
//! that need approval raise a y/a/N modal (the TUI installs its own [`ChannelApprover`]).

pub mod theme;
pub mod toolview;

mod markdown;
mod plan;

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;

use flux_agent::AgentSink;
use flux_core::Usage;
use flux_flow::engine::FlowEngine;
use flux_runtime::{ApprovalChoice, Approver, ToolResult};
use flux_spec::IntentSet;

use crate::theme::Theme;

/// Braille spinner frames (shared idiom with the CLI).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Streaming cursor block appended to an in-progress assistant message.
const CURSOR: &str = "▍";

/// Severity of a system notice, picking its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sev {
    Info,
    Warn,
    Err,
}

/// A slash command shown in the `/` menu.
struct SlashCmd {
    name: &'static str,
    desc: &'static str,
}

/// The available slash commands (all argument-free).
const COMMANDS: &[SlashCmd] = &[
    SlashCmd {
        name: "help",
        desc: "show keybindings",
    },
    SlashCmd {
        name: "clear",
        desc: "clear the transcript",
    },
    SlashCmd {
        name: "new",
        desc: "clear and start fresh",
    },
    SlashCmd {
        name: "model",
        desc: "show the active model",
    },
    SlashCmd {
        name: "quit",
        desc: "exit flux",
    },
];

/// Commands matching `query` (lowercased, no leading `/`): prefix matches first, then substring.
fn slash_matches(query: &str) -> Vec<&'static SlashCmd> {
    let mut out: Vec<&SlashCmd> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(query))
        .collect();
    out.extend(
        COMMANDS
            .iter()
            .filter(|c| !c.name.starts_with(query) && c.name.contains(query)),
    );
    out
}

/// The `/help` body.
const HELP_TEXT: &str = "keybindings:\n\
    ↵ send · Ctrl-J / Alt-↵ newline · ↑/↓ history · Ctrl-E expand tools\n\
    PgUp/PgDn / wheel scroll · /command menu · Ctrl-C interrupt · Esc quit";

/// One item in the transcript. Each renders to one or more styled [`Line`]s at a given width.
#[derive(Debug)]
enum Entry {
    /// A user message (may contain newlines once the input is multiline).
    User(String),
    /// An assistant reply — plain while streaming, Markdown once done (cached per width).
    Assistant(Assistant),
    /// Live extended-thinking tokens streamed during the planning phase, rendered as Markdown
    /// once sealed (same `Assistant` widget, distinct entry so it doesn't merge with the reply).
    Thinking(Assistant),
    /// A dispatched tool/op call + (once it returns) its result — rendered as one card.
    Tool(ToolEntry),
    /// An observation/notice (skill activation, destructive flag, error).
    Notice { text: String, sev: Sev },
    /// The planner's compiled DAG (the `flow.plan` observation payload).
    Plan(serde_json::Value),
}

/// A tool/op call paired with its result, rendered as a card: a `→ verb arg … [badge]` header, a
/// one-line summary, and (when expanded) the full detail (a diff for `edit`/`write`, else output).
#[derive(Debug)]
struct ToolEntry {
    name: String,
    call: toolview::Call,
    /// The op input (so a diff/preview can be rendered exactly).
    input: serde_json::Value,
    started: Instant,
    /// `None` while the op is still running.
    result: Option<ToolOutcome>,
}

#[derive(Debug)]
struct ToolOutcome {
    is_error: bool,
    content: String,
    /// A one-line summary (e.g. `3 matches`) when [`toolview::format_result`] has one.
    summary: Option<String>,
    elapsed: Duration,
}

impl ToolEntry {
    fn new(name: String, input: serde_json::Value) -> Self {
        let call = toolview::format_call(&name, &input);
        ToolEntry {
            name,
            call,
            input,
            started: Instant::now(),
            result: None,
        }
    }
}

/// A streaming-then-finalized assistant message with a per-width render cache.
#[derive(Debug, Default)]
struct Assistant {
    text: String,
    done: bool,
    /// `(width, rendered lines)` — only populated once `done`, recomputed when the width changes.
    cache: RefCell<Option<(u16, Vec<Line<'static>>)>>,
}

impl Assistant {
    fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if !self.done {
            // Streaming: plain text (half-parsed Markdown flickers) + a cursor on the last line.
            let mut lines: Vec<Line> = self
                .text
                .split('\n')
                .map(|l| Line::styled(l.to_string(), theme.assistant_style()))
                .collect();
            if lines.is_empty() {
                lines.push(Line::default());
            }
            if let Some(last) = lines.last_mut() {
                last.spans.push(Span::styled(CURSOR, theme.accent_style()));
            }
            return lines;
        }
        if let Some((w, cached)) = self.cache.borrow().as_ref() {
            if *w == width {
                return cached.clone();
            }
        }
        let lines = markdown::render(&self.text, width).lines;
        *self.cache.borrow_mut() = Some((width, lines.clone()));
        lines
    }
}

/// Build a fresh, configured input editor (placeholder + no cursor-line highlight). Used at startup
/// and to clear the box after a submit, preserving its configuration.
fn fresh_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_placeholder_text("Type a message…  (Enter sends · Ctrl-J / Alt-↵ newline)");
    ta.set_cursor_line_style(Style::default());
    ta
}

/// The chat view's state.
#[derive(Debug)]
pub struct ChatState {
    entries: Vec<Entry>,
    /// The multiline input editor.
    input: TextArea<'static>,
    /// When set, an approval modal is shown over the transcript.
    pub modal: Option<String>,
    /// Whether the last entry is the in-progress (streaming) assistant message.
    assistant_open: bool,
    /// What the agent is doing right now (drives the status/spinner row).
    phase: Phase,
    /// Start of the running turn (for the elapsed timer + spinner frame).
    turn_start: Option<Instant>,
    model: String,
    theme: Theme,
    /// Whether tool cards show their full detail (toggled with Ctrl-E).
    expand_tools: bool,
    /// Selected row in the slash-command menu.
    slash_sel: usize,
    // --- session metrics (header/footer) ---
    /// Cumulative input/output tokens this session.
    tokens_in: u64,
    tokens_out: u64,
    /// Tool ops run during the in-progress / most recent turn.
    steps: usize,
    /// Wall-clock of the most recent finished turn.
    last_elapsed: Option<Duration>,
    // --- input history (Up/Down recall) ---
    /// Previously submitted prompts, oldest first.
    history: Vec<String>,
    /// Cursor into `history` while recalling; `None` when editing fresh input.
    history_pos: Option<usize>,
    /// The in-progress text stashed when recall began, restored on Down past the newest entry.
    history_draft: String,
    // --- scrollback ---
    /// Top wrapped-line offset; ignored while `follow` is set.
    scroll: u16,
    /// Stick to the bottom as new content arrives (detached by scrolling up).
    follow: bool,
    /// Last-rendered max scroll offset + viewport height, so the event loop can clamp paging.
    last_max_scroll: Cell<u16>,
    last_page: Cell<u16>,
}

/// What the agent is doing — drives the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Thinking,
    Planning,
}

impl ChatState {
    fn new(model: String) -> Self {
        ChatState {
            entries: Vec::new(),
            input: fresh_textarea(),
            modal: None,
            assistant_open: false,
            phase: Phase::Idle,
            turn_start: None,
            model,
            theme: Theme::default(),
            expand_tools: false,
            slash_sel: 0,
            tokens_in: 0,
            tokens_out: 0,
            steps: 0,
            last_elapsed: None,
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            scroll: 0,
            follow: true,
            last_max_scroll: Cell::new(0),
            last_page: Cell::new(1),
        }
    }

    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        self.assistant_open = false;
    }

    /// Append a user message.
    fn push_user(&mut self, text: impl Into<String>) {
        self.push(Entry::User(text.into()));
    }

    /// Open a fresh thinking entry for the upcoming planning call (called on `Planning(true)`).
    fn begin_thinking(&mut self) {
        // Only open a new thinking entry if there isn't already an open one.
        if !matches!(self.entries.last(), Some(Entry::Thinking(a)) if !a.done) {
            self.entries.push(Entry::Thinking(Assistant {
                text: String::new(),
                done: false,
                cache: RefCell::new(None),
            }));
            self.assistant_open = false;
        }
    }

    /// Append a thinking-token delta to the open thinking entry.
    fn stream_thinking(&mut self, delta: &str) {
        if let Some(Entry::Thinking(a)) = self.entries.last_mut() {
            if !a.done {
                a.text.push_str(delta);
                return;
            }
        }
        // No open thinking entry — open one on the fly.
        self.entries.push(Entry::Thinking(Assistant {
            text: delta.to_string(),
            done: false,
            cache: RefCell::new(None),
        }));
        self.assistant_open = false;
    }

    /// Seal the open thinking entry (called on `Planning(false)`).
    fn end_thinking(&mut self) {
        if let Some(Entry::Thinking(a)) = self.entries.last_mut() {
            if !a.done {
                a.text = a.text.trim_end().to_string();
                a.done = true;
            }
        }
    }

    /// Append a streamed assistant token, extending the live assistant message (or starting one).
    fn stream_text(&mut self, delta: &str) {
        if self.assistant_open {
            if let Some(Entry::Assistant(a)) = self.entries.last_mut() {
                a.text.push_str(delta);
                return;
            }
        }
        self.entries.push(Entry::Assistant(Assistant {
            text: delta.to_string(),
            done: false,
            cache: RefCell::new(None),
        }));
        self.assistant_open = true;
    }

    fn end_stream(&mut self) {
        if self.assistant_open {
            if let Some(Entry::Assistant(a)) = self.entries.last_mut() {
                a.text = a.text.trim_end().to_string();
                a.done = true;
            }
        }
        self.assistant_open = false;
    }

    /// Visual rows the input box wants (content lines, clamped 1..=6), excluding borders.
    fn input_rows(&self) -> u16 {
        (self.input.lines().len() as u16).clamp(1, 6)
    }

    /// True when the input is empty or whitespace-only.
    fn input_blank(&self) -> bool {
        self.input.lines().iter().all(|l| l.trim().is_empty())
    }

    /// Take the input text (lines joined with `\n`) and reset the editor to empty.
    fn take_input(&mut self) -> String {
        let text = self.input.lines().join("\n");
        self.input = fresh_textarea();
        text
    }

    /// The slash-menu query: `Some(rest)` when the input is a single line `/rest` with no whitespace
    /// (so the menu only shows while choosing a command, not while typing its arguments).
    fn slash_query(&self) -> Option<String> {
        let lines = self.input.lines();
        if lines.len() != 1 {
            return None;
        }
        let rest = lines[0].strip_prefix('/')?;
        if rest.contains(char::is_whitespace) {
            return None;
        }
        Some(rest.to_lowercase())
    }

    fn slash_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let s = self.slash_sel.min(n - 1);
        self.slash_sel = if s == 0 { n - 1 } else { s - 1 };
    }

    fn slash_down(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let s = self.slash_sel.min(n - 1);
        self.slash_sel = if s + 1 >= n { 0 } else { s + 1 };
    }

    /// Replace the input with `text`, cursor at the end (used by history recall).
    fn set_input(&mut self, text: &str) {
        let mut ta = fresh_textarea();
        ta.insert_str(text);
        self.input = ta;
    }

    /// Recall the previous history entry (Up at the top of the input).
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => {
                self.history_draft = self.input.lines().join("\n");
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        let text = self.history[pos].clone();
        self.set_input(&text);
    }

    /// Recall the next history entry, or restore the stashed draft past the newest (Down at the
    /// bottom of the input).
    fn history_next(&mut self) {
        let Some(p) = self.history_pos else {
            return;
        };
        if p + 1 < self.history.len() {
            self.history_pos = Some(p + 1);
            let text = self.history[p + 1].clone();
            self.set_input(&text);
        } else {
            self.history_pos = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_input(&draft);
        }
    }

    /// Record a submitted prompt and persist if it was new.
    fn record_history(&mut self, text: &str) {
        self.history_pos = None;
        self.history_draft.clear();
        if self.push_history(text) {
            save_history(&self.history);
        }
    }

    /// Append to in-memory history, skipping empties and consecutive duplicates. Returns whether the
    /// entry was added (so the caller can decide to persist).
    fn push_history(&mut self, text: &str) -> bool {
        if text.is_empty() || self.history.last().map(String::as_str) == Some(text) {
            return false;
        }
        self.history.push(text.to_string());
        true
    }

    /// Attach a result to the most recent still-running tool card. Ops dispatch sequentially, so the
    /// newest result-less [`Entry::Tool`] is the one that just returned.
    fn finish_tool(&mut self, name: &str, content: String, is_error: bool) {
        let summary = toolview::format_result(name, &content, is_error);
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Tool(tool) = entry {
                if tool.result.is_none() {
                    tool.result = Some(ToolOutcome {
                        is_error,
                        elapsed: tool.started.elapsed(),
                        summary,
                        content,
                    });
                    return;
                }
            }
        }
        // No matching call (shouldn't happen) — surface it as a notice so nothing is lost.
        self.push(Entry::Notice {
            text: content,
            sev: if is_error { Sev::Err } else { Sev::Info },
        });
    }

    /// Flatten the transcript to styled lines at `width`, with a blank line between entries.
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        let t = &self.theme;
        let mut out: Vec<Line> = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(Line::default());
            }
            match entry {
                Entry::User(text) => {
                    for (j, raw) in text.split('\n').enumerate() {
                        let prefix = if j == 0 { "› " } else { "  " };
                        out.push(Line::from(vec![
                            Span::styled(prefix, t.user_style()),
                            Span::styled(raw.to_string(), t.user_style()),
                        ]));
                    }
                }
                Entry::Assistant(a) => out.extend(a.lines(width, t)),
                Entry::Thinking(a) => {
                    // Prefix the thinking block with a dimmed header line.
                    if !a.text.is_empty() {
                        out.push(Line::styled("🤔 thinking…".to_string(), t.muted_style()));
                        out.extend(a.lines(width, t).into_iter().map(|mut l| {
                            // Dim the whole thinking block so it reads as secondary content.
                            for span in &mut l.spans {
                                span.style = span.style.patch(t.muted_style());
                            }
                            l
                        }));
                    }
                }
                Entry::Tool(tool) => out.extend(self.tool_lines(tool, width)),
                Entry::Notice { text, sev } => {
                    let style = match sev {
                        Sev::Info => t.muted_style(),
                        Sev::Warn => t.warn_style(),
                        Sev::Err => t.err_style(),
                    };
                    for raw in text.split('\n') {
                        out.push(Line::styled(raw.to_string(), style));
                    }
                }
                Entry::Plan(data) => out.extend(plan::render(data, t)),
            }
        }
        out
    }

    /// Render one tool card: a `→ verb arg … [badge]` header, a one-line summary, and — when
    /// `expand_tools` is set — the full detail (a unified diff for `edit`/`write`, else the output,
    /// capped).
    fn tool_lines(&self, tool: &ToolEntry, width: u16) -> Vec<Line<'static>> {
        const MAX_DETAIL: usize = 30;
        let t = &self.theme;
        let mut out: Vec<Line> = Vec::new();

        // Badge (right-aligned, fixed idea of width): running shows live elapsed, done shows ✓/✗.
        let (badge, badge_style) = match &tool.result {
            None => (
                format!("◌ {}", fmt_elapsed(tool.started.elapsed())),
                t.warn_style(),
            ),
            Some(o) if o.is_error => (format!("✗ {}", fmt_elapsed(o.elapsed)), t.err_style()),
            Some(o) => (format!("✓ {}", fmt_elapsed(o.elapsed)), t.ok_style()),
        };

        // Header: `→ verb  arg`, with the arg truncated so the badge sits flush right on one row.
        let verb = &tool.call.verb;
        let badge_w = badge.chars().count();
        let fixed = 2 + verb.chars().count() + 2; // "→ " + verb + "  "
        let arg_room = (width as usize).saturating_sub(fixed + badge_w + 1);
        let arg = truncate(&tool.call.arg, arg_room.max(4));
        let used = fixed + arg.chars().count();
        let pad = (width as usize).saturating_sub(used + badge_w).max(1);
        out.push(Line::from(vec![
            Span::styled("→ ", t.tool_style()),
            Span::styled(verb.clone(), t.tool_style().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(arg, t.muted_style()),
            Span::raw(" ".repeat(pad)),
            Span::styled(badge, badge_style),
        ]));

        // One-line summary (always, once the result is in).
        if let Some(o) = &tool.result {
            let summary = o
                .summary
                .clone()
                .or_else(|| o.content.trim().lines().next().map(str::to_string))
                .unwrap_or_else(|| "done".into());
            let style = if o.is_error {
                t.err_style()
            } else {
                t.muted_style()
            };
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(truncate(&summary, width.saturating_sub(2) as usize), style),
            ]));

            // Full detail, when expanded.
            if self.expand_tools {
                let detail =
                    toolview::format_detail(&tool.name, &tool.input, &o.content, o.is_error);
                let shown = detail.len().min(MAX_DETAIL);
                for (kind, text) in detail.iter().take(MAX_DETAIL) {
                    let style = match kind {
                        toolview::DetailKind::Add => t.ok_style(),
                        toolview::DetailKind::Del => t.err_style(),
                        toolview::DetailKind::Meta => t.accent_style(),
                        toolview::DetailKind::Plain => t.muted_style(),
                    };
                    out.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(text.clone(), style),
                    ]));
                }
                if detail.len() > shown {
                    out.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            format!("… {} more lines", detail.len() - shown),
                            t.muted_style(),
                        ),
                    ]));
                }
            }
        }
        out
    }

    /// The top header bar: identity + model on the left, cumulative session tokens on the right.
    fn header_line(&self, width: u16) -> Line<'static> {
        let t = &self.theme;
        let left = vec![
            Span::styled("▌ ", t.accent_style()),
            Span::styled("flux", t.accent_style().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", self.model), t.muted_style()),
        ];
        let mut right = Vec::new();
        if self.tokens_in + self.tokens_out > 0 {
            right.push(Span::styled(
                format!(
                    "Σ ↑{} ↓{} tok ",
                    fmt_count(self.tokens_in),
                    fmt_count(self.tokens_out)
                ),
                t.muted_style(),
            ));
        }
        bar_line(left, right, width)
    }

    /// The bottom footer bar: an animated spinner + phase + elapsed while running, else keybinding
    /// hints — with the last turn's step count + duration on the right.
    fn footer_line(&self, width: u16) -> Line<'static> {
        let t = &self.theme;
        let left = match self.phase {
            Phase::Idle => vec![Span::styled(
                " ↵ send · ^J newline · ↑↓ history · ^E expand · /cmds · ^C/Esc quit",
                t.muted_style(),
            )],
            Phase::Thinking | Phase::Planning => {
                let elapsed = self.turn_start.map(|s| s.elapsed()).unwrap_or_default();
                let frame = SPINNER[(elapsed.as_millis() / 80) as usize % SPINNER.len()];
                let label = if self.phase == Phase::Planning {
                    "composing plan…"
                } else {
                    "thinking…"
                };
                vec![
                    Span::styled(format!(" {frame} "), t.accent_style()),
                    Span::raw(label.to_string()),
                    Span::styled(format!("  · {}", fmt_elapsed(elapsed)), t.muted_style()),
                ]
            }
        };
        let mut right = Vec::new();
        if let Some(e) = self.last_elapsed {
            let plural = if self.steps == 1 { "" } else { "s" };
            right.push(Span::styled(
                format!("{} step{plural} · {} ", self.steps, fmt_elapsed(e)),
                t.muted_style(),
            ));
        }
        bar_line(left, right, width)
    }

    fn running(&self) -> bool {
        self.turn_start.is_some()
    }
}

/// Compose a one-row bar: `left` spans, padding, then `right` spans flush to `width`.
fn bar_line(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let span_w =
        |spans: &[Span]| -> usize { spans.iter().map(|s| s.content.chars().count()).sum() };
    let pad = (width as usize)
        .saturating_sub(span_w(&left) + span_w(&right))
        .max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    Line::from(spans)
}

/// Format a token count compactly: `840`, `1.2k`, `3.4M`.
fn fmt_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Truncate `s` to `max` display columns (approximated by char count), appending `…` when cut.
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Format an elapsed duration compactly: `820µs` / `12ms` / `1.4s` (mirrors `flux-cli`'s helper).
fn fmt_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    if ms == 0 {
        format!("{}µs", d.as_micros())
    } else if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Max persisted history entries.
const HISTORY_CAP: usize = 500;

/// Path to the persisted input history (`~/.flux/history`), if `$HOME` is known.
fn history_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join(".flux").join("history"))
}

/// Load persisted input history (oldest first), newest [`HISTORY_CAP`] kept. Newlines were escaped
/// on save (one entry per line), so unescape them here.
fn load_history() -> Vec<String> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.replace("\\n", "\n"))
        .collect();
    if lines.len() > HISTORY_CAP {
        lines.drain(0..lines.len() - HISTORY_CAP);
    }
    lines
}

/// Persist input history (best-effort, capped). Newlines in a prompt are escaped so each entry stays
/// on one line.
fn save_history(history: &[String]) {
    let Some(path) = history_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let start = history.len().saturating_sub(HISTORY_CAP);
    let body = history[start..]
        .iter()
        .map(|h| h.replace('\n', "\\n"))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(path, body);
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

/// Render the chat: scrollable transcript, a status/spinner row, the input box, optional modal.
pub fn render(frame: &mut Frame, state: &ChatState) {
    let input_h = state.input_rows() + 2; // + borders
    let slash = state
        .slash_query()
        .map(|q| slash_matches(&q))
        .unwrap_or_default();
    let menu_h = (slash.len().min(6)) as u16;
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(menu_h),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let (header_area, transcript_area, menu_area, input_area, footer_area) =
        (chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]);

    // --- header bar ---
    frame.render_widget(
        Paragraph::new(state.header_line(header_area.width)),
        header_area,
    );

    // --- transcript (scrollable) ---
    let inner_w = transcript_area.width.saturating_sub(2);
    let inner_h = transcript_area.height.saturating_sub(2);
    let lines = state.transcript_lines(inner_w);
    let transcript = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(state.theme.muted_style()),
    );
    let total = transcript.line_count(inner_w) as u16;
    let max_scroll = total.saturating_sub(inner_h);
    state.last_max_scroll.set(max_scroll);
    state.last_page.set(inner_h.max(1));
    let offset = if state.follow {
        max_scroll
    } else {
        state.scroll.min(max_scroll)
    };
    frame.render_widget(transcript.scroll((offset, 0)), transcript_area);

    // --- slash-command menu (between transcript and input) ---
    if !slash.is_empty() {
        let theme = &state.theme;
        let sel = state.slash_sel.min(slash.len() - 1);
        let rows: Vec<Line> = slash
            .iter()
            .take(6)
            .enumerate()
            .map(|(i, c)| {
                let style = if i == sel {
                    Style::default().bg(theme.sel_bg).fg(theme.accent)
                } else {
                    theme.muted_style()
                };
                Line::from(vec![
                    Span::styled(if i == sel { " ▸ " } else { "   " }, style),
                    Span::styled(format!("/{}", c.name), style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("   {}", c.desc), style),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(rows), menu_area);
    }

    // --- input (multiline; tui-textarea owns its cursor + scrolling) ---
    let mut input = state.input.clone();
    input.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(state.theme.accent_style()),
    );
    frame.render_widget(&input, input_area);

    // --- footer bar ---
    frame.render_widget(
        Paragraph::new(state.footer_line(footer_area.width)),
        footer_area,
    );

    // --- approval modal ---
    if let Some(modal) = &state.modal {
        let area = centered(frame.area(), 64, 7);
        frame.render_widget(Clear, area);
        let p = Paragraph::new(modal.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("approve")
                    .border_style(state.theme.warn_style()),
            );
        frame.render_widget(p, area);
    }
}

/// A UI event produced by the running turn (on a background task) for the event loop to render.
enum UiEvent {
    Text(String),
    /// A live thinking-token delta streamed during the planning phase.
    Thinking(String),
    /// The planner is composing (`true`) / done (`false`) — drives the status line.
    Planning(bool),
    /// The compiled plan (`flow.plan` observation `data`).
    Plan(serde_json::Value),
    ToolCall {
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        name: String,
        content: String,
        is_error: bool,
    },
    /// End-of-turn token usage (for the footer metrics).
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

/// Forwards a turn's streamed output to the event loop over an mpsc channel.
struct ChannelSink {
    tx: mpsc::UnboundedSender<UiEvent>,
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, t: &str) {
        let _ = self.tx.send(UiEvent::Text(t.to_string()));
    }
    fn thinking_delta(&mut self, t: &str) {
        let _ = self.tx.send(UiEvent::Thinking(t.to_string()));
    }
    fn planning(&mut self, active: bool) {
        let _ = self.tx.send(UiEvent::Planning(active));
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let _ = self.tx.send(UiEvent::ToolCall {
            name: name.to_string(),
            input: input.clone(),
        });
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let _ = self.tx.send(UiEvent::ToolResult {
            name: name.to_string(),
            content: result.content.clone(),
            is_error: result.is_error,
        });
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        if let Some(u) = usage {
            let _ = self.tx.send(UiEvent::Usage(u));
        }
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        if o.kind == "flow.plan" {
            let _ = self.tx.send(UiEvent::Plan(o.data.clone()));
        } else if o.kind == flux_evidence::KIND_DESTRUCTIVE {
            let _ = self.tx.send(UiEvent::Notice {
                text: "⚠ destructive operation flagged".into(),
                sev: Sev::Warn,
            });
        } else if o.kind == "skill.activated" {
            if let Some(name) = o.data.get("skill").and_then(|v| v.as_str()) {
                let _ = self.tx.send(UiEvent::Notice {
                    text: format!("✦ skill activated: {name}"),
                    sev: Sev::Info,
                });
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

type Tui = Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Run the interactive TUI against `agent`/`session_id`. Requires a real terminal. Installs a modal
/// approver unless `auto_approve` is set (i.e. `--yes` was passed), then always restores the
/// terminal (raw mode + alternate screen + mouse capture) even on error.
pub async fn run(
    mut agent: FlowEngine,
    session_id: String,
    auto_approve: bool,
) -> anyhow::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    let (tx, rx) = mpsc::unbounded_channel::<UiEvent>();
    // Only replace the approver with the modal when NOT auto-approving; if --yes was passed,
    // build_agent already installed AllowApprover and we must not clobber it.
    if !auto_approve {
        agent
            .executor
            .set_approver(Arc::new(ChannelApprover { tx: tx.clone() }));
    }
    let model = agent.model.clone();
    let agent = Arc::new(agent);

    enable_raw_mode()?;
    let mut out = std::io::stdout();
    crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(out))?;

    let mut state = ChatState::new(model);
    state.history = load_history();
    let result = event_loop(&mut terminal, agent, &session_id, &mut state, tx, rx).await;

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
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
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

    let mut cancel = CancellationToken::new();
    let mut pending_reply: Option<(String, oneshot::Sender<ApprovalChoice>)> = None;
    // A message typed while a turn was running, started as soon as the turn finishes.
    let mut pending_input: Option<String> = None;

    // Read terminal input on a dedicated OS thread so the main loop can stay async: blocking
    // `event::read()` here (not on a runtime worker) lets the loop `.await` below, which is what
    // actually drives the spawned turn — a synchronous `event::poll` loop would starve it.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    loop {
        // Drain everything the running turn has produced.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                UiEvent::Text(t) => state.stream_text(&t),
                UiEvent::Thinking(t) => state.stream_thinking(&t),
                UiEvent::Planning(active) => {
                    if active {
                        // Starting a new planning call: open a fresh thinking entry.
                        state.begin_thinking();
                        state.phase = Phase::Planning;
                    } else {
                        // Planning done: seal the thinking entry and move to Thinking phase
                        // (the engine will emit text_delta or another Planning shortly).
                        state.end_thinking();
                        state.phase = Phase::Thinking;
                    }
                }
                UiEvent::Plan(data) => state.push(Entry::Plan(data)),
                UiEvent::ToolCall { name, input } => {
                    state.steps += 1;
                    state.push(Entry::Tool(ToolEntry::new(name, input)));
                }
                UiEvent::ToolResult {
                    name,
                    content,
                    is_error,
                } => state.finish_tool(&name, content, is_error),
                UiEvent::Usage(u) => {
                    state.tokens_in += u.input_tokens;
                    state.tokens_out += u.output_tokens;
                }
                UiEvent::Notice { text, sev } => state.push(Entry::Notice { text, sev }),
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
                    state.end_stream();
                    state.phase = Phase::Idle;
                    state.last_elapsed = state.turn_start.map(|s| s.elapsed());
                    state.turn_start = None;
                    // A message composed while this turn ran starts now.
                    if let Some(queued) = pending_input.take() {
                        cancel = start_turn(&agent, session_id, &tx, state, queued);
                    }
                }
            }
        }

        terminal.draw(|f| render(f, state))?;

        // Await the next input event or a ~30 fps tick. The `.await` here yields to the runtime so
        // the spawned turn task is actually polled (the engine's model call + streaming run on it);
        // the tick keeps the spinner animating and flushes streamed tokens while a turn is running.
        let ev = tokio::select! {
            maybe = input_rx.recv() => match maybe {
                Some(ev) => ev,
                None => break, // input reader gone
            },
            _ = tokio::time::sleep(Duration::from_millis(33)) => continue,
        };
        match ev {
            Event::Resize(_, _) => continue,
            Event::Mouse(m) => {
                match m.kind {
                    MouseEventKind::ScrollUp => scroll_up(state, 3),
                    MouseEventKind::ScrollDown => scroll_down(state, 3),
                    _ => {}
                }
                continue;
            }
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Modal mode: the next key answers the pending approval.
                if state.modal.is_some() {
                    if let Some((tool, reply)) = pending_reply.take() {
                        let choice = match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => ApprovalChoice::Allow,
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                ApprovalChoice::AllowAlways(tool)
                            }
                            _ => ApprovalChoice::Deny,
                        };
                        let _ = reply.send(choice);
                    }
                    state.modal = None;
                    continue;
                }

                // Paging the transcript works whether or not a turn is running. Home/End are left
                // for the input editor (line start/end); PgDn reattaches follow when it reaches the
                // bottom, so a dedicated jump-to-bottom isn't needed.
                match key.code {
                    KeyCode::PageUp => {
                        scroll_up(state, state.last_page.get());
                        continue;
                    }
                    KeyCode::PageDown => {
                        scroll_down(state, state.last_page.get());
                        continue;
                    }
                    _ => {}
                }

                // Slash-command menu: when the input is a bare `/cmd` prefix with matches, ↑/↓ select,
                // Tab/Enter run the command, Esc dismisses; other keys fall through to edit/filter.
                if let Some(query) = state.slash_query() {
                    let matches = slash_matches(&query);
                    if !matches.is_empty() {
                        match key.code {
                            KeyCode::Up => {
                                state.slash_up(matches.len());
                                continue;
                            }
                            KeyCode::Down => {
                                state.slash_down(matches.len());
                                continue;
                            }
                            KeyCode::Esc => {
                                state.input = fresh_textarea();
                                continue;
                            }
                            KeyCode::Tab | KeyCode::Enter => {
                                let name = matches[state.slash_sel.min(matches.len() - 1)].name;
                                state.input = fresh_textarea();
                                state.slash_sel = 0;
                                match name {
                                    "quit" => break,
                                    "clear" | "new" => {
                                        state.entries.clear();
                                        state.follow = true;
                                        state.scroll = 0;
                                    }
                                    "help" => state.push(Entry::Notice {
                                        text: HELP_TEXT.into(),
                                        sev: Sev::Info,
                                    }),
                                    "model" => {
                                        let m = state.model.clone();
                                        state.push(Entry::Notice {
                                            text: format!("model: {m}"),
                                            sev: Sev::Info,
                                        });
                                    }
                                    _ => {}
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                }

                let running = state.running();
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                // Insert a newline (rather than submit) on Ctrl-J, Alt-↵ or Shift-↵.
                let want_newline = (matches!(key.code, KeyCode::Enter)
                    && key
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT))
                    || (matches!(key.code, KeyCode::Char('j')) && ctrl);
                // Up/Down recall history only at the top/bottom row of the input (so they still move
                // the cursor inside a multiline message).
                let (cur_row, _) = state.input.cursor();
                let last_row = state.input.lines().len().saturating_sub(1);

                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Up if cur_row == 0 && !ctrl => state.history_prev(),
                    KeyCode::Down if cur_row == last_row && !ctrl => state.history_next(),
                    KeyCode::Char('c') if ctrl => {
                        if running {
                            // Cancel the running turn (input stays live so you can keep typing).
                            cancel.cancel();
                            state.push(Entry::Notice {
                                text: "(interrupting…)".into(),
                                sev: Sev::Info,
                            });
                        } else if state.input_blank() {
                            break; // empty line → quit
                        } else {
                            state.input = fresh_textarea(); // non-empty line → clear it
                        }
                    }
                    KeyCode::Char('e') if ctrl => state.expand_tools = !state.expand_tools,
                    _ if want_newline => state.input.insert_newline(),
                    KeyCode::Enter => {
                        if state.input_blank() {
                            let _ = state.take_input();
                            continue;
                        }
                        let text = state.take_input();
                        if running {
                            pending_input = Some(text);
                            state.push(Entry::Notice {
                                text: "↩ queued — sends when the current turn finishes".into(),
                                sev: Sev::Info,
                            });
                        } else {
                            cancel = start_turn(&agent, session_id, &tx, state, text);
                        }
                    }
                    // Everything else (text, backspace, arrows, word-nav, home/end) edits the input —
                    // live even while a turn runs, so you can compose the next message.
                    _ => {
                        state.input.input(key);
                    }
                }
            }
            _ => continue,
        }
    }
    Ok(())
}

/// Push `input` as a user message and spawn the agent turn that streams back into the transcript.
/// Returns the turn's cancellation token (Ctrl-C cancels it).
fn start_turn(
    agent: &Arc<FlowEngine>,
    session_id: &str,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    input: String,
) -> CancellationToken {
    state.record_history(&input);
    state.push_user(input.clone());
    state.phase = Phase::Thinking;
    state.turn_start = Some(Instant::now());
    state.steps = 0;
    state.follow = true;

    let cancel = CancellationToken::new();
    let task_agent = agent.clone();
    let task_sid = session_id.to_string();
    let task_tx = tx.clone();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        // Run the turn on an inner task so a *panic* inside the engine is caught (its `JoinError`
        // carries `is_panic`) and surfaced — otherwise a panicked turn would die silently: no
        // output, no `Finished`, and the spinner spinning forever.
        let inner_tx = task_tx.clone();
        let run = tokio::spawn(async move {
            let mut sink = ChannelSink { tx: inner_tx };
            task_agent
                .run_turn_cancellable(&task_sid, &input, &mut sink, &task_cancel)
                .await
        });
        let note = match run.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("error: {e}")),
            Err(join) if join.is_cancelled() => None,
            Err(join) => Some(format!("the turn crashed: {join}")),
        };
        if let Some(text) = note {
            let _ = task_tx.send(UiEvent::Notice {
                text,
                sev: Sev::Err,
            });
        }
        let _ = task_tx.send(UiEvent::Finished);
    });
    cancel
}

/// Scroll the transcript up by `n` wrapped lines (detaches follow mode).
fn scroll_up(state: &mut ChatState, n: u16) {
    let base = if state.follow {
        state.last_max_scroll.get()
    } else {
        state.scroll
    };
    state.follow = false;
    state.scroll = base.saturating_sub(n);
}

/// Scroll the transcript down by `n` wrapped lines (re-attaches follow at the bottom).
fn scroll_down(state: &mut ChatState, n: u16) {
    let max = state.last_max_scroll.get();
    let base = if state.follow { max } else { state.scroll };
    let next = (base + n).min(max);
    state.scroll = next;
    state.follow = next >= max;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_transcript_and_input() {
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        let mut state = ChatState::new("opus".into());
        state.push_user("hello flux");
        state.stream_text("hi there");
        state.end_stream();
        state.input.insert_str("next message");

        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);

        assert!(content.contains("hello flux"));
        assert!(content.contains("hi there"));
        assert!(content.contains("next message"));
        assert!(content.contains("flux")); // border title + idle hint
    }

    #[test]
    fn multiline_input_grows_and_take_resets() {
        let mut state = ChatState::new("opus".into());
        assert_eq!(state.input_rows(), 1);
        assert!(state.input_blank());
        state.input.insert_str("line one");
        state.input.insert_newline();
        state.input.insert_str("line two");
        assert_eq!(state.input_rows(), 2);
        assert!(!state.input_blank());
        assert_eq!(state.take_input(), "line one\nline two");
        assert!(state.input_blank()); // reset after take
        assert_eq!(state.input_rows(), 1);
    }

    #[test]
    fn streams_text_into_one_assistant_message_and_renders_modal() {
        let mut state = ChatState::new("opus".into());
        state.stream_text("Hel");
        state.stream_text("lo");
        assert_eq!(state.entries.len(), 1);
        // a discrete entry closes the stream; the next delta starts a fresh assistant message
        state.push(Entry::Tool(ToolEntry::new(
            "bash".into(),
            serde_json::json!({"command": "ls"}),
        )));
        state.stream_text("done");
        assert_eq!(state.entries.len(), 3);

        state.modal = Some("approve `bash`\n[y]es [a]lways [N]o".to_string());
        let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("approve"));
    }

    #[test]
    fn tool_card_pairs_call_with_result_and_badge() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::Tool(ToolEntry::new(
            "bash".into(),
            serde_json::json!({"command": "cargo test"}),
        )));
        state.finish_tool("bash", "182 passed; 0 failed".into(), false);
        // still one entry — the result attached to the call, not a new line
        assert_eq!(state.entries.len(), 1);

        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("→ bash"));
        assert!(content.contains("$ cargo test"));
        assert!(content.contains("✓")); // done badge
        assert!(content.contains("exit 0 · 1 line")); // bash result collapses to a compact summary
    }

    #[test]
    fn history_recall_walks_entries_and_restores_draft() {
        let mut state = ChatState::new("opus".into());
        state.history = vec!["first".into(), "second".into()];
        state.set_input("draft");
        state.history_prev(); // stash draft, show newest
        assert_eq!(state.input.lines().join("\n"), "second");
        state.history_prev();
        assert_eq!(state.input.lines().join("\n"), "first");
        state.history_prev(); // clamp at oldest
        assert_eq!(state.input.lines().join("\n"), "first");
        state.history_next();
        assert_eq!(state.input.lines().join("\n"), "second");
        state.history_next(); // past newest → restore draft
        assert_eq!(state.input.lines().join("\n"), "draft");
        assert!(state.history_pos.is_none());
    }

    #[test]
    fn push_history_skips_empties_and_consecutive_dupes() {
        let mut state = ChatState::new("opus".into());
        assert!(state.push_history("a"));
        assert!(!state.push_history("a")); // dupe
        assert!(state.push_history("b"));
        assert!(!state.push_history("")); // empty
        assert_eq!(state.history, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn header_and_footer_show_identity_and_metrics() {
        let mut state = ChatState::new("anthropic/opus".into());
        state.tokens_in = 12_300;
        state.tokens_out = 840;
        state.steps = 3;
        state.last_elapsed = Some(Duration::from_millis(4200));
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("flux"));
        assert!(content.contains("anthropic/opus"));
        assert!(content.contains("12.3k")); // cumulative tokens in the header
        assert!(content.contains("3 steps")); // last-turn metrics in the footer
    }

    #[test]
    fn fmt_count_scales() {
        assert_eq!(fmt_count(840), "840");
        assert_eq!(fmt_count(12_300), "12.3k");
        assert_eq!(fmt_count(3_400_000), "3.4M");
    }

    #[test]
    fn slash_menu_filters_and_renders() {
        let mut state = ChatState::new("opus".into());
        assert!(state.slash_query().is_none());
        state.set_input("/cl");
        assert_eq!(state.slash_query().as_deref(), Some("cl"));
        assert!(slash_matches("cl").iter().any(|c| c.name == "clear"));
        // a space (typing an argument) closes the menu
        state.set_input("/clear x");
        assert!(state.slash_query().is_none());

        state.set_input("/");
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("/help"));
        assert!(content.contains("/quit"));
    }

    #[test]
    fn expanded_edit_card_shows_a_diff() {
        let mut state = ChatState::new("opus".into());
        state.expand_tools = true;
        state.push(Entry::Tool(ToolEntry::new(
            "edit".into(),
            serde_json::json!({"path": "a.rs", "old_string": "old line", "new_string": "new line"}),
        )));
        state.finish_tool("edit", "edited a.rs".into(), false);

        let mut terminal = Terminal::new(TestBackend::new(72, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("- old line"));
        assert!(content.contains("+ new line"));
    }

    #[test]
    fn spinner_shows_while_running() {
        let mut state = ChatState::new("opus".into());
        state.phase = Phase::Thinking;
        state.turn_start = Some(Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("thinking…"));
    }

    #[test]
    fn scroll_up_detaches_follow_and_down_reattaches() {
        let mut state = ChatState::new("opus".into());
        state.last_max_scroll.set(10);
        assert!(state.follow);
        scroll_up(&mut state, 3);
        assert!(!state.follow);
        assert_eq!(state.scroll, 7);
        scroll_down(&mut state, 3);
        assert!(state.follow); // back at bottom
        assert_eq!(state.scroll, 10);
    }

    #[test]
    fn plan_entry_renders_tree() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::Plan(serde_json::json!({
            "plan": "flow\n└─ $x = read(\"README.md\")   !read",
            "risk": "low",
            "ops": 1,
        })));
        let mut terminal = Terminal::new(TestBackend::new(70, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("plan"));
        assert!(content.contains("read"));
    }
}
