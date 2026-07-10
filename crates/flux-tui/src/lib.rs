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

use flux_core::Usage;
use flux_flow::engine::FlowEngine;
use flux_flow::AgentSink;
use flux_runtime::{ApprovalChoice, Approver, ToolResult};
use flux_spec::IntentSet;

use crate::theme::Theme;

/// Braille spinner frames (shared idiom with the CLI).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Streaming cursor block appended to an in-progress assistant message.
const CURSOR: &str = "▍";
/// Max expanded-detail lines per tool card. Lifted entirely under verbose (`flux tui -v` /
/// `FLUX_VERBOSE`), whose promise is tool output in full, no truncation.
const MAX_DETAIL: usize = 30;

/// The footer's planning-spinner label (A-15, mirrors the CLI's `phase_spinner_label`):
/// phase-derived so it reads "orienting…"/"gathering…" for the collect passes and "planning…" for
/// the execute pass's first round. "revising…" only once the execute phase has already produced a
/// round THIS turn — a plain counter over the `loop.phase` observations already reaching the
/// sink, not a new flux-flow signal. A phase-less turn (no `loop.phase` observed) falls back to
/// today's "composing plan…".
fn loop_phase_label(phase: Option<&str>, execute_rounds: usize) -> &'static str {
    match phase {
        Some("orient") => "orienting…",
        Some("gather") => "gathering…",
        Some("execute") => {
            if execute_rounds > 1 {
                "revising…"
            } else {
                "planning…"
            }
        }
        _ => "composing plan…",
    }
}

