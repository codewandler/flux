//! Durable chat state owned by the TUI surface.

use super::*;

/// The chat view's state.
#[derive(Debug)]
pub struct ChatState {
    pub(super) entries: Vec<Entry>,
    pub(super) transcript_revision: u64,
    pub(super) transcript_layout: RefCell<Option<TranscriptLayout>>,
    pub(super) input: TextArea<'static>,
    /// When set, an approval sheet is shown over the transcript.
    pub approval: Option<ApprovalView>,
    pub(super) assistant_open: bool,
    pub(super) phase: Phase,
    pub(super) turn_start: Option<Instant>,
    pub(super) session_id: String,
    pub(super) model: String,
    pub(super) model_spec: Option<String>,
    /// Command files (D-186) discovered by the surface, already filtered against built-in names.
    pub(super) file_commands: Vec<flux_runtime::metadata::CommandFile>,
    pub(super) theme: Theme,
    pub(super) theme_name: String,
    pub(super) expand_tools: bool,
    pub(super) verbose: bool,
    pub(super) slash_sel: usize,
    pub(super) tokens_out: u64,
    pub(super) tokens_reasoning: u64,
    /// Session-cumulative prompt-cache accounting, folded **per model call** (C-139).
    ///
    /// Was three counters summed from `TurnEnded.usage`, which `Usage::accumulate` leaves holding
    /// the turn's *last round* — so a twelve-round turn contributed round twelve only and the header
    /// systematically under-counted. `cache.fresh` is the old `tokens_in`; read and write are now
    /// separate figures rather than one combined `cache N`, so a session reading from cache no
    /// longer renders identically to one re-writing it.
    pub(super) cache: flux_core::CacheEfficiency,
    /// The same accounting scoped to the turn in progress — the `/usage` overlay's headline (C-140).
    pub(super) turn_cache: flux_core::CacheEfficiency,
    /// One entry per model call of the turn in progress, oldest first, for the overlay's per-round
    /// bars. Cleared when a new turn starts; bounded by [`MAX_TURN_ROUNDS`](crate::MAX_TURN_ROUNDS).
    pub(super) turn_rounds: Vec<RoundUsage>,
    pub(super) cost_usd: Option<f64>,
    pub(super) cost_model: Option<(String, flux_core::PricingTable)>,
    pub(super) cost_unpriced: bool,
    pub(super) steps: usize,
    pub(super) last_elapsed: Option<Duration>,
    pub(super) history: Vec<String>,
    pub(super) history_pos: Option<usize>,
    pub(super) history_draft: String,
    /// The mid-turn steering queue (A-94), shared with the engine via
    /// [`flux_flow::FlowEngine::set_steering`]: while a turn runs, the engine drains it at the
    /// next planner consultation; while idle, leftovers start ordinary follow-up turns. Items
    /// stay editable/retractable here until the engine consumes them.
    pub(super) queue: Arc<SteeringQueue>,
    pub(super) queue_open: bool,
    pub(super) queue_sel: usize,
    /// Id of the queued item being edited in the composer (id-based so a concurrent engine
    /// drain invalidates the edit instead of retargeting a neighbour).
    pub(super) queue_edit: Option<u64>,
    pub(super) session_picker: Option<Vec<flux_events::SessionSummary>>,
    pub(super) session_sel: usize,
    pub(super) scroll: u16,
    pub(super) follow: bool,
    /// Whether the terminal's mouse capture is on (Ctrl-T toggles it live so terminal-native
    /// text selection/copy works while off, C-105).
    pub(super) mouse_capture: bool,
    /// Active Ctrl-R reverse incremental history search (C-107).
    pub(super) history_search: Option<HistorySearch>,
    /// Active Ctrl-F transcript search (C-108).
    pub(super) search: Option<TranscriptSearch>,
    /// Whether the help overlay is open (F1 / `/help`, C-110).
    pub(super) help_open: bool,
    /// The `/usage` overlay (C-140): this turn's cache accounting, per-round bars, session totals.
    pub(super) usage_open: bool,
    /// Focused transcript entry (Shift-↑/↓ moves it, Esc clears; C-111). Enter toggles the
    /// focused tool card's per-card expansion, `y` yanks the entry via OSC 52.
    pub(super) focused: Option<usize>,
    /// Workspace file inventory for `@` path completion (C-112) — built lazily on the first `@`,
    /// cached for the session (staleness across turns is a documented v1 limitation).
    pub(super) file_inventory: Option<Arc<Vec<String>>>,
    /// Selected row in the `@` path-completion popup.
    pub(super) path_sel: usize,
    /// The exact `@token` dismissed with Esc — the popup stays hidden until the token changes.
    pub(super) path_dismissed: Option<String>,
    /// Whether `--yes` auto-approve is active — surfaced as a header badge (C-116).
    pub(super) auto_approve: bool,
    /// The effort level chosen via `/effort`, mirrored here so the sync render path can badge
    /// it without touching the engine lock (C-116). `None` = provider default (no badge).
    pub(super) effort: Option<String>,
    pub(super) last_max_scroll: Cell<u16>,
    pub(super) last_page: Cell<u16>,
    pub(super) plan_phase: Option<String>,
    pub(super) execute_rounds: usize,
    pub(super) gather_mode: bool,
    pub(super) unread: usize,
    pub(super) next_action_id: u64,
    pub(super) active_action_id: Option<u64>,
}

/// One model call of the turn in progress, as the `/usage` overlay renders it (C-140). Sourced from
/// the engine's per-call `model.call` observation, so it is the same data `flux usage` reads offline
/// from the `CallUsage` event log — not a slice of the turn total.
#[derive(Debug, Clone)]
pub(super) struct RoundUsage {
    /// The model that served this call — a mid-turn `/model` switch makes these differ within a turn.
    pub(super) model: String,
    /// The engine stage that issued it (`intent`, `explore`, `stage.<name>`, …).
    pub(super) stage: String,
    /// How many operations were advertised to the model on this call. A change between rounds is
    /// the tool-set churn that cold-writes the cached prefix (tools render before system on the
    /// Anthropic wire), so the overlay marks it — derived from the engine's own metric, not guessed.
    pub(super) operations: usize,
    pub(super) usage: Usage,
}

impl RoundUsage {
    /// This call's cache share of its own prompt, in `0.0..=1.0`.
    pub(super) fn hit_rate(&self) -> f64 {
        match self.usage.context_tokens() {
            0 => 0.0,
            total => self.usage.cache_read_input_tokens as f64 / total as f64,
        }
    }
}

/// What the agent is doing — drives the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Idle,
    Thinking,
    Planning,
}
