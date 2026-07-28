//! `flux-tui` — a ratatui chat frontend for the agent.
//!
//! [`render`] draws a dense, borderless chat: a viewport-only transcript, compact header/footer,
//! and multiline composer separated solely by its background. [`run`] drives the async crossterm
//! loop: turns stream Markdown, plans and tool cards inline; follow-ups queue visibly; sessions can
//! be resumed with their durable activity; PgUp/PgDn/wheel scroll; Ctrl-C interrupts; and guarded
//! operations raise a y/a/N approval sheet. Headless layout behavior is pinned with `TestBackend`.

mod controller;
mod projection;
mod rendering;
pub mod spinners;
pub mod splash;
mod state;
mod terminal_io;

pub use controller::ApprovalView;
use controller::{
    approval_key, send_action_event, show_next_approval, ApprovalAction, ChannelApprover,
    ChannelSink, PendingApproval, UiEvent,
};
#[cfg(test)]
use projection::staged_intent_entry;
use projection::{historical_observation_entry, load_history};
pub use rendering::render;
pub use state::ChatState;
use state::Phase;
use terminal_io::{TerminalGuard, Tui};

pub mod theme;
pub mod toolview;

mod markdown;
mod plan;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use flux_core::humanize::{fmt_count, fmt_elapsed};
use flux_core::Usage;
use flux_flow::engine::FlowEngine;
use flux_flow::AgentSink;
use flux_provider::Provider;
use flux_runtime::{ApprovalChoice, Approver, ToolResult};
use flux_spec::IntentSet;

use crate::theme::Theme;

/// Provider/model resolved by the CLI for an in-TUI `/model` switch.
pub struct ResolvedModel {
    pub provider: Arc<dyn Provider>,
    /// Provider-facing model id stored on [`FlowEngine`].
    pub wire_model: String,
    /// Canonical user-facing provider/model spec used for cost attribution.
    pub model_spec: String,
}

/// Surface-owned model factory. `flux-tui` stays provider-neutral; `flux-cli` supplies the same
/// resolver its command-line `-m` path uses.
pub trait ModelResolver: Send + Sync {
    fn resolve(&self, spec: &str) -> anyhow::Result<ResolvedModel>;
}

/// Optional capabilities for the richer TUI entry point.
pub struct TuiRunOptions {
    /// Preserve the engine's headless allow approver instead of installing the interactive sheet.
    pub auto_approve: bool,
    /// Canonical provider/model spec used for header display and cost attribution.
    pub model_spec: Option<String>,
    /// Optional surface-owned resolver that enables `/model <spec>`.
    pub model_resolver: Option<Arc<dyn ModelResolver>>,
    /// Configured theme name (`dark` / `light` / `mono`); `None` falls back to `dark` (C-104).
    pub theme: Option<String>,
}

impl TuiRunOptions {
    pub fn new(auto_approve: bool, model_spec: Option<String>) -> Self {
        Self {
            auto_approve,
            model_spec,
            model_resolver: None,
            theme: None,
        }
    }
}

/// Braille spinner frames (shared idiom with the CLI); the fallback when the
/// terminal lacks truecolor for the animated `spinners` footer bar.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Width of the animated footer effect bar.
const FOOTER_BAR_WIDTH: usize = 12;

/// Whether the terminal advertises 24-bit color and color isn't disabled — gates the
/// truecolor footer effects (`Color::Rgb` would emit raw truecolor SGR regardless).
fn terminal_truecolor() -> bool {
    static TRUECOLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRUECOLOR.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && std::env::var("COLORTERM")
                .map(|v| v.contains("truecolor") || v.contains("24bit"))
                .unwrap_or(false)
    })
}

/// Whether the operator disabled color output entirely (forces the `mono` theme, C-104).
fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

/// Resolve a theme name for this terminal, falling back to `dark` for `None`/unknown names.
fn resolve_theme(name: Option<&str>) -> (String, Theme) {
    let name = name.unwrap_or("dark");
    match Theme::by_name(name, terminal_truecolor(), no_color()) {
        Some(theme) => (name.to_string(), theme),
        None => (
            "dark".to_string(),
            Theme::by_name("dark", terminal_truecolor(), no_color()).unwrap_or(Theme::DARK),
        ),
    }
}
/// Streaming cursor block appended to an in-progress assistant message.
const CURSOR: &str = "▍";
/// Max expanded-detail lines per tool card. Lifted entirely under verbose (`flux tui -v` /
/// `FLUX_VERBOSE`), whose promise is tool output in full, no truncation.
const MAX_DETAIL: usize = 30;

