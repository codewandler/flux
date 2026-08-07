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
    /// Active typed question, rendered above the composer and outside the transcript.
    pub(crate) interaction: Option<crate::interaction::InteractionView>,
    pub(super) assistant_open: bool,
    pub(super) phase: Phase,
    pub(super) turn_start: Option<Instant>,
    pub(super) session_id: String,
    pub(super) model: String,
    pub(super) model_spec: Option<String>,
    /// Remote endpoint + canonical workspace, kept visible for the whole session.
    pub(super) execution_target: Option<String>,
    /// The surface's cwd at launch, shown as-is in the empty-transcript orientation card (C-157).
    /// Empty for headless/test construction, which just omits the segment rather than showing a
    /// placeholder.
    pub(super) workspace_root: String,
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
    /// C-542: the engine's last published budget projection — spent versus declared for the enforced
    /// envelope, straight from the ledger that enforces it. The surface renders this and re-derives
    /// nothing, so the header can never disagree with the breach that stops the run. `None` means no
    /// envelope has published yet: no budget to show, which is not the same as zero spent.
    pub(super) budget: Option<flux_core::BudgetProjection>,
    pub(super) cost_unpriced: bool,
    pub(super) steps: usize,
    pub(super) last_elapsed: Option<Duration>,
    /// When the in-flight model call started (`Planning(true)`), so the footer can show the wait the
    /// user is currently in rather than only the turn total (C-180). `None` between calls.
    pub(super) model_call_start: Option<Instant>,
    /// Model wait accumulated across the turn in progress, summed from each `model.call`'s own
    /// duration — the split that tells a slow model apart from a slow op.
    pub(super) turn_llm_wait: Duration,
    /// The finished turn's model wait, rendered beside `last_elapsed`. `None` for a turn that made
    /// no model call at all (a `/`-command, a resumed transcript).
    pub(super) last_llm_wait: Option<Duration>,
    /// A provider retry announced but not yet resolved (C-181) — takes over the footer's model
    /// timer while it stands, because a backoff is the one wait that is not the model's fault.
    pub(super) retry: Option<RetryView>,
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
    /// Typed filter over the open session picker (C-153).
    pub(super) session_query: String,
    /// Non-empty durable sessions other than the active one, advertised by the empty-state card.
    pub(super) previous_sessions: usize,
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
    /// Historical, metadata-only Usage Observatory. `None` preserves the live `/usage` mode.
    pub(super) observatory: Option<crate::observatory::UsageObservatory>,
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
    /// When a blank-composer, idle Ctrl-C armed the quit confirmation (C-156). `None` while
    /// unarmed; a stale arm (older than [`crate::CTRL_C_QUIT_WINDOW`]) reads as unarmed too, so a
    /// far-apart pair of presses re-arms instead of quitting.
    pub(super) ctrl_c_armed_at: Option<Instant>,
    /// Host- and agent-pushed panes (C-221), keyed by id and bounded by [`crate::panes`]'s own
    /// constants.
    ///
    /// Two writers reach this, and only two: the surface itself (the C-224 fleet pane) and the
    /// model's `pane.*` ops, which arrive through [`ChatState::pane_queue`] and never touch the
    /// store directly. Panes live entirely outside [`ChatState::transcript_viewport`]: no
    /// layout-cache entry, no `focused` index, no scroll bookkeeping, and not found by transcript
    /// search.
    pub(super) panes: PaneStore,
    /// The agent's pane channel (C-305), when a surface minted one before assembling the agent.
    ///
    /// Held rather than drained-on-arrival because the sink is written from inside a running tool on
    /// another task, while the store may only be touched by the event loop that draws it.
    /// [`ChatState::apply_pending_panes`] is the one crossing point.
    pub(super) pane_queue: Option<Arc<crate::panes::PaneQueue>>,
    /// Agent-to-event-loop typed question bridge.
    pub(super) interaction_queue: Option<Arc<crate::interaction::InteractionQueue>>,
    /// Whether the last drain of [`ChatState::pane_queue`] found the channel refusing commands
    /// (C-324) — the edge [`ChatState::report_dropped_panes`] triggers on, so a sustained overflow
    /// costs the transcript one notice rather than one per frame.
    pub(super) panes_overflowing: bool,
    /// The live sub-agent fleet (C-224), folded from A-79's `subagent.activity` stream by
    /// [`crate::fleet::FleetProjection`]. This is the state; [`ChatState::fleet_rows`] is its
    /// resolution against a particular `now`.
    pub(super) fleet: crate::fleet::FleetProjection,
    /// The fleet resolved against the last `now` [`ChatState::refresh_fleet`] was given — what the
    /// host fleet pane renders.
    ///
    /// Held rather than derived inside `render` so the renderer stays a pure function of state and
    /// so every test can pin the clock, which is the same reason [`crate::fleet`] takes `now` as a
    /// parameter throughout instead of reading it.
    pub(super) fleet_rows: Vec<crate::fleet::WorkerRow>,
    /// Attached Board/Fleet operations projection. `None` is an explicitly standalone chat.
    pub(super) operations: Option<crate::operations::OperationsState>,
    /// C-543: the loop binding the next start will run, as the engine resolved it (C-569) — the
    /// profile, revision and digest it admitted, never an ambient filename. `None` before any
    /// binding is known (headless construction), which renders no segment rather than a placeholder.
    pub(super) loop_binding: Option<flux_core::AgentLoopBindingMetadata>,
    /// The binding this session has already admitted. `Some` makes a selection an explicit
    /// new-session/re-admission decision instead of a silent switch of a running agent.
    pub(super) loop_admitted: Option<flux_core::AgentLoopBindingMetadata>,
    /// Directories rescanned for `*.flux` loops on every selector open, so a loop authored while
    /// the TUI runs (C-544) appears without a restart.
    pub(super) loop_dirs: Vec<std::path::PathBuf>,
    /// The open loop selector (C-543).
    pub(super) loop_selector: Option<crate::agent_loop::LoopSelector>,
    /// The short overlay a selection raises: the outer loop's structure and its description.
    pub(super) loop_overlay: Option<crate::agent_loop::LoopOverlay>,
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

/// A pending provider retry, as the footer renders it (C-181).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetryView {
    pub(super) attempt: u32,
    /// The connect loop's retry budget; `0` for the budget-free recoveries (OAuth refresh,
    /// transport fallback), which render without an `N/M` counter.
    pub(super) max_attempts: u32,
    pub(super) delay: Duration,
    /// Short reason label (`http 429`, `transport`, `auth refresh`, `transport fallback`).
    pub(super) reason: String,
}

impl RetryView {
    /// The footer text: `↻ retry 2/6 · http 429 · 4s`. The `N/M` counter and the delay are each
    /// dropped when they carry no information (a budget-free recovery, an immediate retry), so the
    /// line never claims precision the event doesn't have.
    pub(super) fn label(&self) -> String {
        let mut out = String::from("↻ retry");
        if self.max_attempts > 0 {
            out.push_str(&format!(" {}/{}", self.attempt, self.max_attempts));
        }
        out.push_str(&format!(" · {}", self.reason));
        if !self.delay.is_zero() {
            out.push_str(&format!(" · waiting {}", crate::fmt_elapsed(self.delay)));
        }
        out
    }
}

/// The latency badge attached to one sealed thinking entry (C-180).
#[derive(Debug, Clone)]
pub(super) struct ModelCallBadge {
    pub(super) stage: String,
    pub(super) round: usize,
    pub(super) timing: ModelCallTiming,
}

/// What the agent is doing — drives the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Idle,
    Thinking,
    Planning,
}
