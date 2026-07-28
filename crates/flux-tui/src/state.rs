//! Durable chat state owned by the TUI surface.

use super::*;

/// The chat view's state.
#[derive(Debug)]
pub struct ChatState {
    pub(super) entries: Vec<Entry>,
    pub(super) transcript_revision: u64,
    pub(super) transcript_layout: RefCell<Option<TranscriptLayout>>,
    pub(super) input: TextArea<'static>,
    /// When set, an approval modal is shown over the transcript.
    pub modal: Option<String>,
    pub(super) assistant_open: bool,
    pub(super) phase: Phase,
    pub(super) turn_start: Option<Instant>,
    pub(super) session_id: String,
    pub(super) model: String,
    pub(super) model_spec: Option<String>,
    /// Command files (D-186) discovered by the surface, already filtered against built-in names.
    pub(super) file_commands: Vec<flux_runtime::metadata::CommandFile>,
    pub(super) theme: Theme,
    pub(super) expand_tools: bool,
    pub(super) verbose: bool,
    pub(super) slash_sel: usize,
    pub(super) tokens_in: u64,
    pub(super) tokens_out: u64,
    pub(super) tokens_cache_read: u64,
    pub(super) tokens_cache_write: u64,
    pub(super) tokens_reasoning: u64,
    pub(super) cost_usd: Option<f64>,
    pub(super) cost_model: Option<(String, flux_core::PricingTable)>,
    pub(super) cost_unpriced: bool,
    pub(super) steps: usize,
    pub(super) last_elapsed: Option<Duration>,
    pub(super) history: Vec<String>,
    pub(super) history_pos: Option<usize>,
    pub(super) history_draft: String,
    pub(super) queue: VecDeque<String>,
    pub(super) queue_open: bool,
    pub(super) queue_sel: usize,
    pub(super) queue_edit_index: Option<usize>,
    pub(super) session_picker: Option<Vec<flux_events::SessionSummary>>,
    pub(super) session_sel: usize,
    pub(super) scroll: u16,
    pub(super) follow: bool,
    pub(super) last_max_scroll: Cell<u16>,
    pub(super) last_page: Cell<u16>,
    pub(super) plan_phase: Option<String>,
    pub(super) execute_rounds: usize,
    pub(super) gather_mode: bool,
    pub(super) unread: usize,
    pub(super) next_action_id: u64,
    pub(super) active_action_id: Option<u64>,
}

/// What the agent is doing — drives the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Idle,
    Thinking,
    Planning,
}