/// Format a `flow.halt` observation's `data` (A-17, mirrors the CLI's `halt_line`) as a plain
/// line: `✗ step N/M <op> failed — revising…`, or `✗ step N/M failed — revising…` when the op
/// isn't directly derivable from the failing statement (a composite/control-flow node).
fn halt_line(data: &serde_json::Value) -> String {
    let step = data.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let of = data.get("of").and_then(|v| v.as_u64()).unwrap_or(0);
    match data.get("op").and_then(|v| v.as_str()) {
        Some(op) => format!("✗ step {step}/{of} {op} failed — revising…"),
        None => format!("✗ step {step}/{of} failed — revising…"),
    }
}

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
    /// The planner's compiled DAG (the `flow.plan` observation payload) — a full execution plan.
    Plan(serde_json::Value),
    /// The orient/gather grounding artifact (design Part 1's `brief: {goal, needs[]}`, A-15):
    /// rendered the moment it's accepted, immediately and compactly.
    Brief { goal: String, needs: Vec<String> },
    /// A bounded, read-only gather round's compiled plan (the `flow.plan` observation payload,
    /// A-15) — rendered as a compact one-liner rather than the full tree + risk badge `Plan` gets.
    GatherPlan(serde_json::Value),
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
    /// Verbose tool output (`flux tui -v` → `FLUX_VERBOSE`): expanded tool cards show their FULL
    /// detail instead of capping at [`MAX_DETAIL`] lines, and cards start expanded so long output
    /// is visible without pressing Ctrl-E (which still toggles). Set via
    /// [`with_verbose`](Self::with_verbose).
    verbose: bool,
    /// Selected row in the slash-command menu.
    slash_sel: usize,
    // --- session metrics (header/footer) ---
    /// Cumulative input/output tokens this session.
    tokens_in: u64,
    tokens_out: u64,
    /// Cumulative cache tokens this session (read + write) — C-06: the header used to ignore cache
    /// entirely, so a heavily-cached session looked no different from an uncached one.
    tokens_cache_read: u64,
    tokens_cache_write: u64,
    /// Cumulative reasoning tokens this session (a subset of `tokens_out`, tracked separately only
    /// for display — mirrors `Usage::reasoning_tokens`'s own accounting).
    tokens_reasoning: u64,
    /// Cumulative dollar cost this session, when a model spec + pricing table are attached
    /// ([`with_cost`](Self::with_cost)); `None` when cost can't be computed (e.g. `-m mock`, or an
    /// unpriced model), so the header shows tokens only rather than a misleading `$0.00`.
    cost_usd: Option<f64>,
    /// The resolved `provider/model` spec + pricing table for cost computation, attached by
    /// [`with_cost`](Self::with_cost). `None` when the TUI wasn't given one (cost stays hidden).
    cost_model: Option<(String, flux_core::PricingTable)>,
    /// C-33: set once any turn hits a pricing-table miss on a **metered cloud** spec (see
    /// [`flux_core::is_metered_cloud_spec`]) — i.e. real dollars were spent but not counted into
    /// `cost_usd`. Once set, the header's cost segment switches to the `$?` (unpriced) state
    /// instead of silently under-reporting the running total; mirrors flux-cli's
    /// `cost_suffix`/`unpriced_marker_applies` rule (local `ollama*`/mock specs never set this,
    /// since nothing is billed there).
    cost_unpriced: bool,
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
    /// The phase of the most recent `loop.phase` observation this turn (design Part 1 / A-15):
    /// `orient`/`gather`/`execute`, or `None` for a phase-less turn. Drives the footer's spinner
    /// label — see `loop_phase_label`.
    plan_phase: Option<String>,
    /// How many `execute`-phase `loop.phase` observations have landed this turn (mirrors the
    /// CLI's `CliSink::execute_rounds`): the first is the turn's actual planning, every one after
    /// it means the prior round didn't finish, so the footer reads "revising…" past 1.
    execute_rounds: usize,
    /// Whether the NEXT `Plan` entry is a bounded, read-only gather round rather than the full
    /// execution plan (mirrors the CLI's `CliSink::gather_mode` — same derivation, same caveats).
    gather_mode: bool,
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
            verbose: false,
            slash_sel: 0,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            tokens_reasoning: 0,
            cost_usd: None,
            cost_model: None,
            cost_unpriced: false,
            steps: 0,
            last_elapsed: None,
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            scroll: 0,
            follow: true,
            last_max_scroll: Cell::new(0),
            last_page: Cell::new(1),
            plan_phase: None,
            execute_rounds: 0,
            gather_mode: false,
        }
    }

    /// Track a `loop.phase` observation (design Part 1 / A-15, mirrors the CLI's
    /// `CliSink::record_phase`): updates the footer's spinner-label state and whether the next
    /// `Plan` entry renders compact (gather) or full (execute). `gather`/`execute` are
    /// unambiguous; `orient` resets to "not gathering yet" — a `Brief` right after (only ever
    /// paired with a `gather: true` plan) flips it back on when orient itself emitted the first
    /// gather round.
    fn record_loop_phase(&mut self, phase: &str) {
        match phase {
            "execute" => {
                self.execute_rounds += 1;
                self.gather_mode = false;
            }
            "gather" => self.gather_mode = true,
            "orient" => self.gather_mode = false,
            _ => {}
        }
        self.plan_phase = Some(phase.to_string());
    }

    /// Attach a resolved `provider/model` spec + pricing table so the header can show a running
    /// dollar cost alongside tokens (C-06) — mirrors the CLI's `CliSink::with_cost`.
    pub fn with_cost(mut self, model_spec: String, pricing: flux_core::PricingTable) -> Self {
        self.cost_model = Some((model_spec, pricing));
        self
    }

    /// Enable verbose tool output (`flux tui -v`, which flux-cli exports as `FLUX_VERBOSE` before
    /// launching the TUI): tool cards start expanded and their detail renders in full instead of
    /// capping at [`MAX_DETAIL`] lines. Ctrl-E still collapses/re-expands the cards.
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self.expand_tools = self.expand_tools || verbose;
        self
    }

    /// Fold one turn's [`Usage`] into the session's cumulative header metrics — EVERY tier (C-06:
    /// the header used to sum only input/output, silently dropping cache reads/writes and
    /// reasoning), and the running dollar cost when a model spec + pricing table are attached.
    ///
    /// C-33: a pricing-table miss on a **metered cloud** spec (no row, and no provider-reported
    /// cost either — see [`flux_core::PricingTable::cost`]'s C-34 short-circuit) sets
    /// `cost_unpriced` instead of silently skipping the turn; otherwise the cumulative header
    /// total would under-report once any turn went unpriced. A local/mock spec (`ollama*`, `mock`)
    /// never sets it — nothing is billed there, so silence stays correct.
    fn record_usage(&mut self, u: &Usage) {
        self.tokens_in += u.input_tokens;
        self.tokens_out += u.output_tokens;
        self.tokens_cache_read += u.cache_read_input_tokens;
        self.tokens_cache_write += u.cache_creation_input_tokens;
        self.tokens_reasoning += u.reasoning_tokens;
        if let Some((spec, pricing)) = &self.cost_model {
            match pricing.cost(u, spec) {
                Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                None if flux_core::is_metered_cloud_spec(spec) => self.cost_unpriced = true,
                None => {}
            }
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
                Entry::Brief { goal, needs } => {
                    out.push(Line::from(vec![
                        Span::styled("◆ ", t.accent_style()),
                        Span::styled("goal: ", t.accent_style().add_modifier(Modifier::BOLD)),
                        Span::raw(goal.clone()),
                    ]));
                    if !needs.is_empty() {
                        out.push(Line::styled(
                            format!("  needs: {}", needs.join(", ")),
                            t.muted_style(),
                        ));
                    }
                }
                Entry::GatherPlan(data) => out.extend(plan::render_compact(data, t)),
            }
        }
        out
    }

    /// Render one tool card: a `→ verb arg … [badge]` header, a one-line summary, and — when
    /// `expand_tools` is set — the full detail (a unified diff for `edit`/`write`, else the output,
    /// capped at [`MAX_DETAIL`] lines unless `verbose`).
    fn tool_lines(&self, tool: &ToolEntry, width: u16) -> Vec<Line<'static>> {
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

            // Full detail, when expanded. Verbose (`-v`/`FLUX_VERBOSE`) lifts the line cap —
            // "tool output in full (no truncation)" is the flag's promise.
            if self.expand_tools {
                let detail =
                    toolview::format_detail(&tool.name, &tool.input, &o.content, o.is_error);
                let cap = if self.verbose {
                    detail.len()
                } else {
                    MAX_DETAIL
                };
                let shown = detail.len().min(cap);
                for (kind, text) in detail.iter().take(cap) {
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
        // C-06: the header used to sum only input/output, silently ignoring cache read/write
        // tokens — a heavily-cached session looked identical to an uncached one. `cache` here is
        // BOTH tiers combined (read + write); a session with either shows the segment.
        let cache = self.tokens_cache_read + self.tokens_cache_write;
        if self.tokens_in + self.tokens_out + cache > 0 {
            let mut s = format!(
                "Σ ↑{} ↓{} tok",
                fmt_count(self.tokens_in),
                fmt_count(self.tokens_out)
            );
            if cache > 0 {
                s.push_str(&format!(" · cache {}", fmt_count(cache)));
            }
            // C-33: an unpriced metered-cloud turn switches the cost segment to the `$?` state
            // (`$X.XXXX+?` when part of the run WAS priced, bare `$?` when none of it was) rather
            // than rendering a total that silently omits real spend — mirrors flux-cli's
            // ` · $? (unpriced)` marker.
            match (self.cost_usd, self.cost_unpriced) {
                (Some(usd), true) => s.push_str(&format!(" · ${usd:.4}+? (unpriced)")),
                (Some(usd), false) => s.push_str(&format!(" · ${usd:.4}")),
                (None, true) => s.push_str(" · $? (unpriced)"),
                (None, false) => {}
            }
            s.push(' ');
            right.push(Span::styled(s, t.muted_style()));
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
                    loop_phase_label(self.plan_phase.as_deref(), self.execute_rounds)
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

/// Whether a `FLUX_*` boolean env value is ON: `1`/`true`/`yes`/`on`, case-insensitive. Mere
/// presence with any other value (e.g. `FLUX_VERBOSE=0`) is OFF — env flags are value-parsed,
/// not presence-tested.
fn flag_on(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
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
    /// The compiled plan (`flow.plan` observation `data`) — a full execution plan or a bounded
    /// gather round; the event loop tells them apart from `ChatState::gather_mode` (A-15).
    Plan(serde_json::Value),
    /// Which pass of the phased turn loop is asking (the `loop.phase` observation's `phase`, A-15).
    Phase(String),
    /// The orient/gather grounding artifact (the `flow.brief` observation, A-15).
    Brief {
        goal: String,
        needs: Vec<String>,
    },
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
        } else if o.kind == "loop.phase" {
            if let Some(phase) = o.data.get("phase").and_then(|v| v.as_str()) {
                let _ = self.tx.send(UiEvent::Phase(phase.to_string()));
            }
        } else if o.kind == "flow.brief" {
            let goal = o
                .data
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let needs = o
                .data
                .get("needs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let _ = self.tx.send(UiEvent::Brief { goal, needs });
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
        } else if o.kind == "flow.halt" {
            // A-17: reuse the plain `Notice`/`Sev::Err` machinery already used for other real-time
            // cues (destructive-op flags, skill activation) rather than a dedicated `Entry`/`UiEvent`
            // variant — the halt line is exactly that shape: a one-off red status line.
            let _ = self.tx.send(UiEvent::Notice {
                text: halt_line(&o.data),
                sev: Sev::Err,
            });
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
/// terminal (raw mode + alternate screen + mouse capture) even on error. `model_spec` is the
/// resolved `provider/model` (e.g. `codex/gpt-5.5`, mirroring the CLI's `CliSink::with_cost`); when
/// given, the header shows a running dollar cost alongside tokens (C-06). Pricing is the builtin
/// table overlaid by `~/.flux/pricing.toml` (same loader the CLI uses). Reads `FLUX_VERBOSE`
/// (exported by `flux tui -v`, value-parsed — see [`flag_on`]) once at startup: verbose starts
/// tool cards expanded and shows their output in full instead of capped at [`MAX_DETAIL`] lines.
pub async fn run(
    agent: FlowEngine,
    session_id: String,
    auto_approve: bool,
    model_spec: Option<String>,
) -> anyhow::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use std::io::IsTerminal;

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

    let mut out = std::io::stdout();
    if !std::io::stdin().is_terminal() || !out.is_terminal() {
        anyhow::bail!("flux tui requires a real terminal on stdin and stdout");
    }

    enable_raw_mode()?;
    crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(out))?;

    let verbose = std::env::var("FLUX_VERBOSE").is_ok_and(|v| flag_on(&v));
    let mut state = ChatState::new(model).with_verbose(verbose);
    if let Some(spec) = model_spec {
        state = state.with_cost(spec, flux_credentials::load_pricing_table());
    }
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
                UiEvent::Plan(data) => {
                    // A-17 (closes the A-15 residual): prefer `flow.plan`'s own `gather` field,
                    // computed host-side from the plan's own `settled` signal, over the tracked
                    // `gather_mode` inference — which couldn't tell an orient-phase gather plan
                    // apart from orient emitting the full plan directly when the model's `brief`
                    // was unusable. Falls back to the tracked state for a phase-less/stale caller.
                    let gather = data
                        .get("gather")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(state.gather_mode);
                    if gather {
                        state.push(Entry::GatherPlan(data));
                    } else {
                        state.push(Entry::Plan(data));
                    }
                }
                UiEvent::Phase(phase) => state.record_loop_phase(&phase),
                UiEvent::Brief { goal, needs } => {
                    // A brief only ever accompanies a `gather: true` plan (mirrors the CLI):
                    // its arrival marks gather mode even when the phase alone (`orient`) is
                    // ambiguous between a gather round and a full plan emitted directly.
                    state.gather_mode = true;
                    state.push(Entry::Brief { goal, needs });
                }
                UiEvent::ToolCall { name, input } => {
                    state.steps += 1;
                    state.push(Entry::Tool(ToolEntry::new(name, input)));
                }
                UiEvent::ToolResult {
                    name,
                    content,
                    is_error,
                } => state.finish_tool(&name, content, is_error),
                UiEvent::Usage(u) => state.record_usage(&u),
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

    /// C-06 cache-aware surfacing: the TUI header must show cache tokens (previously ignored
    /// entirely — `UiEvent::Usage` only summed input/output) and a running dollar cost when a model
    /// spec + pricing table are attached via `with_cost`. The story's named failing-first test.
    #[test]
    fn usage_annotation_includes_cache_and_cost() {
        let mut state = ChatState::new("claude-sonnet-4-6".into()).with_cost(
            "anthropic/claude-sonnet-4-6".into(),
            flux_core::PricingTable::builtin(),
        );
        state.record_usage(&Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_creation_input_tokens: 200_000,
            cache_read_input_tokens: 500_000,
            reasoning_tokens: 0,
            ..Default::default()
        });

        // Tokens are accumulated across EVERY tier, not just input/output.
        assert_eq!(state.tokens_in, 1_000_000);
        assert_eq!(state.tokens_out, 100_000);
        assert_eq!(state.tokens_cache_read, 500_000);
        assert_eq!(state.tokens_cache_write, 200_000);
        // cost = 1·3 + 0.1·15 + 0.2·3.75 + 0.5·0.30 = 3 + 1.5 + 0.75 + 0.15 = 5.4
        assert!(
            (state.cost_usd.unwrap() - 5.4).abs() < 1e-9,
            "got {:?}",
            state.cost_usd
        );

        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(
            content.contains("cache"),
            "the header must show cache tokens, not just input/output: {content}"
        );
        assert!(
            content.contains("$5.4"),
            "the header must show the running dollar cost: {content}"
        );

        // Without `with_cost`, no cost segment appears (no model spec/pricing to compute from) —
        // tokens (incl. cache) still show.
        let mut plain = ChatState::new("mock".into());
        plain.record_usage(&Usage {
            input_tokens: 100,
            cache_read_input_tokens: 50,
            ..Default::default()
        });
        assert!(plain.cost_usd.is_none());
        let mut terminal2 = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal2.draw(|f| render(f, &plain)).unwrap();
        let content2 = screen(&terminal2);
        assert!(content2.contains("cache"));
        assert!(!content2.contains('$'));
    }

    /// C-34: an OpenRouter (or any reporting-provider) call with NO pricing-table row still
    /// accumulates a running dollar cost, because `record_usage` prices through
    /// `PricingTable::cost`, which now short-circuits on `Usage.reported_cost_usd` — the TUI header
    /// needs zero code changes to inherit the fix, the same as every other cost sink.
    #[test]
    fn record_usage_accumulates_reported_cost_for_untabled_model() {
        let mut state = ChatState::new("openrouter/deepseek/deepseek-v4-flash:nitro".into())
            .with_cost(
                "openrouter/deepseek/deepseek-v4-flash:nitro".into(),
                flux_core::PricingTable::builtin(),
            );
        // The builtin table has no row for this model — without reported cost this would leave
        // `cost_usd` at `None` forever (the `$?` case).
        state.record_usage(&Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            reported_cost_usd: Some(0.0023),
            ..Default::default()
        });
        assert!(
            (state.cost_usd.unwrap() - 0.0023).abs() < 1e-9,
            "got {:?}",
            state.cost_usd
        );

        // A second reporting call accumulates (sums), like the token-only path already does.
        state.record_usage(&Usage {
            input_tokens: 200,
            output_tokens: 100,
            reported_cost_usd: Some(0.0005),
            ..Default::default()
        });
        assert!(
            (state.cost_usd.unwrap() - 0.0028).abs() < 1e-9,
            "got {:?}",
            state.cost_usd
        );
    }

    /// C-33: a pricing-table miss on a **metered cloud** spec (no row, no `reported_cost_usd`)
    /// must flip the header's cost segment to the `$?` (unpriced) state instead of silently
    /// leaving `cost_usd` untouched — the cumulative total would otherwise under-report once any
    /// turn went unpriced. This is the story's named failing-first test.
    #[test]
    fn unpriced_metered_cloud_turn_switches_header_to_question_mark() {
        let mut state = ChatState::new("anthropic/claude-nonexistent-model".into()).with_cost(
            "anthropic/claude-nonexistent-model".into(),
            flux_core::PricingTable::builtin(),
        );
        // The builtin table has no row for this model, and there's no provider-reported cost
        // either — a genuine table miss on a metered cloud provider.
        state.record_usage(&Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        });
        assert!(
            state.cost_unpriced,
            "table miss on a cloud spec must set cost_unpriced"
        );
        assert!(state.cost_usd.is_none());

        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(
            content.contains("$?"),
            "the header must show the unpriced marker, not silently omit cost: {content}"
        );

        // A mock/ollama spec with the same kind of table miss must NOT flip the marker — nothing
        // is billed there, so silence stays correct.
        let mut ollama = ChatState::new("ollama/llama3".into())
            .with_cost("ollama/llama3".into(), flux_core::PricingTable::builtin());
        ollama.record_usage(&Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        });
        assert!(
            !ollama.cost_unpriced,
            "a local/ollama spec must never set cost_unpriced"
        );
        let mut terminal2 = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal2.draw(|f| render(f, &ollama)).unwrap();
        let content2 = screen(&terminal2);
        assert!(
            !content2.contains('$'),
            "a local/ollama spec must show no cost segment at all: {content2}"
        );

        let mut mock = ChatState::new("mock".into())
            .with_cost("mock".into(), flux_core::PricingTable::builtin());
        mock.record_usage(&Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        });
        assert!(
            !mock.cost_unpriced,
            "a mock spec must never set cost_unpriced"
        );
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

    /// `flux tui -v` promises "tool output in full (no truncation)": verbose lifts the expanded
    /// cards' [`MAX_DETAIL`] line cap and starts cards expanded, so a long tool output is fully
    /// visible instead of eliding past 30 lines behind an "… N more lines" note.
    #[test]
    fn verbose_shows_long_tool_output_in_full() {
        let output: String = (1..=40).map(|i| format!("out line {i}\n")).collect();
        let transcript = |state: &ChatState| -> String {
            state
                .transcript_lines(80)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Default (no -v): the expanded detail is capped at MAX_DETAIL lines with an elision note.
        let mut capped = ChatState::new("opus".into());
        assert!(!capped.expand_tools, "cards start collapsed without -v");
        capped.expand_tools = true;
        capped.push(Entry::Tool(ToolEntry::new(
            "bash".into(),
            serde_json::json!({"command": "seq 40"}),
        )));
        capped.finish_tool("bash", output.clone(), false);
        let content = transcript(&capped);
        assert!(content.contains("out line 30"));
        assert!(
            !content.contains("out line 31"),
            "without -v the detail keeps the {MAX_DETAIL}-line cap: {content}"
        );
        assert!(content.contains("… 10 more lines"));

        // Verbose: cards start expanded and the cap is lifted — the full output is shown.
        let mut verbose = ChatState::new("opus".into()).with_verbose(true);
        assert!(
            verbose.expand_tools,
            "verbose starts tool cards expanded so the output is visible without Ctrl-E"
        );
        verbose.push(Entry::Tool(ToolEntry::new(
            "bash".into(),
            serde_json::json!({"command": "seq 40"}),
        )));
        verbose.finish_tool("bash", output, false);
        let content = transcript(&verbose);
        assert!(content.contains("out line 31"));
        assert!(
            content.contains("out line 40"),
            "verbose must show the tool output in full: {content}"
        );
        assert!(!content.contains("more lines"));
    }

    /// `FLUX_VERBOSE` is value-parsed, not presence-tested: only `1|true|yes|on`
    /// (case-insensitive) turn verbose on — `FLUX_VERBOSE=0` must stay off.
    #[test]
    fn verbose_env_flag_is_value_parsed() {
        for on in ["1", "true", "TRUE", "yes", "On", " on "] {
            assert!(flag_on(on), "{on:?} must be ON");
        }
        for off in ["", "0", "false", "no", "off", "2", "verbose"] {
            assert!(!flag_on(off), "{off:?} must be OFF");
        }
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

    /// A-15 parity: the footer's spinner label is phase-derived, mirroring the CLI's
    /// `CliSink`/`phase_spinner_label` — "orienting…"/"gathering…" for the collect passes,
    /// "planning…" for the execute phase's first round, "revising…" once it has already produced
    /// a round this turn, and today's "composing plan…" before any `loop.phase` is observed.
    #[test]
    fn loop_phase_observation_drives_the_phase_labeled_spinner() {
        let mut state = ChatState::new("opus".into());
        state.phase = Phase::Planning;
        state.turn_start = Some(Instant::now());

        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("composing plan…"),
            "no loop.phase observed yet -> byte-compatible fallback"
        );

        state.record_loop_phase("orient");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("orienting…"));
        assert!(!state.gather_mode);

        state.record_loop_phase("gather");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("gathering…"));
        assert!(state.gather_mode, "a gather-phase round renders compact");

        state.record_loop_phase("execute");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("planning…"),
            "the execute phase's first round this turn is a plain plan, not a revision"
        );
        assert!(!state.gather_mode, "execute is never a gather round");

        state.record_loop_phase("execute");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("revising…"),
            "a second execute-phase round this turn means the prior one didn't finish"
        );
    }

    /// A-15 parity: the `ChannelSink` (the TUI's `AgentSink` wiring) forwards `loop.phase` and
    /// `flow.brief` observations as their own `UiEvent`s, same as `flow.plan`/skill/destructive
    /// already are.
    #[test]
    fn channel_sink_forwards_phase_and_brief_observations() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx };

        sink.observation(&flux_evidence::Observation::new(
            "loop.phase",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "phase": "gather" }),
        ));
        match rx.try_recv().expect("a Phase event was sent") {
            UiEvent::Phase(p) => assert_eq!(p, "gather"),
            _ => panic!("expected UiEvent::Phase"),
        }

        sink.observation(&flux_evidence::Observation::new(
            "flow.brief",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "goal": "find the bug", "needs": ["stack trace"] }),
        ));
        match rx.try_recv().expect("a Brief event was sent") {
            UiEvent::Brief { goal, needs } => {
                assert_eq!(goal, "find the bug");
                assert_eq!(needs, vec!["stack trace".to_string()]);
            }
            _ => panic!("expected UiEvent::Brief"),
        }
    }

    /// A-17: the `ChannelSink` forwards a `flow.halt` observation as a `Notice`/`Sev::Err` — the
    /// same real-time-cue machinery destructive-op flags and skill activation already use — with
    /// `halt_line`'s `✗ step N/M <op> failed — revising…` text.
    #[test]
    fn channel_sink_forwards_flow_halt_as_a_notice() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx };

        sink.observation(&flux_evidence::Observation::new(
            "flow.halt",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "step": 4, "of": 9, "op": "edit", "kind": "runtime", "fatal": false }),
        ));
        match rx.try_recv().expect("a Notice event was sent") {
            UiEvent::Notice { text, sev } => {
                assert_eq!(text, "✗ step 4/9 edit failed — revising…");
                assert_eq!(sev, Sev::Err);
            }
            _ => panic!("expected UiEvent::Notice"),
        }
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

    /// A-15: the brief renders immediately and compactly — `◆ goal: …` plus a dim needs list —
    /// the "feedback within seconds" artifact design Part 1 asks for.
    #[test]
    fn brief_entry_renders_goal_and_needs() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::Brief {
            goal: "find the bug".into(),
            needs: vec!["stack trace".into(), "repro steps".into()],
        });
        let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("goal"));
        assert!(content.contains("find the bug"));
        assert!(content.contains("stack trace"));
    }

    /// A-15: a gather plan (small, read-only) renders as a compact one-liner — op names, not the
    /// full tree + risk badge a full execution `Plan` entry gets (`plan_entry_renders_tree` above,
    /// unchanged by this story).
    #[test]
    fn gather_plan_entry_renders_compact_not_full_tree() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::GatherPlan(serde_json::json!({
            "plan": "flow\n└─ $x = read(\"Cargo.toml\")   !read",
            "plan_ast": {
                "body": [{
                    "kind": "bind",
                    "name": "x",
                    "value": {
                        "kind": "call",
                        "op": "read",
                        "args": [{ "kind": "lit", "value": { "path": "Cargo.toml" } }],
                    },
                }],
            },
            "risk": "low",
            "ops": 1,
        })));
        let mut terminal = Terminal::new(TestBackend::new(70, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("gathering"));
        assert!(content.contains("read"));
        // `render`'s full-tree header is the bold word "plan" + the risk badge ("low") — neither
        // appears in the compact one-liner (which never touches `plan_ast`'s tree text at all;
        // `content.contains('└')` isn't a safe check here since the TUI's OWN box borders use the
        // same corner glyph).
        assert!(
            !content.contains("plan") && !content.contains("low"),
            "a compact one-liner must not show the full-tree header/risk badge, got: {content}"
        );
    }

    /// A-17: a resumed/halted plan (`resumed: true`) renders its ✓/✗/· marker-prefixed `plan` text
    /// directly — the CLI/TUI residual this story closes (the surface used to always reconstruct an
    /// unmarked full tree from `plan_ast` instead, silently dropping the markers `flow.plan` already
    /// carried since A-16).
    #[test]
    fn resumed_plan_entry_renders_marker_colored_lines_not_the_full_tree() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::Plan(serde_json::json!({
            "plan": "✓ 0: $a = echo(\"first\")\n✗ 1: boom()",
            "plan_ast": {
                "body": [
                    {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
                    {"kind":"call","op":"boom","args":[]}
                ],
            },
            "risk": "low",
            "ops": 2,
            "resumed": true,
        })));
        let mut terminal = Terminal::new(TestBackend::new(70, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains('✓'), "{content}");
        assert!(content.contains('✗'), "{content}");
        assert!(content.contains("echo"), "{content}");
        assert!(content.contains("boom"), "{content}");
    }
}