/// The footer's model-stage spinner label (mirrors the CLI's `phase_spinner_label`). Current turns
/// use `intent`/`explore`; the older `orient`/`gather`/`execute` labels remain readable when a
/// historical session is projected. A phase-less turn falls back to a neutral label.
fn loop_phase_label(phase: Option<&str>, execute_rounds: usize) -> &'static str {
    match phase {
        Some("intent") => "routing intent…",
        Some("explore") => "exploring…",
        Some("orient") => "orienting…",
        Some("gather") => "gathering…",
        Some("execute") => {
            if execute_rounds > 1 {
                "revising…"
            } else {
                "planning…"
            }
        }
        _ => "working…",
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

/// The available slash commands.
const COMMANDS: &[SlashCmd] = &[
    SlashCmd {
        name: "help",
        desc: "show keybindings",
    },
    SlashCmd {
        name: "clear",
        desc: "start a fresh session",
    },
    SlashCmd {
        name: "new",
        desc: "clear and start fresh",
    },
    SlashCmd {
        name: "model",
        desc: "show or switch model",
    },
    SlashCmd {
        name: "effort",
        desc: "show or set reasoning effort",
    },
    SlashCmd {
        name: "quit",
        desc: "exit flux",
    },
    SlashCmd {
        name: "compact",
        desc: "compact session context",
    },
    SlashCmd {
        name: "shell",
        desc: "toggle the generic bash op",
    },
    SlashCmd {
        name: "tools",
        desc: "list registered tools",
    },
    SlashCmd {
        name: "evidence",
        desc: "show durable evidence",
    },
    SlashCmd {
        name: "session",
        desc: "show the active session",
    },
    SlashCmd {
        name: "sessions",
        desc: "list recent sessions",
    },
    SlashCmd {
        name: "resume",
        desc: "resume a session id",
    },
    SlashCmd {
        name: "queue",
        desc: "manage queued follow-ups",
    },
    SlashCmd {
        name: "theme",
        desc: "show or switch the color theme",
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

/// The help overlay's keybinding rows (C-110): `(keys, what)`. The slash-command half of the
/// overlay iterates [`COMMANDS`] directly so it can never drift from the real table.
const HELP_KEYS: &[(&str, &str)] = &[
    ("↵", "send (or queue while a turn runs)"),
    ("Ctrl-J / Alt-↵ / Shift-↵", "newline"),
    ("↑/↓", "history recall (at the input's edge)"),
    (
        "Ctrl-R",
        "reverse history search (shadows redo; Ctrl-U undo)",
    ),
    ("Ctrl-F", "transcript search · n/N step matches"),
    ("PgUp/PgDn / wheel", "scroll transcript"),
    ("Ctrl-End", "jump to latest"),
    ("Ctrl-E", "expand/collapse tool details"),
    (
        "Ctrl-T",
        "toggle mouse capture (native select/copy while off)",
    ),
    (
        "y / a / n·Esc",
        "approval: allow / always / deny (other keys ignored)",
    ),
    ("Ctrl-C", "interrupt · clear · quit"),
    ("Ctrl-D", "quit (empty input)"),
    ("F1 / Esc", "open/close this help"),
];

/// One item in the transcript. Each renders to one or more styled [`Line`]s at a given width.
#[derive(Debug)]
enum Entry {
    /// A user message (may contain newlines once the input is multiline).
    User(String),
    /// An assistant reply — plain while streaming, Markdown once done (cached per width).
    Assistant(Assistant),
    /// Live extended-thinking tokens streamed during a model-backed stage, rendered as Markdown
    /// once sealed (same `Assistant` widget, distinct entry so it doesn't merge with the reply).
    Thinking(Assistant),
    /// A dispatched tool/op call + (once it returns) its result — rendered as one card.
    Tool(ToolEntry),
    /// An observation/notice (skill activation, destructive flag, error).
    Notice { text: String, sev: Sev },
    /// The accepted adaptive intent, reconstructed from the durable `turn.intent` observation.
    Intent(IntentEntry),
    /// A durable authored or host-built DAG (`flow.plan` observation payload).
    Plan(serde_json::Value),
    /// The orient/gather grounding artifact (design Part 1's `brief: {goal, needs[]}`, A-15):
    /// rendered the moment it's accepted, immediately and compactly.
    Brief { goal: String, needs: Vec<String> },
    /// A bounded, read-only gather round's compiled plan (the `flow.plan` observation payload,
    /// A-15) — rendered as a compact one-liner rather than the full tree + risk badge `Plan` gets.
    GatherPlan(serde_json::Value),
}

#[derive(Debug)]
struct IntentEntry {
    intent: String,
    families: Vec<String>,
    operations: Vec<String>,
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
    timing: Option<flux_core::OperationTiming>,
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
    approval_wait: Option<Duration>,
}

impl ToolEntry {
    fn new(name: String, input: serde_json::Value) -> Self {
        let call = toolview::format_call(&name, &input);
        ToolEntry {
            name,
            call,
            input,
            started: Instant::now(),
            timing: None,
            result: None,
        }
    }

    fn historical(
        name: String,
        input: serde_json::Value,
        content: String,
        is_error: bool,
        elapsed: Duration,
    ) -> Self {
        let call = toolview::format_call(&name, &input);
        let summary = toolview::format_result(&name, &content, is_error);
        ToolEntry {
            name,
            call,
            input,
            started: Instant::now()
                .checked_sub(elapsed)
                .unwrap_or_else(Instant::now),
            timing: None,
            result: Some(ToolOutcome {
                is_error,
                content,
                summary,
                elapsed,
                approval_wait: None,
            }),
        }
    }

    fn historical_reduced(name: String, error: Option<String>, elapsed: Duration) -> Self {
        let input = serde_json::Value::Null;
        let call = toolview::format_call(&name, &input);
        let is_error = error.is_some();
        let content = error.unwrap_or_default();
        let summary = if is_error {
            toolview::format_result(&name, &content, true)
        } else {
            Some("completed".into())
        };
        ToolEntry {
            name,
            call,
            input,
            started: Instant::now()
                .checked_sub(elapsed)
                .unwrap_or_else(Instant::now),
            timing: None,
            result: Some(ToolOutcome {
                is_error,
                content,
                summary,
                elapsed,
                approval_wait: None,
            }),
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

/// Cached, fully wrapped transcript layout. State changes invalidate the cache; animation-only
/// frames reuse it and clone only the rows currently visible in the viewport.
#[derive(Debug)]
struct TranscriptLayout {
    revision: u64,
    width: u16,
    lines: Vec<Line<'static>>,
    /// `(wrapped row, entry index)` of each running tool card's header row, in order — the
    /// viewport patches these with a live spinner + elapsed badge per tick (C-109).
    running_rows: Vec<(u16, usize)>,
}

/// Ctrl-R reverse incremental history search (C-107): readline behavior — the query edits live,
/// the newest match lands in the composer, Ctrl-R again steps older, Esc restores the draft.
#[derive(Debug, Default)]
pub(crate) struct HistorySearch {
    pub(crate) query: String,
    /// Index into `history` of the current match (`None` = no match yet / query empty).
    pub(crate) index: Option<usize>,
    /// The composer content stashed when the search opened (restored on Esc).
    pub(crate) draft: String,
}

/// Ctrl-F transcript search (C-108) over wrapped transcript rows. `matches` are wrapped-row
/// indices, valid for exactly one `(revision, width)` layout — recomputed lazily when stale.
#[derive(Debug, Default)]
pub(crate) struct TranscriptSearch {
    pub(crate) query: String,
    /// True while the query is being edited; Enter commits, then n/N step matches.
    pub(crate) typing: bool,
    pub(crate) matches: Vec<u16>,
    /// Index into `matches` of the current match.
    pub(crate) current: usize,
    /// The `(transcript_revision, width)` the matches were computed against.
    pub(crate) valid_for: (u64, u16),
}

/// Search `history` backwards (newest first) for a case-insensitive substring match, starting
/// strictly before `before` (or from the end for `None`). Pure, unit-testable (C-107).
fn rsearch(history: &[String], query: &str, before: Option<usize>) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let query = query.to_lowercase();
    let end = before.unwrap_or(history.len());
    history[..end.min(history.len())]
        .iter()
        .rposition(|entry| entry.to_lowercase().contains(&query))
}

/// Wrapped-row indices whose flattened text contains `query` (case-insensitive). A match that
/// spans a wrap boundary is not found — documented v1 limitation (C-108).
fn find_match_rows(lines: &[Line<'_>], query: &str) -> Vec<u16> {
    if query.is_empty() {
        return Vec::new();
    }
    let query = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .take(u16::MAX as usize)
        .filter(|(_, line)| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .to_lowercase()
                .contains(&query)
        })
        .map(|(i, _)| i as u16)
        .collect()
}

/// Re-style the parts of `line` that match `query` (case-insensitive): REVERSED for every match,
/// plus `current_style` when this is the current match's row. Splits spans at match boundaries so
/// only the matching cells change; used on the cloned viewport slice only — the cached layout is
/// never touched (C-108).
fn highlight_matches(line: &Line<'static>, query: &str, current: Option<Style>) -> Line<'static> {
    if query.is_empty() {
        return line.clone();
    }
    // Flatten to chars with their span styles; match on a per-char lowercase projection.
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|(c, _)| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let needle: Vec<char> = query
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    if needle.is_empty() || chars.len() < needle.len() {
        return line.clone();
    }
    let mut hit = vec![false; chars.len()];
    let mut i = 0;
    while i + needle.len() <= lower.len() {
        if lower[i..i + needle.len()] == needle[..] {
            hit[i..i + needle.len()].iter_mut().for_each(|h| *h = true);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    if !hit.iter().any(|h| *h) {
        return line.clone();
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut run: Option<(Style, bool)> = None;
    for (idx, (c, style)) in chars.iter().enumerate() {
        let key = (*style, hit[idx]);
        if run != Some(key) {
            if let Some((style, matched)) = run.take() {
                spans.push(styled_run(
                    std::mem::take(&mut buf),
                    style,
                    matched,
                    current,
                ));
            }
            run = Some(key);
        }
        buf.push(*c);
    }
    if let Some((style, matched)) = run {
        spans.push(styled_run(buf, style, matched, current));
    }
    let mut out = Line::from(spans);
    out.style = line.style;
    out.alignment = line.alignment;
    out
}

/// One tool-card header row: `→ verb  arg … badge` with the arg truncated so the badge sits
/// flush right. Shared by the cached build (`tool_lines`) and the per-tick running-badge patch
/// (C-109) so the pad math cannot drift between the two.
fn tool_header_line(
    t: &Theme,
    verb: &str,
    arg_full: &str,
    badge: String,
    badge_style: Style,
    width: u16,
) -> Line<'static> {
    let badge_w = UnicodeWidthStr::width(badge.as_str());
    let fixed = 2 + UnicodeWidthStr::width(verb) + 2; // "→ " + verb + "  "
    let arg_room = (width as usize).saturating_sub(fixed + badge_w + 1);
    let arg = truncate(arg_full, arg_room.max(4));
    let used = fixed + UnicodeWidthStr::width(arg.as_str());
    let pad = (width as usize).saturating_sub(used + badge_w).max(1);
    Line::from(vec![
        Span::styled("→ ", t.tool_style()),
        Span::styled(
            verb.to_string(),
            t.tool_style().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(arg, t.muted_style()),
        Span::raw(" ".repeat(pad)),
        Span::styled(badge, badge_style),
    ])
}

/// The static in-flight badge recorded into the cached layout (C-109 patches it per tick in the
/// viewport only, so the cache stays untouched across animation frames).
const RUNNING_BADGE: &str = "◌ running";

fn styled_run(text: String, style: Style, matched: bool, current: Option<Style>) -> Span<'static> {
    if !matched {
        return Span::styled(text, style);
    }
    let mut style = style.add_modifier(Modifier::REVERSED);
    if let Some(accent) = current {
        style = style.patch(accent);
    }
    Span::styled(text, style)
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

impl ChatState {
    #[cfg(test)]
    fn new(model: String) -> Self {
        Self::for_session(model, String::new())
    }

    fn for_session(model: String, session_id: String) -> Self {
        ChatState {
            entries: Vec::new(),
            transcript_revision: 0,
            transcript_layout: RefCell::new(None),
            input: fresh_textarea(),
            approval: None,
            assistant_open: false,
            phase: Phase::Idle,
            turn_start: None,
            session_id,
            model,
            model_spec: None,
            theme: Theme::default(),
            theme_name: "dark".into(),
            mouse_capture: true,
            history_search: None,
            search: None,
            help_open: false,
            auto_approve: false,
            effort: None,
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
            queue: VecDeque::new(),
            queue_open: false,
            queue_sel: 0,
            queue_edit_index: None,
            session_picker: None,
            session_sel: 0,
            scroll: 0,
            follow: true,
            last_max_scroll: Cell::new(0),
            last_page: Cell::new(1),
            plan_phase: None,
            execute_rounds: 0,
            gather_mode: false,
            unread: 0,
            next_action_id: 1,
            active_action_id: None,
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
            "orient" | "intent" | "explore" => self.gather_mode = false,
            _ => {}
        }
        self.plan_phase = Some(phase.to_string());
    }

    /// Attach a resolved `provider/model` spec + pricing table so the header can show a running
    /// dollar cost alongside tokens (C-06) — mirrors the CLI's `CliSink::with_cost`.
    pub fn with_cost(mut self, model_spec: String, pricing: flux_core::PricingTable) -> Self {
        self.model_spec = Some(model_spec.clone());
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

    fn mark_transcript_dirty(&mut self) {
        self.transcript_revision = self.transcript_revision.saturating_add(1);
        self.transcript_layout.get_mut().take();
    }

    fn toggle_details(&mut self) {
        self.expand_tools = !self.expand_tools;
        self.mark_transcript_dirty();
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
        self.mark_transcript_dirty();
        self.assistant_open = false;
        if !self.follow {
            self.unread = self.unread.saturating_add(1);
        }
    }

    fn enqueue(&mut self, text: String) {
        if !text.trim().is_empty() {
            self.queue.push_back(text);
            self.queue_sel = self.queue_sel.min(self.queue.len().saturating_sub(1));
        }
    }

    fn queue_remove_selected(&mut self) -> Option<String> {
        if self.queue.is_empty() {
            return None;
        }
        let index = self.queue_sel.min(self.queue.len() - 1);
        let removed = self.queue.remove(index);
        self.queue_edit_index = self.queue_edit_index.and_then(|editing| {
            if editing == index {
                None
            } else if editing > index {
                Some(editing - 1)
            } else {
                Some(editing)
            }
        });
        self.queue_sel = self.queue_sel.min(self.queue.len().saturating_sub(1));
        removed
    }

    fn queue_begin_edit(&mut self) -> Option<String> {
        if self.queue.is_empty() {
            return None;
        }
        let index = self.queue_sel.min(self.queue.len() - 1);
        let text = self.queue.get(index)?.clone();
        self.queue_edit_index = Some(index);
        Some(text)
    }

    fn queue_commit_edit(&mut self, text: String) -> bool {
        let Some(index) = self.queue_edit_index.take() else {
            return false;
        };
        let Some(item) = self.queue.get_mut(index) else {
            return false;
        };
        *item = text;
        true
    }

    fn queue_cancel_edit(&mut self) -> bool {
        self.queue_edit_index.take().is_some()
    }

    fn queue_move(&mut self, delta: isize) {
        if self.queue.len() < 2 {
            return;
        }
        let from = self.queue_sel.min(self.queue.len() - 1);
        let to = from
            .saturating_add_signed(delta)
            .min(self.queue.len().saturating_sub(1));
        if from != to {
            self.queue.swap(from, to);
            if self.queue_edit_index == Some(from) {
                self.queue_edit_index = Some(to);
            } else if self.queue_edit_index == Some(to) {
                self.queue_edit_index = Some(from);
            }
            self.queue_sel = to;
        }
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
            self.mark_transcript_dirty();
            self.assistant_open = false;
        }
    }

    /// Append a thinking-token delta to the open thinking entry.
    fn stream_thinking(&mut self, delta: &str) {
        if let Some(Entry::Thinking(a)) = self.entries.last_mut() {
            if !a.done {
                a.text.push_str(delta);
                self.mark_transcript_dirty();
                return;
            }
        }
        // No open thinking entry — open one on the fly.
        self.entries.push(Entry::Thinking(Assistant {
            text: delta.to_string(),
            done: false,
            cache: RefCell::new(None),
        }));
        self.mark_transcript_dirty();
        self.assistant_open = false;
    }

    /// Seal the open thinking entry (called on `Planning(false)`).
    fn end_thinking(&mut self) {
        if let Some(Entry::Thinking(a)) = self.entries.last_mut() {
            if !a.done {
                a.text = a.text.trim_end().to_string();
                a.done = true;
                self.mark_transcript_dirty();
            }
        }
    }

    /// Append a streamed assistant token, extending the live assistant message (or starting one).
    fn stream_text(&mut self, delta: &str) {
        if self.assistant_open {
            if let Some(Entry::Assistant(a)) = self.entries.last_mut() {
                a.text.push_str(delta);
                self.mark_transcript_dirty();
                return;
            }
        }
        self.entries.push(Entry::Assistant(Assistant {
            text: delta.to_string(),
            done: false,
            cache: RefCell::new(None),
        }));
        self.mark_transcript_dirty();
        self.assistant_open = true;
    }

    fn end_stream(&mut self) {
        if self.assistant_open {
            if let Some(Entry::Assistant(a)) = self.entries.last_mut() {
                a.text = a.text.trim_end().to_string();
                a.done = true;
                self.mark_transcript_dirty();
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
        self.push_history(text);
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
                if tool.result.is_none() && tool.name == name {
                    let elapsed = tool
                        .timing
                        .and_then(|timing| timing.execution_us)
                        .map(Duration::from_micros)
                        .unwrap_or_else(|| tool.started.elapsed());
                    let approval_wait = tool
                        .timing
                        .and_then(|timing| timing.approval_wait_us)
                        .map(Duration::from_micros);
                    tool.result = Some(ToolOutcome {
                        is_error,
                        elapsed,
                        approval_wait,
                        summary,
                        content,
                    });
                    self.mark_transcript_dirty();
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

    fn time_tool(&mut self, name: &str, timing: flux_core::OperationTiming) {
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Tool(tool) = entry {
                if tool.result.is_none() && tool.name == name {
                    tool.timing = Some(timing);
                    return;
                }
            }
        }
    }

    /// Flatten the transcript to styled logical lines at `width`, with a blank line between
    /// entries. [`Self::ensure_transcript_layout`] wraps and caches these rows.
    fn build_transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
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
                    if !a.text.is_empty() {
                        let count = a.text.lines().count().max(1);
                        out.push(Line::styled(
                            if a.done {
                                format!(
                                    "thinking · {count} line{} · Ctrl-E details",
                                    if count == 1 { "" } else { "s" }
                                )
                            } else {
                                "thinking…".to_string()
                            },
                            t.muted_style(),
                        ));
                        if self.expand_tools {
                            out.extend(a.lines(width, t).into_iter().map(|mut l| {
                                for span in &mut l.spans {
                                    span.style = span.style.patch(t.muted_style());
                                }
                                l
                            }));
                        }
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
                Entry::Intent(intent) => {
                    let intent_cap = usize::from(width).saturating_sub(12).clamp(24, 160);
                    out.push(Line::from(vec![
                        Span::styled("◆ ", t.accent_style()),
                        Span::styled("intent: ", t.accent_style().add_modifier(Modifier::BOLD)),
                        Span::raw(truncate(&intent.intent, intent_cap)),
                    ]));
                    let capabilities = if intent.families.is_empty() {
                        "none".to_string()
                    } else {
                        intent.families.join(", ")
                    };
                    let plural = if intent.operations.len() == 1 {
                        "operation"
                    } else {
                        "operations"
                    };
                    out.push(Line::styled(
                        format!(
                            "  capabilities: {capabilities} · {} {plural}",
                            intent.operations.len()
                        ),
                        t.muted_style(),
                    ));
                    if self.verbose && !intent.operations.is_empty() {
                        out.push(Line::styled(
                            format!("  operations: {}", intent.operations.join(", ")),
                            t.muted_style(),
                        ));
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

    fn ensure_transcript_layout(&self, width: u16) {
        let current = self.transcript_layout.borrow();
        let valid = current.as_ref().is_some_and(|layout| {
            layout.revision == self.transcript_revision && layout.width == width
        });
        drop(current);
        if valid {
            return;
        }
        let mut lines = wrap_styled_lines(self.build_transcript_lines(width), width);
        const MAX_LAYOUT_LINES: usize = u16::MAX as usize;
        if lines.len() > MAX_LAYOUT_LINES {
            let omitted = lines.len() - MAX_LAYOUT_LINES + 1;
            lines.drain(0..omitted);
            lines.insert(
                0,
                Line::styled(
                    format!("… {omitted} older rows omitted"),
                    self.theme.muted_style(),
                ),
            );
        }
        // C-109: pair each `◌ running` header row with its running tool entry, in order. The
        // header line is width-fitted (badge flush right) so wrapping never splits it — the
        // static badge span survives as the row's last span.
        let running_entries: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, Entry::Tool(tool) if tool.result.is_none()))
            .map(|(i, _)| i)
            .collect();
        let running_rows: Vec<(u16, usize)> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                line.spans.last().is_some_and(|span| {
                    span.content.as_ref() == RUNNING_BADGE && span.style == self.theme.warn_style()
                })
            })
            .map(|(row, _)| row as u16)
            .zip(running_entries)
            .collect();
        *self.transcript_layout.borrow_mut() = Some(TranscriptLayout {
            revision: self.transcript_revision,
            width,
            lines,
            running_rows,
        });
    }

    /// All wrapped transcript rows, primarily for tests and non-viewport projections.
    #[cfg(test)]
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.ensure_transcript_layout(width);
        self.transcript_layout
            .borrow()
            .as_ref()
            .map(|layout| layout.lines.clone())
            .unwrap_or_default()
    }

    /// Clone only the visible wrapped rows. Layout is cached across spinner frames, so a long
    /// transcript does not get rebuilt or handed wholesale to the terminal widget on every tick.
    fn transcript_viewport(&self, width: u16, height: u16) -> Vec<Line<'static>> {
        self.ensure_transcript_layout(width);
        let layout = self.transcript_layout.borrow();
        let Some(layout) = layout.as_ref() else {
            return Vec::new();
        };
        let total = layout.lines.len().min(u16::MAX as usize) as u16;
        let max_scroll = total.saturating_sub(height);
        self.last_max_scroll.set(max_scroll);
        self.last_page.set(height.max(1));
        let offset = if self.follow {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        let mut visible: Vec<Line<'static>> = layout
            .lines
            .iter()
            .skip(offset as usize)
            .take(height as usize)
            .cloned()
            .collect();
        // C-109: patch visible running tool headers with a live spinner + elapsed badge. Only
        // the cloned slice changes — the cached layout is untouched, so animation frames never
        // invalidate it.
        for (row, entry_idx) in &layout.running_rows {
            let Some(slot) = row
                .checked_sub(offset)
                .map(usize::from)
                .filter(|slot| *slot < visible.len())
            else {
                continue;
            };
            let Some(Entry::Tool(tool)) = self.entries.get(*entry_idx) else {
                continue;
            };
            let elapsed = tool.started.elapsed();
            let frame = SPINNER[(elapsed.as_millis() / 80) as usize % SPINNER.len()];
            visible[slot] = tool_header_line(
                &self.theme,
                &tool.call.verb,
                &tool.call.arg,
                format!("{frame} running · {}", fmt_elapsed(elapsed)),
                self.theme.warn_style(),
                width,
            );
        }
        // C-108: highlight matches on the cloned visible slice only — the cache stays untouched.
        if let Some(search) = self.search.as_ref().filter(|s| !s.query.is_empty()) {
            let current_row = search.matches.get(search.current).copied();
            return visible
                .into_iter()
                .enumerate()
                .map(|(i, line)| {
                    let row = offset + i as u16;
                    let current =
                        (Some(row) == current_row).then(|| Style::default().fg(self.theme.accent));
                    highlight_matches(&line, &search.query, current)
                })
                .collect();
        }
        visible
    }

    /// Recompute the transcript-search match rows against the current cached layout (no-op when
    /// no layout has been built yet). Keeps `current` clamped and pointing at the last match
    /// after a fresh query edit when it was unset.
    fn refresh_search_matches(&mut self) {
        let Some(query) = self.search.as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let computed = {
            let layout = self.transcript_layout.borrow();
            layout
                .as_ref()
                .map(|l| (find_match_rows(&l.lines, &query), l.revision, l.width))
        };
        let Some((matches, revision, width)) = computed else {
            return;
        };
        if let Some(search) = self.search.as_mut() {
            search.current = matches.len().saturating_sub(1);
            search.matches = matches;
            search.valid_for = (revision, width);
        }
    }

    /// Whether the search matches are stale for the current layout (resize / new content).
    fn search_matches_stale(&self) -> bool {
        let layout = self.transcript_layout.borrow();
        match (self.search.as_ref(), layout.as_ref()) {
            (Some(search), Some(layout)) => search.valid_for != (layout.revision, layout.width),
            _ => false,
        }
    }

    /// Center the current transcript-search match in the viewport (detaches follow mode).
    fn center_current_match(&mut self) {
        let Some(row) = self
            .search
            .as_ref()
            .and_then(|s| s.matches.get(s.current).copied())
        else {
            return;
        };
        self.follow = false;
        self.unread = 0;
        let half = self.last_page.get().max(1) / 2;
        self.scroll = row.saturating_sub(half).min(self.last_max_scroll.get());
    }

    /// Render one tool card: a `→ verb arg … [badge]` header, a one-line summary, and — when
    /// `expand_tools` is set — the full detail (a unified diff for `edit`/`write`, else the output,
    /// capped at [`MAX_DETAIL`] lines unless `verbose`).
    fn tool_lines(&self, tool: &ToolEntry, width: u16) -> Vec<Line<'static>> {
        let t = &self.theme;
        let mut out: Vec<Line> = Vec::new();

        // Badge (right-aligned, fixed idea of width): running is static, done shows ✓/✗ + elapsed.
        let (badge, badge_style) = match &tool.result {
            // The in-flight badge is static IN THE CACHE; the viewport patches it with a live
            // spinner + elapsed per tick (C-109), so cached rows stay untouched across frames.
            None => (RUNNING_BADGE.to_string(), t.warn_style()),
            Some(o) if o.is_error => (format!("✗ {}", fmt_tool_timing(o)), t.err_style()),
            Some(o) => (format!("✓ {}", fmt_tool_timing(o)), t.ok_style()),
        };

        out.push(tool_header_line(
            t,
            &tool.call.verb,
            &tool.call.arg,
            badge,
            badge_style,
            width,
        ));

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
            Span::styled("flux", t.accent_style().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(
                    "  {} · {}",
                    self.session_id,
                    self.model_spec.as_deref().unwrap_or(&self.model)
                ),
                t.muted_style(),
            ),
        ];
        let mut right: Vec<Vec<Span<'static>>> = Vec::new();
        // C-06: the header used to sum only input/output, silently ignoring cache read/write
        // tokens — a heavily-cached session looked identical to an uncached one. `cache` here is
        // BOTH tiers combined (read + write); a session with either shows the segment.
        //
        // C-102: tokens / cache / cost are separate droppable segments — on a narrow terminal the
        // bar sheds cost first, then cache, keeping the token total visible the longest.
        // C-116: safety-relevant `auto-ok` is the most precious right segment — first in the
        // vec, so bar_line sheds everything else before it.
        if self.auto_approve {
            right.push(vec![Span::styled("auto-ok", t.warn_style())]);
        }
        let cache = self.tokens_cache_read + self.tokens_cache_write;
        if self.tokens_in + self.tokens_out + cache > 0 {
            right.push(vec![Span::styled(
                format!(
                    "Σ ↑{} ↓{} tok",
                    fmt_count(self.tokens_in),
                    fmt_count(self.tokens_out)
                ),
                t.muted_style(),
            )]);
            if cache > 0 {
                right.push(vec![Span::styled(
                    format!("cache {}", fmt_count(cache)),
                    t.muted_style(),
                )]);
            }
            // C-33: an unpriced metered-cloud turn switches the cost segment to the `$?` state
            // (`$X.XXXX+?` when part of the run WAS priced, bare `$?` when none of it was) rather
            // than rendering a total that silently omits real spend — mirrors flux-cli's
            // ` · $? (unpriced)` marker.
            let cost = match (self.cost_usd, self.cost_unpriced) {
                (Some(usd), true) => Some(format!("${usd:.4}+? (unpriced)")),
                (Some(usd), false) => Some(format!("${usd:.4}")),
                (None, true) => Some("$? (unpriced)".to_string()),
                (None, false) => None,
            };
            if let Some(cost) = cost {
                right.push(vec![Span::styled(cost, t.muted_style())]);
            }
        }
        // C-116: mode badges, shown only when active/non-default. Least precious — dropped first.
        if flux_runtime::shell_opt_in() {
            right.push(vec![Span::styled("shell", t.warn_style())]);
        }
        if self.gather_mode {
            right.push(vec![Span::styled("gather", t.accent_style())]);
        }
        if let Some(effort) = &self.effort {
            right.push(vec![Span::styled(
                format!("effort:{effort}"),
                t.muted_style(),
            )]);
        }
        // Segment order [auto-ok, tokens, cache, cost, shell, gather, effort]; bar_line drops
        // from the end, so the badges shed first and auto-ok survives the longest (C-102/C-116).
        for seg in right.iter_mut().skip(1) {
            seg.insert(0, Span::styled(" · ", t.muted_style()));
        }
        bar_line(left, right, width)
    }

    /// The bottom footer bar: an animated spinner + phase + elapsed while running, else keybinding
    /// hints — with the last turn's step count + duration on the right.
    fn footer_line(&self, width: u16) -> Line<'static> {
        let t = &self.theme;
        // Footer takeover precedence (C-107/C-108): transcript search > history search > normal.
        if let Some(search) = &self.search {
            let counter = if search.matches.is_empty() {
                "0/0".to_string()
            } else {
                format!("{}/{}", search.current + 1, search.matches.len())
            };
            let hint = if search.typing {
                "Enter commit · Esc close"
            } else {
                "n/N next/prev · Esc close"
            };
            return bar_line(
                vec![
                    Span::styled(" search: ", t.accent_style()),
                    Span::raw(search.query.clone()),
                    Span::styled(if search.typing { CURSOR } else { "" }, t.accent_style()),
                    Span::styled(format!("  {counter}"), t.muted_style()),
                ],
                vec![vec![Span::styled(hint.to_string(), t.muted_style())]],
                width,
            );
        }
        if let Some(hs) = &self.history_search {
            return bar_line(
                vec![
                    Span::styled(" (reverse-i-search) '", t.accent_style()),
                    Span::raw(hs.query.clone()),
                    Span::styled("': ", t.accent_style()),
                    Span::styled(
                        if hs.index.is_none() && !hs.query.is_empty() {
                            "no match"
                        } else {
                            ""
                        },
                        t.warn_style(),
                    ),
                ],
                vec![vec![Span::styled(
                    "Ctrl-R older · Enter keep · Esc cancel".to_string(),
                    t.muted_style(),
                )]],
                width,
            );
        }
        let left = match self.phase {
            Phase::Idle if self.unread > 0 => vec![Span::styled(
                format!(" ↓ {} new · Ctrl-End latest", self.unread),
                t.accent_style(),
            )],
            // C-105: while capture is off the idle hint IS the indicator — it can never be
            // dropped by the width fight the right-side segments play.
            Phase::Idle if !self.mouse_capture => vec![Span::styled(
                " mouse off · select/copy · Ctrl-T re-enable",
                t.warn_style(),
            )],
            Phase::Idle => vec![Span::styled(
                " Enter send · Ctrl-J newline · / commands",
                t.muted_style(),
            )],
            Phase::Thinking | Phase::Planning => {
                let elapsed = self.turn_start.map(|s| s.elapsed()).unwrap_or_default();
                let label = if self.phase == Phase::Planning {
                    loop_phase_label(self.plan_phase.as_deref(), self.execute_rounds)
                } else {
                    "thinking…"
                };
                // Truecolor terminals get the animated effect bar, cycling one catalog
                // entry per execute round; others keep the braille glyph.
                let mut left = if terminal_truecolor() {
                    let tick = (elapsed.as_millis() / spinners::FPS_MS as u128) as usize;
                    let effect = spinners::by_round(self.execute_rounds);
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(spinners::cells_to_spans(&(effect.frame)(
                        tick,
                        FOOTER_BAR_WIDTH,
                    )));
                    spans.push(Span::raw(" "));
                    spans
                } else {
                    let frame = SPINNER[(elapsed.as_millis() / 80) as usize % SPINNER.len()];
                    vec![Span::styled(format!(" {frame} "), t.accent_style())]
                };
                left.push(Span::raw(label.to_string()));
                left.push(Span::styled(
                    format!("  · {}", fmt_elapsed(elapsed)),
                    t.muted_style(),
                ));
                left
            }
        };
        let mut right: Vec<Vec<Span<'static>>> = Vec::new();
        // C-105: while a turn runs the left side is the spinner, so the capture state rides the
        // right side — first segment, so the metrics drop before it on narrow bars.
        if !self.mouse_capture && self.phase != Phase::Idle {
            right.push(vec![Span::styled("mouse off (Ctrl-T)", t.warn_style())]);
        }
        // C-106: scroll position while detached from follow mode.
        if !self.follow && self.last_max_scroll.get() > 0 {
            let pct = (self.scroll as u32 * 100) / self.last_max_scroll.get().max(1) as u32;
            right.push(vec![Span::styled(format!("⤓ {pct}%"), t.accent_style())]);
        }
        if let Some(e) = self.last_elapsed {
            let plural = if self.steps == 1 { "" } else { "s" };
            right.push(vec![Span::styled(
                format!("{} step{plural} · {}", self.steps, fmt_elapsed(e)),
                t.muted_style(),
            )]);
        }
        // Separators lead each non-first segment, so end-dropped segments never strand one.
        for seg in right.iter_mut().skip(1) {
            seg.insert(0, Span::styled(" · ", t.muted_style()));
        }
        bar_line(left, right, width)
    }

    fn running(&self) -> bool {
        self.active_action_id.is_some()
    }

    fn begin_action(&mut self) -> u64 {
        let id = self.next_action_id;
        self.next_action_id = self.next_action_id.saturating_add(1);
        self.active_action_id = Some(id);
        id
    }

    fn accept_ui_event(&self, event: UiEvent) -> Option<UiEvent> {
        match event {
            UiEvent::Tagged { action_id, event } if self.active_action_id == Some(action_id) => {
                Some(*event)
            }
            UiEvent::Tagged { .. } => None,
            event => Some(event),
        }
    }

    /// Replace the visible state with a read-only projection of one durable session. Nothing in
    /// this fold can dispatch an op or mutate the event store.
    fn project_session(
        &mut self,
        events: &flux_events::EventStore,
        session_id: &str,
    ) -> anyhow::Result<()> {
        use flux_events::EventKind;
        use flux_flow::ast::RunEvent;

        let stored = events
            .load_stream(session_id, None)
            .map_err(|e| anyhow::anyhow!("load session {session_id}: {e}"))?;
        events
            .info(session_id)
            .map_err(|e| anyhow::anyhow!("load session {session_id}: {e}"))?;

        let mut entries = Vec::new();
        let mut starts: HashMap<String, (String, i64)> = HashMap::new();
        let mut turn_usage = Vec::new();
        let mut call_usage: Vec<(String, Usage)> = Vec::new();
        let mut proposed_plan_recorded = false;

        for event in stored {
            match event.kind {
                EventKind::Message(message) => {
                    let text = message.text();
                    if text.trim().is_empty() {
                        continue;
                    }
                    match message.role {
                        flux_core::Role::User => entries.push(Entry::User(text)),
                        flux_core::Role::Assistant => {
                            // `plan_turn` stores "Proposed plan: …" as provider context after the
                            // accepted attempt. The attempt itself is the richer durable UI entry;
                            // do not render the same tree twice on resume.
                            if proposed_plan_recorded && text.starts_with("Proposed plan:\n") {
                                proposed_plan_recorded = false;
                                continue;
                            }
                            proposed_plan_recorded = false;
                            entries.push(Entry::Assistant(Assistant {
                                text,
                                done: true,
                                cache: RefCell::new(None),
                            }));
                        }
                        _ => {}
                    }
                }
                EventKind::Compacted { .. } => entries.push(Entry::Notice {
                    text: "◇ context compacted".into(),
                    sev: Sev::Info,
                }),
                EventKind::PlanAttempted {
                    outcome,
                    error,
                    plan_text,
                    phase,
                    ..
                } => match outcome.as_str() {
                    "accepted" => {
                        if let Some(plan_text) = plan_text {
                            proposed_plan_recorded = true;
                            entries.push(Entry::Plan(serde_json::json!({
                                "plan": plan_text,
                                "ops": 0,
                                "historical": true,
                                "phase": phase,
                            })));
                        }
                    }
                    "compile_error" => entries.push(Entry::Notice {
                        text: format!(
                            "planning failed: {}",
                            error.unwrap_or_else(|| "unknown error".into())
                        ),
                        sev: Sev::Err,
                    }),
                    "rejected" => entries.push(Entry::Notice {
                        text: "plan rejected".into(),
                        sev: Sev::Warn,
                    }),
                    _ => {}
                },
                EventKind::Run(RunEvent::StepStarted { step, op, .. }) => {
                    if !flux_flow::engine::is_loop_machinery_op(&op) {
                        starts.insert(step.0, (op, event.ts_ms));
                    }
                }
                EventKind::Run(RunEvent::OpRecorded {
                    step,
                    op,
                    input_view,
                    input_view_truncated,
                    content,
                    view,
                    is_error,
                    denied,
                    ..
                }) => {
                    if flux_flow::engine::is_loop_machinery_op(&op) {
                        starts.remove(&step.0);
                        continue;
                    }
                    let elapsed_ms = starts
                        .remove(&step.0)
                        .map(|(_, start)| event.ts_ms.saturating_sub(start).max(0) as u64)
                        .unwrap_or(0);
                    let input = input_view
                        .and_then(|raw| {
                            if input_view_truncated {
                                Some(serde_json::Value::String(raw))
                            } else {
                                serde_json::from_str(&raw)
                                    .ok()
                                    .or(Some(serde_json::Value::String(raw)))
                            }
                        })
                        .unwrap_or(serde_json::Value::Null);
                    entries.push(Entry::Tool(ToolEntry::historical(
                        op,
                        input,
                        view.unwrap_or(content),
                        is_error || denied,
                        Duration::from_millis(elapsed_ms),
                    )));
                }
                EventKind::Run(RunEvent::StepSucceeded { step, .. }) => {
                    if let Some((op, start)) = starts.remove(&step.0) {
                        let elapsed_ms = event.ts_ms.saturating_sub(start).max(0) as u64;
                        entries.push(Entry::Tool(ToolEntry::historical_reduced(
                            op,
                            None,
                            Duration::from_millis(elapsed_ms),
                        )));
                    }
                }
                EventKind::Run(RunEvent::StepFailed { step, error }) => {
                    if let Some((op, start)) = starts.remove(&step.0) {
                        let elapsed_ms = event.ts_ms.saturating_sub(start).max(0) as u64;
                        entries.push(Entry::Tool(ToolEntry::historical_reduced(
                            op,
                            Some(error),
                            Duration::from_millis(elapsed_ms),
                        )));
                    }
                }
                EventKind::Observation(observation) => {
                    if let Some(entry) = historical_observation_entry(&observation) {
                        entries.push(entry);
                    }
                }
                EventKind::ModelChanged { model } => entries.push(Entry::Notice {
                    text: format!("model switched to {model}"),
                    sev: Sev::Info,
                }),
                EventKind::TurnEnded {
                    usage: Some(usage), ..
                } => turn_usage.push(usage),
                EventKind::CallUsage { model, usage } => call_usage.push((model, usage)),
                _ => {}
            }
        }

        self.entries = entries;
        self.mark_transcript_dirty();
        self.session_id = session_id.to_string();
        // The engine, not historical registry metadata, owns the model that will execute the next
        // turn. Keep the active model initialized by the caller across startup/resume projection.
        self.assistant_open = false;
        self.phase = Phase::Idle;
        self.turn_start = None;
        self.active_action_id = None;
        self.steps = 0;
        self.last_elapsed = None;
        self.session_picker = None;
        self.session_sel = 0;
        self.plan_phase = None;
        self.execute_rounds = 0;
        self.gather_mode = false;
        self.scroll = 0;
        self.follow = true;
        self.unread = 0;
        self.tokens_in = 0;
        self.tokens_out = 0;
        self.tokens_cache_read = 0;
        self.tokens_cache_write = 0;
        self.tokens_reasoning = 0;
        self.cost_usd = None;
        self.cost_unpriced = false;
        for usage in &turn_usage {
            self.tokens_in += usage.input_tokens;
            self.tokens_out += usage.output_tokens;
            self.tokens_cache_read += usage.cache_read_input_tokens;
            self.tokens_cache_write += usage.cache_creation_input_tokens;
            self.tokens_reasoning += usage.reasoning_tokens;
        }
        if let Some((_, pricing)) = &self.cost_model {
            if call_usage.is_empty() {
                if let Some(spec) = self.model_spec.as_deref() {
                    for usage in &turn_usage {
                        match pricing.cost(usage, spec) {
                            Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                            None if flux_core::is_metered_cloud_spec(spec) => {
                                self.cost_unpriced = true
                            }
                            None => {}
                        }
                    }
                }
            } else {
                for (model, usage) in &call_usage {
                    match pricing.cost(usage, model) {
                        Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                        None if flux_core::is_metered_cloud_spec(model) => {
                            self.cost_unpriced = true
                        }
                        None => {}
                    }
                }
            }
        }
        Ok(())
    }
}

/// Compose a one-row bar: `left` spans, padding, then right-side segments flush to `width`.
///
/// `right` is an ordered list of droppable segments: when the bar doesn't fit, segments are
/// dropped one at a time from the END of the list until it does (an empty right side is the
/// floor), so callers order them least-precious last.
fn bar_line(
    left: Vec<Span<'static>>,
    mut right: Vec<Vec<Span<'static>>>,
    width: u16,
) -> Line<'static> {
    let span_w = |spans: &[Span]| -> usize {
        spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum()
    };
    let right_w = |segs: &[Vec<Span>]| -> usize { segs.iter().map(|s| span_w(s)).sum() };
    // +2: one column of gap before the right side and one of margin after it.
    while !right.is_empty() && span_w(&left) + right_w(&right) + 2 > width as usize {
        right.pop();
    }
    let mut flat: Vec<Span<'static>> = right.into_iter().flatten().collect();
    if !flat.is_empty() {
        flat.push(Span::raw(" "));
    }
    let pad = (width as usize).saturating_sub(span_w(&left) + span_w(&flat));
    let mut spans = left;
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.extend(flat);
    Line::from(spans)
}

/// Truncate `s` to `max` display columns (approximated by char count), appending `…` when cut.
fn truncate(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let target = max.saturating_sub(1);
        let mut width = 0;
        let mut out = String::new();
        for ch in s.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + ch_width > target {
                break;
            }
            out.push(ch);
            width += ch_width;
        }
        out.push('…');
        out
    }
}

/// Hard-wrap styled lines to terminal display columns while preserving line/span styles. Markdown
/// is already word-wrapped; this closes the remaining long-user-input/tool/notice cases so viewport
/// offsets are exact and Ratatui never has to reflow rows hidden outside the viewport.
fn wrap_styled_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let max = width as usize;
    if max == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines {
        let Line {
            style: line_style,
            alignment,
            spans,
        } = line;
        if spans.is_empty() {
            out.push(Line {
                style: line_style,
                alignment,
                spans,
            });
            continue;
        }

        let mut row = Vec::new();
        let mut columns: usize = 0;
        for span in spans {
            let span_style = span.style;
            let mut chunk = String::new();
            for ch in span.content.chars() {
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if columns > 0 && columns.saturating_add(ch_width) > max {
                    if !chunk.is_empty() {
                        row.push(Span::styled(std::mem::take(&mut chunk), span_style));
                    }
                    out.push(Line {
                        style: line_style,
                        alignment,
                        spans: std::mem::take(&mut row),
                    });
                    columns = 0;
                }
                chunk.push(ch);
                columns = columns.saturating_add(ch_width);
            }
            if !chunk.is_empty() {
                row.push(Span::styled(chunk, span_style));
            }
        }
        out.push(Line {
            style: line_style,
            alignment,
            spans: row,
        });
    }
    out
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

fn fmt_tool_timing(outcome: &ToolOutcome) -> String {
    match outcome.approval_wait {
        Some(wait) => format!(
            "exec {} + approval {}",
            fmt_elapsed(outcome.elapsed),
            fmt_elapsed(wait)
        ),
        None => fmt_elapsed(outcome.elapsed),
    }
}

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
    run_with_options(
        agent,
        session_id,
        TuiRunOptions::new(auto_approve, model_spec),
    )
    .await
}

/// A throwaway [`AgentSink`] that discards every event — the TUI's D-183 resurrect-on-open step
/// runs before the terminal (and its live [`controller::ChannelSink`]) exist, and the resurrected
/// turn's persisted result is picked up anyway once `project_session`/`load_history` project the
/// session, so nothing needs to consume the stream here.
struct DiscardSink;
impl AgentSink for DiscardSink {}

/// D-183: the shared [`flux_flow::resurrect::resurrect_on_open`] step, reported to plain stderr —
/// the TUI has no `flux-cli`-style color chrome of its own, so every line (status or error) just
/// prints as-is. Mirrors the CLI's own reporter over the same shared step
/// (`flux-cli/src/execution.rs`'s `resurrect_on_open`) one-for-one, minus the coloring.
async fn resurrect_on_open(agent: &FlowEngine, session_id: &str) {
    let mut sink = DiscardSink;
    flux_flow::resurrect::resurrect_on_open(
        &agent.events,
        &agent.flow,
        &agent.executor,
        session_id,
        &agent.composites.active_for_session(session_id),
        &mut sink,
        |line| match line {
            flux_flow::resurrect::OnOpenLine::Info(msg) => eprintln!("{msg}"),
            flux_flow::resurrect::OnOpenLine::Error(msg) => eprintln!("{msg}"),
            other => eprintln!("resurrect: {other:?}"),
        },
    )
    .await;
}

/// Run the interactive TUI with optional surface capabilities such as live model resolution.
pub async fn run_with_options(
    agent: FlowEngine,
    session_id: String,
    options: TuiRunOptions,
) -> anyhow::Result<()> {
    use std::io::IsTerminal;

    let (tx, rx) = mpsc::unbounded_channel::<UiEvent>();
    // Only replace the approver with the modal when NOT auto-approving; if --yes was passed,
    // build_agent already installed AllowApprover and we must not clobber it.
    if !options.auto_approve {
        agent
            .executor
            .set_approver(Arc::new(ChannelApprover { tx: tx.clone() }));
    }
    let model = agent.model.clone();
    let events = agent.events.clone();

    let out = std::io::stdout();
    if !std::io::stdin().is_terminal() || !out.is_terminal() {
        anyhow::bail!("flux tui requires a real terminal on stdin and stdout");
    }

    // D-183: the TUI is a turn-entry point too — finish an interrupted turn from a prior crash
    // BEFORE the terminal takes over the screen (a plain stderr line the user can actually read,
    // same `FLUX_AUTO_RESURRECT=0` opt-out as the CLI's REPL and one-shot `flux run`) and before
    // `project_session`/`load_history` project the session below, so the resurrected turn's own
    // persisted messages show up in the transcript like any other turn's.
    resurrect_on_open(&agent, &session_id).await;

    let verbose = std::env::var("FLUX_VERBOSE").is_ok_and(|v| flag_on(&v));
    let mut state = ChatState::for_session(model, session_id.clone()).with_verbose(verbose);
    // C-104: resolve the configured theme for this terminal (NO_COLOR → mono, truecolor → RGB).
    let (theme_name, theme) = resolve_theme(options.theme.as_deref());
    state.theme = theme;
    state.theme_name = theme_name;
    // C-116: seed the header mode badges — auto-approve from the launch options, effort from
    // the engine's current setting (later `/effort` changes are mirrored by the handler).
    state.auto_approve = options.auto_approve;
    state.effort = agent.effort.map(|e| e.as_str().to_string());
    if let Some(spec) = options.model_spec.clone() {
        state = state.with_cost(spec, flux_credentials::load_pricing_table());
    }
    state.project_session(&events, &session_id)?;
    state.history = load_history(&events);
    let agent = Arc::new(tokio::sync::RwLock::new(agent));

    let (mut terminal, mut guard) = TerminalGuard::enter(out)?;
    // Decorative boot splash; any driver error just skips it. Runs before the
    // EventStream below exists, so its blocking `event::poll` has no competitor.
    let _ = splash::splash_intro(&mut terminal);
    let result = event_loop(
        &mut terminal,
        agent,
        &mut state,
        tx,
        rx,
        options.model_resolver,
    )
    .await;
    let restore = guard.restore(terminal.backend_mut());
    result.and(restore)
}

async fn event_loop(
    terminal: &mut Tui,
    agent: Arc<tokio::sync::RwLock<FlowEngine>>,
    state: &mut ChatState,
    tx: mpsc::UnboundedSender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<UiEvent>,
    model_resolver: Option<Arc<dyn ModelResolver>>,
) -> anyhow::Result<()> {
    use crossterm::event::{
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    };
    use futures_util::StreamExt as _;

    let mut cancel = CancellationToken::new();
    let mut pending_reply: Option<(String, oneshot::Sender<ApprovalChoice>)> = None;
    let mut approval_queue: VecDeque<PendingApproval> = VecDeque::new();
    // A message typed while a turn was running, started as soon as the turn finishes.
    let mut input = EventStream::new();
    let mut pending_ui: Option<UiEvent> = None;
    let mut exit_after_finish = false;

    loop {
        // Drain everything the running turn has produced.
        while let Some(ev) = pending_ui.take().or_else(|| rx.try_recv().ok()) {
            let Some(ev) = state.accept_ui_event(ev) else {
                continue;
            };
            match ev {
                UiEvent::Tagged { .. } => unreachable!("tagged events are unwrapped above"),
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
                UiEvent::Intent(intent) => state.push(Entry::Intent(intent)),
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
                UiEvent::ToolTiming { name, timing } => state.time_tool(&name, timing),
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
                    approval_queue.push_back((tool, subjects, reply));
                    show_next_approval(state, &mut pending_reply, &mut approval_queue);
                }
                UiEvent::Finished => {
                    if let Some((_tool, reply)) = pending_reply.take() {
                        let _ = reply.send(ApprovalChoice::Deny);
                    }
                    for (_tool, _subjects, reply) in approval_queue.drain(..) {
                        let _ = reply.send(ApprovalChoice::Deny);
                    }
                    state.approval = None;
                    state.end_stream();
                    state.phase = Phase::Idle;
                    state.last_elapsed = state.turn_start.map(|s| s.elapsed());
                    state.turn_start = None;
                    state.active_action_id = None;
                    // A queued message starts only after the prior task's Finished marker.
                    if !state.queue_open && state.queue_edit_index.is_none() {
                        if let Some(queued) = state.queue.pop_front() {
                            cancel = start_turn(&agent, &tx, state, queued);
                        }
                    }
                }
            }
        }

        if exit_after_finish && !state.running() {
            break;
        }

        terminal.draw(|f| render(f, state))?;

        // Idle blocks without a timer. While active, an 80ms tick is enough for spinner/elapsed
        // animation; streamed UI events wake the loop immediately and are batched above.
        let ev = tokio::select! {
            maybe = input.next() => match maybe {
                Some(Ok(ev)) => ev,
                Some(Err(error)) => return Err(error.into()),
                None => break,
            },
            maybe = rx.recv() => {
                pending_ui = maybe;
                if pending_ui.is_none() { break; }
                continue;
            }
            // 62 ms lands redraws on the 16 fps boundaries of the animated footer bar.
            _ = tokio::time::sleep(Duration::from_millis(spinners::FPS_MS)), if state.running() => continue,
        };
        match ev {
            Event::Resize(_, _) => continue,
            Event::Paste(text) => {
                state.input.insert_str(text);
                continue;
            }
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

                // Approval sheet: only explicit keys act; anything else is swallowed so a stray
                // keystroke can't silently deny (C-103).
                if let Some(view) = state.approval.as_mut() {
                    match approval_key(key.code) {
                        ApprovalAction::Ignore => {}
                        ApprovalAction::Scroll(delta) => {
                            view.scroll = view
                                .scroll
                                .saturating_add_signed(delta)
                                .min(view.subjects.len().saturating_sub(1));
                        }
                        action => {
                            if let Some((tool, reply)) = pending_reply.take() {
                                let choice = match action {
                                    ApprovalAction::Allow => ApprovalChoice::Allow,
                                    ApprovalAction::AllowAlways => {
                                        ApprovalChoice::AllowAlways(tool)
                                    }
                                    _ => ApprovalChoice::Deny,
                                };
                                let _ = reply.send(choice);
                            }
                            state.approval = None;
                            show_next_approval(state, &mut pending_reply, &mut approval_queue);
                        }
                    }
                    continue;
                }

                // C-110: help overlay — F1/Esc/q close, everything else is swallowed.
                if state.help_open {
                    if matches!(
                        key.code,
                        KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('q') | KeyCode::Enter
                    ) {
                        state.help_open = false;
                    }
                    continue;
                }
                if key.code == KeyCode::F(1) {
                    state.help_open = true;
                    continue;
                }

                if state.session_picker.is_some() {
                    match key.code {
                        KeyCode::Esc => state.session_picker = None,
                        KeyCode::Up => {
                            state.session_sel = state.session_sel.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            let last = state
                                .session_picker
                                .as_ref()
                                .map_or(0, |sessions| sessions.len().saturating_sub(1));
                            state.session_sel = (state.session_sel + 1).min(last);
                        }
                        KeyCode::Enter if state.running() => state.push(Entry::Notice {
                            text: "session switching waits for the active action to finish".into(),
                            sev: Sev::Warn,
                        }),
                        KeyCode::Enter => {
                            let selected = state.session_picker.as_ref().and_then(|sessions| {
                                sessions
                                    .get(state.session_sel.min(sessions.len().saturating_sub(1)))
                                    .map(|session| session.id.clone())
                            });
                            if let Some(session_id) = selected {
                                let engine = agent.read().await;
                                let active_model = engine.model.clone();
                                let events = engine.events.clone();
                                drop(engine);
                                match state.project_session(&events, &session_id) {
                                    Ok(()) => {
                                        state.model = active_model;
                                        state.history = load_history(&events);
                                    }
                                    Err(error) => state.push(Entry::Notice {
                                        text: error.to_string(),
                                        sev: Sev::Err,
                                    }),
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                if state.queue_open {
                    match key.code {
                        KeyCode::Esc => {
                            state.queue_open = false;
                            if !state.running() {
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_turn(&agent, &tx, state, next);
                                }
                            }
                        }
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                            state.queue_move(-1)
                        }
                        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                            state.queue_move(1)
                        }
                        KeyCode::Up => {
                            state.queue_sel = state.queue_sel.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            state.queue_sel =
                                (state.queue_sel + 1).min(state.queue.len().saturating_sub(1));
                        }
                        KeyCode::Delete | KeyCode::Backspace => {
                            state.queue_remove_selected();
                            if state.queue.is_empty() {
                                state.queue_open = false;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(edit) = state.queue_begin_edit() {
                                state.set_input(&edit);
                            }
                            state.queue_open = false;
                        }
                        _ => {}
                    }
                    continue;
                }

                // Paging the transcript works whether or not a turn is running. Home/End are left
                // for the input editor (line start/end); PgDn reattaches follow when it reaches the
                // bottom, so a dedicated jump-to-bottom isn't needed.
                match key.code {
                    KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.follow = true;
                        state.scroll = state.last_max_scroll.get();
                        state.unread = 0;
                        continue;
                    }
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

                // C-108: transcript-search mode — search keys win until Esc closes it.
                if state.search.is_some() {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let typing = state.search.as_ref().is_some_and(|s| s.typing);
                    match key.code {
                        KeyCode::Esc => {
                            state.search = None;
                        }
                        KeyCode::Enter => {
                            if let Some(s) = state.search.as_mut() {
                                s.typing = false;
                            }
                        }
                        KeyCode::Char('f') if ctrl => {
                            if let Some(s) = state.search.as_mut() {
                                s.typing = true;
                            }
                        }
                        KeyCode::Backspace if typing => {
                            if let Some(s) = state.search.as_mut() {
                                s.query.pop();
                            }
                            state.refresh_search_matches();
                            state.center_current_match();
                        }
                        KeyCode::Char(c) if typing && !ctrl => {
                            if let Some(s) = state.search.as_mut() {
                                s.query.push(c);
                            }
                            state.refresh_search_matches();
                            state.center_current_match();
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') if !typing => {
                            if state.search_matches_stale() {
                                state.refresh_search_matches();
                            } else if let Some(s) = state.search.as_mut() {
                                let len = s.matches.len();
                                if len > 0 {
                                    let forward = key.code == KeyCode::Char('n')
                                        && !key.modifiers.contains(KeyModifiers::SHIFT);
                                    s.current = if forward {
                                        (s.current + 1) % len
                                    } else {
                                        (s.current + len - 1) % len
                                    };
                                }
                            }
                            state.center_current_match();
                        }
                        _ => {}
                    }
                    continue;
                }

                // C-107: reverse-i-search mode — readline behavior, Esc restores the draft.
                if state.history_search.is_some() {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Esc => {
                            if let Some(hs) = state.history_search.take() {
                                state.set_input(&hs.draft);
                            }
                        }
                        KeyCode::Enter => {
                            // Keep whatever the search put in the composer; do not send.
                            state.history_search = None;
                        }
                        KeyCode::Char('r') if ctrl => {
                            let (query, before) = match state.history_search.as_ref() {
                                Some(hs) => (hs.query.clone(), hs.index),
                                None => continue,
                            };
                            if let Some(found) = rsearch(&state.history, &query, before) {
                                let text = state.history[found].clone();
                                state.set_input(&text);
                                if let Some(hs) = state.history_search.as_mut() {
                                    hs.index = Some(found);
                                }
                            }
                        }
                        KeyCode::Backspace | KeyCode::Char(_) => {
                            let edited = match key.code {
                                KeyCode::Backspace => state
                                    .history_search
                                    .as_mut()
                                    .is_some_and(|hs| hs.query.pop().is_some()),
                                KeyCode::Char(c) if !ctrl => {
                                    if let Some(hs) = state.history_search.as_mut() {
                                        hs.query.push(c);
                                        true
                                    } else {
                                        false
                                    }
                                }
                                _ => false,
                            };
                            if edited {
                                let query = state
                                    .history_search
                                    .as_ref()
                                    .map(|hs| hs.query.clone())
                                    .unwrap_or_default();
                                let found = rsearch(&state.history, &query, None);
                                if let Some(i) = found {
                                    let text = state.history[i].clone();
                                    state.set_input(&text);
                                }
                                if let Some(hs) = state.history_search.as_mut() {
                                    hs.index = found;
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Slash-command menu: when the input is a bare `/cmd` prefix with matches, ↑/↓ select,
                // Tab/Enter run the command, Esc dismisses; other keys fall through to edit/filter.
                if state.queue_edit_index.is_none() {
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
                                KeyCode::Tab => {
                                    let name = matches[state.slash_sel.min(matches.len() - 1)].name;
                                    let needs_arg = matches!(name, "model" | "resume");
                                    state.set_input(&format!(
                                        "/{name}{}",
                                        if needs_arg { " " } else { "" }
                                    ));
                                    state.slash_sel = 0;
                                    continue;
                                }
                                KeyCode::Enter => {
                                    let name = matches[state.slash_sel.min(matches.len() - 1)].name;
                                    state.set_input(&format!("/{name}"));
                                    state.slash_sel = 0;
                                }
                                _ => {}
                            }
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
                    KeyCode::Esc => {
                        if state.queue_cancel_edit() {
                            state.input = fresh_textarea();
                            if !running {
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_turn(&agent, &tx, state, next);
                                }
                            }
                        } else if state.slash_query().is_some() {
                            state.input = fresh_textarea();
                        }
                    }
                    KeyCode::Char('d') if ctrl && !running && state.input_blank() => break,
                    KeyCode::Up if cur_row == 0 && !ctrl && state.queue_edit_index.is_none() => {
                        state.history_prev()
                    }
                    KeyCode::Down
                        if cur_row == last_row && !ctrl && state.queue_edit_index.is_none() =>
                    {
                        state.history_next()
                    }
                    KeyCode::Char('c') if ctrl => {
                        if running {
                            // Cancel the running turn (input stays live so you can keep typing).
                            cancel.cancel();
                            state.push(Entry::Notice {
                                text: "(interrupting…)".into(),
                                sev: Sev::Info,
                            });
                        } else if state.input_blank() {
                            if state.queue_cancel_edit() {
                                state.input = fresh_textarea();
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_turn(&agent, &tx, state, next);
                                }
                            } else {
                                break; // empty line → quit
                            }
                        } else {
                            let cancelled_edit = state.queue_cancel_edit();
                            state.input = fresh_textarea(); // non-empty line → clear it
                            if cancelled_edit {
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_turn(&agent, &tx, state, next);
                                }
                            }
                        }
                    }
                    KeyCode::Char('e') if ctrl => state.toggle_details(),
                    // C-107: reverse history search. Deliberately shadows tui-textarea's redo
                    // (Ctrl-U undo remains) — readline muscle memory wins.
                    KeyCode::Char('r') if ctrl => {
                        state.history_search = Some(HistorySearch {
                            query: String::new(),
                            index: None,
                            draft: state.input.lines().join("\n"),
                        });
                    }
                    // C-108: transcript search. Shadows tui-textarea's forward-char (arrows
                    // remain) — same precedent as Ctrl-E.
                    KeyCode::Char('f') if ctrl => {
                        state.search = Some(TranscriptSearch {
                            typing: true,
                            ..Default::default()
                        });
                        state.refresh_search_matches();
                    }
                    // C-105: live mouse-capture toggle so terminal-native select/copy works.
                    // Wheel scroll is lost while off; PgUp/PgDn remain. (Ctrl-T is unbound in
                    // tui-textarea, so nothing is shadowed.)
                    KeyCode::Char('t') if ctrl => {
                        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
                        state.mouse_capture = !state.mouse_capture;
                        let _ = if state.mouse_capture {
                            crossterm::execute!(std::io::stdout(), EnableMouseCapture)
                        } else {
                            crossterm::execute!(std::io::stdout(), DisableMouseCapture)
                        };
                    }
                    _ if want_newline => state.input.insert_newline(),
                    KeyCode::Enter => {
                        if state.input_blank() {
                            let _ = state.take_input();
                            if state.queue_cancel_edit() && !running {
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_turn(&agent, &tx, state, next);
                                }
                            }
                            continue;
                        }
                        let text = state.take_input();
                        if state.queue_edit_index.is_none() && text.trim_start().starts_with('/') {
                            let wants_quit = handle_command(
                                &text,
                                &agent,
                                &tx,
                                state,
                                &mut cancel,
                                model_resolver.as_ref(),
                            )
                            .await?;
                            if wants_quit {
                                state.queue.clear();
                                if running {
                                    cancel.cancel();
                                    exit_after_finish = true;
                                } else {
                                    break;
                                }
                            }
                            continue;
                        }
                        if state.queue_commit_edit(text.clone()) {
                            if !running {
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_turn(&agent, &tx, state, next);
                                }
                            }
                        } else if running {
                            state.enqueue(text);
                        } else {
                            cancel = start_turn(&agent, &tx, state, text);
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

async fn handle_command(
    text: &str,
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    cancel: &mut CancellationToken,
    model_resolver: Option<&Arc<dyn ModelResolver>>,
) -> anyhow::Result<bool> {
    let command = text.trim().trim_start_matches('/');
    let (name, args) = command
        .split_once(char::is_whitespace)
        .map(|(name, args)| (name, args.trim()))
        .unwrap_or((command, ""));
    let busy = state.running();
    let read_only = command_is_read_only(name, args);
    if busy && !read_only && !matches!(name, "quit" | "exit") {
        state.push(Entry::Notice {
            text: format!("/{name} waits for an idle session — interrupt the current action first"),
            sev: Sev::Warn,
        });
        return Ok(false);
    }

    match name {
        "" | "help" => state.help_open = true,
        "quit" | "exit" => return Ok(true),
        "queue" => {
            if state.queue.is_empty() {
                state.push(Entry::Notice {
                    text: "queue is empty".into(),
                    sev: Sev::Info,
                });
            } else {
                state.queue_open = true;
                state.queue_sel = state.queue_sel.min(state.queue.len() - 1);
            }
        }
        "shell" => {
            let was_on = flux_runtime::shell_opt_in();
            flux_runtime::set_shell_opt_in(!was_on);
            state.push(Entry::Notice {
                text: format!(
                    "shell (bash) {} from the next turn",
                    if was_on { "off" } else { "on" }
                ),
                sev: Sev::Info,
            });
        }
        "tools" => {
            let engine = agent.read().await;
            let mut names = engine.executor.registry().names();
            names.sort();
            state.push(Entry::Notice {
                text: format!("tools ({}): {}", names.len(), names.join(", ")),
                sev: Sev::Info,
            });
        }
        "evidence" => {
            let engine = agent.read().await;
            match engine.events.observations(&state.session_id) {
                Ok(observations) if observations.is_empty() => state.push(Entry::Notice {
                    text: "no durable evidence in this session".into(),
                    sev: Sev::Info,
                }),
                Ok(observations) => {
                    let lines = observations
                        .iter()
                        .rev()
                        .take(80)
                        .rev()
                        .map(|o| format!("{}  {}", o.kind, o.data))
                        .collect::<Vec<_>>()
                        .join("\n");
                    state.push(Entry::Notice {
                        text: lines,
                        sev: Sev::Info,
                    });
                }
                Err(error) => state.push(Entry::Notice {
                    text: format!("evidence: {error}"),
                    sev: Sev::Err,
                }),
            }
        }
        "session" => state.push(Entry::Notice {
            text: format!("session {} · model {}", state.session_id, state.model),
            sev: Sev::Info,
        }),
        "sessions" if args == "--prune" => {
            let engine = agent.read().await;
            match engine
                .events
                .prune_empty_excluding(std::slice::from_ref(&state.session_id))
            {
                Ok(count) => state.push(Entry::Notice {
                    text: format!(
                        "pruned {count} empty session{}",
                        if count == 1 { "" } else { "s" }
                    ),
                    sev: Sev::Info,
                }),
                Err(error) => state.push(Entry::Notice {
                    text: format!("prune sessions: {error}"),
                    sev: Sev::Err,
                }),
            }
        }
        "sessions" => {
            let engine = agent.read().await;
            match engine.events.list(30) {
                Ok(sessions) if sessions.is_empty() => state.push(Entry::Notice {
                    text: "no sessions yet".into(),
                    sev: Sev::Info,
                }),
                Ok(sessions) => {
                    state.session_sel = sessions
                        .iter()
                        .position(|session| session.id == state.session_id)
                        .unwrap_or(0);
                    state.session_picker = Some(sessions);
                    state.queue_open = false;
                }
                Err(error) => state.push(Entry::Notice {
                    text: format!("sessions: {error}"),
                    sev: Sev::Err,
                }),
            }
        }
        "resume" => {
            if args.is_empty() {
                state.push(Entry::Notice {
                    text: "usage: /resume <session_id>".into(),
                    sev: Sev::Warn,
                });
            } else {
                let engine = agent.read().await;
                let active_model = engine.model.clone();
                let events = engine.events.clone();
                drop(engine);
                match state.project_session(&events, args) {
                    Ok(()) => {
                        state.model = active_model;
                        state.history = load_history(&events);
                    }
                    Err(error) => state.push(Entry::Notice {
                        text: error.to_string(),
                        sev: Sev::Err,
                    }),
                }
            }
        }
        "new" | "clear" => {
            let engine = agent.read().await;
            let active_model = engine.model.clone();
            let events = engine.events.clone();
            match events.create_session(&active_model) {
                Ok(session) => {
                    drop(engine);
                    match state.project_session(&events, &session) {
                        Ok(()) => {
                            state.model = active_model;
                            state.history = load_history(&events);
                        }
                        Err(error) => state.push(Entry::Notice {
                            text: format!("new session: {error}"),
                            sev: Sev::Err,
                        }),
                    }
                }
                Err(error) => state.push(Entry::Notice {
                    text: format!("new session: {error}"),
                    sev: Sev::Err,
                }),
            }
        }
        "compact" => {
            *cancel = start_compaction(agent, tx, state);
        }
        "theme" if args.is_empty() => state.push(Entry::Notice {
            text: format!(
                "themes: {} · current: {}",
                Theme::names().join(" "),
                state.theme_name
            ),
            sev: Sev::Info,
        }),
        "theme" => match Theme::by_name(args, terminal_truecolor(), no_color()) {
            Some(theme) => {
                state.theme = theme;
                state.theme_name = args.to_string();
                state.mark_transcript_dirty();
                let persisted = flux_runtime::metadata::persist_user_theme(args);
                state.push(Entry::Notice {
                    text: match persisted {
                        Ok(()) => format!("theme: {args} (saved to ~/.flux/config.toml)"),
                        Err(error) => format!("theme: {args} (not saved: {error})"),
                    },
                    sev: Sev::Info,
                });
            }
            None => state.push(Entry::Notice {
                text: format!(
                    "unknown theme `{args}` — themes: {}",
                    Theme::names().join(" ")
                ),
                sev: Sev::Warn,
            }),
        },
        "model" if args.is_empty() => state.push(Entry::Notice {
            text: format!(
                "model: {}",
                state.model_spec.as_deref().unwrap_or(&state.model)
            ),
            sev: Sev::Info,
        }),
        "model" => {
            let Some(resolver) = model_resolver.cloned() else {
                state.push(Entry::Notice {
                    text: "model switching is unavailable on this embedding".into(),
                    sev: Sev::Warn,
                });
                return Ok(false);
            };
            let spec = args.to_string();
            let resolved = match tokio::task::spawn_blocking(move || resolver.resolve(&spec)).await
            {
                Ok(Ok(resolved)) => resolved,
                Ok(Err(error)) => {
                    state.push(Entry::Notice {
                        text: format!("model: {error}"),
                        sev: Sev::Err,
                    });
                    return Ok(false);
                }
                Err(error) => {
                    state.push(Entry::Notice {
                        text: format!("model resolver crashed: {error}"),
                        sev: Sev::Err,
                    });
                    return Ok(false);
                }
            };
            let mut engine = agent.write().await;
            if let Err(error) = engine.switch_model_for_session(
                &state.session_id,
                resolved.provider,
                resolved.wire_model.clone(),
            ) {
                state.push(Entry::Notice {
                    text: format!("switch model: {error}"),
                    sev: Sev::Err,
                });
                return Ok(false);
            }
            state.model = resolved.wire_model;
            state.model_spec = Some(resolved.model_spec.clone());
            state.cost_model = Some((
                resolved.model_spec.clone(),
                flux_credentials::load_pricing_table(),
            ));
            state.push(Entry::Notice {
                text: format!("switched to {}", resolved.model_spec),
                sev: Sev::Info,
            });
        }
        "effort" if args.is_empty() => {
            let engine = agent.read().await;
            let current = engine
                .effort
                .map(|e| e.as_str())
                .unwrap_or("(provider default)");
            state.push(Entry::Notice {
                text: format!("effort: {current} · usage: /effort <low|medium|high|xhigh|max|off>"),
                sev: Sev::Info,
            });
        }
        "effort" => {
            let effort = match args.to_ascii_lowercase().as_str() {
                "off" | "none" | "default" => Some(None),
                "low" => Some(Some(flux_provider::Effort::Low)),
                "medium" => Some(Some(flux_provider::Effort::Medium)),
                "high" => Some(Some(flux_provider::Effort::High)),
                "xhigh" => Some(Some(flux_provider::Effort::Xhigh)),
                "max" => Some(Some(flux_provider::Effort::Max)),
                _ => None,
            };
            match effort {
                Some(effort) => {
                    agent.write().await.set_effort(effort);
                    // C-116: mirror into ChatState so the sync render path can badge it.
                    state.effort = effort.map(|e| e.as_str().to_string());
                    let shown = effort.map(|e| e.as_str()).unwrap_or("(provider default)");
                    state.push(Entry::Notice {
                        text: format!("effort: {shown} — takes effect from the next turn"),
                        sev: Sev::Info,
                    });
                }
                None => state.push(Entry::Notice {
                    text: format!(
                        "effort: expected low, medium, high, xhigh, max, or off; got {args:?}"
                    ),
                    sev: Sev::Err,
                }),
            }
        }
        other => state.push(Entry::Notice {
            text: format!("unknown command /{other} · try /help"),
            sev: Sev::Warn,
        }),
    }
    Ok(false)
}

fn command_is_read_only(name: &str, args: &str) -> bool {
    matches!(
        name,
        "help" | "tools" | "evidence" | "session" | "queue" | "theme"
    ) || (name == "sessions" && args != "--prune")
        || (name == "effort" && args.is_empty())
}

fn start_compaction(
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
) -> CancellationToken {
    let action_id = state.begin_action();
    state.phase = Phase::Thinking;
    state.turn_start = Some(Instant::now());
    state.follow = true;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_agent = agent.clone();
    let task_tx = tx.clone();
    let session = state.session_id.clone();
    tokio::spawn(async move {
        let inner_tx = task_tx.clone();
        let run = tokio::spawn(async move {
            let mut sink = ChannelSink {
                tx: inner_tx,
                action_id,
            };
            let engine = task_agent.read().await;
            engine
                .maybe_compact(&session, &mut sink, &task_cancel)
                .await
        });
        let notice = match run.await {
            Ok(Ok(())) => ("compaction check complete".to_string(), Sev::Info),
            Ok(Err(error)) => (format!("compact: {error}"), Sev::Err),
            Err(join) => (format!("compaction crashed: {join}"), Sev::Err),
        };
        send_action_event(
            &task_tx,
            action_id,
            UiEvent::Notice {
                text: notice.0,
                sev: notice.1,
            },
        );
        send_action_event(&task_tx, action_id, UiEvent::Finished);
    });
    cancel
}

/// Push `input` as a user message and spawn the agent turn that streams back into the transcript.
/// Returns the turn's cancellation token (Ctrl-C cancels it).
fn start_turn(
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    input: String,
) -> CancellationToken {
    let action_id = state.begin_action();
    state.follow = true;
    state.unread = 0;
    state.record_history(&input);
    state.push_user(input.clone());
    state.phase = Phase::Thinking;
    state.turn_start = Some(Instant::now());
    state.steps = 0;
    state.plan_phase = None;
    state.execute_rounds = 0;
    state.gather_mode = false;

    let cancel = CancellationToken::new();
    let task_agent = agent.clone();
    let task_sid = state.session_id.clone();
    let task_tx = tx.clone();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        // Run the turn on an inner task so a *panic* inside the engine is caught (its `JoinError`
        // carries `is_panic`) and surfaced — otherwise a panicked turn would die silently: no
        // output, no `Finished`, and the spinner spinning forever.
        let inner_tx = task_tx.clone();
        let run = tokio::spawn(async move {
            let mut sink = ChannelSink {
                tx: inner_tx,
                action_id,
            };
            let agent = task_agent.read().await;
            agent
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
    if state.follow {
        state.unread = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
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

    fn untag(event: UiEvent) -> UiEvent {
        match event {
            UiEvent::Tagged { event, .. } => *event,
            event => event,
        }
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
    fn composer_is_background_only_without_border_or_padding() {
        let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
        let mut state = ChatState::new("mock".into());
        state.input.insert_str("draft");
        terminal.draw(|f| render(f, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        let symbols: String = buffer.content.iter().map(|c| c.symbol()).collect();
        assert!(
            !symbols.contains('┌')
                && !symbols.contains('┐')
                && !symbols.contains('└')
                && !symbols.contains('┘'),
            "permanent transcript/composer boxes must be gone: {symbols}"
        );
        let draft = buffer
            .content
            .iter()
            .find(|c| c.symbol() == "d")
            .expect("draft cell");
        assert_eq!(draft.bg, state.theme.composer_bg);
        assert_eq!(buffer.cell((0, 8)).expect("composer origin").symbol(), "d");
        assert!((0..48).all(|x| {
            buffer
                .cell((x, 8))
                .is_some_and(|cell| cell.bg == state.theme.composer_bg)
        }));
    }

    #[test]
    fn responsive_layout_preserves_composer_at_36x10() {
        let mut terminal = Terminal::new(TestBackend::new(36, 10)).unwrap();
        let mut state = ChatState::new("mock".into());
        state.push_user("a narrow transcript");
        state.input.insert_str("narrow draft");
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("narrow transcript"));
        assert!(content.contains("narrow draft"));
        assert!(!content.contains('┌'));
    }

    #[test]
    fn terminal_too_small_has_stable_fallback() {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        let state = ChatState::new("mock".into());
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("terminal too small"));
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

        state.approval = Some(ApprovalView {
            tool: "bash".into(),
            subjects: vec!["ls".into()],
            scroll: 0,
        });
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
    fn tool_card_separates_approval_wait_from_execution() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::Tool(ToolEntry::new(
            "write".into(),
            serde_json::json!({"path": "README.md"}),
        )));
        state.time_tool(
            "write",
            flux_core::OperationTiming {
                total_us: 30_005_000,
                approval_wait_us: Some(30_000_000),
                execution_us: Some(5_000),
            },
        );
        state.finish_tool("write", "wrote README.md".into(), false);
        let Entry::Tool(tool) = &state.entries[0] else {
            panic!("expected tool entry")
        };
        assert_eq!(
            fmt_tool_timing(tool.result.as_ref().unwrap()),
            "exec 5ms + approval 30.0s"
        );
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
    fn queued_messages_are_fifo_editable_reorderable_and_never_overwritten() {
        let mut state = ChatState::new("mock".into());
        state.enqueue("first".into());
        state.enqueue("second".into());
        state.enqueue("third".into());
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            ["first", "second", "third"]
        );

        state.queue_sel = 1;
        state.queue_move(-1);
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            ["second", "first", "third"]
        );
        assert_eq!(state.queue_remove_selected().as_deref(), Some("second"));
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            ["first", "third"]
        );
    }

    #[test]
    fn editing_a_queued_message_preserves_its_fifo_position() {
        let mut state = ChatState::new("mock".into());
        state.enqueue("first".into());
        state.enqueue("second".into());
        state.enqueue("third".into());
        state.queue_sel = 1;

        assert_eq!(state.queue_begin_edit().as_deref(), Some("second"));
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            ["first", "second", "third"],
            "beginning an edit must not remove or reorder the item"
        );
        assert!(state.queue_commit_edit("second refined".into()));
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            ["first", "second refined", "third"]
        );
        assert_eq!(state.queue.pop_front().as_deref(), Some("first"));
        assert_eq!(state.queue.pop_front().as_deref(), Some("second refined"));
    }

    #[test]
    fn stale_action_events_cannot_enter_the_next_turn() {
        let mut state = ChatState::new("mock".into());
        let old = state.begin_action();
        let current = state.begin_action();
        assert!(state
            .accept_ui_event(UiEvent::Tagged {
                action_id: old,
                event: Box::new(UiEvent::Text("stale".into())),
            })
            .is_none());
        assert!(matches!(
            state.accept_ui_event(UiEvent::Tagged {
                action_id: current,
                event: Box::new(UiEvent::Text("current".into())),
            }),
            Some(UiEvent::Text(text)) if text == "current"
        ));
    }

    #[tokio::test]
    async fn dropping_an_approval_sheet_is_deny_by_default() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let approver = ChannelApprover { tx };
        let request = tokio::spawn(async move {
            approver
                .request("write", &["README.md".into()], &IntentSet::default())
                .await
        });
        match rx.recv().await.expect("approval request") {
            UiEvent::Approval { reply, .. } => drop(reply),
            _ => panic!("expected approval event"),
        }
        assert!(matches!(request.await.unwrap(), ApprovalChoice::Deny));
    }

    /// C-105: while mouse capture is off, the footer indicates it (warn style) so the state is
    /// never invisible; the metrics segment drops before the indicator on narrow bars.
    #[test]
    fn mouse_off_footer_indicator() {
        let mut state = ChatState::new("mock".into());
        state.mouse_capture = false;
        state.steps = 3;
        state.last_elapsed = Some(Duration::from_secs(2));
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("mouse off"), "{content}");
        assert!(content.contains("Ctrl-T re-enable"), "{content}");
        assert!(content.contains("3 steps"), "{content}");

        // Narrow: the idle-hint indicator survives (it owns the left side).
        let mut narrow = Terminal::new(TestBackend::new(46, 10)).unwrap();
        narrow.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&narrow).contains("mouse off"));

        // While running, the state rides the right side as a short segment.
        state.phase = Phase::Thinking;
        state.turn_start = Some(Instant::now());
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("mouse off (Ctrl-T)"));
        state.phase = Phase::Idle;
        state.turn_start = None;

        state.mouse_capture = true;
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("mouse off"));
    }

    /// C-106: detaching from follow mode shows a scrollbar on the transcript's right column and
    /// a percent segment in the footer; follow mode shows neither.
    #[test]
    fn scroll_indicator_appears_only_while_detached() {
        let mut state = ChatState::new("mock".into());
        for i in 0..40 {
            state.push_user(format!("message number {i}"));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        // Following: no indicator.
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains('%'));

        scroll_up(&mut state, 5);
        assert!(!state.follow);
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("⤓") && content.contains('%'), "{content}");
        let buffer = terminal.backend().buffer();
        let transcript_rows = 1..(12 - 2);
        let bar_col = 59;
        let has_bar_glyph = transcript_rows
            .map(|y| {
                buffer
                    .cell((bar_col, y))
                    .expect("cell")
                    .symbol()
                    .to_string()
            })
            .any(|s| s != " ");
        assert!(
            has_bar_glyph,
            "scrollbar glyphs expected in the last column"
        );

        // Reattach: indicator gone.
        state.follow = true;
        state.scroll = state.last_max_scroll.get();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains('%'));
    }

    /// C-110: the help overlay lists keys and every slash command from the COMMANDS table, and
    /// only renders while open.
    #[test]
    fn help_overlay_lists_keys_and_all_commands() {
        let mut state = ChatState::new("mock".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("help · Esc close"));

        state.help_open = true;
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("help · Esc close"), "{content}");
        assert!(content.contains("Ctrl-J"), "{content}");
        assert!(content.contains("Ctrl-R"), "{content}");
        assert!(content.contains("Ctrl-T"), "{content}");
        for c in COMMANDS {
            assert!(
                content.contains(&format!("/{}", c.name)),
                "missing /{}",
                c.name
            );
        }

        state.help_open = false;
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("help · Esc close"));
    }

    /// C-107: `rsearch` — backwards, case-insensitive, stepping strictly older via `before`.
    #[test]
    fn rsearch_steps_backwards_case_insensitive() {
        let history: Vec<String> = ["fix the Bug", "run tests", "fix bugs again", "deploy"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(rsearch(&history, "bug", None), Some(2));
        assert_eq!(rsearch(&history, "bug", Some(2)), Some(0));
        assert_eq!(rsearch(&history, "bug", Some(0)), None);
        assert_eq!(rsearch(&history, "BUG", None), Some(2));
        assert_eq!(rsearch(&history, "nope", None), None);
        assert_eq!(rsearch(&history, "", None), None);
    }

    /// C-107: the footer takes over with the reverse-i-search line while active, and the matched
    /// entry lands in the composer.
    #[test]
    fn history_search_footer_takeover_renders() {
        let mut state = ChatState::new("mock".into());
        state.history = vec!["fix the bug".into(), "run tests".into()];
        state.history_search = Some(HistorySearch {
            query: "bug".into(),
            index: Some(0),
            draft: String::new(),
        });
        state.set_input("fix the bug");
        let mut terminal = Terminal::new(TestBackend::new(70, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("(reverse-i-search) 'bug':"), "{content}");
        assert!(content.contains("fix the bug"), "{content}");
    }

    /// C-108: `find_match_rows` matches per wrapped row, case-insensitive.
    #[test]
    fn find_match_rows_matches_flattened_rows() {
        let lines = vec![
            Line::from(vec![Span::raw("alpha "), Span::raw("Beta")]),
            Line::from("gamma"),
            Line::from("beta again"),
        ];
        assert_eq!(find_match_rows(&lines, "beta"), vec![0, 2]);
        assert_eq!(find_match_rows(&lines, "ALPHA"), vec![0]);
        assert_eq!(find_match_rows(&lines, "delta"), Vec::<u16>::new());
        assert_eq!(find_match_rows(&lines, ""), Vec::<u16>::new());
        // A match crossing a span boundary within one row IS found (rows are flattened).
        assert_eq!(find_match_rows(&lines, "alpha b"), vec![0]);
    }

    /// C-108: an active search highlights visible matches (REVERSED), centers the current match,
    /// and the footer shows the counter — all without touching the cached layout.
    #[test]
    fn transcript_search_highlights_and_centers() {
        let mut state = ChatState::new("mock".into());
        for i in 0..30 {
            state.push_user(format!("filler {i}"));
        }
        state.push_user("the needle message");
        for i in 0..30 {
            state.push_user(format!("padding {i}"));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap(); // build the layout
        let revision_before = state.transcript_revision;

        state.search = Some(TranscriptSearch {
            query: "needle".into(),
            typing: false,
            ..Default::default()
        });
        state.refresh_search_matches();
        assert_eq!(
            state.search.as_ref().unwrap().matches.len(),
            1,
            "one matching row expected"
        );
        state.center_current_match();
        assert!(!state.follow);

        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("needle"), "{content}");
        assert!(content.contains("1/1"), "{content}");
        assert!(content.contains("n/N next/prev"), "{content}");

        // The match cells carry REVERSED; the cache revision is untouched.
        let buffer = terminal.backend().buffer();
        let reversed = buffer
            .content
            .iter()
            .any(|c| c.modifier.contains(Modifier::REVERSED) && c.symbol() == "n");
        assert!(reversed, "match cells must be REVERSED");
        assert_eq!(state.transcript_revision, revision_before);

        // Esc-equivalent: clearing the search removes highlights.
        state.search = None;
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("n/N next/prev"));
    }

    /// C-109: running tool cards get a live spinner + elapsed badge patched into the viewport
    /// per frame — WITHOUT invalidating the cached transcript layout — and stop animating the
    /// moment the result lands.
    #[test]
    fn running_tool_card_animates_without_cache_invalidation() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            "bash".into(),
            serde_json::json!({"command": "sleep 5"}),
        )));
        if let Some(Entry::Tool(tool)) = state.entries.last_mut() {
            tool.started = Instant::now() - Duration::from_secs(2);
        }
        let mut terminal = Terminal::new(TestBackend::new(72, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("running · 2"), "{content}");
        assert!(
            SPINNER.iter().any(|frame| content.contains(frame)),
            "animated glyph expected: {content}"
        );
        assert!(!content.contains(RUNNING_BADGE), "static badge is patched");

        // A second frame re-patches from the SAME cached layout: revision untouched.
        let revision = state.transcript_revision;
        terminal.draw(|f| render(f, &state)).unwrap();
        assert_eq!(state.transcript_revision, revision);
        assert!(
            state
                .transcript_layout
                .borrow()
                .as_ref()
                .is_some_and(|l| l.revision == revision && !l.running_rows.is_empty()),
            "cached layout must survive animation frames"
        );

        // Result lands → done badge, no more patching.
        state.finish_tool("bash", "ok".into(), false);
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("✓"), "{content}");
        assert!(!content.contains("running ·"), "{content}");
        assert!(state
            .transcript_layout
            .borrow()
            .as_ref()
            .is_some_and(|l| l.running_rows.is_empty()));
    }

    /// C-104: `Theme::by_name` — NO_COLOR forces mono, truecolor picks the RGB tuning, unknown
    /// names are rejected.
    #[test]
    fn theme_by_name_resolves_variants() {
        use ratatui::style::Color;
        assert!(matches!(
            Theme::by_name("dark", false, false),
            Some(t) if t.accent == Theme::DARK.accent
        ));
        assert!(matches!(
            Theme::by_name("dark", true, false),
            Some(t) if matches!(t.accent, Color::Rgb(..))
        ));
        assert!(matches!(
            Theme::by_name("light", true, false),
            Some(t) if matches!(t.base_bg, Color::Rgb(..))
        ));
        // NO_COLOR wins over everything, including truecolor.
        assert!(matches!(
            Theme::by_name("light", true, true),
            Some(t) if t.accent == Color::Reset && t.base_bg == Color::Reset
        ));
        assert!(Theme::by_name("solarized", true, false).is_none());
        assert!(Theme::by_name("solarized", true, true).is_none());
    }

    /// C-104: switching the theme restyles the screen — a known cell's colors change and the
    /// light theme paints the root background.
    #[test]
    fn theme_switch_restyles_screen() {
        let mut state = ChatState::new("mock".into());
        state.input.insert_str("draft");
        let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let dark_bg = terminal.backend().buffer().cell((0, 0)).expect("cell").bg;
        assert_eq!(dark_bg, Theme::DARK.base_bg);

        state.theme = Theme::LIGHT;
        state.theme_name = "light".into();
        state.mark_transcript_dirty();
        terminal.draw(|f| render(f, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, 0)).expect("cell").bg, Theme::LIGHT.base_bg);
        let draft = buffer
            .content
            .iter()
            .find(|c| c.symbol() == "d")
            .expect("draft cell");
        assert_eq!(draft.bg, Theme::LIGHT.composer_bg);
    }

    /// C-103: only explicit keys act on the approval sheet — a stray keystroke is ignored (the
    /// sheet stays, the reply is not consumed) instead of silently denying.
    #[test]
    fn approval_key_only_acts_on_explicit_keys() {
        assert_eq!(approval_key(KeyCode::Char('y')), ApprovalAction::Allow);
        assert_eq!(approval_key(KeyCode::Char('Y')), ApprovalAction::Allow);
        assert_eq!(
            approval_key(KeyCode::Char('a')),
            ApprovalAction::AllowAlways
        );
        assert_eq!(
            approval_key(KeyCode::Char('A')),
            ApprovalAction::AllowAlways
        );
        assert_eq!(approval_key(KeyCode::Char('n')), ApprovalAction::Deny);
        assert_eq!(approval_key(KeyCode::Char('N')), ApprovalAction::Deny);
        assert_eq!(approval_key(KeyCode::Esc), ApprovalAction::Deny);
        assert_eq!(approval_key(KeyCode::Up), ApprovalAction::Scroll(-1));
        assert_eq!(approval_key(KeyCode::Down), ApprovalAction::Scroll(1));
        // Everything else — including the keys that used to deny — is ignored.
        for code in [
            KeyCode::Char('x'),
            KeyCode::Char('q'),
            KeyCode::Char(' '),
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Backspace,
            KeyCode::PageDown,
        ] {
            assert_eq!(approval_key(code), ApprovalAction::Ignore, "{code:?}");
        }
    }

    /// C-103: the redesigned sheet renders subjects verbatim (no Debug `["…"]`), windows long
    /// lists with a `+N more` marker, and colors its key hints.
    #[test]
    fn approval_sheet_windows_subjects_and_styles_hints() {
        let mut state = ChatState::new("mock".into());
        let subjects: Vec<String> = (0..10).map(|i| format!("path/to/file-{i}.rs")).collect();
        state.approval = Some(ApprovalView {
            tool: "write".into(),
            subjects,
            scroll: 0,
        });
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("approve"), "{content}");
        assert!(content.contains("write"), "{content}");
        assert!(content.contains("path/to/file-0.rs"), "{content}");
        assert!(!content.contains("[\""), "no Debug formatting: {content}");
        assert!(content.contains("more"), "windowed list marker: {content}");
        assert!(content.contains('┌'), "bordered sheet: {content}");
        assert!(
            content.contains("[y]") && content.contains("[n/Esc]"),
            "{content}"
        );

        // Hint keys carry their semantic colors.
        let buffer = terminal.backend().buffer();
        let mut found_ok = false;
        for cell in &buffer.content {
            if cell.symbol() == "y" && cell.fg == state.theme.ok {
                found_ok = true;
            }
        }
        assert!(found_ok, "[y] hint must use the ok color");
    }

    /// C-103: a stray key while the sheet is open must NOT resolve the pending reply.
    #[tokio::test]
    async fn stray_key_does_not_resolve_approval() {
        let mut state = ChatState::new("mock".into());
        let mut current = None;
        let mut queued = VecDeque::new();
        let (reply, mut reply_rx) = oneshot::channel();
        queued.push_back(("bash".into(), vec!["rm -rf tmp".into()], reply));
        show_next_approval(&mut state, &mut current, &mut queued);
        assert!(state.approval.is_some());

        // Simulate the event-loop branch for a stray key: Ignore → nothing happens.
        assert_eq!(approval_key(KeyCode::Char('x')), ApprovalAction::Ignore);
        assert!(
            reply_rx.try_recv().is_err(),
            "stray key must not consume the reply"
        );
        assert!(state.approval.is_some(), "sheet must stay open");

        // An explicit deny resolves it.
        let (_tool, reply) = current.take().unwrap();
        let _ = reply.send(ApprovalChoice::Deny);
        assert!(matches!(reply_rx.try_recv(), Ok(ApprovalChoice::Deny)));
    }

    #[test]
    fn concurrent_approvals_are_presented_fifo() {
        let mut state = ChatState::new("mock".into());
        let mut current = None;
        let mut queued = VecDeque::new();
        let (first, _first_rx) = oneshot::channel();
        let (second, _second_rx) = oneshot::channel();
        queued.push_back(("write".into(), vec!["a".into()], first));
        queued.push_back(("bash".into(), vec!["b".into()], second));

        show_next_approval(&mut state, &mut current, &mut queued);
        assert!(matches!(current.as_ref(), Some((tool, _)) if tool == "write"));
        assert!(state
            .approval
            .as_ref()
            .is_some_and(|view| view.tool == "write" && view.subjects == ["a"]));
        current.take();
        state.approval = None;
        show_next_approval(&mut state, &mut current, &mut queued);
        assert!(matches!(current.as_ref(), Some((tool, _)) if tool == "bash"));
        assert!(state
            .approval
            .as_ref()
            .is_some_and(|view| view.tool == "bash" && view.subjects == ["b"]));
    }

    #[test]
    fn resumed_session_projects_full_durable_activity() {
        use flux_events::{EventStore, NewEvent, PlanAttempt};
        use flux_flow::ast::{RunEvent, StepId};

        let events = EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        events
            .record_message(&sid, &flux_core::Message::user_text("inspect it"))
            .unwrap();
        let turn = events.begin_turn(&sid, "inspect it", "mock").unwrap();
        events
            .record_plan_attempt(
                &sid,
                turn,
                PlanAttempt {
                    step: 1,
                    outcome: "accepted".into(),
                    plan_text: Some("flow\n└─ read(\"README.md\")".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let step = StepId("step_read_fixture".into());
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepStarted {
                    step: step.clone(),
                    op: "read".into(),
                    input_hash: "h".into(),
                }),
            )
            .unwrap();
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::OpRecorded {
                    seq: 0,
                    step,
                    op: "read".into(),
                    input_hash: "h".into(),
                    input_hash_redacted: None,
                    input_view: Some(r#"{"path":"README.md"}"#.into()),
                    input_view_truncated: false,
                    content: "hello".into(),
                    view: None,
                    is_error: false,
                    denied: false,
                    redacted: false,
                    truncated: false,
                }),
            )
            .unwrap();
        events
            .record_message(&sid, &flux_core::Message::assistant_text("done"))
            .unwrap();

        let mut state = ChatState::for_session("mock".into(), String::new());
        state.project_session(&events, &sid).unwrap();
        state.expand_tools = true;
        let text = state
            .transcript_lines(80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("inspect it"));
        assert!(text.contains("README.md"));
        assert!(text.contains("hello"));
        assert!(text.contains("done"));
        assert!(text.contains("plan"));
    }

    #[test]
    fn resumed_session_projects_reduced_tool_cards_without_cassette_cells() {
        use flux_events::{EventStore, NewEvent};
        use flux_flow::ast::{RunEvent, StepId, ValueId};

        let events = EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        let read = StepId("step_read_without_cell".into());
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepStarted {
                    step: read.clone(),
                    op: "read".into(),
                    input_hash: "read-hash".into(),
                }),
            )
            .unwrap();
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepSucceeded {
                    step: read,
                    output: ValueId("v_read".into()),
                }),
            )
            .unwrap();

        let write = StepId("step_write_without_cell".into());
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepStarted {
                    step: write.clone(),
                    op: "write".into(),
                    input_hash: "write-hash".into(),
                }),
            )
            .unwrap();
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepFailed {
                    step: write,
                    error: "disk full".into(),
                }),
            )
            .unwrap();

        let machinery = StepId("step_observe_without_cell".into());
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepStarted {
                    step: machinery.clone(),
                    op: "observe".into(),
                    input_hash: "observe-hash".into(),
                }),
            )
            .unwrap();
        events
            .append(
                &sid,
                NewEvent::run(RunEvent::StepSucceeded {
                    step: machinery,
                    output: ValueId("v_observe".into()),
                }),
            )
            .unwrap();

        let mut state = ChatState::new("mock".into());
        state.project_session(&events, &sid).unwrap();
        state.expand_tools = true;
        let text = state
            .transcript_lines(80)
            .into_iter()
            .flat_map(|line| line.spans.into_iter())
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("read"));
        assert!(text.contains("completed"));
        assert!(text.contains("write"));
        assert!(text.contains("disk full"));
        assert!(!text.contains("observe"), "loop machinery stays hidden");
    }

    #[test]
    fn compaction_snapshot_does_not_duplicate_visible_messages() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        let old = flux_core::Message::user_text("old request");
        events.record_message(&sid, &old).unwrap();
        events
            .record_compaction(&sid, &[flux_core::Message::assistant_text("summary")])
            .unwrap();
        events
            .record_message(&sid, &flux_core::Message::assistant_text("new answer"))
            .unwrap();
        let mut state = ChatState::new("mock".into());
        state.project_session(&events, &sid).unwrap();
        let text = state
            .transcript_lines(80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("old request").count(), 1);
        assert!(
            !text.contains("summary"),
            "snapshot messages are model context, not new activity"
        );
        assert!(text.contains("context compacted"));
        assert!(text.contains("new answer"));
    }

    #[test]
    fn initial_session_projection_preserves_the_active_engine_model() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let sid = events.create_session("stored-old-model").unwrap();
        let mut state = ChatState::for_session("active-new-model".into(), sid.clone());

        state.project_session(&events, &sid).unwrap();

        assert_eq!(state.model, "active-new-model");
    }

    #[test]
    fn resumed_plan_uses_the_attempt_without_duplicate_context_message() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        let turn = events.begin_turn(&sid, "inspect", "mock").unwrap();
        events
            .record_plan_attempt(
                &sid,
                turn,
                flux_events::PlanAttempt {
                    outcome: "accepted".into(),
                    plan_text: Some("flow\n└─ read(\"README.md\")".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        events
            .record_message(
                &sid,
                &flux_core::Message::assistant_text("Proposed plan:\nflow\n└─ read(\"README.md\")"),
            )
            .unwrap();

        let mut state = ChatState::new("mock".into());
        state.project_session(&events, &sid).unwrap();
        let text = state
            .transcript_lines(80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert_eq!(text.matches("README.md").count(), 1);
        assert!(!text.contains("Proposed plan:"));
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

    /// C-102 graceful narrow-width bars: `bar_line` drops right-side segments one at a time from
    /// the end (least-precious last) instead of clearing the whole right side at once.
    #[test]
    fn bar_line_drops_right_segments_progressively() {
        let seg = |s: &str| vec![Span::raw(s.to_string())];
        let render_at = |width: u16| -> String {
            bar_line(
                vec![Span::raw("left")],
                vec![seg("tok"), seg(" · cache"), seg(" · cost")],
                width,
            )
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
        };
        // Wide: everything fits (left 4 + right 19 + 2 = 25).
        let wide = render_at(40);
        assert!(wide.contains("tok") && wide.contains("cache") && wide.contains("cost"));
        // Mid: cost dropped, cache survives (4 + 11 + 2 = 17).
        let mid = render_at(20);
        assert!(mid.contains("tok") && mid.contains("cache"), "{mid}");
        assert!(!mid.contains("cost"), "{mid}");
        // Narrow: only tokens survive (4 + 3 + 2 = 9).
        let narrow = render_at(12);
        assert!(narrow.contains("tok"), "{narrow}");
        assert!(
            !narrow.contains("cache") && !narrow.contains("cost"),
            "{narrow}"
        );
        // Floor: right side empties entirely rather than truncating the left.
        let floor = render_at(6);
        assert!(floor.contains("left") && !floor.contains("tok"), "{floor}");
    }

    /// C-102: on a narrow terminal the header sheds cost → cache but keeps the token total.
    #[test]
    fn narrow_header_keeps_tokens_drops_cost() {
        let mut state = ChatState::new("m".into());
        state.tokens_in = 12_300;
        state.tokens_out = 840;
        state.tokens_cache_read = 1_000;
        state.cost_usd = Some(1.2345);
        let mut terminal = Terminal::new(TestBackend::new(46, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("12.3k"), "tokens must survive: {content}");
        assert!(!content.contains("$1.2345"), "cost drops first: {content}");
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
    fn fmt_count_hands_off_units_at_the_boundary() {
        // The private `fmt_count` used to render `999_999` as `1000.0k` (rounding after choosing the
        // unit). Now it shares the L0 humanizer, which rounds first and hands off cleanly to `1.0M`.
        assert_eq!(fmt_count(999_999), "1.0M", "never `1000.0k`");
    }

    #[test]
    fn unicode_layout_uses_terminal_cell_width() {
        assert_eq!(truncate("界界", 3), "界…");
        assert!(UnicodeWidthStr::width(truncate("a界b", 3).as_str()) <= 3);

        let wrapped = wrap_styled_lines(vec![Line::raw("a界bc")], 3);
        let rows = wrapped
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, ["a界", "bc"]);
    }

    #[test]
    fn long_transcript_materializes_only_the_visible_viewport() {
        let mut state = ChatState::new("mock".into());
        for index in 0..100 {
            state.push(Entry::Notice {
                text: format!("row-{index}"),
                sev: Sev::Info,
            });
        }
        let visible = state.transcript_viewport(40, 5);
        assert_eq!(visible.len(), 5);
        assert!(visible.iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.contains("row-99"))));
        assert!(state.last_max_scroll.get() > 5);

        // A transcript mutation invalidates the cached layout before the next viewport read.
        state.push(Entry::Notice {
            text: "latest".into(),
            sev: Sev::Info,
        });
        let visible = state.transcript_viewport(40, 5);
        assert!(visible.iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.contains("latest"))));
    }

    #[test]
    fn session_picker_is_dense_and_marks_the_active_session() {
        let mut state = ChatState::for_session("mock".into(), "s_2".into());
        state.session_picker = Some(vec![
            flux_events::SessionSummary {
                id: "s_2".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: 2,
                messages: 4,
                context: Default::default(),
            },
            flux_events::SessionSummary {
                id: "s_1".into(),
                model: "anthropic/sonnet".into(),
                created_at_ms: 0,
                updated_at_ms: 1,
                messages: 2,
                context: Default::default(),
            },
        ]);
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("sessions"));
        assert!(content.contains("● s_2"));
        assert!(!content.contains('┌'));
    }

    #[test]
    fn only_non_mutating_commands_are_available_while_busy() {
        assert!(command_is_read_only("sessions", ""));
        assert!(!command_is_read_only("sessions", "--prune"));
        assert!(command_is_read_only("evidence", ""));
        assert!(!command_is_read_only("model", "mock"));
        assert!(!command_is_read_only("shell", ""));
    }

    #[test]
    fn durable_history_keeps_prompts_superseded_by_compaction() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        events
            .record_message(&sid, &flux_core::Message::user_text("before compact"))
            .unwrap();
        events
            .record_compaction(&sid, &[flux_core::Message::assistant_text("summary")])
            .unwrap();
        events
            .record_message(&sid, &flux_core::Message::user_text("after compact"))
            .unwrap();
        assert_eq!(load_history(&events), ["before compact", "after compact"]);
    }

    #[test]
    fn historical_plan_never_recomputes_current_risk() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Plan(serde_json::json!({
            "plan": "flow\n└─ read(\"README.md\")",
            "historical": true,
            "risk": "high · destructive",
            "ops": 9,
        })));
        let text = state
            .transcript_lines(80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert!(text.contains("historical"));
        assert!(!text.contains("high"));
        assert!(!text.contains("9 ops"));
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
    /// `CliSink`/`phase_spinner_label`, including historical phase labels and the neutral fallback.
    #[test]
    fn loop_phase_observation_drives_the_phase_labeled_spinner() {
        let mut state = ChatState::new("opus".into());
        state.phase = Phase::Planning;
        state.turn_start = Some(Instant::now());

        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("working…"),
            "no loop.phase observed yet -> neutral fallback"
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

        state.record_loop_phase("intent");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("routing intent…"));

        state.record_loop_phase("explore");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("exploring…"));

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

    /// A-15/A-72 parity: the `ChannelSink` forwards phase, brief, and accepted staged-intent
    /// observations as their own `UiEvent`s, same as `flow.plan`/skill/destructive already are.
    #[test]
    fn channel_sink_forwards_phase_and_brief_observations() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx, action_id: 1 };

        sink.observation(&flux_evidence::Observation::new(
            "loop.phase",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "phase": "gather" }),
        ));
        match untag(rx.try_recv().expect("a Phase event was sent")) {
            UiEvent::Phase(p) => assert_eq!(p, "gather"),
            _ => panic!("expected UiEvent::Phase"),
        }

        sink.observation(&flux_evidence::Observation::new(
            "flow.brief",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "goal": "find the bug", "needs": ["stack trace"] }),
        ));
        match untag(rx.try_recv().expect("a Brief event was sent")) {
            UiEvent::Brief { goal, needs } => {
                assert_eq!(goal, "find the bug");
                assert_eq!(needs, vec!["stack trace".to_string()]);
            }
            _ => panic!("expected UiEvent::Brief"),
        }

        sink.observation(&flux_evidence::Observation::new(
            flux_evidence::KIND_TURN_INTENT,
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "intent": "answer from evidence",
                "families": ["workspace.read"],
                "operations": ["glob", "read"]
            }),
        ));
        match untag(rx.try_recv().expect("an Intent event was sent")) {
            UiEvent::Intent(intent) => {
                assert_eq!(intent.intent, "answer from evidence");
                assert_eq!(intent.families, vec!["workspace.read"]);
                assert_eq!(intent.operations, vec!["glob", "read"]);
            }
            _ => panic!("expected UiEvent::Intent"),
        }
    }

    #[test]
    fn staged_intent_renders_concisely_live_and_from_history() {
        let observation = flux_evidence::Observation::new(
            flux_evidence::KIND_TURN_INTENT,
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "intent": "  answer   from\nworkspace evidence ",
                "families": ["workspace.read"],
                "operations": ["glob", "read"]
            }),
        );
        let entry = historical_observation_entry(&observation)
            .expect("the durable staged intent is replayable");
        assert!(matches!(entry, Entry::Intent(_)));

        let mut state = ChatState::new("mock".into());
        state.push(entry);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let normal = screen(&terminal);
        assert!(normal.contains("◆ intent: answer from workspace evidence"));
        assert!(normal.contains("capabilities: workspace.read · 2 operations"));
        assert!(!normal.contains("operations: glob, read"));

        let mut verbose = ChatState::new("mock".into()).with_verbose(true);
        verbose.push(Entry::Intent(
            staged_intent_entry(&observation.data).expect("valid staged intent"),
        ));
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| render(f, &verbose)).unwrap();
        assert!(screen(&terminal).contains("operations: glob, read"));

        let signal_only = flux_evidence::Observation::new(
            flux_evidence::KIND_TURN_INTENT,
            flux_evidence::Phase::Turn,
            serde_json::json!({"signal": "slack"}),
        );
        assert!(
            historical_observation_entry(&signal_only).is_none(),
            "keyword-derived surfacing signals are not staged intent summaries"
        );
    }

    /// A-17: the `ChannelSink` forwards a `flow.halt` observation as a `Notice`/`Sev::Err` — the
    /// same real-time-cue machinery destructive-op flags and skill activation already use — with
    /// `halt_line`'s `✗ step N/M <op> failed — revising…` text.
    #[test]
    fn channel_sink_forwards_flow_halt_as_a_notice() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx, action_id: 1 };

        sink.observation(&flux_evidence::Observation::new(
            "flow.halt",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "step": 4, "of": 9, "op": "edit", "kind": "runtime", "fatal": false }),
        ));
        match untag(rx.try_recv().expect("a Notice event was sent")) {
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
