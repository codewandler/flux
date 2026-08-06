//! `flux-tui` — a ratatui chat frontend for the agent.
//!
//! [`render`] draws a dense, borderless chat: a viewport-only transcript, compact header/footer,
//! and multiline composer separated solely by its background. [`run`] drives the async crossterm
//! loop: turns stream Markdown, plans and tool cards inline; follow-ups queue visibly; sessions can
//! be resumed with their durable activity; PgUp/PgDn/wheel scroll; Ctrl-C interrupts; and guarded
//! operations raise a y/a/N approval sheet. Headless layout behavior is pinned with `TestBackend`.

mod controller;
pub mod fleet;
mod interaction;
pub mod loopmock;
mod observatory;
pub mod operations;
mod panes;
mod projection;
mod rendering;
pub mod spinners;
pub mod splash;
mod state;
mod terminal_io;

pub use controller::ApprovalView;
use controller::{
    approval_key, send_action_event, show_next_approval, ApprovalAction, ChannelApprover,
    ChannelSink, ModelCallTiming, PendingApproval, UiEvent,
};
pub use interaction::InteractionQueue;
use panes::PaneStore;
pub use panes::{PaneListing, PaneQueue};
#[cfg(test)]
use projection::staged_intent_entry;
use projection::{historical_observation_entry, load_history};
pub use rendering::render;
pub use state::ChatState;
use state::{ModelCallBadge, Phase, RetryView, RoundUsage};
use terminal_io::{TerminalGuard, Tui};

pub mod theme;
pub mod toolview;

mod markdown;
mod plan;
mod trust;

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

use flux_core::humanize::{fmt_age, fmt_count, fmt_elapsed};
use flux_core::{DispatchId, Usage};
use flux_flow::engine::FlowEngine;
use flux_flow::{AgentSink, SteeringQueue};
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
    /// Persistent execution-target identity shown by the surface. `None` means native local mode.
    pub execution_target: Option<String>,
    /// Optional surface-owned resolver that enables `/model <spec>`.
    pub model_resolver: Option<Arc<dyn ModelResolver>>,
    /// Command files (D-186) discovered by the surface — already filtered against its own
    /// built-in names (built-ins always win a clash; the caller warns at load).
    pub file_commands: Vec<flux_runtime::metadata::CommandFile>,
    /// Configured theme name (`dark` / `light` / `mono`); `None` falls back to `dark` (C-104).
    pub theme: Option<String>,
    /// The pane channel this surface minted **before** assembling the agent (C-305), and therefore
    /// the same handle the agent's `pane.*` ops write to. `None` leaves the agent with no pane
    /// vocabulary at all — the caller must not register the ops without passing it.
    pub pane_queue: Option<Arc<PaneQueue>>,
    /// Typed-question channel minted before agent assembly; `None` keeps the surface headless.
    pub interaction_queue: Option<Arc<InteractionQueue>>,
    /// Typed Board/Fleet projection and bounded mutation bridge. `None` is explicitly standalone.
    pub operations_source: Option<operations::SharedFleetBoardSource>,
    /// Cheap truthful attachment seed rendered while the first full projection loads off-thread.
    pub operations_initial_snapshot: Option<operations::FleetBoardSnapshot>,
    /// Source token corresponding to [`Self::operations_initial_snapshot`].
    pub operations_refresh_token: Option<String>,
    /// Surface workspace root when it differs from the process cwd (Fleet-root attachment).
    pub workspace_root: Option<String>,
}

impl TuiRunOptions {
    pub fn new(auto_approve: bool, model_spec: Option<String>) -> Self {
        Self {
            auto_approve,
            model_spec,
            execution_target: None,
            model_resolver: None,
            file_commands: Vec::new(),
            theme: None,
            pane_queue: None,
            interaction_queue: None,
            operations_source: None,
            operations_initial_snapshot: None,
            operations_refresh_token: None,
            workspace_root: None,
        }
    }
}

/// Braille spinner frames (shared idiom with the CLI); the fallback when the
/// terminal lacks truecolor for the animated `spinners` footer bar.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Width of the animated footer effect bar.
const FOOTER_BAR_WIDTH: usize = 12;

/// C-156: how long a first blank-composer, idle Ctrl-C stays armed for a confirming second press
/// before it decays back to a fresh "first press" (rather than quitting on a stale arm).
const CTRL_C_QUIT_WINDOW: Duration = Duration::from_secs(2);

/// Outcome of [`ChatState::arm_or_confirm_quit`] — the event loop only ever exits on `Quit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CtrlCQuit {
    /// First press: the confirmation is now showing in the footer; the event loop keeps running.
    Armed,
    /// Second press within the window: the event loop breaks.
    Quit,
}

/// Direction requested by the failed-tool-card navigation chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureNavigation {
    Next,
    Previous,
}

/// Decode the two failed-card navigation chords across crossterm's case representations.
///
/// Depending on the terminal keyboard protocol, Ctrl-Shift-G can arrive as either lowercase `g`
/// plus [`KeyModifiers::SHIFT`] or uppercase `G` with shift normalized into the character. Ignore
/// lock-key state, but reject unrelated extra modifiers so the binding remains exactly Ctrl-G.
fn failure_navigation_key(key: crossterm::event::KeyEvent) -> Option<FailureNavigation> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    if key.kind != KeyEventKind::Press
        || !key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.intersects(
            KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
        )
    {
        return None;
    }
    match key.code {
        KeyCode::Char('G') => Some(FailureNavigation::Previous),
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(FailureNavigation::Previous)
        }
        KeyCode::Char('g') => Some(FailureNavigation::Next),
        _ => None,
    }
}

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
/// `FLUX_VERBOSE`), whose promise is tool output in full, no truncation. The number lives in
/// [`toolview::budget`] beside the CLI's caps (C-539) so the surfaces cannot drift silently.
const MAX_DETAIL: usize = toolview::budget::TUI_DETAIL_LINES;

/// How many in-flight output lines a running tool card keeps (C-158). Deliberately small and NOT
/// lifted by verbose: this is a "still moving, here's roughly where" signal on a card that has no
/// result yet, not a log viewer — the full output arrives seconds later as the card's real detail,
/// which verbose does lift. A tight bound also keeps a chatty command from dominating the
/// transcript while it runs.
const MAX_PARTIAL_LINES: usize = 3;

/// How many per-call rows the `/usage` overlay keeps for the turn in progress (C-140). A turn is
/// bounded by the adaptive model-call budget well below this; the cap exists so a runaway loop can't
/// grow the row list without bound. The session-level cache totals are unaffected — they keep
/// folding every call.
pub(crate) const MAX_TURN_ROUNDS: usize = 256;

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

/// A slash command shown in the `/` menu — a fixed built-in or a discovered command file (D-186).
#[derive(Clone)]
struct SlashCmd {
    name: String,
    desc: String,
}

/// The built-in slash commands. A command file naming one of these is dropped at load (with a
/// warning) rather than shadowing it — see `flux-cli`'s `TUI_BUILTIN_COMMANDS`.
const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("help", "show keybindings"),
    ("clear", "start a fresh session"),
    ("new", "clear and start fresh"),
    ("model", "show or switch model"),
    ("effort", "show or set reasoning effort"),
    ("quit", "exit flux"),
    (
        "restart",
        "relaunch on the installed binary, resuming this session",
    ),
    ("usage", "live usage; `history` opens the observatory"),
    ("insights", "summarize current-session facts"),
    ("compact", "compact session context"),
    ("shell", "toggle the generic bash op"),
    ("tools", "list registered tools"),
    ("evidence", "show durable evidence"),
    ("session", "show the active session"),
    ("sessions", "list recent sessions"),
    ("resume", "resume a session id"),
    ("queue", "manage queued follow-ups"),
    (
        "fleet",
        "operations pane · also /fleet:restart, /fleet:refresh",
    ),
    ("board", "open Board work and decisions"),
    ("theme", "show or switch the color theme"),
];

/// The full slash-menu source: built-ins followed by discovered command files (already filtered
/// against built-in names by the caller — see `flux-cli`'s `load_command_files`).
fn all_slash_commands(file_commands: &[flux_runtime::metadata::CommandFile]) -> Vec<SlashCmd> {
    let mut out: Vec<SlashCmd> = BUILTIN_COMMANDS
        .iter()
        .map(|(name, desc)| SlashCmd {
            name: (*name).to_string(),
            desc: (*desc).to_string(),
        })
        .collect();
    out.extend(file_commands.iter().map(|c| SlashCmd {
        name: c.name.clone(),
        desc: if c.argument_hint.is_empty() {
            c.description.clone()
        } else {
            format!("{} ({})", c.description, c.argument_hint)
        },
    }));
    out
}

/// Commands matching `query` (lowercased, no leading `/`), ranked through the same `fuzzy_rank`
/// tiering as `@`-path completion (C-153): prefix beats substring beats subsequence, so `/thm`
/// finds `/theme`. Exact-prefix behavior is unchanged — a prefix query still ranks every
/// prefix-matching command ahead of any substring/subsequence one.
fn slash_matches(
    query: &str,
    file_commands: &[flux_runtime::metadata::CommandFile],
) -> Vec<SlashCmd> {
    let all = all_slash_commands(file_commands);
    let names: Vec<String> = all.iter().map(|c| c.name.clone()).collect();
    fuzzy_rank_indices(&names, query)
        .into_iter()
        .map(|i| all[i].clone())
        .collect()
}

/// The help overlay's keybinding rows (C-110): `(keys, what)`. The slash-command half of the
/// overlay iterates the merged command table (`all_slash_commands`) so it can never drift.
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
    (
        "Ctrl-End / Ctrl-G / Ctrl-Shift-G",
        "latest / next/previous failed card",
    ),
    ("Ctrl-E", "expand/collapse tool details (all cards)"),
    (
        "Shift-↑/↓",
        "focus transcript entry · ↵ expand card · y copy · Esc",
    ),
    (
        "Ctrl-T",
        "toggle mouse capture (native select/copy while off)",
    ),
    (
        "y / a / n·Esc / d",
        "approval: allow / always / deny / deny with reason",
    ),
    ("Ctrl-C", "interrupt · clear · quit"),
    ("Ctrl-D", "quit (empty input)"),
    ("F1 / Esc", "open/close this help"),
];

/// Look up `name` among discovered command files and, if found, substitute `args` into its body
/// (D-186's `$ARGUMENTS`/`$1..$9`) — the prompt [`start_turn`] then runs exactly like typed input.
fn file_command_prompt(
    name: &str,
    args: &str,
    file_commands: &[flux_runtime::metadata::CommandFile],
) -> Option<String> {
    file_commands
        .iter()
        .find(|c| c.name == name)
        .map(|c| flux_runtime::metadata::expand_command_arguments(&c.body, args))
}

/// One item in the transcript. Each renders to one or more styled [`Line`]s at a given width.
#[derive(Debug)]
enum Entry {
    /// A user message, optionally preceded by a compact prior-turn boundary.
    User {
        text: String,
        prior_elapsed: Option<Duration>,
        show_separator: bool,
    },
    /// An assistant reply — plain while streaming, Markdown once done (cached per width).
    Assistant(Assistant),
    /// Live extended-thinking tokens streamed during a model-backed stage, rendered as Markdown
    /// once sealed (same `Assistant` widget, distinct entry so it doesn't merge with the reply).
    ///
    /// One entry per model call. `call` is the latency badge, attached once the call's `model.call`
    /// observation lands — which is always *after* the entry seals, so it never races the sealing
    /// (C-180). Carried here rather than as its own entry so it stays bound to the round it
    /// measures and still renders for a stage that emitted no thinking tokens at all.
    Thinking {
        body: Assistant,
        call: Option<ModelCallBadge>,
    },
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
    /// C-531: the dispatch this card was opened for, so its result/timing land here and nowhere
    /// else. `None` for a card rebuilt from durable history on resume — that card is already
    /// finished and can never receive a live event.
    dispatch: Option<DispatchId>,
    call: toolview::Call,
    /// The op input (so a diff/preview can be rendered exactly).
    input: serde_json::Value,
    started: Instant,
    timing: Option<flux_core::OperationTiming>,
    /// `None` while the op is still running.
    result: Option<ToolOutcome>,
    /// Per-card expansion override (C-111): `None` follows the global `expand_tools`; Enter on
    /// the focused card sets `Some(!effective)` so one card can open/close independently.
    expanded: Option<bool>,
    /// C-158: the last few already-redacted output lines while this op is still running. Always
    /// empty once `result` is set — the real summary/detail supersedes it.
    partial: Vec<String>,
}

#[derive(Debug)]
struct ToolOutcome {
    is_error: bool,
    cancelled: bool,
    content: String,
    /// A one-line summary (e.g. `3 matches`) when [`toolview::format_result`] has one.
    summary: Option<String>,
    elapsed: Duration,
    approval_wait: Option<Duration>,
}

impl ToolEntry {
    fn new(dispatch: DispatchId, name: String, input: serde_json::Value) -> Self {
        let call = toolview::format_call(&name, &input);
        ToolEntry {
            name,
            dispatch: Some(dispatch),
            call,
            input,
            started: Instant::now(),
            timing: None,
            result: None,
            expanded: None,
            partial: Vec::new(),
        }
    }

    fn historical(
        name: String,
        input: serde_json::Value,
        content: String,
        is_error: bool,
        elapsed: Duration,
    ) -> Self {
        // C-533: resumed sessions cross the same transcript boundary as live ones.
        let content = trust::sanitize_tool_output(&content);
        let call = toolview::format_call(&name, &input);
        let summary = toolview::format_result(&name, &content, is_error);
        ToolEntry {
            name,
            dispatch: None,
            call,
            input,
            started: Instant::now()
                .checked_sub(elapsed)
                .unwrap_or_else(Instant::now),
            timing: None,
            result: Some(ToolOutcome {
                is_error,
                cancelled: false,
                content,
                summary,
                elapsed,
                approval_wait: None,
            }),
            expanded: None,
            partial: Vec::new(),
        }
    }

    fn historical_reduced(name: String, error: Option<String>, elapsed: Duration) -> Self {
        let input = serde_json::Value::Null;
        let call = toolview::format_call(&name, &input);
        let is_error = error.is_some();
        // C-533: an engine error string can embed tool output; sanitize like any other content.
        let content = error
            .map(|error| trust::sanitize_tool_output(&error))
            .unwrap_or_default();
        let summary = if is_error {
            toolview::format_result(&name, &content, true)
        } else {
            Some("completed".into())
        };
        ToolEntry {
            name,
            dispatch: None,
            call,
            input,
            started: Instant::now()
                .checked_sub(elapsed)
                .unwrap_or_else(Instant::now),
            timing: None,
            result: Some(ToolOutcome {
                is_error,
                cancelled: false,
                content,
                summary,
                elapsed,
                approval_wait: None,
            }),
            expanded: None,
            partial: Vec::new(),
        }
    }
}

/// A streaming-then-finalized assistant message with a per-width render cache.
#[derive(Debug, Default)]
struct Assistant {
    text: String,
    done: bool,
    /// `(width, source byte length, rendered lines)` — the sealed message caches its full render;
    /// while streaming the SEALED PREFIX caches under its byte length (C-114), so a stable prefix
    /// renders exactly once and its lines are byte-identical across stream states by construction.
    cache: RefCell<Option<(u16, usize, Vec<Line<'static>>)>>,
}

/// Cached, fully wrapped transcript layout. State changes invalidate the cache; animation-only
/// frames reuse it and clone only the rows currently visible in the viewport.
#[derive(Debug)]
struct TranscriptLayout {
    revision: u64,
    width: u16,
    /// Per-entry wrapped row spans `(entry index, first row, row count)` — focus navigation
    /// (C-111) uses them to highlight and center the focused entry.
    entry_rows: Vec<(usize, u16, u16)>,
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

/// Entry cap for the `@` completion's workspace file inventory (C-112) — bounds the lazy walk.
const PATH_INVENTORY_CAP: usize = 20_000;

/// Bounded, ignore-aware workspace file walk for `@` path completion (C-112): skips hidden
/// entries, `target`, and `node_modules`; does not follow symlinked directories; stops at `cap`
/// files. Returns workspace-relative paths, sorted.
fn workspace_file_inventory(root: &std::path::Path, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= cap {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if out.len() >= cap {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    out.sort();
    out
}

/// Rank positions into `items` against a fuzzy `query` (C-112, shared by C-153's slash-command
/// and session-picker matching): path-segment prefix beats substring beats subsequence; ties go
/// to the shorter candidate. Case-insensitive. Returns indices rather than borrowed strings so any
/// caller can map back to its own parallel type (a `SlashCmd`, a `SessionSummary`, …) instead of
/// only a `&str`. An empty query is the identity permutation — `items` in its original order —
/// rather than every entry tying at tier 0 and falling back to the length tie-break; that would
/// reshuffle the slash menu's hand-curated command order (and a bare `@`'s alphabetical file
/// listing) into a length sort the moment the popup opens, before the user has typed anything.
/// Pure and unit-tested (via `fuzzy_rank` below, its `&str`-returning wrapper).
fn fuzzy_rank_indices(items: &[String], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    fn is_subsequence(hay: &str, needle: &str) -> bool {
        let mut chars = needle.chars();
        let mut want = chars.next();
        for c in hay.chars() {
            if Some(c) == want {
                want = chars.next();
            }
        }
        want.is_none()
    }
    let query = query.to_lowercase();
    let mut scored: Vec<(u8, usize, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let lower = item.to_lowercase();
            let score = if lower
                .split(['/', '\\'])
                .any(|segment| segment.starts_with(&query))
            {
                0
            } else if lower.contains(&query) {
                1
            } else if is_subsequence(&lower, &query) {
                2
            } else {
                return None;
            };
            Some((score, item.len(), i))
        })
        .collect();
    scored.sort();
    scored.into_iter().map(|(_, _, i)| i).collect()
}

/// Rank workspace paths against a fuzzy query (C-112): path-segment prefix beats substring
/// beats subsequence; ties go to the shorter path. Case-insensitive. Pure and unit-tested.
fn fuzzy_rank<'a>(paths: &'a [String], query: &str) -> Vec<&'a str> {
    fuzzy_rank_indices(paths, query)
        .into_iter()
        .map(|i| paths[i].as_str())
        .collect()
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

/// One tool-card header row: `→ verb  [▸/▾ ]arg … badge` with the arg truncated so the badge sits
/// flush right. Shared by the cached build (`tool_lines`) and the per-tick running-badge patch
/// (C-109) so the pad math cannot drift between the two.
///
/// `marker` is the C-155 collapse/expand affordance (`▸` collapsed, `▾` expanded), rendered
/// between the verb and the arg — never last, so it cannot be mistaken for the C-109 running
/// badge (the running-badge pairing matches the LAST span). `None` when the card has nothing
/// expandable, so no false affordance is shown.
fn tool_header_line(
    t: &Theme,
    verb: &str,
    arg_full: &str,
    badge: String,
    badge_style: Style,
    width: u16,
    marker: Option<char>,
) -> Line<'static> {
    let badge_w = UnicodeWidthStr::width(badge.as_str());
    let marker_w = marker.map_or(0, |_| 2); // glyph + trailing space
    let fixed = 2 + UnicodeWidthStr::width(verb) + 2 + marker_w; // "→ " + verb + "  " + optional "▸ "
    let arg_room = (width as usize).saturating_sub(fixed + badge_w + 1);
    let arg = truncate(arg_full, arg_room.max(4));
    let used = fixed + UnicodeWidthStr::width(arg.as_str());
    let pad = (width as usize).saturating_sub(used + badge_w).max(1);
    let mut spans = vec![
        Span::styled("→ ", t.tool_style()),
        Span::styled(
            verb.to_string(),
            t.tool_style().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    if let Some(m) = marker {
        spans.push(Span::styled(format!("{m} "), t.muted_style()));
    }
    spans.push(Span::styled(arg, t.muted_style()));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(badge, badge_style));
    Line::from(spans)
}

/// Whether `tool` has anything to show when expanded (C-155): a running card (no result yet) or
/// a finished card whose diff/detail projection is empty has nothing to expand, so
/// [`tool_header_line`]'s marker must stay `None` rather than promise a detail view that turns
/// out blank.
fn tool_has_detail(tool: &ToolEntry) -> bool {
    let Some(o) = &tool.result else {
        return false;
    };
    if o.cancelled {
        return false;
    }
    if !o.is_error {
        if let Some(diff) = toolview::format_diff(&tool.name, &tool.input) {
            return !diff.is_empty();
        }
    }
    !toolview::format_detail(&tool.name, &tool.input, &o.content, o.is_error).is_empty()
}

/// The static in-flight badge recorded into the cached layout (C-109 patches it per tick in the
/// viewport only, so the cache stays untouched across animation frames).
const RUNNING_BADGE: &str = "◌ running";

/// C-149: the transcript's one-column turn-boundary rail — a glyph, not a background tint, so it
/// survives `Theme::MONO` (which zeroes every color field). One trailing space separates it from
/// the entry's own content; [`GUTTER_COLS`] is its total display width, budgeted out of `width`
/// wherever `entry_lines` sizes content so the rail never pushes a row past its wrap boundary.
const GUTTER: &str = "│ ";
const GUTTER_COLS: u16 = 2;

/// The rail style for one entry: bold/accent for the user's own turn, dim for everything the
/// assistant/runtime produced (assistant text, tool cards, notices, and the rest) — a modifier
/// distinction (`user_style` adds `BOLD`), not just a color one, so it still reads under
/// `Theme::MONO`.
fn gutter_style(entry: &Entry, t: &Theme) -> Style {
    match entry {
        Entry::User { .. } => t.user_style(),
        _ => t.muted_style(),
    }
}

/// Prepend the [`GUTTER`] rail span to every line, styled per entry kind (C-149). Applied once at
/// the end of `entry_lines` so every downstream consumer (wrap, row-span recording, the C-111
/// focus `sel_bg` patch, the C-109 running-badge pairing) sees it as an ordinary leading span.
fn prepend_gutter(mut lines: Vec<Line<'static>>, style: Style) -> Vec<Line<'static>> {
    for line in &mut lines {
        line.spans.insert(0, Span::styled(GUTTER, style));
    }
    lines
}

/// A compact, glyph-based boundary that fits in one transcript row. Whole seconds omit the
/// shared latency formatter's decimal (`12s`, not `12.0s`) because this is orientation metadata,
/// not a precision readout. When the duration cannot fit, the rule remains and the label sheds.
fn turn_separator_line(
    elapsed: Option<Duration>,
    content_width: u16,
    theme: &Theme,
) -> Line<'static> {
    let width = usize::from(content_width);
    let duration = elapsed.map(|elapsed| {
        let compact = fmt_elapsed(elapsed);
        compact
            .strip_suffix(".0s")
            .map_or(compact.clone(), |seconds| format!("{seconds}s"))
    });
    let label = duration.map(|duration| format!("── {duration} ──"));
    let text = match label {
        Some(label) if UnicodeWidthStr::width(label.as_str()) <= width => {
            let pad = width - UnicodeWidthStr::width(label.as_str());
            format!("{}{label}", " ".repeat(pad))
        }
        _ => "─".repeat(width.min(8)),
    };
    Line::styled(text, theme.muted_style())
}

/// Max text size accepted for an OSC 52 clipboard write (C-111). Terminals commonly cap the
/// whole sequence around 100 KB; base64 inflates by 4/3, so cap the raw payload at 72 KiB.
const OSC52_MAX_TEXT: usize = 72 * 1024;

/// Build the OSC 52 set-clipboard escape for `text`, or `None` when it exceeds
/// [`OSC52_MAX_TEXT`]. Writes the `c` (clipboard) selection; works over SSH because the
/// sequence travels the terminal stream itself.
fn osc52_copy(text: &str) -> Option<String> {
    use base64::Engine;
    if text.len() > OSC52_MAX_TEXT {
        return None;
    }
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    Some(format!("\x1b]52;c;{payload}\x07"))
}

/// Render one hunk-view diff row (C-115): `@ path` / `@@ … @@` rows in accent, changed rows with
/// a `old new ±` gutter and word-level intraline emphasis (REVERSED). Shared by the expanded
/// edit/write card and the approval sheet's diff preview so the two can't drift.
fn diff_row_line(t: &Theme, row: &toolview::DiffLine, indent: &'static str) -> Line<'static> {
    let (style, marker) = match row.kind {
        toolview::DetailKind::Add => (t.ok_style(), '+'),
        toolview::DetailKind::Del => (t.err_style(), '-'),
        toolview::DetailKind::Plain => (t.muted_style(), ' '),
        toolview::DetailKind::Meta | toolview::DetailKind::Hunk => (t.accent_style(), ' '),
    };
    let mut spans = vec![Span::raw(indent)];
    match row.kind {
        toolview::DetailKind::Meta | toolview::DetailKind::Hunk => {
            let text: String = row.spans.iter().map(|(_, s)| s.as_str()).collect();
            spans.push(Span::styled(text, style));
        }
        _ => {
            let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
            spans.push(Span::styled(
                format!("{:>4} {:>4} {marker} ", num(row.old_no), num(row.new_no)),
                t.muted_style(),
            ));
            for (emph, s) in &row.spans {
                let seg_style = if *emph {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style
                };
                spans.push(Span::styled(s.clone(), seg_style));
            }
        }
    }
    Line::from(spans)
}

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
            // C-114: the COMPLETED block prefix renders through the markdown engine (cached under
            // its byte length, so it renders once and can't flicker); only the trailing
            // unterminated block stays plain + cursor.
            let sealed = split_sealed_prefix(&self.text);
            let mut lines = if sealed > 0 {
                let mut prefix = self.render_cached(&self.text[..sealed], width, sealed);
                // One spacer between the styled prefix and the plain tail — the block boundary
                // the split found IS a blank line, which the renderer swallows.
                if prefix.last().is_some_and(|l| !l.spans.is_empty()) {
                    prefix.push(Line::default());
                }
                prefix
            } else {
                Vec::new()
            };
            let mut tail: Vec<Line> = self.text[sealed..]
                .split('\n')
                .map(|l| Line::styled(l.to_string(), theme.assistant_style()))
                .collect();
            if tail.is_empty() {
                tail.push(Line::default());
            }
            if let Some(last) = tail.last_mut() {
                last.spans.push(Span::styled(CURSOR, theme.accent_style()));
            }
            lines.extend(tail);
            return lines;
        }
        self.render_cached(&self.text, width, self.text.len())
    }

    fn render_cached(&self, src: &str, width: u16, key: usize) -> Vec<Line<'static>> {
        if let Some((w, k, cached)) = self.cache.borrow().as_ref() {
            if *w == width && *k == key {
                return cached.clone();
            }
        }
        let lines = markdown::render(src, width).lines;
        *self.cache.borrow_mut() = Some((width, key, lines.clone()));
        lines
    }
}

/// Byte length of a streaming message's SEALED Markdown prefix (C-114): text up to (and
/// including) the last blank line that (a) sits outside any open ``` / ~~~ fence and (b) is
/// followed by a line that starts a genuinely new block. A successor that is indented, a list
/// item, or a blockquote is held back — it could retroactively restyle the blocks before the
/// blank (e.g. flip a tight list loose) — so those boundaries don't seal. Conservative by
/// design: blank-line boundaries only.
fn split_sealed_prefix(text: &str) -> usize {
    fn is_list_marker(s: &str) -> bool {
        if let Some(rest) = s.strip_prefix(['-', '*', '+']) {
            return rest.starts_with(' ');
        }
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        (1..=9).contains(&digits)
            && matches!(s.as_bytes().get(digits), Some(b'.') | Some(b')'))
            && matches!(s.as_bytes().get(digits + 1), Some(b' '))
    }
    let mut in_fence = false;
    let mut best = 0;
    let mut pos = 0;
    // Byte offset just past a blank line, waiting for a safe successor line to seal at.
    let mut pending_blank_end: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let stripped = line.trim_end_matches(['\n', '\r']);
        let trimmed = stripped.trim_start();
        let fence_marker = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if in_fence {
            if fence_marker {
                in_fence = false;
            }
            pending_blank_end = None;
        } else if fence_marker {
            // A fence opener is a safe successor: it can't join the blocks before the blank.
            if let Some(end) = pending_blank_end.take() {
                best = end;
            }
            in_fence = true;
        } else if trimmed.is_empty() {
            pending_blank_end = Some(pos + line.len());
        } else if let Some(end) = pending_blank_end.take() {
            let joins_backwards = stripped.starts_with(' ')
                || stripped.starts_with('\t')
                || trimmed.starts_with('>')
                || is_list_marker(trimmed);
            if !joins_backwards {
                best = end;
            }
        }
        pos += line.len();
    }
    best
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

    /// A fresh state for one session. `pub` so an integration test can drive the same surface the
    /// event loop draws; the TUI itself builds it in [`run_with_options`].
    pub fn for_session(model: String, session_id: String) -> Self {
        ChatState {
            entries: Vec::new(),
            transcript_revision: 0,
            transcript_layout: RefCell::new(None),
            input: fresh_textarea(),
            approval: None,
            interaction: None,
            assistant_open: false,
            phase: Phase::Idle,
            turn_start: None,
            session_id,
            model,
            model_spec: None,
            execution_target: None,
            workspace_root: String::new(),
            file_commands: Vec::new(),
            theme: Theme::default(),
            theme_name: "dark".into(),
            mouse_capture: true,
            history_search: None,
            search: None,
            help_open: false,
            usage_open: false,
            observatory: None,
            focused: None,
            file_inventory: None,
            path_sel: 0,
            path_dismissed: None,
            auto_approve: false,
            effort: None,
            expand_tools: false,
            verbose: false,
            slash_sel: 0,
            tokens_out: 0,
            tokens_reasoning: 0,
            cache: flux_core::CacheEfficiency::default(),
            turn_cache: flux_core::CacheEfficiency::default(),
            turn_rounds: Vec::new(),
            cost_usd: None,
            cost_model: None,
            cost_unpriced: false,
            budget: None,
            steps: 0,
            last_elapsed: None,
            model_call_start: None,
            turn_llm_wait: Duration::ZERO,
            last_llm_wait: None,
            retry: None,
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            queue: Arc::new(SteeringQueue::default()),
            queue_open: false,
            queue_sel: 0,
            queue_edit: None,
            session_picker: None,
            session_sel: 0,
            session_query: String::new(),
            previous_sessions: 0,
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
            ctrl_c_armed_at: None,
            panes: PaneStore::default(),
            pane_queue: None,
            interaction_queue: None,
            panes_overflowing: false,
            fleet: crate::fleet::FleetProjection::new(),
            fleet_rows: Vec::new(),
            operations: None,
        }
    }

    /// Apply one pane command (C-221). Bounds — count, rows, width — are enforced by
    /// [`crate::panes`], never by the command.
    ///
    /// Reached two ways and no others: the surface's own C-224 fleet pane calls it directly, and the
    /// model's `pane.*` ops arrive through [`ChatState::apply_pending_panes`].
    pub fn apply_pane_command(&mut self, command: flux_runtime::PaneCommand) {
        self.panes.apply(command);
    }

    /// Attach the pane channel the surface minted before assembling the agent (C-305).
    pub fn with_pane_queue(mut self, queue: Arc<crate::panes::PaneQueue>) -> Self {
        self.pane_queue = Some(queue);
        self
    }

    /// Attach the typed-question channel minted before agent assembly.
    pub fn with_interaction_queue(
        mut self,
        queue: Arc<crate::interaction::InteractionQueue>,
    ) -> Self {
        self.interaction_queue = Some(queue);
        self
    }

    /// Apply every pane command the agent has pushed since the last call, and report how many.
    ///
    /// This is the model's *only* door into the pane store, and it is driven by the event loop
    /// immediately before it draws — so a pane the agent opened mid-turn appears on the next frame,
    /// and one belonging to a turn that has just ended is cleared by `end_turn` rather than
    /// resurrected by a late command.
    pub fn apply_pending_panes(&mut self) -> usize {
        let Some(queue) = self.pane_queue.clone() else {
            return 0;
        };
        let drained = queue.drain();
        let applied = drained.commands.len();
        for command in drained.commands {
            self.apply_pane_command(command);
        }
        self.report_dropped_panes(drained.dropped);
        applied
    }

    /// Put a full pane channel in front of the operator (C-324).
    ///
    /// The drop itself happens in [`crate::panes::PaneQueue`] and cannot be reported back to the
    /// caller: that seam is send-only by construction, so `emit` has no return channel to answer
    /// through — not because the `pane.*` op has finished (it has not; the call is synchronous).
    /// This is the one place that both knows a command was refused and has somewhere to say it, so
    /// the operator is told here rather than nowhere. The model is told nothing, and this surface
    /// gives it no way to check either — see the reasoning at the drop site, and C-306.
    ///
    /// **Edge-triggered on purpose.** An overflow is a condition, not an event: a caller flooding
    /// the channel would otherwise earn a notice on every 62 ms frame and bury the transcript under
    /// the very symptom it is describing. The operator is told when the channel starts refusing and
    /// again only after it has recovered — so `dropped` here is *this drain's* count, not a running
    /// total, and the notice says so rather than implying otherwise.
    fn report_dropped_panes(&mut self, dropped: usize) {
        if dropped == 0 {
            self.panes_overflowing = false;
            return;
        }
        if std::mem::replace(&mut self.panes_overflowing, true) {
            return;
        }
        self.push(Entry::Notice {
            text: format!(
                "pane channel full — {dropped} pane command(s) dropped in this frame, and more \
                 will be for as long as it stays full. The agent's op reported success, so a pane \
                 it believes is open may not be on screen."
            ),
            sev: Sev::Warn,
        });
    }

    /// Every pane currently open, each labelled with who owns it (C-224).
    ///
    /// This is the surface-side query `PaneCommand` deliberately has no `list` variant for — that
    /// channel is send-only. C-223 wires the `pane.list` op to this so the model can see that the
    /// fleet pane is already up, **and that it is host-owned**, rather than opening a second one it
    /// would then be unable to keep in sync.
    pub fn open_panes(&self) -> Vec<panes::PaneListing> {
        self.panes.listing()
    }

    /// Fold one live sub-agent activity event (C-224) and re-resolve the fleet pane against `now`.
    ///
    /// Only the structural half of A-79's contract is read, by [`crate::fleet`]: the child's tool
    /// input and observation data are never touched, so no worker secret can reach this surface
    /// through a field it does not read.
    pub(crate) fn record_spawn_activity(
        &mut self,
        activity: &flux_runtime::SpawnActivity,
        now: Instant,
    ) {
        self.fleet.apply(activity, now);
        self.refresh_fleet(now);
    }

    /// Re-resolve the fleet against `now`, raising the host pane while workers are tracked and
    /// retiring it once none are.
    ///
    /// Called on every fleet event **and** on every frame of a running turn, because both lifetime
    /// rules that matter are time-based rather than event-driven: a quiet worker becomes `stalled`
    /// and a finished worker retires with no further event arriving. The loop's 62 ms tick only
    /// runs while a turn is active (`state.running()`), so once a turn ends the pane can outlive
    /// its retention until the next input event wakes the loop — cosmetic, and cheaper than giving
    /// every idle session a wakeup timer it otherwise does not need.
    pub(crate) fn refresh_fleet(&mut self, now: Instant) {
        // Retirement is time-driven: a fleet whose last worker finished sends no further event, so
        // the retention clock has to be advanced here or the pane would never come down.
        self.fleet.prune(now);
        self.fleet_rows = self.fleet.rows(now);
        if self.fleet_rows.is_empty() {
            self.panes.retire_fleet();
        } else {
            self.panes.raise_fleet();
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

    /// Attach the surface's discovered command files (D-186) — listed in `/help` and the slash
    /// menu, dispatched by name from [`handle_command`].
    pub fn with_file_commands(
        mut self,
        file_commands: Vec<flux_runtime::metadata::CommandFile>,
    ) -> Self {
        self.file_commands = file_commands;
        self
    }

    /// The call args of the newest still-running tool entry named `tool` — the approval sheet's
    /// diff-preview source (C-115). The card is pushed when the call dispatches, before its
    /// approval resolves; if event ordering ever leaves no matching entry the sheet just renders
    /// without a preview.
    pub(crate) fn pending_approval_input(&self, tool: &str) -> Option<&serde_json::Value> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Tool(t) if t.name == tool && t.result.is_none() => Some(&t.input),
            _ => None,
        })
    }

    fn mark_transcript_dirty(&mut self) {
        self.transcript_revision = self.transcript_revision.saturating_add(1);
        self.transcript_layout.get_mut().take();
    }

    fn toggle_details(&mut self) {
        self.expand_tools = !self.expand_tools;
        // C-111: the global toggle wins over any per-card overrides — after Ctrl-E every card
        // follows the new global state again.
        for entry in &mut self.entries {
            if let Entry::Tool(tool) = entry {
                tool.expanded = None;
            }
        }
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
    /// C-139: the prompt side is **not** folded here. `u` is the turn's accumulated `Usage`, whose
    /// input/cache fields `Usage::accumulate` leaves holding the last round's snapshot — summing
    /// those across turns under-counts a multi-round session badly. Prompt tiers arrive per model
    /// call through [`Self::record_call_usage`] instead. Generated output and cost stay here: output
    /// is genuinely summed by `accumulate`, and cost is turn spend.
    fn record_usage(&mut self, u: &Usage) {
        self.tokens_out += u.output_tokens;
        self.tokens_reasoning += u.reasoning_tokens;
        if let Some((spec, pricing)) = &self.cost_model {
            match pricing.cost(u, spec) {
                Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                None if flux_core::is_metered_cloud_spec(spec) => self.cost_unpriced = true,
                None => {}
            }
        }
    }

    /// Fold one model call's prompt tiers into the session and current-turn cache accounting, and
    /// record it as a round for the `/usage` overlay (C-139/C-140).
    fn record_call_usage(&mut self, model: &str, stage: &str, operations: usize, u: &Usage) {
        self.cache.add(u);
        self.turn_cache.add(u);
        if self.turn_rounds.len() < MAX_TURN_ROUNDS {
            self.turn_rounds.push(RoundUsage {
                model: model.to_string(),
                stage: stage.to_string(),
                operations,
                usage: u.clone(),
            });
        }
    }

    /// Fold a non-turn provider call into session totals only. `/insights` is not a new user turn
    /// and must not rewrite the per-turn `/usage` overlay the operator just inspected.
    fn record_background_usage(&mut self, model: &str, usage: &Usage) {
        self.cache.add(usage);
        self.tokens_out += usage.output_tokens;
        self.tokens_reasoning += usage.reasoning_tokens;
        if let Some((_, pricing)) = &self.cost_model {
            match pricing.cost(usage, model) {
                Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                None if flux_core::is_metered_cloud_spec(model) => self.cost_unpriced = true,
                None => {}
            }
        }
    }

    /// C-542: fold the engine's published budget projection into the surface.
    ///
    /// The projection *is* the number: this stores the enforcing ledger's snapshot and re-derives
    /// nothing, so the header cannot disagree with the breach that stops the run. A crossed line
    /// arrives with the event that crossed it, so the ledger's one-warning-per-dimension rule is
    /// exactly what the transcript shows — a target is announced once, not once per model call.
    pub(crate) fn record_budget(
        &mut self,
        projection: flux_core::BudgetProjection,
        warning: Option<flux_core::BudgetBreach>,
        exhausted: Option<flux_core::BudgetBreach>,
    ) {
        self.budget = Some(projection);
        if let Some(breach) = warning {
            self.push(Entry::Notice {
                text: format!("⚠ budget target crossed — {breach}; execution continues"),
                sev: Sev::Warn,
            });
        }
        if let Some(breach) = exhausted {
            self.push(Entry::Notice {
                text: format!(
                    "⛔ budget limit reached — {breach}; stopping at the next safe boundary"
                ),
                sev: Sev::Err,
            });
        }
    }

    /// Reset the per-turn cache accounting at the start of a turn. The session totals persist.
    fn begin_turn_usage(&mut self) {
        self.turn_cache = flux_core::CacheEfficiency::default();
        self.turn_rounds.clear();
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
        if self.queue.push(text).is_some() {
            self.queue_sel = self.queue_sel.min(self.queue.len().saturating_sub(1));
        }
    }

    /// Texts currently queued — a point-in-time snapshot; the engine may drain the shared queue
    /// concurrently while a turn runs (A-94).
    fn queue_texts(&self) -> Vec<String> {
        self.queue
            .snapshot()
            .into_iter()
            .map(|item| item.text)
            .collect()
    }

    fn queue_remove_selected(&mut self) -> Option<String> {
        let snapshot = self.queue.snapshot();
        if snapshot.is_empty() {
            return None;
        }
        let index = self.queue_sel.min(snapshot.len() - 1);
        let id = snapshot[index].id;
        if self.queue_edit == Some(id) {
            self.queue_edit = None;
        }
        let removed = self.queue.retract(id);
        self.queue_sel = self.queue_sel.min(self.queue.len().saturating_sub(1));
        removed
    }

    fn queue_begin_edit(&mut self) -> Option<String> {
        let snapshot = self.queue.snapshot();
        if snapshot.is_empty() {
            return None;
        }
        let item = snapshot
            .into_iter()
            .nth(self.queue_sel.min(self.queue.len().saturating_sub(1)))?;
        self.queue_edit = Some(item.id);
        Some(item.text)
    }

    /// `false` when no edit was active — or when the engine consumed the item mid-edit, in which
    /// case the caller treats the text as a fresh submission instead of silently dropping it.
    fn queue_commit_edit(&mut self, text: String) -> bool {
        let Some(id) = self.queue_edit.take() else {
            return false;
        };
        self.queue.edit(id, text)
    }

    fn queue_cancel_edit(&mut self) -> bool {
        self.queue_edit.take().is_some()
    }

    /// A blank-composer, idle Ctrl-C (C-156): the first press arms the quit confirmation and does
    /// NOT exit; a second press within [`CTRL_C_QUIT_WINDOW`] confirms it. A stale arm (the window
    /// elapsed) is treated as no arm at all, so a far-apart pair re-arms rather than quitting on a
    /// press the user has forgotten about.
    fn arm_or_confirm_quit(&mut self) -> CtrlCQuit {
        let now = Instant::now();
        let armed = self
            .ctrl_c_armed_at
            .is_some_and(|at| now.saturating_duration_since(at) < CTRL_C_QUIT_WINDOW);
        if armed {
            CtrlCQuit::Quit
        } else {
            self.ctrl_c_armed_at = Some(now);
            CtrlCQuit::Armed
        }
    }

    /// Clear the transient Ctrl-C quit arm — any key other than a confirming second Ctrl-C
    /// disarms it, and so does the window elapsing (checked lazily wherever the arm is read).
    fn clear_ctrl_c_arm(&mut self) {
        self.ctrl_c_armed_at = None;
    }

    /// Whether the Ctrl-C quit confirmation is currently showing (armed and inside the window) —
    /// what the footer renders and what a second press needs to match.
    fn ctrl_c_armed(&self) -> bool {
        self.ctrl_c_armed_at
            .is_some_and(|at| at.elapsed() < CTRL_C_QUIT_WINDOW)
    }

    fn queue_move(&mut self, delta: isize) {
        let snapshot = self.queue.snapshot();
        if snapshot.len() < 2 {
            return;
        }
        let from = self.queue_sel.min(snapshot.len() - 1);
        let to = from
            .saturating_add_signed(delta)
            .min(snapshot.len().saturating_sub(1));
        if from != to && self.queue.move_by(snapshot[from].id, delta) {
            self.queue_sel = to;
        }
    }
    /// Append a user message, with a compact rule after the preceding turn has completed.
    fn push_user(&mut self, text: impl Into<String>) {
        let previous_user = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::User { .. }));
        let show_separator = previous_user.is_some_and(|user| {
            self.entries[user + 1..]
                .iter()
                .any(|entry| matches!(entry, Entry::Assistant(assistant) if assistant.done))
        });
        self.push(Entry::User {
            text: text.into(),
            prior_elapsed: show_separator.then_some(self.last_elapsed).flatten(),
            show_separator,
        });
    }

    /// Open a fresh thinking entry for the upcoming planning call (called on `Planning(true)`).
    fn begin_thinking(&mut self) {
        // Only open a new thinking entry if there isn't already an open one.
        if !matches!(self.entries.last(), Some(Entry::Thinking { body, .. }) if !body.done) {
            self.entries.push(Entry::Thinking {
                body: Assistant {
                    text: String::new(),
                    done: false,
                    cache: RefCell::new(None),
                },
                call: None,
            });
            self.mark_transcript_dirty();
            self.assistant_open = false;
        }
    }

    /// Append a thinking-token delta to the open thinking entry.
    fn stream_thinking(&mut self, delta: &str) {
        if let Some(Entry::Thinking { body, .. }) = self.entries.last_mut() {
            if !body.done {
                body.text.push_str(delta);
                self.mark_transcript_dirty();
                return;
            }
        }
        // No open thinking entry — open one on the fly.
        self.entries.push(Entry::Thinking {
            body: Assistant {
                text: delta.to_string(),
                done: false,
                cache: RefCell::new(None),
            },
            call: None,
        });
        self.mark_transcript_dirty();
        self.assistant_open = false;
    }

    /// Seal the open thinking entry (called on `Planning(false)`).
    fn end_thinking(&mut self) {
        if let Some(Entry::Thinking { body, .. }) = self.entries.last_mut() {
            if !body.done {
                body.text = body.text.trim_end().to_string();
                body.done = true;
                self.mark_transcript_dirty();
            }
        }
    }

    /// Fold one completed model call into the turn's wait accounting and badge its round (C-180).
    fn record_model_call(&mut self, badge: ModelCallBadge) {
        // A retry that ended the call — rather than being followed by another attempt — never gets
        // a `Planning(false)` of its own to clear the footer badge (C-181).
        self.retry = None;
        self.turn_llm_wait = self.turn_llm_wait.saturating_add(badge.timing.duration);
        self.attach_model_call(badge);
    }

    /// Attach one completed model call's latency badge to the round it measured (C-180).
    ///
    /// Walks back to the newest thinking entry that has no badge yet — `Planning(true)` opens
    /// exactly one per call, and the observation always arrives after that entry sealed. If the
    /// engine ever emitted a `model.call` without a planning bracket, a badge-only entry is created
    /// so the wait is still accounted for rather than silently dropped.
    fn attach_model_call(&mut self, badge: ModelCallBadge) {
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Thinking { call, .. } = entry {
                if call.is_none() {
                    *call = Some(badge);
                    self.mark_transcript_dirty();
                    return;
                }
                break;
            }
        }
        self.entries.push(Entry::Thinking {
            body: Assistant {
                text: String::new(),
                done: true,
                cache: RefCell::new(None),
            },
            call: Some(badge),
        });
        self.mark_transcript_dirty();
        self.assistant_open = false;
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

    /// The `@token` being typed at the cursor (C-112): the whitespace-delimited token ending at
    /// the cursor when it starts with `@` at a token boundary — `@` mid-word (an email) never
    /// triggers. Returns the token INCLUDING the `@`.
    fn at_token(&self) -> Option<String> {
        if self.slash_query().is_some() {
            return None; // the slash menu owns the popup slot
        }
        let (row, col) = self.input.cursor();
        let line = self.input.lines().get(row)?;
        let upto: Vec<char> = line.chars().take(col).collect();
        let start = upto
            .iter()
            .rposition(|c| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let token: String = upto[start..].iter().collect();
        token.starts_with('@').then_some(token)
    }

    /// Ranked completion candidates for the active `@token` (C-112) — empty when no token is
    /// active, it was Esc-dismissed, or the inventory hasn't been built yet.
    fn path_popup_matches(&self) -> Vec<String> {
        let Some(token) = self.at_token() else {
            return Vec::new();
        };
        if self.path_dismissed.as_deref() == Some(token.as_str()) {
            return Vec::new();
        }
        let Some(inventory) = &self.file_inventory else {
            return Vec::new();
        };
        fuzzy_rank(inventory, &token[1..])
            .into_iter()
            .take(50)
            .map(str::to_string)
            .collect()
    }

    /// Sessions in the open picker filtered/ranked by `session_query` (C-153), through the same
    /// `fuzzy_rank` tiering as `@`-path completion and slash-command matching — the ranker stays
    /// one implementation, its callers three. The label ranked against is `"<id> <model>"`, so a
    /// query can find a session by either. An empty query returns every loaded session in its
    /// original (newest-active-first) order, unchanged from before this story. `EventStore::search`
    /// (C-164) is a separate, complementary seam for full conversation-CONTENT search — this
    /// method only ranks/filters the summaries already loaded into the picker (id/model), it does
    /// not re-query the store on every keystroke.
    fn session_picker_matches(&self) -> Vec<&flux_events::SessionSummary> {
        let sessions: &[flux_events::SessionSummary] =
            self.session_picker.as_deref().unwrap_or(&[]);
        if self.session_query.is_empty() {
            return sessions.iter().collect();
        }
        let labels: Vec<String> = sessions
            .iter()
            .map(|s| format!("{} {}", s.id, s.model))
            .collect();
        fuzzy_rank_indices(&labels, &self.session_query)
            .into_iter()
            .map(|i| &sessions[i])
            .collect()
    }

    /// Esc while the session picker is open (C-153): a non-empty typed query is cleared first —
    /// one Esc, one undo step — and the overlay itself only closes on a second Esc once the query
    /// is already empty.
    fn session_esc(&mut self) {
        if !self.session_query.is_empty() {
            self.session_query.clear();
        } else {
            self.session_picker = None;
        }
    }

    /// Replace the active `@token` with `path` (C-112). The composer is rebuilt via the
    /// `set_input` pattern, which leaves the cursor at the end — fine for the tail-of-message
    /// case completion is for.
    fn insert_path_completion(&mut self, path: &str) {
        let (row, col) = self.input.cursor();
        let mut lines: Vec<String> = self.input.lines().to_vec();
        let Some(line) = lines.get_mut(row) else {
            return;
        };
        let chars: Vec<char> = line.chars().collect();
        let upto = &chars[..col.min(chars.len())];
        let start = upto
            .iter()
            .rposition(|c| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix: String = chars[..start].iter().collect();
        let suffix: String = chars[col.min(chars.len())..].iter().collect();
        *line = format!("{prefix}{path}{suffix}");
        let text = lines.join("\n");
        self.set_input(&text);
        self.path_sel = 0;
        self.path_dismissed = None;
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

    /// C-158: record one in-flight output line on the newest still-running card named `name`,
    /// keeping only the last [`MAX_PARTIAL_LINES`].
    ///
    /// C-531 deliberately leaves this on name matching while `finish_tool`/`time_tool` moved to the
    /// dispatch id: a progress line is decoded from a `tool.progress` observation raised inside the
    /// safety envelope, below the interpreter that mints the id, so no id reaches here to match on.
    /// The match is still sound for the only producer — the C-158 bash channel declares
    /// `AccessKind::Process`, which `native_call_parallel_safe` never admits, so two same-name
    /// progress-reporting calls are never in flight together. Plumb the id through
    /// `flux_runtime::ToolProgress` and match on it here the moment a parallel-safe op reports
    /// progress.
    ///
    /// A line arriving after the result landed is dropped: the card has moved on to its real
    /// summary and must not flip back to a partial view.
    fn progress_tool(&mut self, name: &str, line: String) {
        // C-533: strip escapes/control bytes before the line can reach a span — the reporter
        // redacts secrets (C-158) but passes subprocess bytes through verbatim.
        let line = trust::sanitize_tool_output(&line);
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Tool(tool) = entry {
                if tool.result.is_none() && tool.name == name {
                    if tool.partial.len() == MAX_PARTIAL_LINES {
                        tool.partial.remove(0);
                    }
                    tool.partial.push(line);
                    self.mark_transcript_dirty();
                    return;
                }
            }
        }
    }
    /// Attach a result to the card that issued the call. C-531: matched on the call's
    /// [`DispatchId`], never on the op name — the previous "newest still-running card with this
    /// name" scan assumed ops dispatch sequentially, and C-528's parallel gather batches break that
    /// assumption, cross-attaching two concurrent `read`s' bodies.
    fn finish_tool(&mut self, dispatch: DispatchId, name: &str, content: String, is_error: bool) {
        // C-533: the transcript boundary — same sanitation posture as panes, approval prompts
        // and fleet names. Applies equally to the no-matching-card notice fallback below.
        let content = trust::sanitize_tool_output(&content);
        let summary = toolview::format_result(name, &content, is_error);
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Tool(tool) = entry {
                if tool.dispatch != Some(dispatch) {
                    continue;
                }
                // Cancellation is terminal. A tool can finish concurrently with Ctrl-C and its
                // already-queued result may arrive before the turn's `Finished` marker; keep the
                // interrupted card cancelled instead of surfacing that late result as a notice.
                if tool
                    .result
                    .as_ref()
                    .is_some_and(|outcome| outcome.cancelled)
                {
                    return;
                }
                if tool.result.is_none() {
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
                        cancelled: false,
                        elapsed,
                        approval_wait,
                        summary,
                        content,
                    });
                    tool.partial.clear();
                    self.mark_transcript_dirty();
                    return;
                }
            }
        }
        self.push(Entry::Notice {
            text: content,
            sev: if is_error { Sev::Err } else { Sev::Info },
        });
    }

    /// Seal every in-flight tool card when the operator interrupts the turn.
    fn cancel_running_tools(&mut self) {
        let mut changed = false;
        for entry in &mut self.entries {
            if let Entry::Tool(tool) = entry {
                if tool.result.is_none() {
                    tool.result = Some(ToolOutcome {
                        is_error: false,
                        cancelled: true,
                        content: String::new(),
                        summary: None,
                        elapsed: tool.started.elapsed(),
                        approval_wait: None,
                    });
                    tool.partial.clear();
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_transcript_dirty();
        }
    }

    /// C-531: the timing belongs to one dispatch, matched by id like its result.
    fn time_tool(&mut self, dispatch: DispatchId, timing: flux_core::OperationTiming) {
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Tool(tool) = entry {
                if tool.result.is_none() && tool.dispatch == Some(dispatch) {
                    tool.timing = Some(timing);
                    return;
                }
            }
        }
    }

    /// One entry's styled logical lines (pre-wrap) — the per-entry unit
    /// [`Self::ensure_transcript_layout`] wraps chunk-by-chunk (with a blank separator line
    /// between entries) so it can record per-entry row spans (C-111).
    ///
    /// C-149: every line gets a one-column gutter rail (`GUTTER`) prepended as its first span, so
    /// turn boundaries are scannable without adding rows. It's produced here — content generators
    /// below size against `content_width` (`width` minus the rail) — so the rail always lands
    /// inside the row budget `ensure_transcript_layout` wraps at; the wrap + row-span recording
    /// there stays untouched.
    fn entry_lines(&self, entry: &Entry, width: u16) -> Vec<Line<'static>> {
        let t = &self.theme;
        let content_width = width.saturating_sub(GUTTER_COLS);
        let mut out: Vec<Line> = Vec::new();
        match entry {
            Entry::User {
                text,
                prior_elapsed,
                show_separator,
            } => {
                if *show_separator {
                    out.push(turn_separator_line(*prior_elapsed, content_width, t));
                }
                for (j, raw) in text.split('\n').enumerate() {
                    let prefix = if j == 0 { "› " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(prefix, t.user_style()),
                        Span::styled(raw.to_string(), t.user_style()),
                    ]));
                }
            }
            Entry::Assistant(a) => out.extend(a.lines(content_width, t)),
            Entry::Thinking { body, call } => {
                if !body.text.is_empty() {
                    let count = body.text.lines().count().max(1);
                    out.push(Line::styled(
                        if body.done {
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
                        out.extend(body.lines(content_width, t).into_iter().map(|mut l| {
                            for span in &mut l.spans {
                                span.style = span.style.patch(t.muted_style());
                            }
                            l
                        }));
                    }
                }
                // C-180: what this round actually cost in wall clock. Rendered even with no
                // thinking text — a stage without extended thinking still made the user wait.
                if let Some(call) = call {
                    out.push(Line::from(model_call_spans(call, t)));
                }
            }
            Entry::Tool(tool) => out.extend(self.tool_lines(tool, content_width)),
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
                let intent_cap = usize::from(content_width).saturating_sub(12).clamp(24, 160);
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
        let mut out = prepend_gutter(out, gutter_style(entry, t));
        if matches!(
            entry,
            Entry::User {
                show_separator: true,
                ..
            }
        ) {
            if let Some(gutter) = out.first_mut().and_then(|line| line.spans.first_mut()) {
                gutter.style = t.muted_style();
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
        // C-111: wrap chunk-by-chunk (the wrapper is per-line stateless, so this equals wrapping
        // the whole transcript at once) to record each entry's wrapped row span, and paint the
        // focused entry's rows with the selection background. Focus changes bump the revision,
        // so the cache stays coherent.
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut entry_rows: Vec<(usize, u16, u16)> = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            let mut chunk: Vec<Line> = Vec::new();
            if i > 0 {
                chunk.push(Line::default());
            }
            chunk.extend(self.entry_lines(entry, width));
            let mut wrapped = wrap_styled_lines(chunk, width);
            let sep = usize::from(i > 0);
            if self.focused == Some(i) {
                for line in wrapped.iter_mut().skip(sep) {
                    line.style = line.style.bg(self.theme.sel_bg);
                    for span in &mut line.spans {
                        span.style = span.style.bg(self.theme.sel_bg);
                    }
                }
            }
            let start = (lines.len() + sep).min(u16::MAX as usize) as u16;
            let count = wrapped.len().saturating_sub(sep).min(u16::MAX as usize) as u16;
            entry_rows.push((i, start, count));
            lines.extend(wrapped);
        }
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
            // Shift the recorded spans past the drained head; fully-omitted entries drop out.
            entry_rows = entry_rows
                .into_iter()
                .filter_map(|(i, start, count)| {
                    let start = (start as usize).checked_sub(omitted)?;
                    Some((i, (start + 1) as u16, count))
                })
                .collect();
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
                    // fg-only comparison: the C-111 focus highlight patches a bg onto every
                    // span of the focused entry, which must not unpair its running badge.
                    span.content.as_ref() == RUNNING_BADGE
                        && span.style.fg == self.theme.warn_style().fg
                })
            })
            .map(|(row, _)| row as u16)
            .zip(running_entries)
            .collect();
        *self.transcript_layout.borrow_mut() = Some(TranscriptLayout {
            revision: self.transcript_revision,
            width,
            entry_rows,
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
            let mut header = tool_header_line(
                &self.theme,
                &tool.call.verb,
                &tool.call.arg,
                format!("{frame} running · {}", fmt_elapsed(elapsed)),
                self.theme.warn_style(),
                // C-149: same content budget entry_lines used to build the cached row, so the
                // patched row's total width (rail + header) doesn't drift from its neighbors.
                width.saturating_sub(GUTTER_COLS),
                // A running card has no result yet, so no expandable detail — never a marker.
                None,
            );
            // C-149: the cached row carries the rail as its first span (`entry_lines`); the patch
            // rebuilds the whole line, so it must re-add the same span or the row would shift left
            // and lose its rail on every animation frame.
            header
                .spans
                .insert(0, Span::styled(GUTTER, self.theme.muted_style()));
            visible[slot] = header;
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
    /// Move the transcript focus cursor by `delta` entries (C-111): detaches follow, bumps the
    /// revision (the focused entry renders with the selection background), and centers the
    /// entry in the viewport.
    fn focus_move(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let next = match self.focused {
            Some(i) => i.saturating_add_signed(delta).min(last),
            // First press: start at the bottom (newest entry) — that's what the eye is on.
            None => last,
        };
        self.focused = Some(next);
        self.follow = false;
        self.unread = 0;
        self.mark_transcript_dirty();
        self.center_focused_entry();
    }

    /// Cycle failed tool cards, wrapping at either end, and center the selected card.
    fn jump_failure(&mut self, forward: bool) -> bool {
        let failures: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Tool(tool)
                    if tool.result.as_ref().is_some_and(|outcome| outcome.is_error) =>
                {
                    Some(index)
                }
                _ => None,
            })
            .collect();
        let Some(next) = (if forward {
            failures
                .iter()
                .copied()
                .find(|index| self.focused.is_none_or(|current| *index > current))
                .or_else(|| failures.first().copied())
        } else {
            failures
                .iter()
                .rev()
                .copied()
                .find(|index| self.focused.is_none_or(|current| *index < current))
                .or_else(|| failures.last().copied())
        }) else {
            return false;
        };
        self.focused = Some(next);
        self.follow = false;
        self.unread = 0;
        self.mark_transcript_dirty();
        self.center_focused_entry();
        true
    }

    fn focus_clear(&mut self) {
        if self.focused.take().is_some() {
            self.mark_transcript_dirty();
        }
    }

    fn center_focused_entry(&mut self) {
        let Some(idx) = self.focused else { return };
        // The spans live on the layout, which the dirty-bump above just dropped — rebuild at the
        // last known width so centering has fresh rows to aim at.
        let width = self.transcript_layout.borrow().as_ref().map(|l| l.width);
        let width = width.unwrap_or(80);
        self.ensure_transcript_layout(width);
        let layout = self.transcript_layout.borrow();
        let Some((_, start, count)) = layout
            .as_ref()
            .and_then(|l| l.entry_rows.iter().find(|(i, _, _)| *i == idx).copied())
        else {
            return;
        };
        drop(layout);
        let mid = start.saturating_add(count / 2);
        let half = self.last_page.get().max(1) / 2;
        self.scroll = mid.saturating_sub(half).min(self.last_max_scroll.get());
    }

    /// Toggle the focused tool card's per-card expansion (C-111). Returns false when the focus
    /// isn't on a tool card (the key then falls through to the composer).
    fn toggle_focused_card(&mut self) -> bool {
        let Some(idx) = self.focused else {
            return false;
        };
        let global = self.expand_tools;
        if let Some(Entry::Tool(tool)) = self.entries.get_mut(idx) {
            let effective = tool.expanded.unwrap_or(global);
            tool.expanded = Some(!effective);
            self.mark_transcript_dirty();
            return true;
        }
        false
    }

    /// The focused entry's full text for the OSC 52 yank (C-111) — the un-truncated content, not
    /// the wrapped screen rows.
    fn focused_entry_text(&self) -> Option<String> {
        let entry = self.entries.get(self.focused?)?;
        Some(match entry {
            Entry::User { text, .. } => text.clone(),
            Entry::Assistant(a) => a.text.clone(),
            Entry::Thinking { body, .. } => body.text.clone(),
            Entry::Tool(tool) => {
                let header = format!("{} {}", tool.call.verb, tool.call.arg);
                match &tool.result {
                    Some(o) if !o.content.is_empty() => format!("{header}\n{}", o.content),
                    _ => header,
                }
            }
            Entry::Notice { text, .. } => text.clone(),
            Entry::Intent(intent) => intent.intent.clone(),
            Entry::Plan(data) | Entry::GatherPlan(data) => {
                serde_json::to_string_pretty(data).unwrap_or_default()
            }
            Entry::Brief { goal, needs } => format!("{goal}\n{}", needs.join("\n")),
        })
    }

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

        // Badge (right-aligned, fixed idea of width): running is static, terminal states are distinct.
        let (badge, badge_style) = match &tool.result {
            None => (RUNNING_BADGE.to_string(), t.warn_style()),
            Some(o) if o.cancelled => ("⊘ cancelled".to_string(), t.muted_style()),
            Some(o) if o.is_error => (format!("✗ {}", fmt_tool_timing(o)), t.err_style()),
            Some(o) => (format!("✓ {}", fmt_tool_timing(o)), t.ok_style()),
        };

        // C-155: `▸`/`▾` advertises that the card has detail to expand, and which state it's in
        // — a card with nothing expandable (checked via `tool_has_detail`) gets no marker at all,
        // so the affordance is never a false promise.
        let effective_expanded = tool.expanded.unwrap_or(self.expand_tools);
        let marker =
            tool_has_detail(tool).then_some(if effective_expanded { '▾' } else { '▸' });

        out.push(tool_header_line(
            t,
            &tool.call.verb,
            &tool.call.arg,
            badge,
            badge_style,
            width,
            marker,
        ));

        // C-158: while the op is still running, show the tail of what it has produced so far. An op
        // that has produced nothing renders exactly as it did before — no empty placeholder row.
        // These rows sit BELOW the header, so the C-109 badge pairing (which matches each header
        // row by its last span) is unaffected: none of these lines ends with the running badge.
        if tool.result.is_none() {
            for line in &tool.partial {
                out.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        truncate(line, width.saturating_sub(2) as usize),
                        t.muted_style(),
                    ),
                ]));
            }
        }

        // Completed results keep their one-line summary. Cancellation is fully represented by its
        // terminal header badge and deliberately has no result/detail row to expand.
        if let Some(o) = &tool.result {
            if !o.cancelled {
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
                // "tool output in full (no truncation)" is the flag's promise. C-111: a per-card
                // override (Enter on the focused card) beats the global Ctrl-E state.
                if effective_expanded {
                    // C-115: edit/write get a real hunk view (headers, line-number gutter, word-level
                    // intraline emphasis); everything else keeps the flat classified detail.
                    let diff = if o.is_error {
                        None
                    } else {
                        toolview::format_diff(&tool.name, &tool.input)
                    };
                    if let Some(rows) = diff {
                        let cap = if self.verbose { rows.len() } else { MAX_DETAIL };
                        let shown = rows.len().min(cap);
                        for row in rows.iter().take(cap) {
                            out.push(diff_row_line(t, row, "   "));
                        }
                        if rows.len() > shown {
                            out.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    format!("… {} more lines", rows.len() - shown),
                                    t.muted_style(),
                                ),
                            ]));
                        }
                    } else {
                        let detail = toolview::format_detail(
                            &tool.name,
                            &tool.input,
                            &o.content,
                            o.is_error,
                        );
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
                                toolview::DetailKind::Meta | toolview::DetailKind::Hunk => {
                                    t.accent_style()
                                }
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
            }
        }
        // Light palettes need a quiet boundary between completed tool output and prose. Paint
        // only the existing summary/detail rows: headers, in-flight partial output and the
        // transcript gutter remain unchanged, and this adds no layout rows.
        if t.is_light()
            && tool
                .result
                .as_ref()
                .is_some_and(|outcome| !outcome.cancelled)
        {
            for line in out.iter_mut().skip(1) {
                line.style = line.style.bg(t.panel_bg);
                for span in &mut line.spans {
                    span.style = span.style.bg(t.panel_bg);
                }
            }
        }
        out
    }

    /// The top header bar: identity + model on the left, cumulative session tokens on the right.
    fn header_line(&self, width: u16) -> Line<'static> {
        let t = &self.theme;
        let surface = self.operations.as_ref().map(|operations| {
            let wave = operations
                .snapshot
                .active_wave
                .as_ref()
                .map(|wave| wave.id.as_str())
                .unwrap_or("—");
            format!(
                "Fleet main · {} · r{} · F2 · g{}/r{} · wave {}",
                operations.snapshot.connection_label(),
                operations.snapshot.revision,
                operations.snapshot.goals.len(),
                operations.snapshot.goals_revision,
                wave
            )
        });
        let target = self
            .execution_target
            .as_deref()
            .map(|target| format!(" · {target}"))
            .unwrap_or_default();
        let mut identity = Vec::new();
        if let Some(surface) = surface {
            identity.push(surface);
        }
        if !self.session_id.is_empty() {
            identity.push(self.session_id.clone());
        }
        identity.push(format!(
            "{}{}",
            self.model_spec.as_deref().unwrap_or(&self.model),
            target
        ));
        let left = vec![
            Span::styled("flux", t.accent_style().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", identity.join(" · ")), t.muted_style()),
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
        // C-542: the live budget segment sits directly below `auto-ok` in precedence — a declared
        // ceiling the run is about to hit outranks cumulative counters. Its figures are the engine's
        // published projection; the surface adds nothing up itself.
        if let Some(segment) = self.budget_segment() {
            right.push(segment);
        }
        // C-139: `↑` is now every prompt token the session sent (fresh + both cache tiers, summed
        // per model call), so the cache segment's hit % is a share OF it. The old `↑` was the
        // fresh-input side of each turn's last round, and `cache` was read+write added together —
        // which rendered a session reading 3.2M from cache identically to one writing 3.2M into it.
        let prompt = self.cache.prompt_tokens();
        if prompt + self.tokens_out > 0 {
            right.push(vec![Span::styled(
                format!(
                    "Σ ↑{} ↓{} tok",
                    fmt_count(prompt),
                    fmt_count(self.tokens_out)
                ),
                t.muted_style(),
            )]);
            if !self.cache.is_empty() && (self.cache.read > 0 || self.cache.write > 0) {
                right.push(vec![Span::styled(
                    format!(
                        "cache {:.0}% ↺{} ✎{}",
                        self.cache.hit_rate() * 100.0,
                        fmt_count(self.cache.read),
                        fmt_count(self.cache.write)
                    ),
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
        // Standalone identity is the least-precious header segment: visible during ordinary chat,
        // but shed before safety/cost metrics on a narrow or information-dense bar.
        if self.operations.is_none() {
            right.push(vec![Span::styled("standalone", t.muted_style())]);
        }
        // Segment order [auto-ok, budget, tokens, cache, cost, shell, gather, effort, standalone];
        // bar_line drops from the end, so optional identity/badges shed first and auto-ok survives
        // the longest (C-102/C-116).
        for seg in right.iter_mut().skip(1) {
            seg.insert(0, Span::styled(" · ", t.muted_style()));
        }
        bar_line(left, right, width)
    }

    /// C-542: the header's live budget segment — `budget Σ1.6k/4.0k tok`, `budget 3/10 calls`,
    /// `budget 12.0s/1.0m`.
    ///
    /// Every figure comes from the enforcing [`flux_core::BudgetLedger`]'s published projection, so
    /// this surface and the stop that actually fires cannot drift apart. `None` when nothing is
    /// declared: an undeclared dimension renders nothing rather than a reassuring zero ceiling.
    fn budget_segment(&self) -> Option<Vec<Span<'static>>> {
        let t = &self.theme;
        let projection = self.budget.as_ref()?;
        // One bounded segment shows the dimension nearest its declared figure — the one that will
        // bite first. Five competing ratios would not survive a narrow bar anyway.
        let dimension = flux_core::BudgetDimension::ALL
            .into_iter()
            .filter(|dimension| projection.declared(*dimension).is_some())
            .max_by(|a, b| {
                projection
                    .fraction(*a)
                    .unwrap_or(0.0)
                    .total_cmp(&projection.fraction(*b).unwrap_or(0.0))
            })?;
        let declared = projection.declared(dimension)?;
        let spent = projection.spent.get(dimension);
        let figures = match dimension {
            flux_core::BudgetDimension::WallTime => format!(
                "{}/{}",
                fmt_elapsed(Duration::from_millis(spent)),
                fmt_elapsed(Duration::from_millis(declared))
            ),
            flux_core::BudgetDimension::ModelCalls => format!("{spent}/{declared} calls"),
            flux_core::BudgetDimension::InputTokens => {
                format!("↑{}/{} tok", fmt_count(spent), fmt_count(declared))
            }
            flux_core::BudgetDimension::OutputTokens => {
                format!("↓{}/{} tok", fmt_count(spent), fmt_count(declared))
            }
            flux_core::BudgetDimension::TotalTokens => {
                format!("Σ{}/{} tok", fmt_count(spent), fmt_count(declared))
            }
        };
        // The distinction the whole vocabulary turns on stays legible: a crossed hard limit is a stop
        // line and says `limit`; a crossed target is a warning and says that instead.
        let (suffix, style) = if projection
            .exhausted
            .is_some_and(|breach| breach.dimension == dimension)
        {
            (" limit", t.err_style())
        } else if projection
            .warnings
            .iter()
            .any(|breach| breach.dimension == dimension)
        {
            (" over target", t.warn_style())
        } else {
            ("", t.muted_style())
        };
        Some(vec![Span::styled(
            format!("budget {figures}{suffix}"),
            style,
        )])
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
            // C-156: lowest idle-left precedence — the unread indicator and the C-105 mouse-off
            // hint both win over it, since either already occupies the slot for something the
            // user needs to see; the quit confirmation only shows when nothing else claims it.
            Phase::Idle if self.ctrl_c_armed() => {
                vec![Span::styled(" Ctrl-C again to quit", t.warn_style())]
            }
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
                // C-180/C-181: name the wait the user is IN. A pending backoff wins over the model
                // timer — it is the one part of the wait that is not the model thinking, and saying
                // "model 31s" through a retry storm would be actively misleading.
                if let Some(retry) = &self.retry {
                    left.push(Span::styled(
                        format!("  · {}", retry.label()),
                        t.warn_style(),
                    ));
                } else if let Some(started) = self.model_call_start {
                    left.push(Span::styled(
                        format!("  · model {}", fmt_elapsed(started.elapsed())),
                        t.muted_style(),
                    ));
                }
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
            let llm = match self.last_llm_wait {
                Some(wait) => format!(" · llm {}", fmt_elapsed(wait)),
                None => String::new(),
            };
            right.push(vec![Span::styled(
                format!("{} step{plural} · {}{llm}", self.steps, fmt_elapsed(e)),
                t.muted_style(),
            )]);
        }
        let queued = self.queue.len();
        if self.running() && queued > 0 {
            right.push(vec![Span::styled(
                format!("+{queued} queued"),
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
        let mut turn_starts: HashMap<i64, i64> = HashMap::new();
        let mut prior_turn_elapsed = None;
        let mut prior_user_completed = false;
        let mut turn_usage: Vec<(Option<i64>, Usage)> = Vec::new();
        let mut call_usage: Vec<(Option<i64>, String, Usage)> = Vec::new();
        let mut proposed_plan_recorded = false;

        for event in stored {
            match event.kind {
                EventKind::Message(message) => {
                    let text = message.text();
                    if text.trim().is_empty() {
                        continue;
                    }
                    match message.role {
                        flux_core::Role::User => {
                            entries.push(Entry::User {
                                text,
                                prior_elapsed: prior_user_completed
                                    .then_some(prior_turn_elapsed)
                                    .flatten(),
                                show_separator: prior_user_completed,
                            });
                            prior_user_completed = false;
                            prior_turn_elapsed = None;
                        }
                        flux_core::Role::Assistant => {
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
                            prior_user_completed = true;
                        }
                        _ => {}
                    }
                }
                EventKind::Compacted { .. } => entries.push(Entry::Notice {
                    text: "◇ context compacted".into(),
                    sev: Sev::Info,
                }),
                EventKind::TurnStarted { .. } => {
                    turn_starts.insert(event.global_seq, event.ts_ms);
                }
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
                EventKind::TurnEnded { usage, .. } => {
                    if let Some(usage) = usage {
                        turn_usage.push((event.turn_id, usage));
                    }
                    prior_turn_elapsed = event
                        .turn_id
                        .and_then(|turn_id| turn_starts.remove(&turn_id))
                        .map(|started| {
                            Duration::from_millis(event.ts_ms.saturating_sub(started).max(0) as u64)
                        });
                }
                EventKind::CallUsage { model, usage } => {
                    call_usage.push((event.turn_id, model, usage))
                }
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
        self.model_call_start = None;
        self.turn_llm_wait = Duration::ZERO;
        self.last_llm_wait = None;
        self.retry = None;
        self.session_picker = None;
        self.session_sel = 0;
        self.session_query.clear();
        self.plan_phase = None;
        self.execute_rounds = 0;
        self.gather_mode = false;
        self.scroll = 0;
        self.follow = true;
        self.unread = 0;
        self.tokens_out = 0;
        self.tokens_reasoning = 0;
        self.cache = flux_core::CacheEfficiency::default();
        self.turn_cache = flux_core::CacheEfficiency::default();
        self.turn_rounds.clear();
        self.cost_usd = None;
        self.cost_unpriced = false;
        // C-542: the budget projection belongs to the run whose ledger published it, so projecting a
        // different session drops it rather than showing another run's spend.
        self.budget = None;
        // C-221: panes are session-scoped, so projecting a different session (`/resume`) drops them
        // rather than attributing one session's panes to another.
        self.panes.clear();
        // C-224: and so is the fleet — its workers are *this* session's children. Dropping the
        // projection with the panes keeps the host pane from coming back on the next refresh
        // carrying the previous session's workers.
        self.fleet = crate::fleet::FleetProjection::new();
        self.fleet_rows.clear();
        for (_, usage) in &turn_usage {
            self.tokens_out += usage.output_tokens;
            self.tokens_reasoning += usage.reasoning_tokens;
        }
        // Unscoped maintenance calls have no TurnEnded total, so add their generated tokens here;
        // scoped calls are already represented by their owning turn total above.
        for (turn_id, _, usage) in &call_usage {
            if turn_id.is_none() {
                self.tokens_out += usage.output_tokens;
                self.tokens_reasoning += usage.reasoning_tokens;
            }
        }
        // C-139/C-490: use per-call rows for the turns they cover, then retain the old turn-total
        // fallback for every uncovered legacy turn. One unscoped insights call must not suppress a
        // whole old session's prompt accounting.
        let covered_turns: std::collections::HashSet<i64> = call_usage
            .iter()
            .filter_map(|(turn_id, _, _)| *turn_id)
            .collect();
        for (_, _, usage) in &call_usage {
            self.cache.add(usage);
        }
        for (turn_id, usage) in &turn_usage {
            if !turn_id.is_some_and(|id| covered_turns.contains(&id)) {
                self.cache.add(usage);
            }
        }
        if let Some((_, pricing)) = &self.cost_model {
            for (_, model, usage) in &call_usage {
                match pricing.cost(usage, model) {
                    Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                    None if flux_core::is_metered_cloud_spec(model) => self.cost_unpriced = true,
                    None => {}
                }
            }
            if let Some(spec) = self.model_spec.as_deref() {
                for (turn_id, usage) in &turn_usage {
                    if !turn_id.is_some_and(|id| covered_turns.contains(&id)) {
                        match pricing.cost(usage, spec) {
                            Some(money) => *self.cost_usd.get_or_insert(0.0) += money.usd,
                            None if flux_core::is_metered_cloud_spec(spec) => {
                                self.cost_unpriced = true
                            }
                            None => {}
                        }
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
///
/// C-536: continuation rows repeat the logical line's leading rail-and-indent run (the C-149
/// gutter, any nested rail, the card indent) so a wrapped row keeps its left edge instead of
/// dissolving to column 0.
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

        let (prefix, prefix_cols) = hanging_prefix(&spans, max);
        let mut row = Vec::new();
        let mut columns: usize = 0;
        // Columns already occupied when the current row started: 0 for the first row,
        // `prefix_cols` for continuations. A row always accepts at least one character beyond its
        // base, so wrapping makes progress even when the prefix nearly fills the width.
        let mut row_base: usize = 0;
        for span in spans {
            let span_style = span.style;
            let mut chunk = String::new();
            for ch in span.content.chars() {
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if columns > row_base && columns.saturating_add(ch_width) > max {
                    if !chunk.is_empty() {
                        row.push(Span::styled(std::mem::take(&mut chunk), span_style));
                    }
                    out.push(Line {
                        style: line_style,
                        alignment,
                        spans: std::mem::take(&mut row),
                    });
                    row.extend(prefix.iter().cloned());
                    columns = prefix_cols;
                    row_base = prefix_cols;
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

/// C-536: the spans covering a logical line's leading rail-and-indent run — rail glyphs (`│`) and
/// whitespace, kept with their span styles — cloned onto each continuation row by
/// [`wrap_styled_lines`]. Empty when the line has no such run, or when the run would eat the whole
/// width (a degenerate narrow frame wraps content rather than repeating rails it has no room for).
fn hanging_prefix(spans: &[Span<'static>], max: usize) -> (Vec<Span<'static>>, usize) {
    let mut prefix: Vec<Span<'static>> = Vec::new();
    let mut cols = 0usize;
    'spans: for span in spans {
        let mut run = String::new();
        for ch in span.content.chars() {
            if ch == '│' || (ch != '\n' && ch.is_whitespace()) {
                cols += UnicodeWidthChar::width(ch).unwrap_or(0);
                run.push(ch);
            } else {
                if !run.is_empty() {
                    prefix.push(Span::styled(run, span.style));
                }
                break 'spans;
            }
        }
        if !run.is_empty() {
            prefix.push(Span::styled(run, span.style));
        }
    }
    if cols == 0 || cols >= max {
        (Vec::new(), 0)
    } else {
        (prefix, cols)
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

/// One model call's latency badge (C-180): `◇ model explore #2 · 4.2s · ttft 0.9s · ↻ 2 retries`.
///
/// Mirrors the plain CLI's `format_model_call`, minus the schema/op figures the transcript already
/// carries elsewhere. `stage.<name>` is shortened to `<name>` — the prefix is engine bookkeeping and
/// the word "model" already leads the line. Retries are warn-styled: they are the one part of the
/// wait that is not the model thinking.
fn model_call_spans(call: &ModelCallBadge, t: &Theme) -> Vec<Span<'static>> {
    let stage = call.stage.strip_prefix("stage.").unwrap_or(&call.stage);
    let mut spans = vec![Span::styled(
        format!(
            "◇ model {stage} #{} · {}",
            call.round,
            fmt_elapsed(call.timing.duration)
        ),
        t.muted_style(),
    )];
    if let Some(ttft) = call.timing.ttft {
        spans.push(Span::styled(
            format!(" · ttft {}", fmt_elapsed(ttft)),
            t.muted_style(),
        ));
    }
    if call.timing.retries > 0 {
        let plural = if call.timing.retries == 1 { "y" } else { "ies" };
        spans.push(Span::styled(
            format!(" · ↻ {} retr{plural}", call.timing.retries),
            t.warn_style(),
        ));
    }
    spans
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
/// Set by `/restart`, read once the terminal has been handed back.
///
/// A restart cannot happen from inside the event loop: the loop owns the terminal in raw mode on the
/// alternate screen, and a process that replaces itself there leaves the next one drawing into a screen it
/// never set up. So the command only records the intent and asks the loop to end normally, and the re-exec
/// happens after `TerminalGuard::restore` — the same teardown a quit performs.
static RESTART_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Replace this process with the same command line, so a freshly installed binary takes over.
///
/// Exists because the alternative is a documented four-step dance an operator performs by hand — confirm
/// nothing is in flight, stop the surface, install, respawn — and the step everybody forgets is the last
/// one. A surface stopped for an install and never restarted looks exactly like a crash.
///
/// The durable session is on disk, so the replacement resumes the same conversation; nothing about the
/// restart is a fresh start except the executable. Arguments are reused verbatim, which is what makes this
/// safe to offer at all: a restart that quietly changed the model, the fleet root or the posture would be a
/// different session wearing the same name.
#[cfg(unix)]
fn exec_replacement() -> anyhow::Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()
        .map_err(|error| anyhow::Error::new(error).context("resolve the running executable"))?;
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1));
    Err(anyhow::Error::new(command.exec()).context("replace this process"))
}

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

    let mut state = session_state(&agent, &session_id, &options)?;
    // A-94: share the composer's follow-up queue with the engine, which drains it into the
    // running turn at the next planner consultation instead of waiting for the turn to finish.
    agent.set_steering(Some(state.queue.clone()));
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
        EventLoopServices {
            model_resolver: options.model_resolver,
            operations_source: options.operations_source,
            operations_refresh_token: options.operations_refresh_token,
        },
        crossterm::event::EventStream::new(),
    )
    .await;
    let restore = guard.restore(terminal.backend_mut());
    let outcome = result.and(restore);
    // Only after the terminal is back in the shell's hands. A failed exec is reported rather than
    // swallowed: the operator is left in a working shell believing a restart happened, which is worse than
    // an error they can read.
    if RESTART_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        outcome?;
        #[cfg(unix)]
        {
            let error = exec_replacement().expect_err("exec only returns on failure");
            return Err(error);
        }
        #[cfg(not(unix))]
        return Err(anyhow::anyhow!(
            "restart is only implemented where a process can replace itself; quit and relaunch"
        ));
    }
    outcome
}

/// The [`ChatState`] the event loop draws from, assembled from the engine, the session and the
/// launch options — **including C-305's pane-channel install**, the second-to-last link of the
/// `pane.*` delivery chain.
///
/// Split out of [`run_with_options`] because that function cannot be driven from a test at all: it
/// bails without a real TTY on *both* stdin and stdout, and the loop it starts reads that TTY
/// through `crossterm::event::EventStream`. The link is worth pinning rather than reading — handing
/// the loop a state that drains a *different* queue from the one the agent writes to leaves the
/// vocabulary exactly as inert as never registering the ops, and the failure is silence: the model
/// reports a pane it opened and the surface stays blank.
///
/// `pub` only so `flux-cli` — the one crate that can assemble a [`FlowEngine`] to hand it — can
/// call it from a test. It is not part of any surface contract.
#[doc(hidden)]
pub fn session_state(
    agent: &FlowEngine,
    session_id: &str,
    options: &TuiRunOptions,
) -> anyhow::Result<ChatState> {
    let verbose = std::env::var("FLUX_VERBOSE").is_ok_and(|v| flag_on(&v));
    let mut state = ChatState::for_session(agent.model.clone(), session_id.to_string())
        .with_verbose(verbose)
        .with_file_commands(options.file_commands.clone());
    // C-305: connect the pane channel the caller minted before `build_agent` to the state the event
    // loop draws from. Without this the agent's `pane.*` ops enqueue into a channel nobody reads.
    if let Some(queue) = options.pane_queue.clone() {
        state = state.with_pane_queue(queue);
    }
    if let Some(queue) = options.interaction_queue.clone() {
        state = state.with_interaction_queue(queue);
    }
    // C-104: resolve the configured theme for this terminal (NO_COLOR → mono, truecolor → RGB).
    let (theme_name, theme) = resolve_theme(options.theme.as_deref());
    state.theme = theme;
    state.theme_name = theme_name;
    // C-157: the surface's cwd at launch, shown in the empty-transcript orientation card. A
    // single read at startup — same posture as `session_id`/`model`, not re-read per frame.
    state.workspace_root = options.workspace_root.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    });
    // C-116: seed the header mode badges — auto-approve from the launch options, effort from
    // the engine's current setting (later `/effort` changes are mirrored by the handler).
    state.auto_approve = options.auto_approve;
    state.effort = agent.effort.map(|e| e.as_str().to_string());
    if let Some(spec) = options.model_spec.clone() {
        state = state.with_cost(spec, flux_credentials::load_pricing_table());
    }
    state.execution_target = options.execution_target.clone();
    state.project_session(&agent.events, session_id)?;
    state.previous_sessions = previous_session_count(&agent.events, session_id)?;
    state.history = load_history(&agent.events);
    if options.operations_source.is_some() {
        let snapshot = options.operations_initial_snapshot.clone().ok_or_else(|| {
            anyhow::anyhow!("Fleet attachment is missing its initial operations snapshot")
        })?;
        state.operations = Some(crate::operations::OperationsState::loading(snapshot));
    }
    Ok(state)
}

/// Count durable transcripts that the active session can resume through `/sessions`.
///
/// Empty sessions have no transcript to resume, and the active session is never a previous one.
fn previous_session_count(
    events: &flux_events::EventStore,
    active_session_id: &str,
) -> flux_core::Result<usize> {
    const UNBOUNDED: usize = i64::MAX as usize;
    Ok(events
        .list(UNBOUNDED)?
        .into_iter()
        .filter(|session| session.id != active_session_id && session.messages > 0)
        .count())
}

/// C-305: drive the production [`event_loop`] against an in-memory backend over a scripted event
/// stream, and hand back the frame it last drew.
///
/// The loop touches the concrete terminal in exactly one place (`terminal.draw`) and the crossterm
/// event source in exactly one place (`input.next()`) — both are parameters for that reason — and
/// the [`ChatState`] it mutates is already an out-parameter. What that buys is the *last* link of
/// the pane delivery chain, `state.apply_pending_panes()` at the top of the loop, being observable
/// without a TTY; before this existed, deleting that line left every test in the repo green while
/// no model pane could ever reach a terminal.
///
/// An **empty** `events` list runs exactly one frame: the stream yields `None` immediately, which
/// is the loop's own end-of-input exit. The sender half is held for the duration so the `rx` arm of
/// the select cannot win by closing first.
///
/// `pub` only so `flux-cli` — the one crate that can assemble a [`FlowEngine`] to hand it — can
/// call it from a test. Nothing in the shipped binary calls it.
#[doc(hidden)]
pub async fn drive_event_loop_headless(
    agent: FlowEngine,
    state: &mut ChatState,
    events: Vec<crossterm::event::Event>,
) -> anyhow::Result<String> {
    let (tx, rx) = mpsc::unbounded_channel::<UiEvent>();
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(
        HEADLESS_WIDTH,
        HEADLESS_HEIGHT,
    ))?;
    let input = futures_util::stream::iter(events.into_iter().map(Ok::<_, std::io::Error>));
    event_loop(
        &mut terminal,
        Arc::new(tokio::sync::RwLock::new(agent)),
        state,
        tx.clone(),
        rx,
        EventLoopServices {
            model_resolver: None,
            operations_source: None,
            operations_refresh_token: None,
        },
        input,
    )
    .await?;
    drop(tx);
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect())
}

/// Frame size [`drive_event_loop_headless`] renders at — wide and tall enough that a right-slot
/// pane is laid out rather than dropped for want of room.
const HEADLESS_WIDTH: u16 = 120;
const HEADLESS_HEIGHT: u16 = 40;

struct EventLoopServices {
    model_resolver: Option<Arc<dyn ModelResolver>>,
    operations_source: Option<operations::SharedFleetBoardSource>,
    operations_refresh_token: Option<String>,
}

async fn event_loop<B, S>(
    terminal: &mut Terminal<B>,
    agent: Arc<tokio::sync::RwLock<FlowEngine>>,
    state: &mut ChatState,
    tx: mpsc::UnboundedSender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<UiEvent>,
    services: EventLoopServices,
    mut input: S,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    S: futures_util::Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin,
{
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
    use futures_util::StreamExt as _;

    let EventLoopServices {
        model_resolver,
        operations_source,
        operations_refresh_token,
    } = services;

    let mut cancel = CancellationToken::new();
    let mut pending_reply: Option<(String, oneshot::Sender<ApprovalChoice>)> = None;
    let mut approval_queue: VecDeque<PendingApproval> = VecDeque::new();
    let mut pending_interaction_reply: Option<oneshot::Sender<flux_runtime::InteractionResponse>> =
        None;
    // A message typed while a turn was running, started as soon as the turn finishes.
    let mut pending_ui: Option<UiEvent> = None;
    let mut exit_after_finish = false;
    // The action whose cancellation token the operator triggered. Keep this until `Finished` so
    // tool-call events already in flight behind the keypress are sealed too.
    let mut interrupted_action_id = None;
    let mut operations_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    operations_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut operations_refresh_in_flight = false;
    let mut operations_force_refresh_pending = false;
    let mut operations_last_refresh_token = operations_refresh_token;
    if let Some(source) = operations_source.as_ref() {
        spawn_operations_snapshot(&tx, source.clone());
        operations_refresh_in_flight = true;
    }

    loop {
        // C-305: the agent's pane commands, applied BEFORE this iteration's UI events on purpose —
        // `Finished` clears `turn`-lifetime panes, and draining after it would let a command from
        // the turn that just ended reopen one that should have expired with it.
        state.apply_pending_panes();
        if pending_interaction_reply
            .as_ref()
            .is_some_and(oneshot::Sender::is_closed)
        {
            pending_interaction_reply = None;
            state.interaction = None;
        }
        if state.interaction.is_none() && state.approval.is_none() {
            if let Some((request, reply)) = state
                .interaction_queue
                .as_ref()
                .and_then(|queue| queue.pop())
            {
                state.interaction = Some(interaction::InteractionView::new(request));
                pending_interaction_reply = Some(reply);
            }
        }
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
                        // C-180: the footer's live model timer runs off this bracket, which is the
                        // engine's own model-stage scope — not a guess at when inference began.
                        state.model_call_start = Some(Instant::now());
                    } else {
                        // Planning done: seal the thinking entry and move to Thinking phase
                        // (the engine will emit text_delta or another Planning shortly).
                        state.end_thinking();
                        state.phase = Phase::Thinking;
                        state.model_call_start = None;
                        state.retry = None;
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
                UiEvent::ToolCall {
                    dispatch,
                    name,
                    input,
                } => {
                    state.steps += 1;
                    state.push(Entry::Tool(ToolEntry::new(dispatch, name, input)));
                }
                UiEvent::ToolProgress { name, line } => state.progress_tool(&name, line),
                UiEvent::ToolTiming { dispatch, timing } => state.time_tool(dispatch, timing),
                UiEvent::ToolResult {
                    dispatch,
                    name,
                    content,
                    is_error,
                } => state.finish_tool(dispatch, &name, content, is_error),
                UiEvent::Usage(u) => state.record_usage(&u),
                UiEvent::CallUsage {
                    model,
                    stage,
                    round,
                    operations,
                    usage,
                    timing,
                } => {
                    state.record_call_usage(&model, &stage, operations, &usage);
                    state.record_model_call(ModelCallBadge {
                        stage,
                        round,
                        timing,
                    });
                }
                UiEvent::BackgroundUsage { model, usage } => {
                    state.record_background_usage(&model, &usage);
                }
                UiEvent::Budget {
                    projection,
                    warning,
                    exhausted,
                } => state.record_budget(*projection, warning, exhausted),
                UiEvent::Retry {
                    attempt,
                    max_attempts,
                    delay,
                    reason,
                } => {
                    state.retry = Some(RetryView {
                        attempt,
                        max_attempts,
                        delay,
                        reason,
                    })
                }
                UiEvent::Notice { text, sev } => {
                    if sev == Sev::Err {
                        if let Some(operations) = state.operations.as_mut() {
                            operations.turn_failed = true;
                        }
                    }
                    state.push(Entry::Notice { text, sev })
                }
                UiEvent::Approval { request, reply } => {
                    approval_queue.push_back((request, reply));
                    show_next_approval(state, &mut pending_reply, &mut approval_queue);
                }
                UiEvent::SpawnActivity(activity) => {
                    state.record_spawn_activity(&activity, Instant::now())
                }
                UiEvent::OperationsSnapshot {
                    result,
                    refresh_token,
                } => {
                    operations_refresh_in_flight = false;
                    if let Some(token) = refresh_token {
                        operations_last_refresh_token = Some(token);
                    }
                    match *result {
                        Ok(snapshot) => {
                            if let Some(operations) = state.operations.as_mut() {
                                operations.refresh(snapshot);
                            }
                        }
                        Err(error) => {
                            if let Some(operations) = state.operations.as_mut() {
                                operations.refresh_failed(error);
                            }
                        }
                    }
                    if operations_force_refresh_pending {
                        operations_force_refresh_pending = false;
                        if let Some(source) = operations_source.as_ref() {
                            source.invalidate_snapshot_cache();
                            spawn_operations_snapshot(&tx, source.clone());
                            operations_refresh_in_flight = true;
                        }
                    }
                }
                UiEvent::Steered(messages) => {
                    // The engine consumed these from the shared queue (the strip empties by
                    // itself); leave a transcript record that the running turn was steered.
                    for text in messages {
                        acknowledge_steered_requirement(state, operations_source.as_ref(), &text);
                        state.push(Entry::Notice {
                            text: format!("↪ steering delivered: {text}"),
                            sev: Sev::Info,
                        });
                    }
                }
                UiEvent::Finished => {
                    if complete_attached_requirements(state, operations_source.as_ref()) {
                        request_operations_snapshot(
                            &tx,
                            operations_source.as_ref(),
                            &mut operations_refresh_in_flight,
                            &mut operations_force_refresh_pending,
                            true,
                        );
                    }
                    seal_interrupted_action(state, &mut interrupted_action_id);
                    if let Some((_tool, reply)) = pending_reply.take() {
                        let _ = reply.send(ApprovalChoice::Deny);
                    }
                    for (_request, reply) in approval_queue.drain(..) {
                        let _ = reply.send(ApprovalChoice::Deny);
                    }
                    state.approval = None;
                    state.end_stream();
                    state.phase = Phase::Idle;
                    state.last_elapsed = state.turn_start.map(|s| s.elapsed());
                    state.last_llm_wait =
                        (!state.turn_llm_wait.is_zero()).then_some(state.turn_llm_wait);
                    state.model_call_start = None;
                    state.retry = None;
                    state.turn_start = None;
                    state.active_action_id = None;
                    // C-221: a `turn`-lifetime pane does not outlive the turn that opened it. This
                    // is the surface's one turn-termination path (normal stop, cancel and error all
                    // arrive as `Finished`), so clearing here covers every way a turn can end.
                    state.panes.end_turn();
                    // A queued message starts only after the prior task's Finished marker.
                    if !state.queue_open && state.queue_edit.is_none() {
                        if let Some(queued) = state.queue.pop_front() {
                            cancel = start_conversation_turn(
                                &agent,
                                &tx,
                                state,
                                queued,
                                operations_source.as_ref(),
                            );
                        }
                    }
                }
            }
        }

        if exit_after_finish && !state.running() {
            break;
        }

        // C-224: re-resolve the fleet before drawing, so worker ages, the stalled threshold and the
        // running indicator advance with the frame rather than only when a child reports.
        if !state.fleet.is_empty() {
            state.refresh_fleet(Instant::now());
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
            _ = operations_tick.tick(), if operations_source.is_some() => {
                if !operations_refresh_in_flight {
                    if let Some(source) = operations_source.as_ref() {
                        match source.refresh_token() {
                            Ok(token) if Some(&token) != operations_last_refresh_token.as_ref() => {
                                spawn_operations_snapshot(&tx, source.clone());
                                operations_refresh_in_flight = true;
                            }
                            Ok(_) => {}
                            Err(error) => {
                                if let Some(operations) = state.operations.as_mut() {
                                    operations.refresh_failed(error.to_string());
                                }
                            }
                        }
                    }
                }
                continue;
            },
        };
        match ev {
            Event::Resize(_, _) => continue,
            Event::Paste(text) => {
                if let Some(view) = state.interaction.as_mut() {
                    match &view.control {
                        interaction::InteractionControl::Json => view.input.push_str(&text),
                        interaction::InteractionControl::Form(fields) => {
                            let index = view.selected.min(fields.len().saturating_sub(1));
                            if matches!(
                                fields[index].control,
                                interaction::FormFieldControl::String
                                    | interaction::FormFieldControl::Integer
                                    | interaction::FormFieldControl::Number
                            ) {
                                view.form_inputs[index].push_str(&text);
                            }
                        }
                        _ => {}
                    }
                } else {
                    state.input.insert_str(text);
                }
                continue;
            }
            Event::Mouse(m) => {
                if state
                    .operations
                    .as_ref()
                    .is_some_and(|operations| operations.open)
                {
                    if let Some(operations) = state.operations.as_mut() {
                        match m.kind {
                            MouseEventKind::ScrollUp => operations.move_selection(-1),
                            MouseEventKind::ScrollDown => operations.move_selection(1),
                            _ => {}
                        }
                    }
                    continue;
                }
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

                // C-156: any key other than Ctrl-C itself disarms a pending quit confirmation —
                // checked up front so it applies uniformly regardless of which mode below ends up
                // consuming the key (overlay, search, composer, …).
                if !(key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    state.clear_ctrl_c_arm();
                }

                // Approval sheet: only explicit keys act; anything else is swallowed so a stray
                // keystroke can't silently deny (C-103).
                if let Some(view) = state.approval.as_mut() {
                    // C-113: the `d` reason-input line takes every key while active — Enter
                    // resolves the denial carrying the reason, Esc returns to the sheet with the
                    // approval still pending (the reply oneshot stays unresolved).
                    if let Some(reason) = view.reason.as_mut() {
                        match key.code {
                            KeyCode::Enter => {
                                let reason = reason.trim().to_string();
                                if let Some((_, reply)) = pending_reply.take() {
                                    let choice = if reason.is_empty() {
                                        ApprovalChoice::Deny
                                    } else {
                                        ApprovalChoice::DenyWithReason(reason)
                                    };
                                    let _ = reply.send(choice);
                                }
                                state.approval = None;
                                show_next_approval(state, &mut pending_reply, &mut approval_queue);
                            }
                            KeyCode::Esc => view.reason = None,
                            KeyCode::Backspace => {
                                reason.pop();
                            }
                            KeyCode::Char(c) => reason.push(c),
                            _ => {}
                        }
                        continue;
                    }
                    match approval_key(key.code) {
                        ApprovalAction::Ignore => {}
                        ApprovalAction::Scroll(delta) => {
                            view.scroll = view
                                .scroll
                                .saturating_add_signed(delta)
                                .min(view.request.subjects.len().saturating_sub(1));
                        }
                        ApprovalAction::DenyWithReason => view.reason = Some(String::new()),
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

                // Typed question: native yes/no, single-select and multi-select controls; schemas
                // outside that common set get a JSON editor. Invalid values keep this sheet open.
                if let Some(view) = state.interaction.as_mut() {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let interrupt_turn = key.code == KeyCode::Char('c') && ctrl;
                    let mut answer = None;
                    let mut assemble_form = false;
                    let mut cancel_interaction = false;
                    match &mut view.control {
                        interaction::InteractionControl::Boolean => match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                answer = Some(serde_json::Value::Bool(true))
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') => {
                                answer = Some(serde_json::Value::Bool(false))
                            }
                            KeyCode::Esc => cancel_interaction = true,
                            _ => {}
                        },
                        interaction::InteractionControl::Single(options) => match key.code {
                            KeyCode::Up => {
                                view.selected = view.selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                view.selected =
                                    (view.selected + 1).min(options.len().saturating_sub(1));
                            }
                            KeyCode::Enter => answer = options.get(view.selected).cloned(),
                            KeyCode::Esc => cancel_interaction = true,
                            _ => {}
                        },
                        interaction::InteractionControl::Multi(options) => match key.code {
                            KeyCode::Up => {
                                view.selected = view.selected.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                view.selected =
                                    (view.selected + 1).min(options.len().saturating_sub(1));
                            }
                            KeyCode::Char(' ') => {
                                if let Some(checked) = view.checked.get_mut(view.selected) {
                                    *checked = !*checked;
                                }
                            }
                            KeyCode::Enter => {
                                answer = Some(serde_json::Value::Array(
                                    options
                                        .iter()
                                        .zip(&view.checked)
                                        .filter(|(_, checked)| **checked)
                                        .map(|(value, _)| value.clone())
                                        .collect(),
                                ));
                            }
                            KeyCode::Esc => cancel_interaction = true,
                            _ => {}
                        },
                        interaction::InteractionControl::Form(fields) => {
                            let field_index = view.selected.min(fields.len().saturating_sub(1));
                            match key.code {
                                KeyCode::Up => {
                                    view.selected = view.selected.saturating_sub(1);
                                }
                                KeyCode::Down => {
                                    view.selected =
                                        (view.selected + 1).min(fields.len().saturating_sub(1));
                                }
                                KeyCode::Left | KeyCode::Right => {
                                    let forward = key.code == KeyCode::Right;
                                    match &fields[field_index].control {
                                        interaction::FormFieldControl::Boolean => {
                                            let value = match view.form_values[field_index]
                                                .as_ref()
                                                .and_then(serde_json::Value::as_bool)
                                            {
                                                Some(current) => !current,
                                                None => forward,
                                            };
                                            view.form_values[field_index] = Some(value.into());
                                        }
                                        interaction::FormFieldControl::Single(options) => {
                                            let cursor = &mut view.form_cursors[field_index];
                                            *cursor = if forward {
                                                (*cursor + 1).min(options.len().saturating_sub(1))
                                            } else {
                                                cursor.saturating_sub(1)
                                            };
                                            view.form_values[field_index] =
                                                options.get(*cursor).cloned();
                                        }
                                        interaction::FormFieldControl::Multi(options) => {
                                            let cursor = &mut view.form_cursors[field_index];
                                            *cursor = if forward {
                                                (*cursor + 1).min(options.len().saturating_sub(1))
                                            } else {
                                                cursor.saturating_sub(1)
                                            };
                                        }
                                        _ => {}
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    if let interaction::FormFieldControl::Multi(options) =
                                        &fields[field_index].control
                                    {
                                        let cursor = view.form_cursors[field_index];
                                        if let Some(checked) =
                                            view.form_checked[field_index].get_mut(cursor)
                                        {
                                            *checked = !*checked;
                                        }
                                        view.form_values[field_index] =
                                            Some(serde_json::Value::Array(
                                                options
                                                    .iter()
                                                    .zip(&view.form_checked[field_index])
                                                    .filter(|(_, checked)| **checked)
                                                    .map(|(value, _)| value.clone())
                                                    .collect(),
                                            ));
                                    } else if matches!(
                                        fields[field_index].control,
                                        interaction::FormFieldControl::String
                                    ) {
                                        view.form_inputs[field_index].push(' ');
                                    }
                                }
                                KeyCode::Backspace => {
                                    if matches!(
                                        fields[field_index].control,
                                        interaction::FormFieldControl::String
                                            | interaction::FormFieldControl::Integer
                                            | interaction::FormFieldControl::Number
                                    ) {
                                        view.form_inputs[field_index].pop();
                                    }
                                }
                                KeyCode::Char(c)
                                    if !ctrl
                                        && matches!(
                                            fields[field_index].control,
                                            interaction::FormFieldControl::String
                                                | interaction::FormFieldControl::Integer
                                                | interaction::FormFieldControl::Number
                                        ) =>
                                {
                                    view.form_inputs[field_index].push(c)
                                }
                                KeyCode::Enter => assemble_form = true,
                                KeyCode::Esc => cancel_interaction = true,
                                _ => {}
                            }
                        }
                        interaction::InteractionControl::Json => match key.code {
                            KeyCode::Enter => match serde_json::from_str(&view.input) {
                                Ok(value) => answer = Some(value),
                                Err(error) => view.error = Some(format!("invalid JSON: {error}")),
                            },
                            KeyCode::Backspace => {
                                view.input.pop();
                            }
                            KeyCode::Char(c) if !ctrl => view.input.push(c),
                            KeyCode::Esc => cancel_interaction = true,
                            _ => {}
                        },
                    }
                    if assemble_form {
                        match view.form_value() {
                            Ok(value) => answer = Some(value),
                            Err(error) => view.error = Some(error),
                        }
                    }
                    if interrupt_turn {
                        cancel_interaction = true;
                    }
                    if let Some(value) = answer {
                        let oversized = serde_json::to_vec(&value)
                            .map(|encoded| {
                                encoded.len() > flux_runtime::MAX_INTERACTION_RESPONSE_BYTES
                            })
                            .unwrap_or(true);
                        if oversized {
                            view.error = Some(format!(
                                "response exceeds {} bytes",
                                flux_runtime::MAX_INTERACTION_RESPONSE_BYTES
                            ));
                        } else {
                            match jsonschema::validator_for(&view.request.schema) {
                                Ok(validator) if validator.is_valid(&value) => {
                                    if let Some(reply) = pending_interaction_reply.take() {
                                        let _ = reply
                                            .send(interaction::InteractionView::response(value));
                                    }
                                    state.interaction = None;
                                }
                                Ok(validator) => {
                                    view.error = validator
                                        .iter_errors(&value)
                                        .next()
                                        .map(|error| error.to_string())
                                        .or_else(|| Some("value does not match the schema".into()));
                                }
                                Err(error) => view.error = Some(format!("invalid schema: {error}")),
                            }
                        }
                    } else if cancel_interaction {
                        if let Some(reply) = pending_interaction_reply.take() {
                            let _ = reply.send(flux_runtime::InteractionResponse::Cancelled);
                        }
                        state.interaction = None;
                        if interrupt_turn {
                            interrupt_active_action(state, &cancel, &mut interrupted_action_id);
                        }
                    }
                    continue;
                }

                // C-518: historical observatory. Every key is handled here so chat state cannot
                // drift while the metadata-only analysis view has focus.
                if let Some(view) = state.observatory.as_mut() {
                    let seek = (view.clock.range.duration_ms() / 20).max(1);
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => state.observatory = None,
                        KeyCode::Char(' ') => view.clock.toggle(),
                        KeyCode::Char('r') => view.clock.restart(),
                        KeyCode::Left => view.clock.seek(-seek),
                        KeyCode::Right => view.clock.seek(seek),
                        KeyCode::Char('+') | KeyCode::Char('=') => view.change_speed(true),
                        KeyCode::Char('-') => view.change_speed(false),
                        KeyCode::Char('f') => view.clock.fit_to(15_000),
                        KeyCode::Char('4') => view.set_window(
                            flux_capabilities::usage_observatory::UsageRange::FOUR_HOURS_MS,
                        ),
                        KeyCode::Char('1') => view
                            .set_window(flux_capabilities::usage_observatory::UsageRange::DAY_MS),
                        KeyCode::Char('7') => view
                            .set_window(flux_capabilities::usage_observatory::UsageRange::WEEK_MS),
                        KeyCode::Char('g') => view.cycle_group(),
                        KeyCode::Char('m') => {
                            view.clock.reduced_motion = !view.clock.reduced_motion
                        }
                        KeyCode::Up => view.focused = view.focused.saturating_sub(1),
                        KeyCode::Down => view.focused = view.focused.saturating_add(1),
                        _ => {}
                    }
                    continue;
                }

                // C-556/C-557: the operations overlay is a surface-owned, typed projection. It
                // captures every key while open; only a twice-confirmed decision reaches a write.
                if key.code == KeyCode::F(2) && state.operations.is_some() {
                    if let Some(operations) = state.operations.as_mut() {
                        operations.open = !operations.open;
                        operations.detail_open = false;
                        operations.confirm_decision = false;
                    }
                    continue;
                }
                if state
                    .operations
                    .as_ref()
                    .is_some_and(|operations| operations.open)
                {
                    let mut decide = None;
                    let mut refresh = false;
                    if let Some(operations) = state.operations.as_mut() {
                        match key.code {
                            KeyCode::Esc if operations.confirm_decision => {
                                operations.confirm_decision = false
                            }
                            KeyCode::Esc if operations.detail_open => {
                                operations.detail_open = false;
                                operations.confirm_decision = false;
                            }
                            KeyCode::Esc | KeyCode::F(2) | KeyCode::Char('q') => {
                                operations.open = false;
                                operations.detail_open = false;
                                operations.confirm_decision = false;
                            }
                            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                let tab = operations.tab.cycle(-1);
                                operations.select_tab(tab);
                            }
                            KeyCode::Tab => {
                                let tab = operations.tab.cycle(1);
                                operations.select_tab(tab);
                            }
                            KeyCode::Char('1') => {
                                operations.select_tab(crate::operations::OperationsTab::Overview)
                            }
                            KeyCode::Char('2') => {
                                operations.select_tab(crate::operations::OperationsTab::Board)
                            }
                            KeyCode::Char('3') => {
                                operations.select_tab(crate::operations::OperationsTab::Workers)
                            }
                            KeyCode::Char('4') => {
                                operations.select_tab(crate::operations::OperationsTab::Decisions)
                            }
                            KeyCode::Char('5') => {
                                operations.select_tab(crate::operations::OperationsTab::Stats)
                            }
                            KeyCode::Up => operations.move_selection(-1),
                            KeyCode::Down => operations.move_selection(1),
                            KeyCode::PageUp => operations.move_selection(-10),
                            KeyCode::PageDown => operations.move_selection(10),
                            KeyCode::Left
                                if operations.detail_open
                                    && operations.tab
                                        == crate::operations::OperationsTab::Decisions =>
                            {
                                operations.decision_option =
                                    operations.decision_option.saturating_sub(1);
                                operations.confirm_decision = false;
                            }
                            KeyCode::Right
                                if operations.detail_open
                                    && operations.tab
                                        == crate::operations::OperationsTab::Decisions =>
                            {
                                let options = operations
                                    .selected_decision()
                                    .map_or(0, |decision| decision.options.len());
                                operations.decision_option =
                                    (operations.decision_option + 1).min(options.saturating_sub(1));
                                operations.confirm_decision = false;
                            }
                            KeyCode::Enter
                                if operations.detail_open
                                    && operations.tab
                                        == crate::operations::OperationsTab::Decisions =>
                            {
                                decide = operations.confirm_selected_decision();
                            }
                            KeyCode::Enter => operations.detail_open = true,
                            KeyCode::Char('r') => refresh = true,
                            _ => {}
                        }
                    }
                    if let Some((decision_ref, outcome)) = decide {
                        match operations_source
                            .as_ref()
                            .ok_or_else(|| {
                                anyhow::anyhow!("Board/Fleet operations source is unavailable")
                            })
                            .and_then(|source| source.decide(&decision_ref, &outcome))
                        {
                            Ok(ack) => {
                                if let Some(operations) = state.operations.as_mut() {
                                    operations.last_ack = Some(ack);
                                    operations.confirm_decision = false;
                                    operations.detail_open = false;
                                }
                                request_operations_snapshot(
                                    &tx,
                                    operations_source.as_ref(),
                                    &mut operations_refresh_in_flight,
                                    &mut operations_force_refresh_pending,
                                    true,
                                );
                            }
                            Err(error) => {
                                if let Some(operations) = state.operations.as_mut() {
                                    operations.refresh_error = Some(error.to_string());
                                    operations.confirm_decision = false;
                                }
                            }
                        }
                    } else if refresh {
                        request_operations_snapshot(
                            &tx,
                            operations_source.as_ref(),
                            &mut operations_refresh_in_flight,
                            &mut operations_force_refresh_pending,
                            true,
                        );
                    }
                    continue;
                }

                // C-140: usage overlay — Esc/q/Enter close, everything else is swallowed.
                if state.usage_open {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                        state.usage_open = false;
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
                    // C-153: rows/selection always go through the same filtered/ranked view the
                    // renderer draws — typing narrows the set, so Up/Down/Enter must clamp and
                    // resolve against it rather than the raw loaded list.
                    let matches_len = state.session_picker_matches().len();
                    let sel = state.session_sel.min(matches_len.saturating_sub(1));
                    match key.code {
                        KeyCode::Esc => state.session_esc(),
                        KeyCode::Up => {
                            state.session_sel = sel.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            state.session_sel = (sel + 1).min(matches_len.saturating_sub(1));
                        }
                        KeyCode::Backspace => {
                            state.session_query.pop();
                        }
                        KeyCode::Char(c) => {
                            state.session_query.push(c);
                        }
                        KeyCode::Enter if state.running() => state.push(Entry::Notice {
                            text: "session switching waits for the active action to finish".into(),
                            sev: Sev::Warn,
                        }),
                        KeyCode::Enter => {
                            let selected = state
                                .session_picker_matches()
                                .get(sel)
                                .map(|session| session.id.clone());
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
                                    cancel = start_conversation_turn(
                                        &agent,
                                        &tx,
                                        state,
                                        next,
                                        operations_source.as_ref(),
                                    );
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
                // Paging and failed-card navigation work whether or not a turn is running.
                if handle_failure_navigation_key(state, key) {
                    continue;
                }
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
                if state.queue_edit.is_none() {
                    if let Some(query) = state.slash_query() {
                        let matches = slash_matches(&query, &state.file_commands);
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
                                    let name = matches[state.slash_sel.min(matches.len() - 1)]
                                        .name
                                        .clone();
                                    let needs_arg = matches!(name.as_str(), "model" | "resume")
                                        || state
                                            .file_commands
                                            .iter()
                                            .any(|c| c.name == name && !c.argument_hint.is_empty());
                                    state.set_input(&format!(
                                        "/{name}{}",
                                        if needs_arg { " " } else { "" }
                                    ));
                                    state.slash_sel = 0;
                                    continue;
                                }
                                KeyCode::Enter => {
                                    let name = matches[state.slash_sel.min(matches.len() - 1)]
                                        .name
                                        .clone();
                                    state.set_input(&format!("/{name}"));
                                    state.slash_sel = 0;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // C-112: `@` path completion. The inventory is built lazily on the first active
                // token (bounded walk, session-cached); popup keys win while candidates show.
                if state.queue_edit.is_none() {
                    if let Some(token) = state.at_token() {
                        if state.file_inventory.is_none() {
                            let root = if state.workspace_root.is_empty() {
                                std::env::current_dir().unwrap_or_else(|_| ".".into())
                            } else {
                                std::path::PathBuf::from(&state.workspace_root)
                            };
                            state.file_inventory = Some(Arc::new(workspace_file_inventory(
                                &root,
                                PATH_INVENTORY_CAP,
                            )));
                        }
                        let matches = state.path_popup_matches();
                        if !matches.is_empty() {
                            match key.code {
                                KeyCode::Up => {
                                    state.path_sel = state.path_sel.saturating_sub(1);
                                    continue;
                                }
                                KeyCode::Down => {
                                    state.path_sel = (state.path_sel + 1).min(matches.len() - 1);
                                    continue;
                                }
                                KeyCode::Esc => {
                                    state.path_dismissed = Some(token);
                                    state.path_sel = 0;
                                    continue;
                                }
                                KeyCode::Tab | KeyCode::Enter => {
                                    let path =
                                        matches[state.path_sel.min(matches.len() - 1)].clone();
                                    state.insert_path_completion(&path);
                                    continue;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // C-111: transcript entry focus. Shift-↑/↓ always move the cursor; Enter/y/Esc
                // act only while focus is active — plain typing keeps going to the composer.
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    && matches!(key.code, KeyCode::Up | KeyCode::Down)
                {
                    state.focus_move(if key.code == KeyCode::Up { -1 } else { 1 });
                    continue;
                }
                if state.focused.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            state.focus_clear();
                            continue;
                        }
                        KeyCode::Enter if key.modifiers.is_empty() => {
                            if state.toggle_focused_card() {
                                continue;
                            }
                        }
                        KeyCode::Char('y') if key.modifiers.is_empty() => {
                            if let Some(text) = state.focused_entry_text() {
                                match osc52_copy(&text) {
                                    Some(seq) => {
                                        use std::io::Write;
                                        let mut out = std::io::stdout();
                                        let _ = out.write_all(seq.as_bytes());
                                        let _ = out.flush();
                                        let n = text.lines().count().max(1);
                                        state.push(Entry::Notice {
                                            text: format!(
                                                "copied {n} line{}",
                                                if n == 1 { "" } else { "s" }
                                            ),
                                            sev: Sev::Info,
                                        });
                                    }
                                    None => state.push(Entry::Notice {
                                        text: format!(
                                            "entry too large to copy (> {} KiB)",
                                            OSC52_MAX_TEXT / 1024
                                        ),
                                        sev: Sev::Warn,
                                    }),
                                }
                            }
                            continue;
                        }
                        _ => {}
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
                                    cancel = start_conversation_turn(
                                        &agent,
                                        &tx,
                                        state,
                                        next,
                                        operations_source.as_ref(),
                                    );
                                }
                            }
                        } else if state.slash_query().is_some() {
                            state.input = fresh_textarea();
                        }
                    }
                    KeyCode::Char('d') if ctrl && !running && state.input_blank() => break,
                    KeyCode::Up if cur_row == 0 && !ctrl && state.queue_edit.is_none() => {
                        state.history_prev()
                    }
                    KeyCode::Down if cur_row == last_row && !ctrl && state.queue_edit.is_none() => {
                        state.history_next()
                    }
                    KeyCode::Char('c') if ctrl => {
                        if running {
                            state.clear_ctrl_c_arm();
                            interrupt_active_action(state, &cancel, &mut interrupted_action_id);
                            state.push(Entry::Notice {
                                text: "(interrupting…)".into(),
                                sev: Sev::Info,
                            });
                        } else if state.input_blank() {
                            if state.queue_cancel_edit() {
                                state.clear_ctrl_c_arm();
                                state.input = fresh_textarea();
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_conversation_turn(
                                        &agent,
                                        &tx,
                                        state,
                                        next,
                                        operations_source.as_ref(),
                                    );
                                }
                            } else {
                                // C-156: blank composer, idle, nothing to cancel — the one
                                // destructive-feeling key in the map now needs a second press
                                // within CTRL_C_QUIT_WINDOW (armed state shown in the footer)
                                // rather than exiting on a single keystroke.
                                match state.arm_or_confirm_quit() {
                                    CtrlCQuit::Quit => break,
                                    CtrlCQuit::Armed => {}
                                }
                            }
                        } else {
                            let cancelled_edit = state.queue_cancel_edit();
                            state.clear_ctrl_c_arm();
                            state.input = fresh_textarea(); // non-empty line → clear it
                            if cancelled_edit {
                                if let Some(next) = state.queue.pop_front() {
                                    cancel = start_conversation_turn(
                                        &agent,
                                        &tx,
                                        state,
                                        next,
                                        operations_source.as_ref(),
                                    );
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
                                    cancel = start_conversation_turn(
                                        &agent,
                                        &tx,
                                        state,
                                        next,
                                        operations_source.as_ref(),
                                    );
                                }
                            }
                            continue;
                        }
                        let text = state.take_input();
                        if state.queue_edit.is_none() && text.trim_start().starts_with('/') {
                            let wants_quit = handle_command(
                                &text,
                                &agent,
                                &tx,
                                state,
                                &mut cancel,
                                model_resolver.as_ref(),
                                operations_source.as_ref(),
                            )
                            .await?;
                            if wants_quit {
                                state.queue.clear();
                                if running {
                                    interrupt_active_action(
                                        state,
                                        &cancel,
                                        &mut interrupted_action_id,
                                    );
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
                                    cancel = start_conversation_turn(
                                        &agent,
                                        &tx,
                                        state,
                                        next,
                                        operations_source.as_ref(),
                                    );
                                }
                            }
                        } else if running {
                            if accept_attached_requirement(
                                state,
                                operations_source.as_ref(),
                                &text,
                                false,
                            ) {
                                state.enqueue(text);
                            } else {
                                state.set_input(&text);
                            }
                        } else {
                            cancel = start_conversation_turn(
                                &agent,
                                &tx,
                                state,
                                text,
                                operations_source.as_ref(),
                            );
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

/// Interrupt the current action and immediately make every visible in-flight tool terminal.
///
/// The action id is retained until its `Finished` event so a tool call that was already queued
/// behind the keypress cannot reintroduce a running card after this immediate seal.
fn interrupt_active_action(
    state: &mut ChatState,
    cancel: &CancellationToken,
    interrupted_action_id: &mut Option<u64>,
) {
    *interrupted_action_id = state.active_action_id;
    state.cancel_running_tools();
    cancel.cancel();
}

/// Seal tool calls that reached the UI channel after the interrupt keypress, then consume the
/// cancellation marker for this action. Normal action completion is a no-op.
fn seal_interrupted_action(state: &mut ChatState, interrupted_action_id: &mut Option<u64>) {
    if interrupted_action_id.is_some() && *interrupted_action_id == state.active_action_id {
        state.cancel_running_tools();
        *interrupted_action_id = None;
    }
}

async fn handle_command(
    text: &str,
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    cancel: &mut CancellationToken,
    model_resolver: Option<&Arc<dyn ModelResolver>>,
    operations_source: Option<&operations::SharedFleetBoardSource>,
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
        "usage" if matches!(args, "history" | "observatory") => {
            let engine = agent.read().await;
            match crate::observatory::UsageObservatory::from_store(
                &engine.events,
                &flux_core::PricingTable::builtin(),
            ) {
                Ok(observatory) => {
                    state.usage_open = false;
                    state.observatory = Some(observatory);
                }
                Err(error) => state.push(Entry::Notice {
                    text: format!("usage observatory: {error}"),
                    sev: Sev::Err,
                }),
            }
        }
        "usage" => state.usage_open = true,
        "quit" | "exit" => return Ok(true),
        // The `fleet:` family. Routed on the prefix rather than one arm per verb, so the verbs still to be
        // written — doctor, gate, park/unpark, land, attention, each already a story — are one arm each, and
        // an unrecognised name gets the list rather than silence.
        name if name.starts_with("fleet:") => {
            let Some(source) = operations_source else {
                state.push(Entry::Notice {
                    text:
                        "standalone chat has no attached fleet · relaunch with `flux tui --fleet`"
                            .into(),
                    sev: Sev::Info,
                });
                return Ok(false);
            };
            match name.trim_start_matches("fleet:") {
                "restart" => match source.restart() {
                    Ok(ack) => state.push(Entry::Notice {
                        text: format!("{} · revision {}", ack.message, ack.revision),
                        sev: Sev::Info,
                    }),
                    Err(error) => state.push(Entry::Notice {
                        text: format!("fleet restart refused: {error}"),
                        sev: Sev::Warn,
                    }),
                },
                "refresh" => {
                    source.invalidate_snapshot_cache();
                    match source.snapshot() {
                        Ok(snapshot) => state.push(Entry::Notice {
                            text: format!(
                                "fleet refreshed · revision {} · {} active worker(s) · {} board item(s)",
                                snapshot.revision,
                                snapshot.capacity.active,
                                snapshot.items.len()
                            ),
                            sev: Sev::Info,
                        }),
                        Err(error) => state.push(Entry::Notice {
                            text: format!("fleet refresh failed: {error}"),
                            sev: Sev::Err,
                        }),
                    }
                }
                other => state.push(Entry::Notice {
                    text: format!("unknown fleet command `{other}` · available: restart, refresh"),
                    sev: Sev::Warn,
                }),
            }
        }
        "restart" => {
            RESTART_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            state.push(Entry::Notice {
                text: "restarting on the installed binary — this session resumes from its durable store"
                    .into(),
                sev: Sev::Info,
            });
            return Ok(true);
        }
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
        "fleet" => match state.operations.as_mut() {
            Some(operations) => {
                operations.select_tab(crate::operations::OperationsTab::Overview);
                operations.open = true;
            }
            None => state.push(Entry::Notice {
                text: "standalone chat has no Fleet attachment · relaunch with `flux tui --fleet`"
                    .into(),
                sev: Sev::Info,
            }),
        },
        "board" => match state.operations.as_mut() {
            Some(operations) => {
                operations.select_tab(crate::operations::OperationsTab::Board);
                operations.open = true;
            }
            None => state.push(Entry::Notice {
                text: "standalone chat has no attached Board · relaunch with `flux tui --fleet`"
                    .into(),
                sev: Sev::Info,
            }),
        },
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
                    state.session_query.clear();
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
        "insights" => {
            *cancel = start_insights(agent, tx, state, args.to_string());
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
        other => match file_command_prompt(other, args, &state.file_commands) {
            Some(prompt) => {
                *cancel = start_turn(agent, tx, state, prompt);
            }
            None => state.push(Entry::Notice {
                text: format!("unknown command /{other} · try /help"),
                sev: Sev::Warn,
            }),
        },
    }
    Ok(false)
}

fn command_is_read_only(name: &str, args: &str) -> bool {
    matches!(
        name,
        "help" | "tools" | "evidence" | "session" | "queue" | "theme" | "fleet" | "board"
    ) || (name == "sessions" && args != "--prune")
        || (name == "effort" && args.is_empty())
}

fn compaction_notice(outcome: flux_flow::engine::CompactionOutcome) -> (String, Sev) {
    use flux_flow::engine::CompactionOutcome;

    match outcome {
        // The observed `context.compacted` event owns the success marker and its real counts.
        CompactionOutcome::Compacted { .. } => ("compaction check complete".into(), Sev::Info),
        CompactionOutcome::Unchanged => ("context unchanged".into(), Sev::Info),
        CompactionOutcome::Disabled => ("context compaction is disabled".into(), Sev::Info),
        CompactionOutcome::Cancelled => ("compaction cancelled".into(), Sev::Warn),
    }
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
            Ok(Ok(outcome)) => compaction_notice(outcome),
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

fn start_insights(
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    direction: String,
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
    let pricing = state
        .cost_model
        .as_ref()
        .map(|(_, pricing)| pricing.clone())
        .unwrap_or_else(flux_core::PricingTable::builtin);
    tokio::spawn(async move {
        let engine = task_agent.read().await;
        let redactor = engine.executor.context().redactor.clone();
        let facts = flux_flow::insights::collect_facts(
            &engine.events,
            &flux_flow::insights::InsightScope::Session {
                root: session.clone(),
                label: format!("current session · {session}"),
            },
            &pricing,
            &redactor,
        );
        let facts = match facts {
            Ok(facts) => facts,
            Err(error) => {
                send_action_event(
                    &task_tx,
                    action_id,
                    UiEvent::Notice {
                        text: format!("insights: {error}"),
                        sev: Sev::Err,
                    },
                );
                send_action_event(&task_tx, action_id, UiEvent::Finished);
                return;
            }
        };
        send_action_event(
            &task_tx,
            action_id,
            UiEvent::Notice {
                text: facts.render(),
                sev: Sev::Info,
            },
        );
        if facts.is_empty() {
            send_action_event(&task_tx, action_id, UiEvent::Finished);
            return;
        }
        let (summary, usage) = flux_flow::insights::narrate(
            engine.provider.as_ref(),
            &engine.model,
            &facts,
            (!direction.is_empty()).then_some(direction.as_str()),
            &redactor,
            &task_cancel,
        )
        .await;
        let model = flux_core::canonical_model_spec(Some(engine.provider.name()), &engine.model);
        let accounting = engine
            .events
            .record_unscoped_call_usage(&session, &model, usage.clone());
        if accounting.is_ok() {
            send_action_event(
                &task_tx,
                action_id,
                UiEvent::BackgroundUsage { model, usage },
            );
        }
        let (text, sev) = match (summary, accounting) {
            (Ok(summary), Ok(())) => (format!("Summary\n{summary}"), Sev::Info),
            (_, Err(error)) => (format!("insights accounting: {error}"), Sev::Err),
            (Err(error), Ok(())) => (format!("insights: {error}"), Sev::Err),
        };
        send_action_event(&task_tx, action_id, UiEvent::Notice { text, sev });
        send_action_event(&task_tx, action_id, UiEvent::Finished);
    });
    cancel
}

/// Build the potentially expensive Board/Fleet projection away from the terminal event loop.
fn spawn_operations_snapshot(
    tx: &mpsc::UnboundedSender<UiEvent>,
    source: operations::SharedFleetBoardSource,
) {
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        // The token is intentionally read before the snapshot. If durable state changes while the
        // snapshot is being built, the next one-second poll observes a different token and starts
        // another refresh instead of incorrectly treating the older projection as current.
        let refresh_token = source.refresh_token().ok();
        let result = source.snapshot().map_err(|error| error.to_string());
        let _ = tx.send(UiEvent::OperationsSnapshot {
            result: Box::new(result),
            refresh_token,
        });
    });
}

fn request_operations_snapshot(
    tx: &mpsc::UnboundedSender<UiEvent>,
    source: Option<&operations::SharedFleetBoardSource>,
    in_flight: &mut bool,
    force_pending: &mut bool,
    invalidate: bool,
) {
    let Some(source) = source else {
        return;
    };
    if *in_flight {
        *force_pending |= invalidate;
        return;
    }
    if invalidate {
        source.invalidate_snapshot_cache();
    }
    spawn_operations_snapshot(tx, source.clone());
    *in_flight = true;
}

fn accept_attached_requirement(
    state: &mut ChatState,
    source: Option<&operations::SharedFleetBoardSource>,
    text: &str,
    deliver: bool,
) -> bool {
    let Some(current) = state.operations.as_ref() else {
        return true;
    };
    if !current.snapshot.can_send() {
        let main_status = current.snapshot.main_status.clone();
        state.push(Entry::Notice {
            text: format!(
                "Fleet main is {} · run `flux fleet start` and refresh with F2/r before sending",
                main_status
            ),
            sev: Sev::Warn,
        });
        return false;
    }
    let Some(source) = source else {
        state.push(Entry::Notice {
            text: "Fleet attachment lost its typed operations source".into(),
            sev: Sev::Err,
        });
        return false;
    };
    let session = state.session_id.clone();
    let pending_index = state.operations.as_ref().and_then(|operations| {
        operations
            .pending
            .iter()
            .position(|pending| !pending.delivered && pending.text == text)
    });
    let index = if let Some(index) = pending_index {
        index
    } else {
        match source.accept_requirement(text, &session) {
            Ok(ack) => {
                let Some(operations) = state.operations.as_mut() else {
                    return false;
                };
                operations.last_ack = Some(ack.clone());
                operations.pending.push(operations::PendingRequirement {
                    id: ack.id,
                    text: text.to_string(),
                    delivered: false,
                });
                operations.pending.len() - 1
            }
            Err(error) => {
                state.push(Entry::Notice {
                    text: format!("Fleet intake refused: {error}"),
                    sev: Sev::Err,
                });
                return false;
            }
        }
    };
    if !deliver {
        return true;
    }
    let id = state
        .operations
        .as_ref()
        .and_then(|operations| operations.pending.get(index))
        .map(|pending| pending.id.clone())
        .unwrap_or_default();
    match source.deliver_requirement(&id, &session) {
        Ok(ack) => {
            if let Some(operations) = state.operations.as_mut() {
                if let Some(pending) = operations.pending.get_mut(index) {
                    pending.delivered = true;
                }
                operations.last_ack = Some(ack.clone());
                operations.snapshot.main_status = "working".into();
                if let Ok(revision) = ack.revision.parse() {
                    operations.snapshot.revision = revision;
                }
                operations.turn_failed = false;
            }
            true
        }
        Err(error) => {
            state.push(Entry::Notice {
                text: format!("Fleet delivery failed: {error}"),
                sev: Sev::Err,
            });
            false
        }
    }
}

fn acknowledge_steered_requirement(
    state: &mut ChatState,
    source: Option<&operations::SharedFleetBoardSource>,
    text: &str,
) {
    let Some(source) = source else {
        return;
    };
    let Some(index) = state.operations.as_ref().and_then(|operations| {
        operations
            .pending
            .iter()
            .position(|pending| !pending.delivered && pending.text == text)
    }) else {
        return;
    };
    let id = state.operations.as_ref().unwrap().pending[index].id.clone();
    match source.deliver_requirement(&id, &state.session_id) {
        Ok(ack) => {
            if let Some(operations) = state.operations.as_mut() {
                operations.pending[index].delivered = true;
                operations.last_ack = Some(ack);
            }
        }
        Err(error) => {
            if let Some(operations) = state.operations.as_mut() {
                operations.refresh_error = Some(format!("delivery {id}: {error}"));
            }
        }
    }
}

fn complete_attached_requirements(
    state: &mut ChatState,
    source: Option<&operations::SharedFleetBoardSource>,
) -> bool {
    let Some(source) = source else {
        return false;
    };
    let Some(operations) = state.operations.as_ref() else {
        return false;
    };
    let failed = operations.turn_failed;
    let ids = operations
        .pending
        .iter()
        .filter(|pending| pending.delivered)
        .map(|pending| pending.id.clone())
        .collect::<Vec<_>>();
    for id in &ids {
        match source.complete_requirement(
            id,
            &state.session_id,
            !failed,
            failed.then_some("Fleet-main TUI turn failed; inspect the durable transcript"),
        ) {
            Ok(ack) => {
                if let Some(operations) = state.operations.as_mut() {
                    operations.last_ack = Some(ack);
                }
            }
            Err(error) => {
                if let Some(operations) = state.operations.as_mut() {
                    operations.refresh_error = Some(format!("completion {id}: {error}"));
                }
            }
        }
    }
    if let Some(operations) = state.operations.as_mut() {
        operations
            .pending
            .retain(|pending| !pending.delivered || !ids.contains(&pending.id));
        operations.turn_failed = false;
    }
    !ids.is_empty()
}

fn start_conversation_turn(
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    input: String,
    source: Option<&operations::SharedFleetBoardSource>,
) -> CancellationToken {
    if accept_attached_requirement(state, source, &input, true) {
        start_turn(agent, tx, state, input)
    } else {
        state.set_input(&input);
        CancellationToken::new()
    }
}

fn start_turn(
    agent: &Arc<tokio::sync::RwLock<FlowEngine>>,
    tx: &mpsc::UnboundedSender<UiEvent>,
    state: &mut ChatState,
    input: String,
) -> CancellationToken {
    let action_id = state.begin_action();
    // C-140: a new *turn* starts the overlay's per-turn view empty. Deliberately here rather than
    // in `begin_action`, which also covers `/compact` — a maintenance action that would otherwise
    // erase the usage of the turn the user just watched finish. Session totals are untouched.
    state.begin_turn_usage();
    state.follow = true;
    state.unread = 0;
    state.record_history(&input);
    state.push_user(input.clone());
    state.phase = Phase::Thinking;
    state.turn_start = Some(Instant::now());
    state.steps = 0;
    state.turn_llm_wait = Duration::ZERO;
    state.last_llm_wait = None;
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

/// Apply a failed-card navigation key, returning whether this binding consumed the event.
fn handle_failure_navigation_key(state: &mut ChatState, key: crossterm::event::KeyEvent) -> bool {
    let Some(direction) = failure_navigation_key(key) else {
        return false;
    };
    let forward = direction == FailureNavigation::Next;
    if !state.jump_failure(forward) {
        state.push(Entry::Notice {
            text: "no failed tool cards".into(),
            sev: Sev::Info,
        });
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use ratatui::backend::TestBackend;
    use std::sync::Barrier;

    /// A deterministic dispatch id for a test-constructed tool card (C-531). Live cards get theirs
    /// from the interpreter; a test states the pairing it means to exercise.
    fn dispatch(raw: u64) -> DispatchId {
        DispatchId::from_raw(raw)
    }

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

    fn fleet_snapshot() -> operations::FleetBoardSnapshot {
        operations::FleetBoardSnapshot {
            schema: "flux.tui-board-fleet/v1".into(),
            root: "/workspace".into(),
            running: true,
            main_status: "running".into(),
            main_session: Some("s-main".into()),
            revision: 7,
            goals_revision: 0,
            goals: Vec::new(),
            active_wave: None,
            capacity: operations::FleetCapacityView {
                configured: 5,
                desired: None,
                active: 0,
                draining: None,
                registered: 0,
            },
            workers: Vec::new(),
            workers_total: 0,
            items: Vec::new(),
            items_total: 0,
            decisions: Vec::new(),
            decisions_total: 0,
            documents: Vec::new(),
            documents_total: 0,
            metrics_schema: "unavailable".into(),
            metrics: Vec::new(),
            stats_facts: Vec::new(),
            status_counts: Vec::new(),
            history: Vec::new(),
            failures: Vec::new(),
            failures_total: 0,
            intake: Vec::new(),
            intake_total: 0,
            blocked_items: 0,
            attention_required: false,
        }
    }

    struct BlockingFleetSource {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl operations::FleetBoardSource for BlockingFleetSource {
        fn snapshot(&self) -> anyhow::Result<operations::FleetBoardSnapshot> {
            self.entered.wait();
            self.release.wait();
            Ok(fleet_snapshot())
        }

        fn refresh_token(&self) -> anyhow::Result<String> {
            Ok("revision-7".into())
        }

        fn attach_session(&self, _session: &str) -> anyhow::Result<operations::FleetAck> {
            unreachable!()
        }

        fn accept_requirement(
            &self,
            _text: &str,
            _session: &str,
        ) -> anyhow::Result<operations::FleetAck> {
            unreachable!()
        }

        fn deliver_requirement(
            &self,
            _id: &str,
            _session: &str,
        ) -> anyhow::Result<operations::FleetAck> {
            unreachable!()
        }

        fn complete_requirement(
            &self,
            _id: &str,
            _session: &str,
            _succeeded: bool,
            _error: Option<&str>,
        ) -> anyhow::Result<operations::FleetAck> {
            unreachable!()
        }

        fn decide(
            &self,
            _decision_ref: &str,
            _outcome: &str,
        ) -> anyhow::Result<operations::FleetAck> {
            unreachable!()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn board_fleet_snapshot_does_not_block_the_terminal_event_loop() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let source: operations::SharedFleetBoardSource = Arc::new(BlockingFleetSource {
            entered: entered.clone(),
            release: release.clone(),
        });
        let (tx, mut rx) = mpsc::unbounded_channel();

        spawn_operations_snapshot(&tx, source);
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), rx.recv())
                .await
                .is_err(),
            "a blocked projection must not emit or block the async caller"
        );
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            UiEvent::OperationsSnapshot {
                result,
                refresh_token,
            } => {
                let snapshot = result.unwrap();
                assert_eq!(snapshot.revision, 7);
                assert_eq!(refresh_token.as_deref(), Some("revision-7"));
            }
            _ => panic!("unexpected projection event"),
        }
    }

    /// A per-op approval request: no aggregate risk summary, not destructive.
    fn op_request<S: Into<String>>(
        tool: &str,
        subjects: impl IntoIterator<Item = S>,
    ) -> controller::ApprovalRequest {
        controller::ApprovalRequest {
            tool: tool.into(),
            subjects: subjects.into_iter().map(Into::into).collect(),
            ..controller::ApprovalRequest::default()
        }
    }

    /// One host-pushed `log` pane in `slot`, session-lifetime.
    #[cfg(test)]
    fn log_pane(id: &str, slot: flux_runtime::PaneSlot) -> flux_runtime::PaneCommand {
        flux_runtime::PaneCommand::Open(flux_runtime::PaneSpec::new(
            id,
            id,
            slot,
            flux_runtime::PaneLifetime::Session,
            flux_runtime::PaneData::Log {
                lines: vec![format!("{id}-body")],
            },
        ))
    }

    /// A `width`×20 frame of a one-entry session, optionally with a right-slot pane pushed.
    #[cfg(test)]
    fn pane_frame(width: u16, with_pane: bool) -> ratatui::buffer::Buffer {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Notice {
            text: "a transcript row".into(),
            sev: Sev::Info,
        });
        if with_pane {
            state.apply_pane_command(log_pane("fleet", flux_runtime::PaneSlot::Right));
        }
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// C-221 (the story's failing-first test): below the surface's minimum transcript width, panes
    /// are **not drawn at all** — a narrow terminal renders byte-identically, cells and styles
    /// included, to the same session with no panes. That is the posture `EMPTY_CARD_MIN_WIDTH`
    /// (C-157) and C-102's header/footer bars established: a narrow frame drops the aside rather
    /// than squeezing the conversation. Given room, the same pane is drawn.
    #[test]
    fn panes_are_suppressed_below_the_minimum_transcript_width() {
        // 60 columns is under `panes::PANE_MIN_TRANSCRIPT_WIDTH`.
        assert_eq!(
            pane_frame(60, true),
            pane_frame(60, false),
            "a narrow terminal must render exactly as it does with no panes"
        );

        let wide = pane_frame(100, true);
        let text: String = wide.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("fleet"), "pane title drawn: {text}");
        assert!(text.contains("fleet-body"), "pane body drawn: {text}");
        assert_ne!(
            wide,
            pane_frame(100, false),
            "given room, the pane changes the frame"
        );
    }

    /// C-221 / acceptance 5: a pane is drawn, and is nonetheless **invisible to every piece of
    /// transcript machinery** — it takes no layout-cache entry (C-149), no scroll bookkeeping
    /// (C-106), no `focused` index (C-111), and transcript search (C-108) does not find it. This is
    /// the rule `render_empty_state_card`'s doc comment states for the orientation card, asserted
    /// for panes because they render into the same region for the same reason.
    #[test]
    fn panes_never_participate_in_the_transcript_viewport() {
        let entry = || Entry::Notice {
            text: "a transcript row".into(),
            sev: Sev::Info,
        };
        let mut state = ChatState::new("mock".into());
        state.push(entry());
        state.apply_pane_command(log_pane("fleet", flux_runtime::PaneSlot::Right));

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("fleet-body"),
            "the pane IS drawn"
        );

        // The wrapped transcript rows hold the entry and nothing of the pane.
        let visible = state.transcript_viewport(60, 10);
        let flat: String = visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(flat.contains("a transcript row"), "{flat}");
        assert!(
            !flat.contains("fleet-body"),
            "pane content is not a row: {flat}"
        );

        // Transcript search does not reach it (the layout it searches has no pane rows at all).
        state.search = Some(TranscriptSearch {
            query: "fleet-body".into(),
            ..TranscriptSearch::default()
        });
        state.refresh_search_matches();
        assert!(
            state.search.as_ref().unwrap().matches.is_empty(),
            "search must not find pane content"
        );
        state.search = None;

        // Cache + scroll bookkeeping match a pane-less session laid out at the same width: the
        // pane contributes neither an `entry_rows` span nor a wrapped row.
        let mut bare = ChatState::new("mock".into());
        bare.push(entry());
        let _ = bare.transcript_viewport(60, 10);
        let rows = |s: &ChatState| {
            let layout = s.transcript_layout.borrow();
            let layout = layout.as_ref().expect("layout built");
            (layout.entry_rows.len(), layout.lines.len())
        };
        assert_eq!(rows(&state), rows(&bare), "no extra cache entry or row");
        assert_eq!(rows(&state).0, state.entries.len(), "one span per entry");
        assert_eq!(state.last_max_scroll.get(), bare.last_max_scroll.get());

        // Focus walks entries only — it can never land on the pane.
        state.focus_move(1);
        assert_eq!(state.focused, Some(0));
        state.focus_move(1);
        assert_eq!(state.focused, Some(0), "focus stays inside the entries");
    }

    /// C-221 / acceptance 3: pane content is truncated **by the surface**. An oversized payload
    /// loses its tail behind an explicit elision marker instead of growing the pane, and the
    /// transcript keeps its own floor.
    #[test]
    fn an_oversized_pane_is_truncated_by_the_surface() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Notice {
            text: "a transcript row".into(),
            sev: Sev::Info,
        });
        state.apply_pane_command(flux_runtime::PaneCommand::Open(
            flux_runtime::PaneSpec::new(
                "flood",
                "flood",
                flux_runtime::PaneSlot::Right,
                flux_runtime::PaneLifetime::Session,
                flux_runtime::PaneData::Log {
                    lines: (0..200)
                        .map(|i| format!("line-{i}-{}", "x".repeat(200)))
                        .collect(),
                },
            ),
        ));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("line-0"), "the head is shown: {content}");
        assert!(!content.contains("line-40"), "the tail is cut: {content}");
        assert!(content.contains("more"), "the elision is marked: {content}");
        // The transcript is still there and still readable beside it.
        assert!(content.contains("a transcript row"), "{content}");
    }

    /// C-221 / acceptance 2: the horizontal split goes around the **transcript row only** and the
    /// `bottom` slot is one extra vertical constraint — header, footer and composer keep the full
    /// width and their rows in every slot. `overlay` goes through the shared
    /// `render_overlay_panel` chrome (C-152). And panes draw before the approval sheet, which the
    /// sheet's `Clear`ed rect then covers — the ordering C-222's trust invariant rests on.
    #[test]
    fn only_the_transcript_row_is_split_and_the_sheet_draws_over_panes() {
        let frame = |slot: Option<flux_runtime::PaneSlot>, approval: bool| {
            let mut state = ChatState::new("mock".into());
            state.push(Entry::Notice {
                text: "a transcript row".into(),
                sev: Sev::Info,
            });
            if let Some(slot) = slot {
                state.apply_pane_command(log_pane("fleet", slot));
            }
            if approval {
                state.approval = Some(ApprovalView {
                    request: op_request("read", ["README.md"]),
                    scroll: 0,
                    reason: None,
                });
            }
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal.draw(|f| render(f, &state)).unwrap();
            terminal.backend().buffer().clone()
        };
        let row = |buf: &ratatui::buffer::Buffer, r: usize| -> Vec<ratatui::buffer::Cell> {
            buf.content[r * 100..(r + 1) * 100].to_vec()
        };

        let bare = frame(None, false);
        for slot in [
            flux_runtime::PaneSlot::Left,
            flux_runtime::PaneSlot::Right,
            flux_runtime::PaneSlot::Bottom,
            flux_runtime::PaneSlot::Overlay,
        ] {
            let with = frame(Some(slot), false);
            assert_eq!(
                row(&with, 0),
                row(&bare, 0),
                "{slot:?}: header row unchanged"
            );
            assert_eq!(
                row(&with, 23),
                row(&bare, 23),
                "{slot:?}: footer row unchanged"
            );
            assert_eq!(
                row(&with, 22),
                row(&bare, 22),
                "{slot:?}: composer row unchanged"
            );
            let text: String = with.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("fleet-body"), "{slot:?}: pane drawn: {text}");
        }

        // The approval sheet draws last, over its own `Clear`ed rect, so it survives a pane in the
        // rows it occupies.
        let sheet = frame(Some(flux_runtime::PaneSlot::Bottom), true);
        let text: String = sheet.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("approve"), "the sheet still draws: {text}");
        assert!(text.contains("README.md"), "the sheet's subject: {text}");
        assert!(
            !text.contains("fleet-body"),
            "the sheet covers the bottom pane rather than the other way round: {text}"
        );
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

    /// C-157: a fresh session with no transcript entries shows a short centered card naming the
    /// active model, the workspace root, and the primary affordances — the idle footer hint used
    /// to be the only orientation. The card disappears the instant the first entry lands.
    #[test]
    fn empty_transcript_shows_orientation_card_naming_model_workspace_and_affordances() {
        let mut state = ChatState::new("mock".into());
        state.workspace_root = "/home/dev/flux".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("mock"), "{content}");
        assert!(content.contains("/home/dev/flux"), "{content}");
        assert!(content.contains("/help"), "{content}");
        assert!(content.contains('@'), "{content}");

        state.push_user("hello");
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            !screen(&terminal).contains("/home/dev/flux"),
            "the card must not survive the first entry"
        );
    }

    /// C-157: the card is drawn straight into the transcript area — it must never run
    /// `ensure_transcript_layout` (the cache), touch `last_max_scroll`/`last_page` (scrolling), or
    /// otherwise behave like a transcript row.
    #[test]
    fn empty_state_card_never_touches_transcript_layout_cache_or_scroll() {
        let state = ChatState::new("mock".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            state.transcript_layout.borrow().is_none(),
            "an empty session must never populate the transcript layout cache"
        );
        assert_eq!(state.last_max_scroll.get(), 0);
    }

    /// C-157: a narrow terminal skips the card entirely rather than wrapping it into noise —
    /// the same narrow-width posture C-102 established for the header/footer bars.
    #[test]
    fn narrow_terminal_skips_the_empty_state_card() {
        let mut state = ChatState::new("mock".into());
        state.workspace_root = "/home/dev/flux".into();
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("/home/dev/flux"));
    }

    #[test]
    fn composer_is_background_only_without_border_or_padding() {
        let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
        let mut state = ChatState::new("mock".into());
        // A non-empty transcript, so C-157's empty-state card (which also contains a "d" glyph,
        // in "commands") doesn't shadow the composer's own "d" for the `.find()` below.
        state.push_user("hi");
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
        let draft = buffer.cell((1, 8)).expect("draft cell");
        assert_eq!(draft.bg, state.theme.composer_bg);
        assert_eq!(buffer.cell((0, 8)).expect("composer origin").symbol(), "▍");
        assert_eq!(buffer.cell((1, 8)).expect("composer text").symbol(), "d");
        assert!((0..48).all(|x| {
            buffer
                .cell((x, 8))
                .is_some_and(|cell| cell.bg == state.theme.composer_bg)
        }));
    }

    #[test]
    fn composer_accent_bar_tracks_run_state_without_layout_churn() {
        for (name, theme) in [
            ("dark", Theme::DARK),
            ("light", Theme::LIGHT),
            ("mono", Theme::MONO),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
            let mut state = ChatState::new("mock".into());
            state.theme = theme;
            state.theme_name = name.into();
            state.push_user("hi");
            state.set_input("draft");

            terminal.draw(|f| render(f, &state)).unwrap();
            let idle = terminal.backend().buffer();
            let idle_bar = idle.cell((0, 8)).expect("idle composer accent bar");
            assert_eq!(idle_bar.symbol(), "▍", "{name} idle bar glyph");
            assert_eq!(idle_bar.fg, theme.accent, "{name} idle bar color");
            assert_eq!(idle_bar.bg, theme.composer_bg, "{name} idle bar surface");
            assert_eq!(idle.cell((1, 8)).expect("idle composer text").symbol(), "d");
            let idle_symbols: Vec<String> = (0..48)
                .map(|x| {
                    idle.cell((x, 8))
                        .expect("idle composer cell")
                        .symbol()
                        .into()
                })
                .collect();

            state.begin_action();
            terminal.draw(|f| render(f, &state)).unwrap();
            let running = terminal.backend().buffer();
            let running_bar = running.cell((0, 8)).expect("running composer accent bar");
            assert_eq!(running_bar.symbol(), "▍", "{name} running bar glyph");
            assert_eq!(running_bar.fg, theme.muted, "{name} running bar color");
            assert_eq!(
                running_bar.bg, theme.composer_bg,
                "{name} running bar surface"
            );
            assert_eq!(
                running
                    .cell((1, 8))
                    .expect("running composer text")
                    .symbol(),
                "d"
            );
            let running_symbols: Vec<String> = (0..48)
                .map(|x| {
                    running
                        .cell((x, 8))
                        .expect("running composer cell")
                        .symbol()
                        .into()
                })
                .collect();
            assert_eq!(
                running_symbols, idle_symbols,
                "{name} composer geometry changed between idle and running"
            );
        }
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
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "ls"}),
        )));
        state.stream_text("done");
        assert_eq!(state.entries.len(), 3);

        state.approval = Some(ApprovalView {
            request: op_request("bash", ["ls"]),
            scroll: 0,
            reason: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("approve"));
    }

    #[test]
    fn tool_card_pairs_call_with_result_and_badge() {
        let mut state = ChatState::new("opus".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "cargo test"}),
        )));
        state.finish_tool(dispatch(1), "bash", "182 passed; 0 failed".into(), false);
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
            dispatch(1),
            "write".into(),
            serde_json::json!({"path": "README.md"}),
        )));
        state.time_tool(
            dispatch(1),
            flux_core::OperationTiming {
                total_us: 30_005_000,
                approval_wait_us: Some(30_000_000),
                execution_us: Some(5_000),
            },
        );
        state.finish_tool(dispatch(1), "write", "wrote README.md".into(), false);
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
        assert_eq!(state.queue_texts(), ["first", "second", "third"]);

        state.queue_sel = 1;
        state.queue_move(-1);
        assert_eq!(state.queue_texts(), ["second", "first", "third"]);
        assert_eq!(state.queue_remove_selected().as_deref(), Some("second"));
        assert_eq!(state.queue_texts(), ["first", "third"]);
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
            state.queue_texts(),
            ["first", "second", "third"],
            "beginning an edit must not remove or reorder the item"
        );
        assert!(state.queue_commit_edit("second refined".into()));
        assert_eq!(state.queue_texts(), ["first", "second refined", "third"]);
        assert_eq!(state.queue.pop_front().as_deref(), Some("first"));
        assert_eq!(state.queue.pop_front().as_deref(), Some("second refined"));
    }

    #[test]
    fn engine_drain_empties_the_strip_and_steered_messages_reach_the_transcript() {
        let mut state = ChatState::new("mock".into());
        state.enqueue("focus on the parser".into());
        state.enqueue("skip the tests".into());

        let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("focus on the parser"),
            "queued strip renders"
        );

        // The engine consumes the shared queue at its next planner consultation (A-94)…
        let drained = state.queue.drain();
        assert_eq!(drained, vec!["focus on the parser", "skip the tests"]);
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            !screen(&terminal).contains("focus on the parser"),
            "consumed items leave the strip without any UI bookkeeping"
        );

        // …and its `turn.steering` observation leaves a transcript record.
        let action = state.begin_action();
        let event = state
            .accept_ui_event(UiEvent::Tagged {
                action_id: action,
                event: Box::new(UiEvent::Steered(drained)),
            })
            .expect("live action");
        if let UiEvent::Steered(messages) = event {
            for text in messages {
                state.push(Entry::Notice {
                    text: format!("↪ steering delivered: {text}"),
                    sev: Sev::Info,
                });
            }
        } else {
            panic!("expected the steered event back");
        }
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("↪ steering delivered: focus on the parser"));
    }

    #[test]
    fn an_edit_raced_by_engine_consumption_falls_back_to_a_fresh_submission() {
        let mut state = ChatState::new("mock".into());
        state.enqueue("tighten the loop".into());
        assert_eq!(
            state.queue_begin_edit().as_deref(),
            Some("tighten the loop")
        );

        // The engine drains the queue while the user is still editing the item.
        state.queue.drain();

        assert!(
            !state.queue_commit_edit("tighten the outer loop".into()),
            "a consumed item can no longer be edited — the caller must treat the text as new input"
        );
        assert!(state.queue.is_empty());
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

    /// C-154: the per-op path used to discard `intents` entirely (it was bound `_intents` and
    /// never read), so a per-op destructive delete reached the sheet as an ordinary
    /// `ApprovalRequest::default()` — `destructive`/`mutating` both `false`. `intents` was already
    /// a parameter on `Approver::request`; this proves it is now spent, not that the approval
    /// decision path grew a new one.
    #[tokio::test]
    async fn per_op_request_plumbs_destructive_and_mutating_from_intents() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let approver = ChannelApprover { tx };
        let mut intents = IntentSet::default();
        intents.push(flux_spec::Intent {
            behavior: flux_spec::IntentBehavior::CommandExecution,
            target: flux_spec::IntentTarget::Process {
                command: "rm -rf build".into(),
            },
            role: flux_spec::IntentRole::ProcessCommand,
            certainty: flux_spec::IntentCertainty::Certain,
        });
        let raised = tokio::spawn(async move {
            approver
                .request("bash", &["rm -rf build".into()], &intents)
                .await
        });

        let UiEvent::Approval { request, reply } = rx.recv().await.expect("approval raised") else {
            panic!("expected an Approval event");
        };
        assert!(request.destructive, "rm -rf is destructive-shaped");
        assert!(
            request.mutating,
            "a destructive command execution is also mutating"
        );

        let _ = reply.send(ApprovalChoice::Deny);
        let _ = raised.await;
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

    /// C-156: the blank-composer, idle Ctrl-C arm — a first press arms the quit confirmation
    /// (`Armed`, loop survives) and only a second press within the window confirms it (`Quit`).
    /// This drives the same decision the event loop's key handler makes for two Ctrl-C presses.
    #[test]
    fn ctrl_c_on_blank_composer_arms_before_it_quits() {
        let mut state = ChatState::new("mock".into());
        assert!(!state.ctrl_c_armed(), "unarmed at rest");

        // First press: arms, does not quit.
        assert_eq!(state.arm_or_confirm_quit(), CtrlCQuit::Armed);
        assert!(state.ctrl_c_armed());

        // Second press, same instant (well within the window): confirms.
        assert_eq!(state.arm_or_confirm_quit(), CtrlCQuit::Quit);
    }

    /// C-156: the armed footer hint is visible only while armed, and any other input (modeled
    /// here as `clear_ctrl_c_arm`, which the event loop calls for every key but a confirming
    /// Ctrl-C) disarms it — a second Ctrl-C after that starts a fresh arm instead of quitting.
    #[test]
    fn ctrl_c_arm_clears_on_other_input_and_shows_in_the_footer() {
        let mut state = ChatState::new("mock".into());
        state.arm_or_confirm_quit();
        assert!(state.ctrl_c_armed());
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            screen(&terminal).contains("Ctrl-C again to quit"),
            "{}",
            screen(&terminal)
        );

        // Any other input clears the arm (event loop calls this for every key but Ctrl-C).
        state.clear_ctrl_c_arm();
        assert!(!state.ctrl_c_armed());
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("Ctrl-C again to quit"));

        // A press after clearing arms again rather than quitting.
        assert_eq!(state.arm_or_confirm_quit(), CtrlCQuit::Armed);
    }

    /// C-156: a stale arm (older than the window) reads as unarmed — a second press long after
    /// the first re-arms instead of quitting on a press the user has forgotten about.
    #[test]
    fn ctrl_c_arm_expires_after_the_window() {
        let mut state = ChatState::new("mock".into());
        state.ctrl_c_armed_at = Some(Instant::now() - CTRL_C_QUIT_WINDOW - Duration::from_secs(1));
        assert!(!state.ctrl_c_armed(), "a stale arm must read as unarmed");
        assert_eq!(
            state.arm_or_confirm_quit(),
            CtrlCQuit::Armed,
            "a press after the window re-arms rather than quitting"
        );
    }

    /// C-156: the armed footer hint slots into the idle-left precedence BELOW the unread
    /// indicator and the C-105 mouse-off hint — neither is displaced by it.
    #[test]
    fn ctrl_c_armed_hint_does_not_displace_unread_or_mouse_off() {
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();

        // Unread wins over the armed hint.
        let mut state = ChatState::new("mock".into());
        state.unread = 2;
        state.arm_or_confirm_quit();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("new · Ctrl-End latest"), "{content}");
        assert!(!content.contains("Ctrl-C again to quit"), "{content}");

        // Mouse-off wins over the armed hint.
        let mut state = ChatState::new("mock".into());
        state.mouse_capture = false;
        state.arm_or_confirm_quit();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("mouse off"), "{content}");
        assert!(!content.contains("Ctrl-C again to quit"), "{content}");

        // With neither active, the armed hint has the slot.
        let mut state = ChatState::new("mock".into());
        state.arm_or_confirm_quit();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("Ctrl-C again to quit"));
    }

    /// C-341: overflow keeps an overlaid scrollbar on the transcript's right column in both
    /// follow and detached modes. Only the detached thumb is accented; its percent footer remains
    /// a manual-scroll affordance.
    #[test]
    fn overflow_scrollbar_persists_and_accents_only_while_detached() {
        let mut state = ChatState::new("mock".into());
        for i in 0..40 {
            state.push_user(format!("message number {i}"));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();

        // Follow mode keeps the track and a muted thumb visible, but not the manual-scroll footer.
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(!content.contains('%'), "{content}");
        assert!(state.last_max_scroll.get() > 0);
        // C-341 originally asserted `width == 60` — "the overlaid scrollbar must not consume
        // transcript width". That invariant is what produced silent text corruption: the bar is
        // drawn into the transcript's own `Rect` *after* the text, so any character wrapped into the
        // last column was overwritten with nothing on screen to show a character had been lost
        // (observed live as "…the current revi" + "ion, active worker c"). A scrollbar cannot
        // overlay full-width text without destroying some of it, so the column is now reserved.
        // Losing one column of width is strictly better than losing characters out of paths, SHAs
        // and command output.
        assert_eq!(
            state
                .transcript_layout
                .borrow()
                .as_ref()
                .expect("overflow laid out")
                .width,
            59,
            "the scrollbar column is reserved so wrapped text can never land under the bar"
        );
        let buffer = terminal.backend().buffer();
        let follow_thumb = (1..10)
            .filter_map(|y| buffer.cell((59, y)))
            .find(|cell| cell.symbol() == "█")
            .expect("follow-mode scrollbar thumb");
        assert_eq!(follow_thumb.fg, state.theme.muted);
        assert!((1..10)
            .filter_map(|y| buffer.cell((59, y)))
            .any(|cell| { cell.symbol() == "║" && cell.fg == state.theme.muted }));

        scroll_up(&mut state, 5);
        assert!(!state.follow);
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("⤓") && content.contains('%'), "{content}");
        let buffer = terminal.backend().buffer();
        let detached_thumb = (1..10)
            .filter_map(|y| buffer.cell((59, y)))
            .find(|cell| cell.symbol() == "█")
            .expect("detached scrollbar thumb");
        assert_eq!(detached_thumb.fg, state.theme.accent);
        assert!((1..10)
            .filter_map(|y| buffer.cell((59, y)))
            .any(|cell| { cell.symbol() == "║" && cell.fg == state.theme.muted }));

        // Reattaching removes only the manual-scroll footer; the muted scrollbar persists.
        state.follow = true;
        state.scroll = state.last_max_scroll.get();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(!content.contains('%'), "{content}");
        let buffer = terminal.backend().buffer();
        let follow_thumb = (1..10)
            .filter_map(|y| buffer.cell((59, y)))
            .find(|cell| cell.symbol() == "█")
            .expect("reattached scrollbar thumb");
        assert_eq!(follow_thumb.fg, state.theme.muted);
    }

    /// C-110: the help overlay lists keys and every slash command from the merged table, and
    /// only renders while open.
    #[test]
    fn help_overlay_lists_keys_and_all_commands() {
        let mut state = ChatState::new("mock".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 26)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("help · Esc close"));

        state.help_open = true;
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("help · Esc close"), "{content}");
        assert!(content.contains("Ctrl-J"), "{content}");
        assert!(content.contains("Ctrl-R"), "{content}");
        assert!(content.contains("Ctrl-G"), "{content}");
        assert!(content.contains("Ctrl-T"), "{content}");
        for c in all_slash_commands(&state.file_commands) {
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
            dispatch(1),
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
        state.finish_tool(dispatch(1), "bash", "ok".into(), false);
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

    /// C-531 failing-first: two same-name `read` cards are open at once (C-528 admits concurrent
    /// idempotent reads). The first call's result must resolve the FIRST card and leave the second
    /// one running — the name-based LIFO scan resolves the newest card instead, so the body of
    /// `alpha.txt` lands under the `beta.txt` header.
    #[test]
    fn tool_result_resolves_its_own_card_not_the_newest_same_name_card() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "read".into(),
            serde_json::json!({"path": "alpha.txt"}),
        )));
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(2),
            "read".into(),
            serde_json::json!({"path": "beta.txt"}),
        )));

        // The FIRST call's result — the one the name-based LIFO scan used to hand to `beta.txt`.
        state.finish_tool(dispatch(1), "read", "alpha body".into(), false);

        let mut terminal = Terminal::new(TestBackend::new(72, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let frame = screen(&terminal);

        let settled: Vec<(String, bool)> = state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Tool(tool) => Some((
                    tool.input["path"].as_str().unwrap_or_default().to_string(),
                    tool.result.is_some(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            settled,
            vec![
                ("alpha.txt".to_string(), true),
                ("beta.txt".to_string(), false)
            ],
            "the result belongs to the card that issued it: {frame}"
        );

        let lines: Vec<String> = state
            .transcript_lines(72)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let joined = lines.join("\n");
        let alpha = lines
            .iter()
            .position(|line| line.contains("alpha.txt"))
            .unwrap_or_else(|| panic!("alpha card header: {joined}"));
        let beta = lines
            .iter()
            .position(|line| line.contains("beta.txt"))
            .unwrap_or_else(|| panic!("beta card header: {joined}"));
        assert!(
            lines[alpha..beta].iter().any(|line| line.contains('✓')),
            "alpha's card must be the resolved one: {joined}"
        );
        let Entry::Tool(alpha_card) = &state.entries[0] else {
            panic!("expected alpha's tool card")
        };
        assert_eq!(
            alpha_card
                .result
                .as_ref()
                .map(|outcome| outcome.content.as_str()),
            Some("alpha body"),
            "alpha's card must carry its own body, not beta's"
        );
        assert!(
            lines[beta..]
                .iter()
                .any(|line| line.contains(RUNNING_BADGE)),
            "beta's card must stay running: {joined}"
        );
        assert!(
            !lines[alpha..beta]
                .iter()
                .any(|line| line.contains(RUNNING_BADGE)),
            "alpha's card must not still be running: {joined}"
        );
    }

    /// C-155: a collapsed card with expandable detail shows a `▸` marker in the header, rendered
    /// through `tool_header_line` so the pad/width math is shared with the C-109 badge patch. The
    /// marker must not be the header row's last span — the C-109 running-badge pairing matches
    /// the last span against `RUNNING_BADGE`.
    #[test]
    fn collapsed_tool_card_shows_a_collapse_marker() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "echo hi"}),
        )));
        state.finish_tool(dispatch(1), "bash", "hi".into(), false);

        let lines = state.transcript_lines(72);
        let header = &lines[0];
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('▸'), "collapsed header must carry ▸: {text}");
        assert_ne!(
            header.spans.last().unwrap().content.as_ref(),
            "▸",
            "the marker must never be the header row's last span"
        );
    }

    /// C-155: an expanded card shows `▾` instead, so the marker doubles as the current state.
    #[test]
    fn expanded_tool_card_shows_an_expand_marker() {
        let mut state = ChatState::new("mock".into());
        state.expand_tools = true;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "echo hi"}),
        )));
        state.finish_tool(dispatch(1), "bash", "hi".into(), false);

        let lines = state.transcript_lines(72);
        let header = &lines[0];
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('▾'), "expanded header must carry ▾: {text}");
        assert!(!text.contains('▸'), "must not carry both markers: {text}");
    }

    /// C-155: a card with no expandable detail (empty content, no diff) shows no marker at all —
    /// a marker with nothing behind it would be a false affordance.
    #[test]
    fn tool_card_without_detail_shows_no_marker() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "true"}),
        )));
        state.finish_tool(dispatch(1), "bash", String::new(), false);

        let lines = state.transcript_lines(72);
        let header = &lines[0];
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('▸'), "{text}");
        assert!(!text.contains('▾'), "{text}");
    }

    /// C-155: the new marker span must not break the C-109 running-badge pairing, which matches
    /// each header row's LAST span against `RUNNING_BADGE` by content and style.
    #[test]
    fn running_card_still_pairs_with_running_badge() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "sleep 5"}),
        )));

        let lines = state.transcript_lines(72);
        let header = &lines[0];
        assert_eq!(
            header.spans.last().unwrap().content.as_ref(),
            RUNNING_BADGE,
            "running badge must stay the header row's last span"
        );
        assert!(
            state
                .transcript_layout
                .borrow()
                .as_ref()
                .is_some_and(|l| l.running_rows.len() == 1),
            "running-badge pairing must still find the row"
        );
    }

    /// C-158: a running card renders a bounded tail of in-flight output that updates as the op
    /// runs, and is replaced by the normal summary once the result lands.
    #[test]
    fn running_card_shows_a_live_output_tail_then_the_summary() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "cargo build"}),
        )));

        let text = |state: &mut ChatState| -> String {
            state
                .transcript_lines(72)
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

        // Nothing reported yet: the card is exactly the header, as before this story.
        assert_eq!(text(&mut state).lines().count(), 1);

        state.progress_tool("bash", "Compiling serde v1.0".into());
        let shown = text(&mut state);
        assert!(shown.contains("Compiling serde v1.0"), "{shown}");

        // It updates as more arrives.
        state.progress_tool("bash", "Compiling tokio v1.4".into());
        let shown = text(&mut state);
        assert!(shown.contains("Compiling tokio v1.4"), "{shown}");

        // The result supersedes the tail entirely: the card switches to its real summary row and
        // the in-flight lines are gone.
        state.finish_tool(dispatch(1), "bash", "Finished in 3.1s".into(), false);
        let shown = text(&mut state);
        assert!(
            shown.contains('✓'),
            "the card must show its result: {shown}"
        );
        assert!(
            !shown.contains("Compiling serde v1.0") && !shown.contains("Compiling tokio v1.4"),
            "the live tail must not survive alongside the real result: {shown}"
        );
    }

    /// C-158: the tail is bounded — a chatty command keeps only the newest lines and must not
    /// grow the card without limit.
    #[test]
    fn live_output_tail_is_bounded_to_the_newest_lines() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "yes"}),
        )));
        for i in 0..50 {
            state.progress_tool("bash", format!("line-{i}"));
        }
        let Entry::Tool(tool) = &state.entries[0] else {
            panic!("expected a tool entry");
        };
        assert_eq!(tool.partial.len(), MAX_PARTIAL_LINES);
        assert_eq!(tool.partial.last().unwrap(), "line-49");
        assert!(
            !tool.partial.iter().any(|l| l == "line-0"),
            "the oldest lines must be dropped, not kept"
        );
    }

    /// C-158: a line that arrives after the result landed is dropped — the card has moved on to
    /// its real summary and must not flip back to a partial view.
    #[test]
    fn progress_after_the_result_is_ignored() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "echo hi"}),
        )));
        state.finish_tool(dispatch(1), "bash", "hi".into(), false);
        state.progress_tool("bash", "late straggler".into());

        let Entry::Tool(tool) = &state.entries[0] else {
            panic!("expected a tool entry");
        };
        assert!(tool.partial.is_empty());
        let shown: String = state
            .transcript_lines(72)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(!shown.contains("late straggler"), "{shown}");
    }

    /// C-158 acceptance: the extra rows sit below the header, so the C-109 running-badge pairing
    /// (which matches each header row's LAST span) still finds exactly one running row — and it is
    /// still the header row, not a tail row.
    #[test]
    fn live_output_tail_keeps_the_running_badge_pairing() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "sleep 5"}),
        )));
        state.progress_tool("bash", "still working".into());
        state.progress_tool("bash", "nearly there".into());

        let lines = state.transcript_lines(72);
        assert_eq!(
            lines[0].spans.last().unwrap().content.as_ref(),
            RUNNING_BADGE,
            "running badge must stay the header row's last span"
        );
        let layout = state.transcript_layout.borrow();
        let running = &layout.as_ref().unwrap().running_rows;
        assert_eq!(running.len(), 1, "exactly one running row");
        assert_eq!(running[0], (0, 0), "the paired row must be the header row");
    }

    /// C-149: a running tool card's per-tick spinner/elapsed patch (`transcript_viewport`) rebuilds
    /// the whole header row, so it must re-add the same rail span `entry_lines` gave the cached
    /// row — otherwise the row would visibly shift left on every animation frame.
    #[test]
    fn running_card_animation_keeps_the_gutter_rail() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "sleep 5"}),
        )));

        let mut terminal = Terminal::new(TestBackend::new(72, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains(GUTTER.trim_end()), "{content}");
    }

    /// C-149: `entry_lines` prepends a one-column rail per entry kind — the user's own turn reads
    /// distinctly from what the assistant produced (bold/user-colored vs. dim), and the badge
    /// pairing (C-109) still holds since the rail is a leading span, never the last one.
    #[test]
    fn transcript_gutter_marks_user_and_assistant_entries() {
        let mut state = ChatState::new("mock".into());
        state.push(Entry::User {
            text: "hello there".into(),
            prior_elapsed: None,
            show_separator: false,
        });
        state.stream_text("hi back");
        state.end_stream();

        let row_text =
            |l: &Line<'static>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        let lines = state.transcript_lines(60);
        let user_row = lines
            .iter()
            .find(|l| row_text(l).contains("hello there"))
            .expect("user row");
        let assistant_row = lines
            .iter()
            .find(|l| row_text(l).contains("hi back"))
            .expect("assistant row");

        let first = |l: &Line<'static>| l.spans.first().cloned().expect("gutter span");
        let user_gutter = first(user_row);
        let assistant_gutter = first(assistant_row);

        assert_eq!(user_gutter.content.as_ref(), GUTTER);
        assert_eq!(assistant_gutter.content.as_ref(), GUTTER);
        assert_eq!(user_gutter.style, state.theme.user_style(), "user rail");
        assert_eq!(
            assistant_gutter.style,
            state.theme.muted_style(),
            "assistant rail"
        );
        assert_ne!(
            user_gutter.style, assistant_gutter.style,
            "the two turns must read differently"
        );
    }

    /// C-149: the C-111 focus highlight paints `sel_bg` over every span of the focused entry's
    /// rows, including the new leading rail span — the rail must not defeat the focus read.
    #[test]
    fn focused_entry_reads_as_focused_with_gutter_present() {
        let mut state = ChatState::new("mock".into());
        state.push_user("first message");
        state.push_user("second message");
        state.focus_move(-1); // lands on the newest entry
        assert_eq!(state.focused, Some(1));

        let rows = state.transcript_lines(40);
        let sel_bg = state.theme.sel_bg;
        let focused_row = rows
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("second message")))
            .expect("focused row");
        let gutter = focused_row.spans.first().expect("gutter span");
        assert_eq!(gutter.content.as_ref(), GUTTER, "rail still present");
        assert_eq!(
            gutter.style.bg,
            Some(sel_bg),
            "rail span must carry the focus background too"
        );
    }

    #[test]
    fn turn_separator_marks_only_a_completed_turn_boundary() {
        let mut incomplete = ChatState::new("mock".into());
        incomplete.push_user("first turn");
        incomplete.push_user("not a completed boundary");
        assert!(incomplete
            .transcript_lines(80)
            .iter()
            .all(|line| !line.spans.iter().any(|span| span.content.contains('─'))));

        let mut state = ChatState::new("mock".into());
        state.push_user("first turn");
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "read".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(1), "read", "done".into(), false);
        state.stream_text("first answer");
        state.end_stream();
        state.last_elapsed = Some(Duration::from_secs(12));
        state.push_user("second turn");
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(2),
            "write".into(),
            serde_json::json!({}),
        )));

        let lines = state.transcript_lines(80);
        let boundaries: Vec<&Line<'static>> = lines
            .iter()
            .filter(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("── 12s ──"))
            })
            .collect();
        assert_eq!(boundaries.len(), 1, "one rule between the two user turns");
        let boundary = boundaries[0];
        assert_eq!(boundary.spans[0].content.as_ref(), GUTTER);
        assert_eq!(boundary.spans[0].style, state.theme.muted_style());
        assert_eq!(boundary.style, state.theme.muted_style());
        assert!(
            boundary
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .ends_with("── 12s ──"),
            "the compact label is right-aligned: {boundary:?}"
        );

        let cached_again = state.transcript_lines(80);
        assert_eq!(
            lines, cached_again,
            "the boundary is stable in the layout cache"
        );
        assert_eq!(
            state
                .transcript_layout
                .borrow()
                .as_ref()
                .and_then(|layout| layout.entry_rows.iter().find(|(index, _, _)| *index == 3))
                .map(|(_, _, count)| *count),
            Some(2),
            "the rule and prompt remain one cached entry"
        );
    }

    #[test]
    fn resumed_turn_separator_uses_the_durable_prior_duration() {
        use flux_events::{AssistantMessage, EventStore, SessionLog};

        let events = EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        let mut log = SessionLog::open(&events, &sid).unwrap();

        log.open_turn(flux_core::Message::user_text("first turn"))
            .unwrap();
        let first_turn = events.begin_turn(&sid, "first turn", "mock").unwrap();
        events
            .end_turn(&sid, first_turn, "done", 1, "first answer", None)
            .unwrap();
        log.close_turn(AssistantMessage::text("first answer").unwrap())
            .unwrap();

        log.open_turn(flux_core::Message::user_text("second turn"))
            .unwrap();
        let second_turn = events.begin_turn(&sid, "second turn", "mock").unwrap();
        events
            .end_turn(&sid, second_turn, "done", 1, "second answer", None)
            .unwrap();
        log.close_turn(AssistantMessage::text("second answer").unwrap())
            .unwrap();

        let first = events
            .turns(&sid)
            .unwrap()
            .into_iter()
            .next()
            .expect("first durable turn");
        let expected = Duration::from_millis(
            first
                .ended_at_ms
                .expect("completed turn")
                .saturating_sub(first.started_at_ms) as u64,
        );

        let mut state = ChatState::for_session("mock".into(), sid.clone());
        state.project_session(&events, &sid).unwrap();
        let second = state
            .entries
            .iter()
            .find(|entry| matches!(entry, Entry::User { text, .. } if text == "second turn"))
            .expect("second projected user turn");
        assert!(matches!(
            second,
            Entry::User {
                prior_elapsed: Some(elapsed),
                show_separator: true,
                ..
            } if *elapsed == expected
        ));
    }

    /// C-149: `Theme::MONO` zeroes every color field, so the rail must still read via a
    /// non-color attribute — `user_style()`'s `BOLD` modifier survives `NO_COLOR` even though the
    /// glyph and every fg/bg collapse to `Reset`.
    #[test]
    fn transcript_gutter_usable_in_mono() {
        let mut state = ChatState::new("mock".into());
        state.theme = Theme::MONO;
        state.push_user("hello there");
        state.stream_text("hi back");
        state.end_stream();

        let row_text =
            |l: &Line<'static>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        let lines = state.transcript_lines(60);
        let user_row = lines
            .iter()
            .find(|l| row_text(l).contains("hello there"))
            .expect("user row");
        let assistant_row = lines
            .iter()
            .find(|l| row_text(l).contains("hi back"))
            .expect("assistant row");
        let user_gutter = user_row.spans.first().expect("gutter span");
        let assistant_gutter = assistant_row.spans.first().expect("gutter span");
        assert_eq!(user_gutter.content.as_ref(), GUTTER);
        assert_eq!(assistant_gutter.content.as_ref(), GUTTER);
        assert!(
            user_gutter.style.add_modifier.contains(Modifier::BOLD),
            "user rail stays distinguishable via BOLD, not color, in MONO"
        );
        assert!(
            !assistant_gutter.style.add_modifier.contains(Modifier::BOLD),
            "assistant rail is not bold"
        );
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

    /// C-150: the new named palettes (`dracula`, `nord`, `high-contrast`) resolve through the
    /// same truecolor/`NO_COLOR` precedence as `dark`/`light`, are listed by `Theme::names`, and
    /// paint an explicit (non-`Reset`) `base_bg` so they stay correct on a mismatched terminal
    /// background.
    #[test]
    fn theme_by_name_resolves_variants_new_palettes() {
        use ratatui::style::Color;
        for name in ["dracula", "nord", "high-contrast"] {
            assert!(Theme::names().contains(&name), "names() missing {name}");

            // ANSI fallback (no truecolor): resolves, and every role is defined (base_bg/text
            // are non-Reset so the theme reads correctly regardless of the terminal's own bg).
            let ansi = Theme::by_name(name, false, false)
                .unwrap_or_else(|| panic!("{name} should resolve without truecolor"));
            assert_ne!(ansi.base_bg, Color::Reset, "{name} ANSI base_bg");

            // Truecolor tuning: distinct RGB base_bg, still non-Reset.
            let rgb = Theme::by_name(name, true, false)
                .unwrap_or_else(|| panic!("{name} should resolve with truecolor"));
            assert!(
                matches!(rgb.base_bg, Color::Rgb(..)),
                "{name} truecolor base_bg should be Rgb, got {:?}",
                rgb.base_bg
            );

            // NO_COLOR still forces MONO regardless of truecolor.
            assert!(matches!(
                Theme::by_name(name, true, true),
                Some(t) if t.accent == Color::Reset && t.base_bg == Color::Reset
            ));
        }
    }

    /// C-104: switching the theme restyles the screen — a known cell's colors change and the
    /// light theme paints the root background.
    #[test]
    fn theme_switch_restyles_screen() {
        let mut state = ChatState::new("mock".into());
        // A non-empty transcript, so C-157's empty-state card (which also contains a "d" glyph,
        // in "commands") doesn't shadow the composer's own "d" for the `.find()` below.
        state.push_user("hi");
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
        let draft = buffer.cell((1, 8)).expect("draft cell");
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
        // C-113: `d` opens the reason input (the denial only resolves on Enter there).
        assert_eq!(
            approval_key(KeyCode::Char('d')),
            ApprovalAction::DenyWithReason
        );
        assert_eq!(
            approval_key(KeyCode::Char('D')),
            ApprovalAction::DenyWithReason
        );
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
            request: op_request("write", subjects),
            scroll: 0,
            reason: None,
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
        queued.push_back((op_request("bash", ["rm -rf tmp"]), reply));
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
        queued.push_back((op_request("write", ["a"]), first));
        queued.push_back((op_request("bash", ["b"]), second));

        show_next_approval(&mut state, &mut current, &mut queued);
        assert!(matches!(current.as_ref(), Some((tool, _)) if tool == "write"));
        assert!(state
            .approval
            .as_ref()
            .is_some_and(|view| view.request.tool == "write" && view.request.subjects == ["a"]));
        current.take();
        state.approval = None;
        show_next_approval(&mut state, &mut current, &mut queued);
        assert!(matches!(current.as_ref(), Some((tool, _)) if tool == "bash"));
        assert!(state
            .approval
            .as_ref()
            .is_some_and(|view| view.request.tool == "bash" && view.request.subjects == ["b"]));
    }

    /// C-112: fuzzy ranking is pinned — path-segment prefix > substring > subsequence, ties to
    /// the shorter path; non-matches drop out.
    #[test]
    fn fuzzy_rank_orders_prefix_substring_subsequence() {
        let paths: Vec<String> = [
            "crates/flux-tui/src/lib.rs",
            "docs/library-notes.md",
            "crates/flux-cli/src/list.rs",
            "README.md",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let ranked = fuzzy_rank(&paths, "lib");
        // Segment-prefix matches first (lib.rs, library-notes.md), shorter path breaking the tie;
        // then the subsequence match (l…i…s…t → contains? "list" contains "li"+"b"? no 'b' —
        // l-i-b as subsequence of crates/flux-cli/src/list.rs? c-l-i has l,i then b absent → NOT
        // a match). So exactly two results.
        assert_eq!(
            ranked,
            vec!["docs/library-notes.md", "crates/flux-tui/src/lib.rs"]
        );
        // Substring beats subsequence.
        let paths: Vec<String> = ["a/xlibx.rs", "a/l_i_b.rs", "a/lib.rs"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            fuzzy_rank(&paths, "lib"),
            vec!["a/lib.rs", "a/xlibx.rs", "a/l_i_b.rs"]
        );
        // Empty query lists everything (segment-prefix tier).
        assert_eq!(fuzzy_rank(&paths, "").len(), 3);
    }

    /// C-112: the inventory walk skips ignored dirs (`.git`, `target`, `node_modules`, hidden)
    /// and stops at the entry cap.
    #[test]
    fn workspace_inventory_is_ignore_aware_and_capped() {
        let dir = std::env::temp_dir().join(format!("flux-tui-inv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["src", ".git", "target/debug", "node_modules/x"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        std::fs::write(dir.join(".git/config"), "").unwrap();
        std::fs::write(dir.join("target/debug/bin"), "").unwrap();
        std::fs::write(dir.join("node_modules/x/i.js"), "").unwrap();

        let inv = workspace_file_inventory(&dir, 100);
        assert_eq!(
            inv,
            vec!["README.md".to_string(), "src/main.rs".to_string()]
        );

        let capped = workspace_file_inventory(&dir, 1);
        assert_eq!(capped.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-112: the `@` trigger only fires at a token start; mid-word `@` (an email) does not,
    /// and an active slash query suppresses it.
    #[test]
    fn at_token_triggers_only_at_token_start() {
        let mut state = ChatState::new("mock".into());
        state.set_input("check @lib");
        assert_eq!(state.at_token().as_deref(), Some("@lib"));
        state.set_input("@");
        assert_eq!(state.at_token().as_deref(), Some("@"));
        state.set_input("mail me at user@example.com");
        assert_eq!(state.at_token(), None);
        state.set_input("/theme");
        assert_eq!(state.at_token(), None);
        state.set_input("plain text");
        assert_eq!(state.at_token(), None);
    }

    /// C-112: the popup renders ranked candidates in the slash-menu slot and Tab-insertion
    /// replaces the `@token` with the selected path; Esc dismisses until the token changes.
    #[test]
    fn path_completion_popup_renders_and_inserts() {
        let mut state = ChatState::new("mock".into());
        state.file_inventory = Some(Arc::new(vec![
            "crates/flux-tui/src/lib.rs".into(),
            "crates/flux-tui/src/state.rs".into(),
            "docs/roadmap.md".into(),
        ]));
        state.set_input("see @lib");
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("crates/flux-tui/src/lib.rs"), "{content}");
        assert!(!content.contains("docs/roadmap.md"), "{content}");

        // Insertion replaces the token, keeping the surrounding text.
        let selected = state.path_popup_matches()[0].clone();
        state.insert_path_completion(&selected);
        assert_eq!(state.input.lines()[0], "see crates/flux-tui/src/lib.rs");

        // Esc-dismissal hides the popup for the SAME token only.
        state.set_input("see @lib");
        state.path_dismissed = Some("@lib".into());
        assert!(state.path_popup_matches().is_empty());
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("lib.rs"), "dismissed popup");
        state.set_input("see @libr");
        assert!(!state.path_popup_matches().is_empty(), "token changed");

        // The slash menu still owns its popup: a slash query never shows paths.
        state.set_input("/the");
        assert!(state.path_popup_matches().is_empty());
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("/theme"));
    }

    /// C-111: the OSC 52 helper pins the exact escape + base64 payload and enforces the size cap.
    #[test]
    fn osc52_copy_builds_base64_payload_and_caps() {
        assert_eq!(
            osc52_copy("hello").as_deref(),
            Some("\x1b]52;c;aGVsbG8=\x07")
        );
        assert_eq!(osc52_copy("").as_deref(), Some("\x1b]52;c;\x07"));
        let big = "x".repeat(OSC52_MAX_TEXT + 1);
        assert_eq!(osc52_copy(&big), None);
        assert!(osc52_copy(&big[..OSC52_MAX_TEXT]).is_some());
    }

    /// C-111: Shift-↑/↓ focus renders the focused entry with the selection background (its
    /// neighbors keep theirs), detaches follow, and bumps the transcript revision so the
    /// layout cache re-keys.
    #[test]
    fn focus_highlights_entry_and_bumps_revision() {
        let mut state = ChatState::new("mock".into());
        state.push_user("first message");
        state.push_user("second message");
        state.follow = true;
        let before = state.transcript_revision;

        state.focus_move(-1); // first press lands on the newest entry
        assert_eq!(state.focused, Some(1));
        assert!(!state.follow);
        assert!(state.transcript_revision > before);

        let rows = state.transcript_lines(40);
        let sel_bg = state.theme.sel_bg;
        let has_sel = move |needle: &str, rows: &[Line]| {
            rows.iter().any(|l| {
                l.spans.iter().any(|s| s.content.contains(needle)) && l.style.bg == Some(sel_bg)
            })
        };
        assert!(
            has_sel("second message", &rows),
            "focused entry highlighted"
        );
        assert!(!has_sel("first message", &rows), "neighbor not highlighted");

        // Step up: highlight moves; Esc clears it.
        state.focus_move(-1);
        assert_eq!(state.focused, Some(0));
        let rows = state.transcript_lines(40);
        assert!(has_sel("first message", &rows));
        assert!(!has_sel("second message", &rows));
        state.focus_clear();
        assert_eq!(state.focused, None);
        let rows = state.transcript_lines(40);
        assert!(!has_sel("first message", &rows));
    }

    /// C-111: Enter on a focused tool card toggles ONLY that card's expansion — the neighbor
    /// stays collapsed — and Ctrl-E's global toggle resets the per-card overrides.
    #[test]
    fn focused_card_expands_independently_of_neighbors() {
        let mut state = ChatState::new("mock".into());
        for (path, old) in [("a.rs", "alpha old"), ("b.rs", "beta old")] {
            state.push(Entry::Tool(ToolEntry::new(
                dispatch(1),
                "edit".into(),
                serde_json::json!({"path": path, "old_string": old, "new_string": "new"}),
            )));
            state.finish_tool(dispatch(1), "edit", format!("edited {path}"), false);
        }
        let flat = |state: &ChatState| -> String {
            state
                .transcript_lines(80)
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect()
        };
        assert!(!flat(&state).contains("alpha old"), "cards start collapsed");

        state.focused = Some(0);
        assert!(state.toggle_focused_card());
        let text = flat(&state);
        assert!(text.contains("alpha old"), "focused card expanded: {text}");
        assert!(
            !text.contains("beta old"),
            "neighbor stays collapsed: {text}"
        );

        // Ctrl-E resets overrides: everything follows the new global state (all expanded).
        state.toggle_details();
        let text = flat(&state);
        assert!(text.contains("alpha old") && text.contains("beta old"));

        // Focus on a non-tool entry: Enter falls through (no toggle).
        state.push_user("just text");
        state.focused = Some(2);
        assert!(!state.toggle_focused_card());
    }

    /// C-114: the boundary-split helper seals only at blank lines outside fences, holds back
    /// successors that could restyle earlier blocks, and never moves backwards as text appends.
    #[test]
    fn split_sealed_prefix_is_conservative_and_monotonic() {
        // No blank line yet → nothing sealed.
        assert_eq!(split_sealed_prefix("Para one is still going"), 0);
        // Paragraph boundary: seals through the blank line once the successor arrived.
        let s = "Para one.\n\nPara tw";
        assert_eq!(split_sealed_prefix(s), 11);
        // Appending more text never shrinks the sealed prefix (monotonic across states).
        let s2 = "Para one.\n\nPara two.\n\nPara three";
        assert_eq!(split_sealed_prefix(s2), 22);
        // A blank inside an open fence seals nothing; the closing fence re-arms.
        let fenced = "Intro.\n\n```rust\nlet x = 1;\n\nlet y = 2;\nstill code";
        assert_eq!(split_sealed_prefix(fenced), 8);
        let closed = "Intro.\n\n```rust\ncode\n```\n\nAfter fen";
        assert_eq!(split_sealed_prefix(closed), 26);
        // A fence opener right after a blank is a safe successor.
        assert_eq!(split_sealed_prefix("Para.\n\n```rust\nx"), 7);
        // List items / indented / blockquote successors could restyle the block before the
        // blank (tight → loose) — held back.
        assert_eq!(split_sealed_prefix("- item one\n\n- item two"), 0);
        assert_eq!(split_sealed_prefix("- item\n\n  continuation"), 0);
        assert_eq!(split_sealed_prefix("> quote\n\n> more"), 0);
        // …but a plain paragraph after a list does seal.
        assert_eq!(split_sealed_prefix("- item\n\nPlain para"), 8);
    }

    /// C-114: while streaming, the sealed prefix renders styled and stays byte-identical
    /// across two successive stream states; the open tail stays plain with the cursor.
    #[test]
    fn streaming_renders_sealed_prefix_styled_and_stable() {
        let theme = Theme::default();
        let mk = |text: &str| Assistant {
            text: text.into(),
            done: false,
            cache: RefCell::new(None),
        };
        let flat = |lines: &[Line]| -> Vec<String> {
            lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };

        let a = mk("**Bold** lead.\n\nSecond para grow");
        let lines_a = a.lines(60, &theme);
        let text_a = flat(&lines_a);
        // The sealed prefix is styled: the literal `**` markers are gone.
        assert!(text_a[0].contains("Bold lead."), "{text_a:?}");
        assert!(!text_a[0].contains("**"), "{text_a:?}");
        // The open tail is plain and carries the cursor.
        assert!(text_a.last().unwrap().contains("Second para grow"));
        assert!(text_a.last().unwrap().ends_with(CURSOR), "{text_a:?}");

        // Append more: every line rendered for the previous prefix is byte-identical.
        let b = mk("**Bold** lead.\n\nSecond para grown longer.\n\nThird st");
        let lines_b = b.lines(60, &theme);
        let text_b = flat(&lines_b);
        let prefix_rows = a.cache.borrow().as_ref().unwrap().2.len();
        assert_eq!(
            text_a[..prefix_rows],
            text_b[..prefix_rows],
            "sealed-prefix lines must not flicker across appends"
        );

        // An open fence stays plain in its entirety until the closing fence arrives.
        let fenced = mk("Intro.\n\n```rust\nlet x: u8 = 1;\nmore code");
        let text_f = flat(&fenced.lines(60, &theme));
        assert!(
            text_f.iter().any(|l| l.contains("```rust")),
            "open fence must stay plain (fence marker visible): {text_f:?}"
        );

        // Sealing is unchanged: the sealed render equals a direct markdown render.
        let mut done = mk("**Bold** lead.\n\nSecond para.");
        done.done = true;
        let sealed = flat(&done.lines(60, &theme));
        let direct = flat(&markdown::render("**Bold** lead.\n\nSecond para.", 60).lines);
        assert_eq!(sealed, direct);
    }

    /// C-113: `d` switches the sheet into a one-line reason input (cursor + swapped hints);
    /// Esc returns to the plain sheet with the approval untouched, Enter resolves the denial
    /// carrying the reason.
    #[tokio::test]
    async fn deny_reason_input_renders_and_resolves_both_paths() {
        let mut state = ChatState::new("mock".into());
        let mut current = None;
        let mut queued = VecDeque::new();
        let (reply, mut reply_rx) = oneshot::channel();
        queued.push_back((op_request("bash", ["rm -rf tmp"]), reply));
        show_next_approval(&mut state, &mut current, &mut queued);

        // `d` → reason mode (the loop maps DenyWithReason to opening the input).
        assert_eq!(
            approval_key(KeyCode::Char('d')),
            ApprovalAction::DenyWithReason
        );
        state.approval.as_mut().unwrap().reason = Some("wrong dir".into());
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("deny reason: wrong dir"), "{content}");
        assert!(content.contains("[Enter]"), "{content}");
        assert!(!content.contains("[y] allow"), "{content}");

        // Esc path: back to the sheet, approval still pending, reply unresolved.
        state.approval.as_mut().unwrap().reason = None;
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("[y]"), "{content}");
        assert!(content.contains("[d]"), "{content}");
        assert!(state.approval.is_some());
        assert!(reply_rx.try_recv().is_err(), "reply must stay unresolved");

        // Enter path: the denial carries the reason inside the choice.
        let (_tool, reply) = current.take().unwrap();
        let _ = reply.send(ApprovalChoice::DenyWithReason("wrong dir".into()));
        assert!(matches!(
            reply_rx.try_recv(),
            Ok(ApprovalChoice::DenyWithReason(r)) if r == "wrong dir"
        ));
    }

    // --- C-180 / C-181: where the wall clock went ---------------------------------------------

    fn model_call_event(stage: &str, round: usize, duration_us: u64, retries: u32) -> UiEvent {
        UiEvent::CallUsage {
            model: "mock".into(),
            stage: stage.into(),
            round,
            operations: 4,
            usage: Usage::default(),
            timing: ModelCallTiming {
                duration: Duration::from_micros(duration_us),
                ttft: Some(Duration::from_millis(900)),
                retries,
            },
        }
    }

    /// C-180 failing first: a completed model call badges the round it measured with its own
    /// latency. Pre-fix the TUI extracted only `usage` off `model.call` and the wait was invisible.
    #[test]
    fn a_sealed_thinking_entry_carries_its_model_call_latency() {
        let mut state = ChatState::new("mock".into());
        state.begin_thinking();
        state.stream_thinking("weighing options");
        state.end_thinking();
        state.attach_model_call(ModelCallBadge {
            stage: "stage.explore".into(),
            round: 2,
            timing: ModelCallTiming {
                duration: Duration::from_millis(4200),
                ttft: Some(Duration::from_millis(900)),
                retries: 0,
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("model explore #2"), "{content}");
        assert!(content.contains("4.2s"), "{content}");
        assert!(content.contains("ttft"), "{content}");
    }

    /// A stage that streams no thinking tokens still made the user wait, so the badge renders on
    /// its own — the thinking body is what's conditional, not the latency.
    #[test]
    fn a_model_call_without_thinking_tokens_still_renders_its_latency() {
        let mut state = ChatState::new("mock".into());
        state.begin_thinking();
        state.end_thinking();
        state.attach_model_call(ModelCallBadge {
            stage: "intent".into(),
            round: 1,
            timing: ModelCallTiming {
                duration: Duration::from_millis(1500),
                ttft: None,
                retries: 2,
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("model intent #1"), "{content}");
        assert!(
            !content.contains("thinking"),
            "no thinking text means no thinking row: {content}"
        );
        assert!(content.contains("2 retries"), "{content}");
    }

    /// Each call badges its own round rather than piling onto the newest entry.
    #[test]
    fn consecutive_model_calls_badge_their_own_rounds() {
        let mut state = ChatState::new("mock".into());
        for round in 1..=2 {
            state.begin_thinking();
            state.stream_thinking("t");
            state.end_thinking();
            state.attach_model_call(ModelCallBadge {
                stage: "explore".into(),
                round,
                timing: ModelCallTiming::default(),
            });
        }
        let badged = state
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Thinking { call: Some(_), .. }))
            .count();
        assert_eq!(badged, 2, "each round keeps its own badge");
    }

    /// C-180: while a model call is in flight the footer names *that* wait beside the turn total,
    /// so a slow model is distinguishable from a slow op without waiting for the turn to end.
    #[test]
    fn the_footer_shows_the_in_flight_model_wait() {
        let mut state = ChatState::new("mock".into());
        state.phase = Phase::Planning;
        state.turn_start = Some(Instant::now() - Duration::from_secs(18));
        state.model_call_start = Some(Instant::now() - Duration::from_secs(3));

        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("model 3"), "{content}");
    }

    /// C-181: a pending backoff takes the footer over from the model timer — the wait is the
    /// provider's, and calling it "model" would misattribute it.
    #[test]
    fn a_pending_retry_takes_over_the_footer_from_the_model_timer() {
        let mut state = ChatState::new("mock".into());
        state.phase = Phase::Planning;
        state.turn_start = Some(Instant::now());
        state.model_call_start = Some(Instant::now());
        state.retry = Some(RetryView {
            attempt: 2,
            max_attempts: 6,
            delay: Duration::from_secs(4),
            reason: "http 429".into(),
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("retry 2/6"), "{content}");
        assert!(content.contains("http 429"), "{content}");
        assert!(
            !content.contains("model 0"),
            "the retry replaces the model timer: {content}"
        );
    }

    /// A budget-free recovery (OAuth refresh, transport fallback) renders without an `N/M` counter
    /// it cannot honestly fill in.
    #[test]
    fn a_budget_free_recovery_renders_without_a_counter() {
        let view = RetryView {
            attempt: 1,
            max_attempts: 0,
            delay: Duration::ZERO,
            reason: "auth refresh".into(),
        };
        assert_eq!(view.label(), "↻ retry · auth refresh");
    }

    /// C-180: the closing summary splits wall clock into total and model wait.
    #[test]
    fn the_turn_summary_splits_total_time_from_model_wait() {
        let mut state = ChatState::new("mock".into());
        state.steps = 4;
        state.last_elapsed = Some(Duration::from_millis(18_100));
        state.last_llm_wait = Some(Duration::from_millis(12_400));

        let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("4 steps"), "{content}");
        assert!(content.contains("llm 12"), "{content}");
    }

    /// A turn that made no model call at all (a `/`-command) shows no `llm` segment — an
    /// `llm 0s` would read as "the model answered instantly", which is not what happened.
    #[test]
    fn a_turn_without_a_model_call_shows_no_llm_segment() {
        let mut state = ChatState::new("mock".into());
        state.steps = 1;
        state.last_elapsed = Some(Duration::from_millis(120));
        state.last_llm_wait = None;

        let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains("llm"));
    }

    /// The `model.call` observation's latency must survive the sink hop — the field the TUI used
    /// to drop on the floor.
    #[test]
    fn the_sink_carries_model_call_latency_and_retries() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx, action_id: 1 };
        sink.observation(&flux_evidence::Observation::new(
            "model.call",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "stage": "explore",
                "round": 3,
                "operations": 7,
                "duration_us": 4_200_000u64,
                "ttft_us": 900_000u64,
                "retries": 2,
                "usage": {},
            }),
        ));
        match untag(rx.try_recv().unwrap()) {
            UiEvent::CallUsage {
                stage,
                round,
                timing,
                ..
            } => {
                assert_eq!(stage, "explore");
                assert_eq!(round, 3);
                assert_eq!(timing.duration, Duration::from_micros(4_200_000));
                assert_eq!(timing.ttft, Some(Duration::from_micros(900_000)));
                assert_eq!(timing.retries, 2);
            }
            _ => panic!("expected a CallUsage event"),
        }
    }

    /// C-181: a `model.retry` observation becomes a live footer signal.
    #[test]
    fn the_sink_turns_a_model_retry_observation_into_a_live_signal() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx, action_id: 1 };
        sink.observation(&flux_evidence::Observation::new(
            "model.retry",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "attempt": 2,
                "max_attempts": 6,
                "delay_ms": 4_000u64,
                "reason": "http 429",
            }),
        ));
        match untag(rx.try_recv().unwrap()) {
            UiEvent::Retry {
                attempt,
                max_attempts,
                delay,
                reason,
            } => {
                assert_eq!((attempt, max_attempts), (2, 6));
                assert_eq!(delay, Duration::from_secs(4));
                assert_eq!(reason, "http 429");
            }
            _ => panic!("expected a Retry event"),
        }
    }

    /// The per-turn model wait is the SUM of every round's call, not just the last one — the same
    /// mistake `Usage::accumulate` made for cache accounting before C-139.
    #[test]
    fn model_wait_accumulates_across_every_round_of_the_turn() {
        let mut state = ChatState::new("mock".into());
        for round in 1..=3 {
            let UiEvent::CallUsage { timing, stage, .. } =
                model_call_event("explore", round, 1_000_000, 0)
            else {
                unreachable!()
            };
            state.begin_thinking();
            state.end_thinking();
            state.record_model_call(ModelCallBadge {
                stage,
                round,
                timing,
            });
        }
        assert_eq!(state.turn_llm_wait, Duration::from_secs(3));
    }

    /// A completed call clears any retry badge left standing — the last retry of a call has no
    /// `Planning(false)` of its own to clear it (C-181).
    #[test]
    fn a_completed_call_clears_a_standing_retry_badge() {
        let mut state = ChatState::new("mock".into());
        state.retry = Some(RetryView {
            attempt: 1,
            max_attempts: 6,
            delay: Duration::from_secs(1),
            reason: "http 503".into(),
        });
        state.record_model_call(ModelCallBadge {
            stage: "explore".into(),
            round: 1,
            timing: ModelCallTiming::default(),
        });
        assert!(state.retry.is_none());
    }

    // --- C-182: whole-plan approval discloses its ops ------------------------------------------

    fn plan_request() -> flux_runtime::PlanApprovalRequest {
        flux_runtime::PlanApprovalRequest {
            summary: "medium · mutating".into(),
            ops: vec!["read".into(), "edit".into(), "process.exec".into()],
            destructive: false,
            mutating: true,
            intents: flux_spec::IntentSet {
                intents: vec![
                    flux_spec::Intent {
                        behavior: flux_spec::IntentBehavior::CommandExecution,
                        target: flux_spec::IntentTarget::Process {
                            command: "cargo test --workspace".into(),
                        },
                        role: flux_spec::IntentRole::ProcessCommand,
                        certainty: flux_spec::IntentCertainty::Certain,
                    },
                    flux_spec::Intent {
                        behavior: flux_spec::IntentBehavior::Operation,
                        target: flux_spec::IntentTarget::Operation {
                            name: "task.sync".into(),
                            effects: vec![
                                flux_spec::Effect::Process,
                                flux_spec::Effect::LocalSystem,
                            ],
                            semantic_effects: Vec::new(),
                        },
                        role: flux_spec::IntentRole::Operation,
                        certainty: flux_spec::IntentCertainty::Certain,
                    },
                    flux_spec::Intent {
                        behavior: flux_spec::IntentBehavior::Unknown,
                        target: flux_spec::IntentTarget::Operation {
                            name: "task.ghost".into(),
                            effects: Vec::new(),
                            semantic_effects: Vec::new(),
                        },
                        role: flux_spec::IntentRole::Operation,
                        certainty: flux_spec::IntentCertainty::Potential,
                    },
                    flux_spec::Intent {
                        behavior: flux_spec::IntentBehavior::Gate,
                        target: flux_spec::IntentTarget::Gate {
                            name: "confirm".into(),
                        },
                        role: flux_spec::IntentRole::Gate,
                        certainty: flux_spec::IntentCertainty::Certain,
                    },
                ],
            },
            requirements: vec![
                flux_runtime::AuthorityRequirement::operation("invoke", "read"),
                flux_runtime::AuthorityRequirement::workspace_write("src/lib.rs"),
            ],
        }
    }

    /// C-182 failing first: the sheet names the ops and the concrete targets. Pre-fix the TUI never
    /// implemented `request_plan`, so the default collapsed everything to `3 op(s) · medium`.
    #[test]
    fn the_plan_approval_sheet_lists_its_ops_and_targets() {
        let mut state = ChatState::new("mock".into());
        state.approval = Some(ApprovalView {
            request: controller::ApprovalRequest {
                tool: "run plan".into(),
                subjects: controller::plan_detail_lines(&plan_request()),
                summary: Some("medium · mutating".into()),
                destructive: false,
                mutating: true,
            },
            scroll: 0,
            reason: None,
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("run plan"), "{content}");
        assert!(content.contains("medium"), "{content}");
        assert!(
            content.contains("read, edit, process.exec"),
            "the op names must be visible: {content}"
        );
        assert!(content.contains("src/lib.rs"), "{content}");
        assert!(content.contains("cargo test --workspace"), "{content}");
        assert!(content.contains("operation task.sync"), "{content}");
        assert!(
            content.contains("operation task.ghost (unknown)"),
            "{content}"
        );
        assert!(content.contains("gate.confirm"), "{content}");
    }

    /// C-182: the approver must handle whole-plan approval ITSELF. Without an override the trait
    /// default fires, which asks `request("run plan", ["3 op(s) · medium · mutating"])` — a count
    /// with no names. This pins that the plan path is the one taken.
    #[tokio::test]
    async fn the_approver_raises_a_plan_request_with_its_ops_not_a_bare_count() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let approver = ChannelApprover { tx };
        let plan = plan_request();
        let raised = tokio::spawn(async move { approver.request_plan(&plan).await });

        let UiEvent::Approval { request, reply } = rx.recv().await.expect("approval raised") else {
            panic!("expected an Approval event");
        };
        assert_eq!(request.tool, "run plan");
        assert_eq!(request.summary.as_deref(), Some("medium · mutating"));
        assert!(!request.destructive);
        assert!(
            request.mutating,
            "C-154: plan.mutating must reach the sheet's ApprovalRequest"
        );
        assert!(
            request.subjects.iter().any(|s| s.contains("process.exec")),
            "the ops must reach the sheet, not just their count: {:?}",
            request.subjects
        );
        assert!(
            request
                .subjects
                .iter()
                .any(|s| s.contains("operation task.sync")),
            "operation intent reaches the sheet: {:?}",
            request.subjects
        );
        assert!(
            request
                .subjects
                .iter()
                .any(|s| s.contains("operation task.ghost (unknown)")),
            "unknown operation intent reaches the sheet: {:?}",
            request.subjects
        );
        assert!(
            request.subjects.iter().any(|s| s.contains("gate.confirm")),
            "gate intent reaches the sheet: {:?}",
            request.subjects
        );
        assert!(
            !request.subjects.iter().any(|s| s.contains("op(s)")),
            "the default `N op(s)` collapse must be gone: {:?}",
            request.subjects
        );

        let _ = reply.send(ApprovalChoice::Allow);
        assert!(matches!(raised.await.unwrap(), ApprovalChoice::Allow));
    }

    /// Operation-kind requirements only restate the ops line, and `*` targets say nothing — both
    /// are dropped so the list is all signal.
    #[test]
    fn plan_detail_lines_skip_operation_and_wildcard_requirements() {
        let lines = controller::plan_detail_lines(&plan_request());
        assert_eq!(lines[0], "ops: read, edit, process.exec");
        assert!(
            !lines.iter().any(|l| l.contains("invoke →")),
            "operation-kind requirements duplicate the ops line: {lines:?}"
        );
        assert!(lines.iter().any(|l| l == "workspace.write → src/lib.rs"));
        assert!(lines
            .iter()
            .any(|l| l == "process.exec → $ cargo test --workspace"));
        assert!(lines
            .iter()
            .any(|l| l == "operation task.sync (Process, LocalSystem)"));
        assert!(lines.iter().any(|l| l == "operation task.ghost (unknown)"));
        assert!(lines.iter().any(|l| l == "gate.confirm"));
    }

    /// A destructive plan says so on its own row, above the scrollable detail list so it can never
    /// be scrolled out of view.
    #[test]
    fn a_destructive_plan_warns_on_its_own_row() {
        let mut state = ChatState::new("mock".into());
        let mut plan = plan_request();
        plan.destructive = true;
        plan.summary = "destructive · contains a destructive operation".into();
        state.approval = Some(ApprovalView {
            request: controller::ApprovalRequest {
                tool: "run plan".into(),
                subjects: controller::plan_detail_lines(&plan),
                summary: Some(plan.summary.clone()),
                destructive: true,
                mutating: true,
            },
            scroll: 20, // scrolled past the detail list
            reason: None,
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(
            content.contains("destructive operation"),
            "the warning must survive scrolling: {content}"
        );
    }

    /// C-115: a pending `edit` approval embeds its hunk diff in the sheet — headers, gutter
    /// numbers, and a `… more` marker when the preview window overflows.
    #[test]
    fn approval_sheet_embeds_diff_preview_for_pending_edit() {
        let mut state = ChatState::new("mock".into());
        let old: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 2", "line two")
            .replace("line 11", "line eleven");
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "edit".into(),
            serde_json::json!({"path": "src/a.rs", "old_string": old, "new_string": new}),
        )));
        state.approval = Some(ApprovalView {
            request: op_request("edit", ["src/a.rs"]),
            scroll: 0,
            reason: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(70, 26)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("@@ -1,4 +1,4 @@"), "{content}");
        assert!(content.contains("line two"), "{content}");
        assert!(content.contains("more diff lines"), "{content}");

        // A non-diffable pending tool renders the sheet without a preview.
        let mut plain = ChatState::new("mock".into());
        plain.push(Entry::Tool(ToolEntry::new(
            dispatch(2),
            "bash".into(),
            serde_json::json!({"command": "ls"}),
        )));
        plain.approval = Some(ApprovalView {
            request: op_request("bash", ["ls"]),
            scroll: 0,
            reason: None,
        });
        terminal.draw(|f| render(f, &plain)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("approve"), "{content}");
        assert!(!content.contains("@@"), "{content}");
    }

    /// C-154 failing first: border + title used to be a fixed `t.accent_style()` for every pending
    /// call, so a destructive delete and a read/write looked identical. The sheet's `┌` border
    /// corner must now carry a visibly different style (color and/or modifier) per risk tier.
    #[test]
    fn approval_sheet_border_style_reflects_risk_tier() {
        fn border_style(state: &ChatState) -> (Color, Modifier) {
            let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
            terminal.draw(|f| render(f, state)).unwrap();
            let cell = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .find(|c| c.symbol() == "┌")
                .expect("bordered approval sheet")
                .clone();
            (cell.fg, cell.modifier)
        }

        let read_request = controller::ApprovalRequest {
            tool: "read".into(),
            subjects: vec!["src/a.rs".into()],
            summary: None,
            destructive: false,
            mutating: false,
        };
        let write_request = controller::ApprovalRequest {
            mutating: true,
            ..read_request.clone()
        };
        let destructive_request = controller::ApprovalRequest {
            destructive: true,
            mutating: true,
            ..read_request.clone()
        };

        let mut state = ChatState::new("mock".into());
        state.approval = Some(ApprovalView {
            request: read_request,
            scroll: 0,
            reason: None,
        });
        let read_style = border_style(&state);

        state.approval = Some(ApprovalView {
            request: write_request,
            scroll: 0,
            reason: None,
        });
        let write_style = border_style(&state);

        state.approval = Some(ApprovalView {
            request: destructive_request,
            scroll: 0,
            reason: None,
        });
        let destructive_style = border_style(&state);

        assert_ne!(
            destructive_style, write_style,
            "a destructive approval must render a visibly different border than a write approval"
        );
        assert_ne!(
            write_style, read_style,
            "a write approval must render a visibly different border than a read approval"
        );
        assert_ne!(
            destructive_style, read_style,
            "a destructive approval must render a visibly different border than a read approval"
        );

        // MONO/NO_COLOR: every role collapses to the same `Color::Reset`, so color alone can't
        // carry the tier — the title text (and the destructive BOLD modifier) must still.
        let mut mono = ChatState::new("mock".into());
        mono.theme = Theme::MONO;
        mono.approval = Some(ApprovalView {
            request: controller::ApprovalRequest {
                tool: "bash".into(),
                subjects: vec!["rm -rf build".into()],
                summary: None,
                destructive: true,
                mutating: true,
            },
            scroll: 0,
            reason: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
        terminal.draw(|f| render(f, &mono)).unwrap();
        let content = screen(&terminal);
        assert!(
            content.contains("destructive"),
            "MONO must still name the tier in the title text: {content}"
        );

        mono.approval = Some(ApprovalView {
            request: controller::ApprovalRequest {
                tool: "write".into(),
                subjects: vec!["src/a.rs".into()],
                summary: None,
                destructive: false,
                mutating: true,
            },
            scroll: 0,
            reason: None,
        });
        terminal.draw(|f| render(f, &mono)).unwrap();
        let content = screen(&terminal);
        assert!(
            content.contains("write") && !content.contains("destructive"),
            "MONO write tier must read distinctly from the destructive tier: {content}"
        );
    }

    #[test]
    fn resumed_session_projects_full_durable_activity() {
        use flux_events::{AssistantMessage, EventStore, NewEvent, PlanAttempt, SessionLog};
        use flux_flow::ast::{RunEvent, StepId};

        let events = EventStore::in_memory().unwrap();
        let sid = events.create_session("mock").unwrap();
        let mut log = SessionLog::open(&events, &sid).unwrap();
        log.open_turn(flux_core::Message::user_text("inspect it"))
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
        log.close_turn(AssistantMessage::text("done").unwrap())
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
        let mut log = flux_events::SessionLog::open(&events, &sid).unwrap();
        log.open_turn(flux_core::Message::user_text("old request"))
            .unwrap();
        log.rewrite(
            flux_events::ValidHistory::new(vec![flux_core::Message::user_text("summary")]).unwrap(),
        )
        .unwrap();
        log.close_turn(flux_events::AssistantMessage::text("new answer").unwrap())
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
    fn resumed_session_keeps_legacy_usage_beside_unscoped_insight_call() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let sid = events.create_session("legacy-model").unwrap();
        let turn = events
            .begin_turn(&sid, "legacy turn", "legacy-model")
            .unwrap();
        events
            .end_turn(
                &sid,
                turn,
                "ok",
                1,
                "done",
                Some(Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    ..Usage::default()
                }),
            )
            .unwrap();
        events
            .record_unscoped_call_usage(
                &sid,
                "summary-model",
                Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    ..Usage::default()
                },
            )
            .unwrap();

        let mut state = ChatState::new("legacy-model".into());
        state.project_session(&events, &sid).unwrap();

        assert_eq!(state.cache.fresh, 110);
        assert_eq!(state.tokens_out, 22);
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
        // A log that opens on an assistant message: a legacy shape the typed seam cannot produce,
        // and precisely what this projection must still render — so it is appended raw.
        events
            .append(
                &sid,
                flux_events::NewEvent::message(flux_core::Message::assistant_text(
                    "Proposed plan:\nflow\n└─ read(\"README.md\")",
                )),
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
        state.cache.fresh = 12_300;
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
    fn queued_footer_count_requires_a_running_action_and_a_nonempty_queue() {
        let footer_text = |state: &ChatState| -> String {
            state
                .footer_line(120)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let mut state = ChatState::new("mock".into());

        state.enqueue("first".into());
        assert!(
            !footer_text(&state).contains("queued"),
            "an idle queue belongs in the preview, not the running footer"
        );

        state.begin_action();
        assert!(footer_text(&state).contains("+1 queued"));

        state.queue.drain();
        assert!(
            !footer_text(&state).contains("queued"),
            "a running action with an empty queue must not retain a stale count"
        );
    }

    #[test]
    fn queued_footer_count_sheds_before_completed_turn_timing() {
        let footer_text = |state: &ChatState, width| -> String {
            state
                .footer_line(width)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let mut state = ChatState::new("mock".into());
        state.steps = 4;
        state.last_elapsed = Some(Duration::from_secs(12));
        state.enqueue("follow up".into());
        state.begin_action();

        let wide = footer_text(&state, 120);
        assert!(wide.contains("4 steps · 12.0s"), "{wide}");
        assert!(wide.contains("· +1 queued"), "{wide}");

        let narrow = (24..120)
            .map(|width| footer_text(&state, width))
            .find(|text| text.contains("4 steps · 12.0s") && !text.contains("queued"))
            .expect("a narrow width must shed the queue count while preserving turn timing");
        assert!(!narrow.contains("queued"), "{narrow}");
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
        state.cache.fresh = 11_300;
        state.cache.read = 1_000;
        state.tokens_out = 840;
        state.cost_usd = Some(1.2345);
        let mut terminal = Terminal::new(TestBackend::new(46, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("12.3k"), "tokens must survive: {content}");
        assert!(!content.contains("$1.2345"), "cost drops first: {content}");
    }

    /// C-139: the header used to fold `TurnEnded.usage`, which `Usage::accumulate` leaves holding
    /// the turn's LAST round — so a multi-round turn contributed one round's cache read and the
    /// header under-counted. The named failing-first test: three calls of a single turn must all
    /// land in the session total.
    #[test]
    fn header_cache_counts_every_model_call_not_just_the_last_round() {
        let call = |read: u64, fresh: u64| Usage {
            input_tokens: fresh,
            output_tokens: 10,
            cache_read_input_tokens: read,
            ..Default::default()
        };
        let calls = [
            call(90_000, 10_000),
            call(60_000, 40_000),
            call(20_000, 80_000),
        ];

        let mut state = ChatState::new("claude/claude-sonnet-5".into());
        // What the engine actually delivers: one `model.call` observation per call, then one
        // turn-end usage carrying the accumulated (last-round) snapshot.
        let mut turn = Usage::default();
        for c in &calls {
            state.record_call_usage("claude/claude-sonnet-5", "explore", 12, c);
            turn.accumulate(c);
        }
        state.record_usage(&turn);

        // The pre-C-139 header would have shown the turn snapshot: 20k read of a 100k prompt.
        assert_eq!(turn.cache_read_input_tokens, 20_000);
        // The fixed header shows all three calls.
        assert_eq!(state.cache.read, 170_000);
        assert_eq!(state.cache.fresh, 130_000);
        assert_eq!(state.cache.prompt_tokens(), 300_000);
        // Output still comes from the turn usage and is still summed exactly once.
        assert_eq!(state.tokens_out, 30);
    }

    /// C-139: read and write are separate figures. A session that only READS from cache and one that
    /// only WRITES to it used to render the identical `cache N` segment.
    #[test]
    fn header_distinguishes_cache_reads_from_cache_writes() {
        let render = |usage: Usage| -> String {
            let mut state = ChatState::new("claude/claude-sonnet-5".into());
            state.record_call_usage("claude/claude-sonnet-5", "explore", 12, &usage);
            let mut terminal = Terminal::new(TestBackend::new(120, 10)).unwrap();
            terminal.draw(|f| render(f, &state)).unwrap();
            screen(&terminal)
        };
        let reader = render(Usage {
            cache_read_input_tokens: 3_200_000,
            ..Default::default()
        });
        let writer = render(Usage {
            cache_creation_input_tokens: 3_200_000,
            ..Default::default()
        });
        assert_ne!(
            reader, writer,
            "a cache-reading session must not render identically to a cache-writing one"
        );
        // The reader is at 100% hit and shows its read tier; the writer is at 0% and shows its write.
        assert!(reader.contains("100%"), "{reader}");
        assert!(reader.contains("↺3.2M"), "{reader}");
        assert!(writer.contains("0%"), "{writer}");
        assert!(writer.contains("✎3.2M"), "{writer}");
    }

    /// C-140: the `/usage` overlay renders the turn per round, so a mid-turn cache collapse is
    /// visible while it happens.
    #[test]
    fn usage_overlay_shows_per_round_hit_rates_and_session_totals() {
        let call = |read: u64, fresh: u64, ops: usize| {
            (
                ops,
                Usage {
                    input_tokens: fresh,
                    output_tokens: 10,
                    cache_read_input_tokens: read,
                    ..Default::default()
                },
            )
        };
        let mut state = ChatState::new("claude/claude-fable-5".into());
        // 91% → 61% → 42%, with the tool set widening at round 3 (12 → 19 operations).
        for (ops, usage) in [
            call(91_000, 9_000, 12),
            call(61_000, 39_000, 12),
            call(42_000, 58_000, 19),
        ] {
            state.record_call_usage("claude/claude-fable-5", "explore", ops, &usage);
        }
        state.usage_open = true;

        let mut terminal = Terminal::new(TestBackend::new(64, 24)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);

        assert!(
            content.contains("usage · "),
            "titled with the session: {content}"
        );
        assert!(content.contains("this turn"), "{content}");
        assert!(content.contains("per round"), "{content}");
        // Each round's own hit rate, not the turn average.
        for pct in ["91%", "61%", "42%"] {
            assert!(content.contains(pct), "missing round {pct}: {content}");
        }
        // The three-way split reconstructs the prompt exactly.
        assert_eq!(state.turn_cache.prompt_tokens(), 300_000);
        assert!(content.contains("read 194.0k"), "{content}");
        assert!(content.contains("fresh 106.0k"), "{content}");
        // The churn marker is derived from the operation count, and only marks the round that changed.
        assert!(content.contains("← tools 19"), "churn marked: {content}");
        assert_eq!(
            content.matches("← tools").count(),
            1,
            "only the changed round: {content}"
        );
        assert!(content.contains("session Σ"), "{content}");
    }

    /// C-140: only a *turn* resets the per-turn view. `/compact` is a maintenance action that goes
    /// through the same `begin_action` bookkeeping, and resetting there erased the usage of the turn
    /// the user had just watched finish — `/usage` read 0% with no rounds while the session totals
    /// still showed the spend.
    #[test]
    fn a_maintenance_action_keeps_the_finished_turns_usage() {
        let mut state = ChatState::new("claude/claude-fable-5".into());
        state.record_call_usage(
            "claude/claude-fable-5",
            "explore",
            12,
            &Usage {
                input_tokens: 10_000,
                output_tokens: 10,
                cache_read_input_tokens: 90_000,
                ..Default::default()
            },
        );
        assert_eq!(state.turn_rounds.len(), 1);

        // `/compact` (and any other non-turn action) allocates an action id but is not a new turn.
        let _ = state.begin_action();
        assert_eq!(
            state.turn_rounds.len(),
            1,
            "the finished turn's rounds survive"
        );
        assert_eq!(state.turn_cache.read, 90_000);

        state.record_background_usage(
            "claude/claude-fable-5",
            &Usage {
                input_tokens: 500,
                output_tokens: 20,
                ..Default::default()
            },
        );
        assert_eq!(state.cache.fresh, 10_500);
        assert_eq!(state.turn_cache.fresh, 10_000);
        assert_eq!(state.turn_rounds.len(), 1);

        // The next real turn does clear it.
        state.begin_turn_usage();
        assert!(state.turn_rounds.is_empty());
        assert!(state.turn_cache.is_empty());
    }

    #[test]
    fn compact_completion_notice_never_calls_a_no_rewrite_success() {
        use flux_flow::engine::CompactionOutcome;

        for (outcome, expected, severity) in [
            (
                CompactionOutcome::Disabled,
                "context compaction is disabled",
                Sev::Info,
            ),
            (CompactionOutcome::Unchanged, "context unchanged", Sev::Info),
            (
                CompactionOutcome::Cancelled,
                "compaction cancelled",
                Sev::Warn,
            ),
        ] {
            let (message, actual_severity) = compaction_notice(outcome);
            assert_eq!(message, expected);
            assert_eq!(actual_severity, severity);
            assert!(!message.contains("context compacted"));
        }

        let (message, severity) = compaction_notice(CompactionOutcome::Compacted {
            from_messages: 8,
            to_messages: 3,
        });
        assert_eq!(message, "compaction check complete");
        assert_eq!(severity, Sev::Info);
    }

    /// C-140: `/usage` is a registered built-in and the command path opens the overlay — the wiring
    /// between the slash table, the dispatcher, and the renderer, not just the state flag.
    #[test]
    fn slash_usage_command_opens_the_overlay() {
        assert!(
            all_slash_commands(&[]).iter().any(|c| c.name == "usage"),
            "/usage must be listed in the slash menu"
        );
        let mut state = ChatState::new("claude/claude-sonnet-5".into());
        state.record_call_usage(
            "claude/claude-sonnet-5",
            "explore",
            12,
            &Usage {
                input_tokens: 1_000,
                cache_read_input_tokens: 3_000,
                ..Default::default()
            },
        );
        assert!(!state.usage_open);
        state.usage_open = true;
        let mut terminal = Terminal::new(TestBackend::new(64, 20)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(screen(&terminal).contains("this turn"), "overlay rendered");
    }

    #[test]
    fn slash_insights_is_listed_as_a_builtin_report() {
        assert!(
            all_slash_commands(&[])
                .iter()
                .any(|command| command.name == "insights"),
            "/insights must be listed in the slash menu"
        );
    }

    /// C-140: a session with no model calls yet renders an empty state, not a bare frame or a
    /// division by zero.
    #[test]
    fn usage_overlay_empty_state() {
        let mut state = ChatState::new("mock".into());
        state.usage_open = true;
        let mut terminal = Terminal::new(TestBackend::new(64, 16)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("no model calls recorded yet"), "{content}");
        assert!(content.contains("esc to close"), "{content}");
    }

    /// C-140: the overlay must fit its frame on a small terminal — the per-round list sheds oldest
    /// rows with a count rather than overflowing.
    #[test]
    fn usage_overlay_degrades_on_a_short_terminal() {
        let mut state = ChatState::new("claude/claude-fable-5".into());
        for round in 0..40u64 {
            state.record_call_usage(
                "claude/claude-fable-5",
                "explore",
                12,
                &Usage {
                    input_tokens: 1_000,
                    cache_read_input_tokens: round * 100,
                    ..Default::default()
                },
            );
        }
        state.usage_open = true;
        let mut terminal = Terminal::new(TestBackend::new(40, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(
            content.contains("earlier"),
            "elision is counted, not silent: {content}"
        );
        // The newest round survives the squeeze; the oldest are the ones elided.
        assert!(content.contains(" 40 "), "newest round kept: {content}");
        assert!(!content.contains("  1 █"), "oldest round elided: {content}");
        // The overlay fits: the composer hint below it still renders, so nothing overflowed the frame.
        assert!(content.contains("esc to close"), "{content}");
        assert!(
            content.contains("Enter send"),
            "overlay overflowed the frame: {content}"
        );
        // No row is clipped mid-token at this width (the read/write/fresh line used to be).
        assert!(
            !content.contains("fresh 40.") || content.contains("fresh 40.0k"),
            "a row is clipped mid-token: {content}"
        );
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
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_creation_input_tokens: 200_000,
            cache_read_input_tokens: 500_000,
            reasoning_tokens: 0,
            ..Default::default()
        };
        // C-139 split the fold: output + cost ride the turn usage, prompt tiers ride the per-call
        // one. A real turn delivers both; this test drives them the same way the event loop does.
        state.record_call_usage("anthropic/claude-sonnet-4-6", "explore", 12, &usage);
        state.record_usage(&usage);

        // Tokens are accumulated across EVERY tier, not just input/output.
        assert_eq!(state.cache.fresh, 1_000_000);
        assert_eq!(state.tokens_out, 100_000);
        assert_eq!(state.cache.read, 500_000);
        assert_eq!(state.cache.write, 200_000);
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
        plain.record_call_usage(
            "mock",
            "explore",
            12,
            &Usage {
                input_tokens: 100,
                cache_read_input_tokens: 50,
                ..Default::default()
            },
        );
        assert!(plain.cost_usd.is_none());
        let mut terminal2 = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal2.draw(|f| render(f, &plain)).unwrap();
        let content2 = screen(&terminal2);
        assert!(content2.contains("cache"));
        assert!(!content2.contains('$'));
    }

    /// C-116: mode badges ride the header's right side, shown only when active/non-default —
    /// `auto-ok`, `gather`, and `effort:<level>` all visible together at full width.
    #[test]
    fn header_shows_mode_badges_when_active() {
        let mut state = ChatState::new("mock".into());
        state.auto_approve = true;
        state.gather_mode = true;
        state.effort = Some("high".into());
        // The shell badge reads the process-global opt-in live; restore it right after the
        // render so concurrently running tests only ever see additive `contains` noise.
        let shell_was = flux_runtime::shell_opt_in();
        flux_runtime::set_shell_opt_in(true);
        let line = state.header_line(100);
        flux_runtime::set_shell_opt_in(shell_was);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("auto-ok"), "{text}");
        assert!(text.contains("shell"), "{text}");
        assert!(text.contains("gather"), "{text}");
        assert!(text.contains("effort:high"), "{text}");

        // Defaults: no badges at all.
        let plain = ChatState::new("mock".into());
        let text: String = plain
            .header_line(100)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!text.contains("auto-ok"), "{text}");
        assert!(!text.contains("gather"), "{text}");
        assert!(!text.contains("effort:"), "{text}");
    }

    /// C-116: on a narrow bar the badges shed before the metrics, and the safety-relevant
    /// `auto-ok` badge is the most precious right segment of all — it survives when even the
    /// token total has been dropped.
    #[test]
    fn narrow_header_drops_badges_last_keeps_auto_ok() {
        let mut state = ChatState::new("a-rather-long-model-name-here".into());
        state.auto_approve = true;
        state.gather_mode = true;
        state.effort = Some("xhigh".into());
        state.record_usage(&Usage {
            input_tokens: 12_345,
            output_tokens: 6_789,
            cache_read_input_tokens: 1_000,
            ..Default::default()
        });

        // Width fits everything minus a few segments: effort/gather shed before tokens.
        let mid: String = state
            .header_line(60)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(mid.contains("auto-ok"), "{mid}");
        assert!(!mid.contains("effort:"), "{mid}");

        // Extremely narrow: only the left identity + auto-ok survive.
        let narrow: String = state
            .header_line(48)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(narrow.contains("auto-ok"), "{narrow}");
        assert!(!narrow.contains("tok"), "{narrow}");
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
    fn session_picker_shows_relative_age_on_one_truncated_line() {
        // C-151: each row renders a compact "… ago" derived from `updated_at_ms`, on the SAME
        // line as the existing marker/id/msg-count/model — never a second row, never untruncated
        // past the overlay width. Ages are set relative to "now" so the assertion never depends
        // on which day the suite happens to run (no wall-clock value baked into the expectation).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut state = ChatState::for_session("mock".into(), "s_2".into());
        state.session_picker = Some(vec![
            flux_events::SessionSummary {
                id: "s_2".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: now_ms - 2 * 3_600_000, // 2h ago
                messages: 4,
                context: Default::default(),
            },
            flux_events::SessionSummary {
                id: "s_1".into(),
                model: "anthropic/sonnet".into(),
                created_at_ms: 0,
                updated_at_ms: now_ms - 60_000, // 1m ago
                messages: 2,
                context: Default::default(),
            },
        ]);
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("2h ago"), "{content}");
        assert!(content.contains("1m ago"), "{content}");
        // Still one line per row: the active marker and msg count are unchanged and share the
        // same row as the new age text.
        assert!(content.contains("● s_2"));
        assert!(content.contains("4 msg"));
    }

    #[test]
    fn session_picker_query_filters_and_ranks_via_the_shared_fuzzy_matcher() {
        // C-153: the session picker gained a typed query, ranked through the same `fuzzy_rank`
        // tiering as `@`-path completion (segment-prefix beats substring beats subsequence).
        let mut state = ChatState::for_session("mock".into(), "s_none".into());
        state.session_picker = Some(vec![
            flux_events::SessionSummary {
                id: "alpha".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
                messages: 1,
                context: Default::default(),
            },
            flux_events::SessionSummary {
                id: "gamma-test".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
                messages: 1,
                context: Default::default(),
            },
            flux_events::SessionSummary {
                id: "beta".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
                messages: 1,
                context: Default::default(),
            },
        ]);
        assert_eq!(
            state.session_picker_matches().len(),
            3,
            "empty query keeps everything"
        );

        state.session_query = "gam".into();
        {
            let matches = state.session_picker_matches();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].id, "gamma-test");
        }

        // A stale out-of-range selection clamps to the filtered set's last valid row rather than
        // panicking or pointing past the end (the same `.min(len - 1)` idiom the slash/path
        // popups already use).
        state.session_sel = 99;
        let matches = state.session_picker_matches();
        let sel = state.session_sel.min(matches.len().saturating_sub(1));
        assert_eq!(sel, 0);
    }

    #[test]
    fn session_picker_esc_clears_query_before_closing_overlay() {
        // C-153: Esc is a two-step undo — first clears a non-empty typed query, only closing the
        // overlay on a second Esc once the query is already empty.
        let mut state = ChatState::for_session("mock".into(), "s_1".into());
        state.session_picker = Some(vec![flux_events::SessionSummary {
            id: "s_1".into(),
            model: "mock".into(),
            created_at_ms: 0,
            updated_at_ms: 0,
            messages: 0,
            context: Default::default(),
        }]);
        state.session_query = "s".into();

        state.session_esc();
        assert!(state.session_query.is_empty());
        assert!(
            state.session_picker.is_some(),
            "first Esc only clears the query"
        );

        state.session_esc();
        assert!(
            state.session_picker.is_none(),
            "second Esc closes the overlay"
        );
    }

    /// C-152: queue, session-picker, and help now go through one shared panel helper, so a panel
    /// whose content doesn't overflow its window is sized EXACTLY `header + body (+ counter)` rows
    /// tall — no reserved-but-unused row. Before the shared helper, the queue and session panels
    /// each hand-rolled `visible + 2`, always reserving a counter row even when the counter was
    /// never rendered; help (which never had a counter) was already exact. Counting `panel_bg`
    /// rows pins that all three now share the identical, waste-free sizing rule.
    #[test]
    fn queue_session_and_help_overlays_size_exactly_to_their_content() {
        fn panel_bg_rows(terminal: &Terminal<TestBackend>, theme: &Theme) -> usize {
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .filter(|&y| {
                    (0..buffer.area.width)
                        .any(|x| buffer.cell((x, y)).is_some_and(|c| c.bg == theme.panel_bg))
                })
                .count()
        }

        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();

        // Queue: one item, nowhere near the 10-row window — no overflow counter, so the panel is
        // exactly 2 rows (header + the one item), never 3.
        let mut state = ChatState::new("mock".into());
        state.enqueue("only item".into());
        state.queue_open = true;
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(!screen(&terminal).contains('┌'), "queue overlay: no border");
        assert_eq!(
            panel_bg_rows(&terminal, &state.theme),
            2,
            "queue overlay: header + 1 row, no reserved blank row"
        );
        state.queue_open = false;

        // Session picker: one session, same story.
        state.session_picker = Some(vec![flux_events::SessionSummary {
            id: "s_1".into(),
            model: "mock".into(),
            created_at_ms: 0,
            updated_at_ms: 0,
            messages: 1,
            context: Default::default(),
        }]);
        terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            !screen(&terminal).contains('┌'),
            "session overlay: no border"
        );
        assert_eq!(
            panel_bg_rows(&terminal, &state.theme),
            2,
            "session overlay: header + 1 row, no reserved blank row"
        );
        state.session_picker = None;

        // Help was already exact-fit before C-152; pinned here so all three share one rule going
        // forward rather than the queue/session pair drifting back to a reserved blank row. A
        // tall terminal so the panel's full content fits with no height-clipping to muddy the count.
        let mut tall_terminal = Terminal::new(TestBackend::new(70, 40)).unwrap();
        state.help_open = true;
        tall_terminal.draw(|f| render(f, &state)).unwrap();
        assert!(
            !screen(&tall_terminal).contains('┌'),
            "help overlay: no border"
        );
        let expected_help_rows = 1 // " help · Esc close " header
            + HELP_KEYS.len()
            + 1 // "commands" sub-header
            + all_slash_commands(&state.file_commands).chunks(2).count();
        assert_eq!(
            panel_bg_rows(&tall_terminal, &state.theme),
            expected_help_rows,
            "help overlay: exact-fit, unchanged by the shared helper"
        );
    }

    #[test]
    fn slash_matches_ranks_subsequence_like_at_path_completion() {
        // C-153: slash matching now shares `fuzzy_rank`'s tiering, so a subsequence query finds a
        // command whose name merely contains its letters in order.
        assert!(slash_matches("thm", &[]).iter().any(|c| c.name == "theme"));
        // Exact-prefix behavior is preserved: "se" still finds both "session" and "sessions".
        let prefix = slash_matches("se", &[]);
        assert!(prefix.iter().any(|c| c.name == "session"));
        assert!(prefix.iter().any(|c| c.name == "sessions"));
    }

    #[test]
    fn slash_menu_shows_overflow_counter_past_the_window() {
        // C-153: the slash menu only ever renders 6 rows; with all built-ins visible on a bare
        // `/`, it must signal that more rows exist below rather than silently hiding them.
        let mut state = ChatState::new("mock".into());
        state.set_input("/");
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("/help"));
        assert!(
            content.contains(&format!("1/{}", BUILTIN_COMMANDS.len())),
            "{content}"
        );
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
        let mut log = flux_events::SessionLog::open(&events, &sid).unwrap();
        log.open_turn(flux_core::Message::user_text("before compact"))
            .unwrap();
        log.rewrite(
            flux_events::ValidHistory::new(vec![
                flux_core::Message::user_text("summary"),
                flux_core::Message::assistant_text("summarised"),
            ])
            .unwrap(),
        )
        .unwrap();
        log.open_turn(flux_core::Message::user_text("after compact"))
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
        assert!(slash_matches("cl", &[]).iter().any(|c| c.name == "clear"));
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

    fn test_command_file(
        name: &str,
        description: &str,
        argument_hint: &str,
    ) -> flux_runtime::metadata::CommandFile {
        flux_runtime::metadata::CommandFile {
            name: name.to_string(),
            description: description.to_string(),
            argument_hint: argument_hint.to_string(),
            body: format!("do the {name} thing: $ARGUMENTS"),
            source: std::path::PathBuf::from(format!(".flux/commands/{name}.md")),
            agent_triggerable: false,
        }
    }

    /// D-186: a discovered command file is listed in the `/` slash menu alongside built-ins, with
    /// its description and argument-hint shown.
    #[test]
    fn slash_menu_lists_discovered_command_files_with_hint() {
        let file_commands = vec![test_command_file("review", "Review a PR", "<pr-number>")];
        let mut state = ChatState::new("opus".into()).with_file_commands(file_commands);
        state.set_input("/rev");
        assert_eq!(state.slash_query().as_deref(), Some("rev"));

        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("/review"));
        assert!(content.contains("Review a PR"));
        assert!(content.contains("pr-number"));
    }

    /// D-186 × C-110: the help overlay lists discovered command files (name, description,
    /// argument-hint) alongside the built-in commands.
    #[test]
    fn help_overlay_lists_discovered_command_files() {
        let file_commands = vec![test_command_file("review", "Review a PR", "<pr-number>")];
        let mut state = ChatState::new("mock".into()).with_file_commands(file_commands);
        state.help_open = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("/review"), "{content}");
        assert!(content.contains("Review a PR"), "{content}");
    }

    /// D-186: dispatching `/name args` substitutes `$ARGUMENTS`/`$1` into the command body —
    /// [`start_turn`] then runs the result exactly like typed input.
    #[test]
    fn file_command_prompt_substitutes_arguments() {
        let file_commands = vec![flux_runtime::metadata::CommandFile {
            name: "greet".to_string(),
            description: "greet someone".to_string(),
            argument_hint: "<name>".to_string(),
            body: "Say hello to $1 ($ARGUMENTS)".to_string(),
            source: std::path::PathBuf::from(".flux/commands/greet.md"),
            agent_triggerable: false,
        }];
        let prompt = file_command_prompt("greet", "world today", &file_commands);
        assert_eq!(prompt.as_deref(), Some("Say hello to world (world today)"));
    }

    /// An undiscovered name resolves to `None` — the caller reports "unknown command" rather than
    /// dispatching a turn.
    #[test]
    fn file_command_prompt_is_none_for_an_unknown_name() {
        let file_commands = vec![test_command_file("review", "Review a PR", "<pr-number>")];
        assert!(file_command_prompt("nope", "", &file_commands).is_none());
    }

    #[test]
    fn expanded_edit_card_shows_a_diff() {
        let mut state = ChatState::new("opus".into());
        state.expand_tools = true;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "edit".into(),
            serde_json::json!({"path": "a.rs", "old_string": "old line", "new_string": "new line"}),
        )));
        state.finish_tool(dispatch(1), "edit", "edited a.rs".into(), false);

        let mut terminal = Terminal::new(TestBackend::new(72, 14)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("- old line"));
        assert!(content.contains("+ new line"));
        // C-115: the expanded card is a real hunk view — header + gutter line numbers.
        assert!(content.contains("@@ -1,1 +1,1 @@"), "{content}");
        assert!(content.contains("@ a.rs"), "{content}");
        assert!(content.contains("1      - old line"), "{content}");
        assert!(content.contains("1 + new line"), "{content}");
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
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "seq 40"}),
        )));
        capped.finish_tool(dispatch(1), "bash", output.clone(), false);
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
            dispatch(2),
            "bash".into(),
            serde_json::json!({"command": "seq 40"}),
        )));
        verbose.finish_tool(dispatch(2), "bash", output, false);
        let content = transcript(&verbose);
        assert!(content.contains("out line 31"));
        assert!(
            content.contains("out line 40"),
            "verbose must show the tool output in full: {content}"
        );
        assert!(!content.contains("more lines"));
    }

    /// C-533: subprocess escape/control bytes never reach a ratatui span. Tool output is
    /// sanitized at the transcript boundary — live results, the C-158 live tail, and historical
    /// ingest — matching the posture of the neighboring surfaces (panes, approval prompts, fleet
    /// names). Escapes are consumed whole; other control bytes are dropped; text survives.
    #[test]
    fn tool_output_is_sanitized_at_the_transcript_boundary() {
        let flat = |state: &ChatState| {
            state
                .transcript_lines(80)
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.clone().into_owned())
                .collect::<String>()
        };
        let clean = |text: &str| {
            !text.contains('\u{1b}') && !text.contains('\u{7}') && !text.contains('\r')
        };

        // The C-158 live tail.
        let mut state = ChatState::new("opus".into());
        state.expand_tools = true;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "cargo test"}),
        )));
        state.progress_tool("bash", "\u{1b}[32mok\u{1b}[0m so far\u{7}".into());
        let live = flat(&state);
        assert!(live.contains("ok so far"), "text survives: {live:?}");
        assert!(clean(&live), "live tail carries no control bytes: {live:?}");

        // The finished result: summary (first line) and expanded detail rows.
        state.finish_tool(
            dispatch(1),
            "bash",
            "\u{1b}[31merror\u{1b}[0m: it broke\nstep 10%\rdone\u{7}".into(),
            true,
        );
        let done = flat(&state);
        assert!(done.contains("error: it broke"), "text survives: {done:?}");
        assert!(
            clean(&done),
            "finished card carries no control bytes: {done:?}"
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let frame = screen(&terminal);
        assert!(
            frame.contains("error: it broke"),
            "text survives: {frame:?}"
        );
        assert!(
            clean(&frame),
            "rendered frame carries no control bytes: {frame:?}"
        );

        // Historical ingest (the resume path).
        let mut resumed = ChatState::new("opus".into());
        resumed.expand_tools = true;
        resumed.push(Entry::Tool(ToolEntry::historical(
            "bash".into(),
            serde_json::json!({"command": "make"}),
            "\u{1b}]0;title\u{7}built\u{1b}[0m fine".into(),
            false,
            Duration::from_secs(1),
        )));
        let historical = flat(&resumed);
        assert!(
            historical.contains("built fine"),
            "text survives: {historical:?}"
        );
        assert!(
            clean(&historical),
            "historical card carries no control bytes: {historical:?}"
        );
    }

    /// C-534: a `git_diff` card's expanded detail renders as a classified diff — hunk headers,
    /// add/del tinting — via the content-shape classifier, although `git_diff` has no
    /// `format_diff` arm. Pinned by style since monochrome carries this one through the theme.
    #[test]
    fn git_diff_card_renders_classified_diff_rows() {
        let mut state = ChatState::new("opus".into());
        state.expand_tools = true;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "git_diff".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(
            dispatch(1),
            "git_diff",
            "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,2 +1,2 @@\n-old\n+new"
                .into(),
            false,
        );
        let rows = state.transcript_lines(80);
        let style_of = |needle: &str| {
            rows.iter()
                .find(|line| {
                    line.spans
                        .iter()
                        .any(|span| span.content.as_ref().trim() == needle)
                })
                .and_then(|line| line.spans.last())
                .map(|span| span.style)
                .unwrap_or_else(|| panic!("row {needle:?} not found"))
        };
        let theme = state.theme;
        assert_eq!(
            style_of("+new").fg,
            theme.ok_style().fg,
            "add row tinted ok"
        );
        assert_eq!(
            style_of("-old").fg,
            theme.err_style().fg,
            "del row tinted err"
        );
        assert_eq!(
            style_of("@@ -1,2 +1,2 @@").fg,
            theme.accent_style().fg,
            "hunk header tinted accent"
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let frame = screen(&terminal);
        assert!(frame.contains("@@ -1,2 +1,2 @@"), "hunk rendered: {frame}");
        assert!(
            frame.contains("-old") && frame.contains("+new"),
            "diff rows rendered: {frame}"
        );
    }

    /// C-536: a wrapped transcript row keeps its left edge — continuation rows repeat the gutter
    /// rail and the logical line's leading indent instead of dissolving to column 0.
    #[test]
    fn wrapped_detail_rows_keep_the_gutter_and_indent() {
        let mut state = ChatState::new("opus".into());
        state.expand_tools = true;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "bash".into(),
            serde_json::json!({"command": "long"}),
        )));
        state.finish_tool(
            dispatch(1),
            "bash",
            format!("first\n{}", "x".repeat(60)),
            false,
        );
        let texts: Vec<String> = state
            .transcript_lines(30)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // The 60-char detail line wraps at 30 columns; every continuation row must still carry
        // the rail + detail indent rather than starting at column 0.
        let wrapped: Vec<&String> = texts.iter().filter(|t| t.contains("xxx")).collect();
        assert!(wrapped.len() >= 2, "the long line wrapped: {texts:?}");
        for row in &wrapped[1..] {
            assert!(
                row.starts_with("│    x"),
                "continuation keeps the left edge: {row:?} in {texts:?}"
            );
        }

        let mut terminal = Terminal::new(TestBackend::new(30, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let frame_rows: Vec<String> = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .filter_map(|x| terminal.backend().buffer().cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect()
            })
            .collect();
        let frame_wrapped: Vec<&String> = frame_rows
            .iter()
            .filter(|row| row.contains("xxx"))
            .collect();
        assert!(
            frame_wrapped.len() >= 2,
            "frame contains wrapped detail: {frame_rows:?}"
        );
        for row in &frame_wrapped[1..] {
            assert!(
                row.starts_with("│    x"),
                "frame continuation keeps its rail: {row:?}"
            );
        }
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
    fn monochrome_queue_and_session_selection_use_a_bold_marker() {
        fn assert_selected_row(terminal: &Terminal<TestBackend>, content_marker: &str) {
            let buffer = terminal.backend().buffer();
            let selected_y = (0..buffer.area.height)
                .find(|&y| {
                    (0..buffer.area.width).any(|x| {
                        buffer
                            .cell((x, y))
                            .is_some_and(|cell| cell.symbol() == content_marker)
                    })
                })
                .expect("selected row content");
            let marker = (0..buffer.area.width)
                .find_map(|x| {
                    buffer
                        .cell((x, selected_y))
                        .filter(|cell| cell.symbol() == "▸")
                })
                .expect("selected row marker");
            let content = (0..buffer.area.width)
                .find_map(|x| {
                    buffer
                        .cell((x, selected_y))
                        .filter(|cell| cell.symbol() == content_marker)
                })
                .expect("selected row content cell");

            assert!(marker.modifier.contains(Modifier::BOLD));
            assert!(content.modifier.contains(Modifier::BOLD));
            assert_eq!(
                buffer
                    .content
                    .iter()
                    .filter(|cell| cell.symbol() == "▸")
                    .count(),
                1,
                "only the selected row carries the marker"
            );
        }

        let mut queue = ChatState::new("mock".into());
        queue.theme = Theme::MONO;
        queue.enqueue("first queued prompt".into());
        queue.enqueue("β selected queue prompt".into());
        queue.queue_sel = 1;
        queue.queue_open = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(frame, &queue)).unwrap();
        assert_selected_row(&terminal, "β");

        let mut sessions = ChatState::for_session("mock".into(), "active".into());
        sessions.theme = Theme::MONO;
        sessions.session_picker = Some(vec![
            flux_events::SessionSummary {
                id: "other".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
                messages: 1,
                context: Default::default(),
            },
            flux_events::SessionSummary {
                id: "β-session".into(),
                model: "mock".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
                messages: 1,
                context: Default::default(),
            },
        ]);
        sessions.session_sel = 1;
        terminal.draw(|frame| render(frame, &sessions)).unwrap();
        assert_selected_row(&terminal, "β");
    }

    #[test]
    fn ui_polish_behaviors_are_structurally_visible() {
        // The composer keeps its glyph boundary in mono and changes only its semantic color by state.
        let mut composer = ChatState::new("mock".into());
        composer.theme = Theme::MONO;
        composer.push_user("hello");
        composer.set_input("draft");
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| render(f, &composer)).unwrap();
        assert_eq!(
            terminal.backend().buffer().cell((0, 18)).unwrap().symbol(),
            "▍"
        );

        // A running turn advertises queued work in the footer.
        composer.begin_action();
        composer.enqueue("follow up".into());
        terminal.draw(|f| render(f, &composer)).unwrap();
        assert!(screen(&terminal).contains("+1 queued"));
    }

    #[test]
    fn queued_previews_and_slash_descriptions_drop_only_below_sixty_columns() {
        let file_commands = vec![test_command_file("review", "Review a PR", "<pr-number>")];
        let mut state = ChatState::new("mock".into()).with_file_commands(file_commands);
        state.push_user("transcript stays visible");
        state.enqueue("queued preview boundary".into());
        state.set_input("/rev");

        let mut wide = Terminal::new(TestBackend::new(60, 16)).unwrap();
        wide.draw(|f| render(f, &state)).unwrap();
        let content = screen(&wide);
        assert!(content.contains("queued preview boundary"), "{content}");
        assert!(content.contains("Review a PR"), "{content}");

        let mut narrow = Terminal::new(TestBackend::new(59, 16)).unwrap();
        narrow.draw(|f| render(f, &state)).unwrap();
        let content = screen(&narrow);
        assert!(content.contains("transcript stays visible"), "{content}");
        assert!(content.contains("/review"), "{content}");
        assert!(!content.contains("queued preview boundary"), "{content}");
        assert!(!content.contains("Review a PR"), "{content}");
    }

    #[test]
    fn multiline_composer_collapses_only_below_forty_columns() {
        let composer_rows = |width| {
            let mut state = ChatState::new("mock".into());
            state.push_user("transcript");
            state.set_input("line one\nline two\nline three");
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            terminal.draw(|f| render(f, &state)).unwrap();
            let rows = (0..terminal.backend().buffer().area.height)
                .filter(|&y| {
                    terminal
                        .backend()
                        .buffer()
                        .cell((1, y))
                        .is_some_and(|cell| cell.bg == state.theme.composer_bg)
                })
                .count();
            (rows, screen(&terminal))
        };

        let (rows, content) = composer_rows(40);
        assert_eq!(
            rows, 3,
            "40 columns retain the multiline composer: {content}"
        );
        assert!(content.contains("line one"), "{content}");
        assert!(content.contains("line three"), "{content}");

        let (rows, content) = composer_rows(39);
        assert_eq!(rows, 1, "39 columns force one composer row: {content}");
        assert!(
            content.contains("line three"),
            "the cursor line remains usable: {content}"
        );
        assert!(
            !content.contains("line one"),
            "off-screen lines stay in the editor: {content}"
        );
    }

    #[test]
    fn fifty_column_layout_keeps_transcript_composer_and_footer_usable() {
        fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
            (0..terminal.backend().buffer().area.width)
                .filter_map(|x| terminal.backend().buffer().cell((x, y)))
                .map(|cell| cell.symbol())
                .collect()
        }

        let mut state = ChatState::new("mock".into());
        state.push_user("transcript marker");
        state.set_input("composer draft");
        let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();

        let content = screen(&terminal);
        assert!(content.contains("transcript marker"), "{content}");
        assert!(content.contains("composer draft"), "{content}");
        assert!(
            row(&terminal, 9).contains("Enter send"),
            "{}",
            row(&terminal, 9)
        );
    }

    #[test]
    fn minimum_supported_terminal_survives_overcommitted_popups() {
        let mut state = ChatState::new("mock".into());
        state.push_user("transcript");
        state.enqueue("queued follow-up".into());
        state.queue_open = true;
        state.set_input("/");

        let mut terminal = Terminal::new(TestBackend::new(24, 6)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        assert_eq!(terminal.backend().buffer().area, Rect::new(0, 0, 24, 6));
    }

    #[test]
    fn interruption_seals_only_in_flight_tools_and_is_terminal() {
        fn outcome(state: &ChatState, index: usize) -> &ToolOutcome {
            match &state.entries[index] {
                Entry::Tool(tool) => tool.result.as_ref().expect("terminal tool outcome"),
                _ => panic!("expected tool entry"),
            }
        }

        let mut state = ChatState::new("mock".into());
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "read".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(1), "read", "loaded".into(), false);
        let succeeded = state.entries.len() - 1;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(2),
            "write".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(2), "write", "denied".into(), true);
        let failed = state.entries.len() - 1;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(3),
            "bash".into(),
            serde_json::json!({}),
        )));
        state.progress_tool("bash", "halfway".into());
        let interrupted = state.entries.len() - 1;

        let action_id = state.begin_action();
        let cancel = CancellationToken::new();
        let mut interrupted_action_id = None;
        interrupt_active_action(&mut state, &cancel, &mut interrupted_action_id);

        assert!(cancel.is_cancelled());
        assert_eq!(interrupted_action_id, Some(action_id));
        assert!(!outcome(&state, succeeded).is_error && !outcome(&state, succeeded).cancelled);
        assert!(outcome(&state, failed).is_error && !outcome(&state, failed).cancelled);
        assert!(!outcome(&state, interrupted).is_error && outcome(&state, interrupted).cancelled);
        assert!(match &state.entries[interrupted] {
            Entry::Tool(tool) => tool.partial.is_empty() && state.tool_lines(tool, 80).len() == 1,
            _ => false,
        });

        // A call already queued behind Ctrl-C is still part of the interrupted action. Its final
        // seal happens when that action's `Finished` marker is handled.
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(4),
            "grep".into(),
            serde_json::json!({}),
        )));
        let raced = state.entries.len() - 1;
        seal_interrupted_action(&mut state, &mut interrupted_action_id);
        assert!(outcome(&state, raced).cancelled);
        assert_eq!(interrupted_action_id, None);

        // A late result must neither replace the cancelled state nor escape as a stray notice.
        let entry_count = state.entries.len();
        state.finish_tool(dispatch(4), "grep", "late success".into(), false);
        assert_eq!(state.entries.len(), entry_count);
        assert!(outcome(&state, raced).cancelled);

        let transcript: String = state
            .transcript_lines(80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(transcript.contains("✓"), "{transcript}");
        assert!(transcript.contains("✗"), "{transcript}");
        assert!(transcript.contains("⊘ cancelled"), "{transcript}");
        assert!(!transcript.contains(RUNNING_BADGE), "{transcript}");
    }

    #[test]
    fn failure_navigation_key_accepts_crossterm_shift_representations() {
        use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        assert_eq!(
            failure_navigation_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL,)),
            Some(FailureNavigation::Next)
        );
        assert_eq!(
            failure_navigation_key(KeyEvent::new(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            Some(FailureNavigation::Previous),
            "CSI-u can preserve shift as a modifier on a lowercase character"
        );
        assert_eq!(
            failure_navigation_key(KeyEvent::new_with_kind_and_state(
                KeyCode::Char('G'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
                KeyEventState::NUM_LOCK,
            )),
            Some(FailureNavigation::Previous),
            "crossterm can normalize shift into the uppercase character"
        );
        assert_eq!(
            failure_navigation_key(KeyEvent::new(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            )),
            None,
            "an unrelated modified chord must remain available"
        );
        assert_eq!(
            failure_navigation_key(KeyEvent::new_with_kind(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL,
                KeyEventKind::Release,
            )),
            None
        );
        assert_eq!(
            failure_navigation_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT,)),
            None
        );
    }

    #[test]
    fn failure_navigation_handler_cycles_centers_and_notices_when_empty() {
        use crossterm::event::{KeyEvent, KeyModifiers};

        let next = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        let previous = KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        let mut state = ChatState::new("mock".into());
        for i in 0..8 {
            state.push_user(format!("lead-in {i}"));
        }
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "read".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(1), "read", "first failure".into(), true);
        let first = state.entries.len() - 1;
        for i in 0..8 {
            state.push_user(format!("between {i}"));
        }
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(2),
            "write".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(2), "write", "second failure".into(), true);
        let second = state.entries.len() - 1;
        state.push_user("tail");

        let _ = state.transcript_viewport(80, 5);
        state.unread = 3;
        assert!(handle_failure_navigation_key(&mut state, next));
        assert_eq!(state.focused, Some(first));
        assert!(!state.follow);
        assert_eq!(state.unread, 0);
        let expected_scroll = {
            let layout = state.transcript_layout.borrow();
            let (_, start, count) = layout
                .as_ref()
                .unwrap()
                .entry_rows
                .iter()
                .find(|(index, _, _)| *index == first)
                .copied()
                .unwrap();
            start
                .saturating_add(count / 2)
                .saturating_sub(state.last_page.get() / 2)
                .min(state.last_max_scroll.get())
        };
        assert_eq!(
            state.scroll, expected_scroll,
            "the selected card is centered"
        );

        assert!(handle_failure_navigation_key(&mut state, next));
        assert_eq!(state.focused, Some(second));
        assert!(handle_failure_navigation_key(&mut state, next));
        assert_eq!(state.focused, Some(first), "next wraps at the end");
        assert!(handle_failure_navigation_key(&mut state, previous));
        assert_eq!(state.focused, Some(second), "previous wraps at the start");

        let mut empty = ChatState::new("mock".into());
        assert!(handle_failure_navigation_key(&mut empty, next));
        assert!(matches!(
            empty.entries.last(),
            Some(Entry::Notice { text, sev: Sev::Info }) if text == "no failed tool cards"
        ));
        assert!(!handle_failure_navigation_key(
            &mut empty,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        ));
    }

    #[test]
    fn failure_navigation_cancelled_badge_and_turn_rules() {
        let mut state = ChatState::new("mock".into());
        state.push_user("first turn");
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(1),
            "read".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(1), "read", "first failure".into(), true);
        let first = state.entries.len() - 1;
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(2),
            "grep".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(2), "grep", "ok".into(), false);
        state.push(Entry::Tool(ToolEntry::new(
            dispatch(3),
            "write".into(),
            serde_json::json!({}),
        )));
        state.finish_tool(dispatch(3), "write", "second failure".into(), true);
        let second = state.entries.len() - 1;
        assert!(state.jump_failure(true));
        assert_eq!(state.focused, Some(first));
        assert!(state.jump_failure(true));
        assert_eq!(state.focused, Some(second));
        assert!(state.jump_failure(false));
        assert_eq!(state.focused, Some(first));

        state.push(Entry::Tool(ToolEntry::new(
            dispatch(4),
            "bash".into(),
            serde_json::json!({}),
        )));
        state.cancel_running_tools();
        let transcript: String = state
            .transcript_lines(80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(transcript.contains("⊘ cancelled"), "{transcript}");
        assert!(!transcript.contains(RUNNING_BADGE), "{transcript}");
    }

    /// Accepted UI item 9: completed tool summary/detail rows get the existing panel surface in
    /// both resolved light palettes. Headers and transcript gutters remain bare, and dark/mono
    /// palettes keep the exact unsurfaced row shape.
    #[test]
    fn completed_tool_rows_use_a_surface_only_in_light_palettes() {
        fn card(theme: Theme, is_error: bool) -> Vec<Line<'static>> {
            let mut state = ChatState::new("mock".into());
            state.theme = theme;
            state.expand_tools = true;
            state.push(Entry::Tool(ToolEntry::new(
                dispatch(1),
                "bash".into(),
                serde_json::json!({"command": "printf 'alpha\\nbeta\\n'"}),
            )));
            state.finish_tool(dispatch(1), "bash", "alpha\nbeta".into(), is_error);
            state.transcript_lines(72)
        }

        let dark = card(Theme::DARK, false);
        let dark_rgb = card(Theme::DARK_RGB, false);
        let mono = card(Theme::MONO, false);
        let light = card(Theme::LIGHT, false);
        let light_rgb = card(Theme::LIGHT_RGB, false);
        let failed_light = card(Theme::LIGHT, true);

        assert!(
            light.len() > 2,
            "expanded card should include summary and detail"
        );
        assert_eq!(light.len(), dark.len(), "surface must not add card rows");
        assert_eq!(
            light_rgb.len(),
            dark_rgb.len(),
            "surface must not add card rows"
        );

        for (theme, lines) in [
            (Theme::LIGHT, light),
            (Theme::LIGHT_RGB, light_rgb),
            (Theme::LIGHT, failed_light),
        ] {
            let header = &lines[0];
            assert_eq!(header.style.bg, None, "header row stays unsurfaced");
            assert!(
                header.spans.iter().all(|span| span.style.bg.is_none()),
                "header and its gutter stay unsurfaced"
            );
            for row in &lines[1..] {
                assert_eq!(row.style.bg, Some(theme.panel_bg));
                assert_eq!(row.spans[0].content.as_ref(), GUTTER);
                assert_eq!(row.spans[0].style.bg, None, "gutter stays outside the card");
                assert!(
                    row.spans[1..]
                        .iter()
                        .all(|span| span.style.bg == Some(theme.panel_bg)),
                    "every summary/detail span gets the card surface"
                );
            }
        }

        for lines in [dark, dark_rgb, mono] {
            assert!(lines.iter().all(|row| {
                row.style.bg.is_none() && row.spans.iter().all(|span| span.style.bg.is_none())
            }));
        }

        let mut light_state = ChatState::new("mock".into());
        light_state.theme = Theme::LIGHT;
        let notice = light_state.entry_lines(
            &Entry::Notice {
                text: "ordinary transcript entry".into(),
                sev: Sev::Info,
            },
            72,
        );
        assert!(notice.iter().all(|row| {
            row.style.bg.is_none() && row.spans.iter().all(|span| span.style.bg.is_none())
        }));
    }

    #[test]
    fn previous_session_count_includes_only_other_non_empty_sessions() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let resumable = events.create_session("mock").unwrap();
        let abandoned = events.create_session("mock").unwrap();
        let active = events.create_session("mock").unwrap();

        flux_events::SessionLog::open(&events, &resumable)
            .unwrap()
            .open_turn(flux_core::Message::user_text("keep this transcript"))
            .unwrap();

        assert_eq!(previous_session_count(&events, &active).unwrap(), 1);
        assert_eq!(previous_session_count(&events, &abandoned).unwrap(), 1);

        flux_events::SessionLog::open(&events, &active)
            .unwrap()
            .open_turn(flux_core::Message::user_text("active transcript"))
            .unwrap();
        assert_eq!(
            previous_session_count(&events, &active).unwrap(),
            1,
            "the active session is not advertised as resumable"
        );
    }

    #[test]
    fn resumable_session_hint_pluralizes_and_only_renders_for_an_empty_transcript() {
        let mut state = ChatState::new("mock".into());
        state.previous_sessions = 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let content = screen(&terminal);
        assert!(content.contains("1 previous session · /sessions to resume"));
        assert!(!content.contains("1 previous sessions"));

        state.previous_sessions = 3;
        terminal.draw(|frame| render(frame, &state)).unwrap();
        assert!(screen(&terminal).contains("3 previous sessions · /sessions to resume"));

        state.push_user("this session is no longer empty");
        terminal.draw(|frame| render(frame, &state)).unwrap();
        assert!(!screen(&terminal).contains("/sessions to resume"));
    }

    #[test]
    fn overflow_scrollbar_and_resumable_empty_state_are_visible_while_following() {
        let mut state = ChatState::new("mock".into());
        for i in 0..30 {
            state.push_user(format!("message {i}"));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!((1..10).any(|y| buffer
            .cell((59, y))
            .is_some_and(|cell| cell.symbol() != " ")));

        let mut empty = ChatState::new("mock".into());
        empty.previous_sessions = 3;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| render(f, &empty)).unwrap();
        assert!(screen(&terminal).contains("3 previous sessions"));
        assert!(screen(&terminal).contains("/sessions to resume"));
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

    /// One measured model call, in the shared budget vocabulary (C-542). The real
    /// [`flux_core::BudgetLedger`] produces every figure these tests assert on, so a surface number
    /// can never be a hand-summed total that disagrees with what actually stops a run.
    fn budget_call(event_id: &str, total_tokens: u64) -> flux_core::BudgetUsageEvent {
        flux_core::BudgetUsageEvent {
            event_id: event_id.into(),
            scope: flux_core::BudgetScope::Segment,
            attribution: flux_core::BudgetAttribution {
                run_id: "run-1".into(),
                session_id: Some("s-1".into()),
                turn_id: Some(1),
                segment: Some("explore".into()),
            },
            spend: flux_core::BudgetSpend {
                model_calls: 1,
                total_tokens,
                ..flux_core::BudgetSpend::default()
            },
            rollup: false,
        }
    }

    /// C-542: the engine's published budget projection is the single source of budget numbers. The
    /// sink decodes it as its own `UiEvent` — projection plus the crossing that rides the event that
    /// crossed it — instead of the surface re-deriving totals from raw usage.
    #[test]
    fn channel_sink_forwards_the_live_budget_projection() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = ChannelSink { tx, action_id: 1 };
        let mut ledger = flux_core::BudgetLedger::new(flux_core::BudgetEnvelope {
            scope: flux_core::BudgetScope::Run,
            target: flux_core::BudgetLimits::with_total_tokens(100),
            limit: flux_core::BudgetLimits::with_total_tokens(400),
        });
        let outcome = ledger.record(&budget_call("call-1", 150));

        sink.observation(&flux_evidence::Observation::new(
            flux_evidence::KIND_BUDGET_PROJECTION,
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "projection": ledger.projection(),
                "warning": outcome.warning,
            }),
        ));
        match untag(rx.try_recv().expect("a Budget event was sent")) {
            UiEvent::Budget {
                projection,
                warning,
                exhausted,
            } => {
                assert_eq!(projection.spent.total_tokens, 150);
                assert_eq!(
                    projection.declared(flux_core::BudgetDimension::TotalTokens),
                    Some(400),
                    "the surface renders the ledger's own declared figure"
                );
                let warning = warning.expect("the crossed target rides its own event");
                assert_eq!(warning.limit, 100);
                assert!(exhausted.is_none(), "a target is never a stop line");
            }
            _ => panic!("expected UiEvent::Budget"),
        }
    }

    /// C-542: budget consumption is visible **while the run executes** — the header shows spent
    /// versus declared and updates as spend accrues, and the two words the vocabulary turns on stay
    /// distinguishable: a crossed target warns, a crossed hard limit is the stop line.
    #[test]
    fn header_shows_live_budget_consumption_and_separates_target_from_limit() {
        let header = |state: &ChatState| -> String {
            state
                .header_line(140)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let transcript = |state: &ChatState| -> String {
            state
                .transcript_lines(140)
                .iter()
                .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                .collect()
        };
        let mut state = ChatState::new("mock".into());
        assert!(
            !header(&state).contains("budget"),
            "an undeclared budget renders nothing at all, never a zero ceiling"
        );

        let mut ledger = flux_core::BudgetLedger::new(flux_core::BudgetEnvelope {
            scope: flux_core::BudgetScope::Run,
            target: flux_core::BudgetLimits::with_total_tokens(1_000),
            limit: flux_core::BudgetLimits::with_total_tokens(4_000),
        });
        let outcome = ledger.record(&budget_call("call-1", 400));
        state.record_budget(ledger.projection(), outcome.warning, outcome.exhausted);
        let running = header(&state);
        assert!(running.contains("budget Σ400/4.0k tok"), "{running}");
        assert!(!running.contains("target"), "{running}");

        // Spend accrues: the header follows the ledger, and crossing the target warns visibly
        // without claiming the run stopped.
        let outcome = ledger.record(&budget_call("call-2", 1_200));
        state.record_budget(ledger.projection(), outcome.warning, outcome.exhausted);
        let over_target = header(&state);
        assert!(
            over_target.contains("budget Σ1.6k/4.0k tok"),
            "{over_target}"
        );
        assert!(over_target.contains("over target"), "{over_target}");
        assert!(
            transcript(&state).contains("budget target crossed"),
            "{}",
            transcript(&state)
        );
        assert!(
            !transcript(&state).contains("budget limit reached"),
            "a target must not be reported as a stop: {}",
            transcript(&state)
        );

        // The hard limit is the stop line, and both surfaces say so.
        let outcome = ledger.record(&budget_call("call-3", 3_000));
        state.record_budget(ledger.projection(), outcome.warning, outcome.exhausted);
        let exhausted = header(&state);
        assert!(exhausted.contains("budget Σ4.6k/4.0k tok"), "{exhausted}");
        assert!(exhausted.contains("limit"), "{exhausted}");
        assert!(
            transcript(&state).contains("budget limit reached"),
            "{}",
            transcript(&state)
        );
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

    #[test]
    fn remote_execution_target_stays_visible_in_the_header() {
        let mut state = ChatState::new("mock".into());
        state.session_id = "s_remote".into();
        state.execution_target = Some("remote https://worker.example:8790 · /srv/project".into());
        let header = state
            .header_line(160)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(header.contains("https://worker.example:8790"), "{header}");
        assert!(header.contains("/srv/project"), "{header}");
    }
}
