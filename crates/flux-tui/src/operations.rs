//! Typed Board/Fleet operations projection for the coordinator TUI.
//!
//! `flux-tui` owns only these bounded presentation types and the interaction contract. The CLI
//! embedding supplies a [`FleetBoardSource`] backed by the same durable readers/mutations as
//! `flux board` and `flux fleet`; this crate never parses repository Markdown, shells out, reads
//! tmux, or captures ANSI output.

use std::cell::Cell;
use std::cmp::Ordering;
use std::iter::Peekable;
use std::str::Chars;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::{truncate, ChatState, Theme};

pub const MAX_WORKERS: usize = 100;
pub const MAX_ITEMS: usize = 200;
pub const MAX_DECISIONS: usize = 100;
pub const MAX_DOCUMENTS: usize = 100;
pub const MAX_FAILURES: usize = 50;
pub const MAX_INTAKE: usize = 50;
pub(crate) const ATTENTION_RAIL_WIDTH: u16 = 34;
pub(crate) const ATTENTION_RAIL_MIN_FRAME: u16 = 104;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetCapacityView {
    /// Configured hard ceiling (`max_workers`).
    pub configured: usize,
    /// C-583's desired assignment-bound capacity, unavailable on older state.
    pub desired: Option<usize>,
    /// Admitted worker agents presently doing assignment work.
    pub active: usize,
    /// Admitted workers finishing before scale-down, unavailable until the state carries it.
    pub draining: Option<usize>,
    /// All durable admitted worker records, including terminal workers.
    pub registered: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetWorkerView {
    pub id: String,
    pub role: String,
    pub status: String,
    pub board_ref: Option<String>,
    pub wave: Option<String>,
    pub session: Option<String>,
    pub worktree: Option<String>,
    pub handoff: Option<String>,
    pub review: Option<String>,
    pub rework_round: Option<u64>,
    pub last_activity: Option<String>,
    pub activity: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardItemView {
    pub board_ref: String,
    pub title: String,
    pub status: String,
    pub priority: Option<i64>,
    pub dependencies: Vec<String>,
    pub design: Option<String>,
    pub epic: Option<String>,
    /// The story's own markdown prose — Goal, Acceptance, Notes — as the projection supplies it,
    /// already stripped of frontmatter and bounded. `None` when the source has no body to hand
    /// over: this crate never reads the repository to find one.
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionOptionView {
    pub id: String,
    pub tradeoff: Option<String>,
    pub recommended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardDecisionView {
    pub decision_ref: String,
    pub board: String,
    pub id: String,
    pub title: String,
    pub question: String,
    pub status: String,
    pub blocks: Vec<String>,
    pub options: Vec<DecisionOptionView>,
    pub outcome: Option<String>,
    pub rationale: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanningDocumentView {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub status: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetricRatioView {
    pub name: String,
    pub schema: String,
    pub done: Option<u64>,
    pub remaining: Option<u64>,
    pub total: Option<u64>,
    pub percent: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetFailureView {
    pub subject: String,
    pub kind: String,
    pub message: String,
    pub candidate: Option<String>,
    pub evidence: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetGoalView {
    pub scope: String,
    pub name: String,
    pub statement: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetWaveView {
    pub id: String,
    pub status: String,
    pub items: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetIntakeView {
    pub id: String,
    pub acknowledgement: String,
    pub source: String,
    pub session: Option<String>,
    pub summary: String,
}

/// One point-in-time, bounded projection. `*_total` fields keep truncation honest.
#[derive(Clone, Debug, PartialEq)]
pub struct FleetBoardSnapshot {
    pub schema: String,
    pub root: String,
    pub running: bool,
    pub main_status: String,
    pub main_session: Option<String>,
    pub revision: u64,
    pub goals_revision: u64,
    pub goals: Vec<FleetGoalView>,
    pub active_wave: Option<FleetWaveView>,
    pub capacity: FleetCapacityView,
    pub workers: Vec<FleetWorkerView>,
    pub workers_total: usize,
    pub items: Vec<BoardItemView>,
    pub items_total: usize,
    pub decisions: Vec<BoardDecisionView>,
    pub decisions_total: usize,
    pub documents: Vec<PlanningDocumentView>,
    pub documents_total: usize,
    pub metrics_schema: String,
    pub metrics: Vec<MetricRatioView>,
    pub stats_facts: Vec<(String, String)>,
    pub status_counts: Vec<(String, u64)>,
    pub history: Vec<(String, u64, u64, u64)>,
    pub failures: Vec<FleetFailureView>,
    pub failures_total: usize,
    pub intake: Vec<FleetIntakeView>,
    pub intake_total: usize,
    pub blocked_items: usize,
    pub attention_required: bool,
}

impl FleetBoardSnapshot {
    pub fn can_send(&self) -> bool {
        self.running && matches!(self.main_status.as_str(), "running" | "working")
    }

    pub fn connection_label(&self) -> &str {
        if !self.running || self.main_status == "stopped" {
            "stopped"
        } else if self.can_send() {
            "connected"
        } else {
            &self.main_status
        }
    }

    pub fn open_decisions(&self) -> usize {
        self.decisions
            .iter()
            .filter(|decision| decision.status == "open")
            .count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetAck {
    pub id: String,
    pub level: String,
    pub revision: String,
    pub message: String,
}

/// Typed in-process operations boundary.
///
/// Mutations are deliberately narrow: coordinator intake acknowledgements, an explicitly confirmed Board
/// decision, and a lifecycle restart. The line is publication and destruction — this boundary exists so a
/// surface cannot push, release, deploy, apply a candidate or clean worktrees, and `restart` does none of
/// those. It re-reads configuration, which is the opposite of a durable side effect: nothing new exists
/// afterwards that did not exist in a file already.
pub trait FleetBoardSource: Send + Sync {
    fn snapshot(&self) -> Result<FleetBoardSnapshot>;
    /// Cheap token for deciding whether the durable runtime projection changed.
    ///
    /// The TUI polls this on its refresh cadence. Implementations must not build a full snapshot
    /// here: a slow token would move the same latency back onto the terminal event loop.
    fn refresh_token(&self) -> Result<String> {
        Ok(String::new())
    }
    /// Drop implementation-owned derived caches before an explicit operator refresh.
    fn invalidate_snapshot_cache(&self) {}
    fn attach_session(&self, session: &str) -> Result<FleetAck>;
    fn accept_requirement(&self, text: &str, session: &str) -> Result<FleetAck>;
    fn deliver_requirement(&self, id: &str, session: &str) -> Result<FleetAck>;
    fn complete_requirement(
        &self,
        id: &str,
        session: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<FleetAck>;
    fn decide(&self, decision_ref: &str, outcome: &str) -> Result<FleetAck>;
    /// Stop and start the fleet so current configuration takes effect.
    ///
    /// Distinct from restarting the process: that changes the executable, this re-reads the fleet — config,
    /// every loop binding, and the coordinator's recorded capability set. Editing `.flux/fleet.toml` and
    /// then wondering whether the attached coordinator has seen it is a question an operator should be able
    /// to answer by asking rather than by reasoning.
    ///
    /// Implementations must refuse while a worker is genuinely active: a stop-and-start beneath a live turn
    /// orphans it, leaving a process running against a coordinator that no longer knows about it.
    fn restart(&self) -> Result<FleetAck> {
        anyhow::bail!("this fleet attachment does not support restart")
    }
}

pub type SharedFleetBoardSource = Arc<dyn FleetBoardSource>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OperationsTab {
    #[default]
    Overview,
    Board,
    Workers,
    Decisions,
    Stats,
}

impl OperationsTab {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Board,
        Self::Workers,
        Self::Decisions,
        Self::Stats,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Board => "board",
            Self::Workers => "workers",
            Self::Decisions => "decisions",
            Self::Stats => "stats",
        }
    }

    pub(crate) fn cycle(self, delta: isize) -> Self {
        let current = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(Self::ALL.len() as isize) as usize;
        Self::ALL[next]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PendingRequirement {
    pub id: String,
    pub text: String,
    pub delivered: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationsState {
    pub snapshot: FleetBoardSnapshot,
    pub projection_status: ProjectionStatus,
    pub open: bool,
    pub tab: OperationsTab,
    pub selected: usize,
    pub detail_open: bool,
    /// First body row shown inside an expanded Board box.
    pub body_scroll: usize,
    /// Deepest scroll the expanded body actually has, as the last frame measured it.
    ///
    /// Only the renderer knows the wrap width, so only it can know where the body ends; recording
    /// it here is what keeps keyboard scrolling from running past a bottom already on screen.
    pub body_scroll_max: Cell<usize>,
    pub decision_option: usize,
    pub confirm_decision: bool,
    pub refresh_error: Option<String>,
    pub last_ack: Option<FleetAck>,
    pub pending: Vec<PendingRequirement>,
    pub turn_failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionStatus {
    Loading,
    Ready,
    Stale,
    Error,
}

impl OperationsState {
    pub(crate) fn new(snapshot: FleetBoardSnapshot) -> Self {
        Self {
            snapshot,
            projection_status: ProjectionStatus::Ready,
            open: false,
            tab: OperationsTab::Overview,
            selected: 0,
            detail_open: false,
            body_scroll: 0,
            body_scroll_max: Cell::new(0),
            decision_option: 0,
            confirm_decision: false,
            refresh_error: None,
            last_ack: None,
            pending: Vec::new(),
            turn_failed: false,
        }
    }

    pub(crate) fn loading(snapshot: FleetBoardSnapshot) -> Self {
        let mut state = Self::new(snapshot);
        state.projection_status = ProjectionStatus::Loading;
        state
    }

    pub(crate) fn select_tab(&mut self, tab: OperationsTab) {
        self.tab = tab;
        self.selected = 0;
        self.detail_open = false;
        self.body_scroll = 0;
        self.confirm_decision = false;
        self.decision_option = 0;
    }

    pub(crate) fn refresh(&mut self, snapshot: FleetBoardSnapshot) {
        self.snapshot = snapshot;
        self.projection_status = ProjectionStatus::Ready;
        self.refresh_error = None;
        self.selected = self.selected.min(self.rows_len().saturating_sub(1));
    }

    pub(crate) fn refresh_failed(&mut self, error: String) {
        self.projection_status = if self.projection_status == ProjectionStatus::Loading {
            ProjectionStatus::Error
        } else {
            ProjectionStatus::Stale
        };
        self.refresh_error = Some(error);
    }

    pub(crate) fn rows_len(&self) -> usize {
        match self.tab {
            OperationsTab::Overview => 1,
            OperationsTab::Board => self.snapshot.items.len(),
            OperationsTab::Workers => self.snapshot.workers.len(),
            OperationsTab::Decisions => self.snapshot.decisions.len(),
            OperationsTab::Stats => self.snapshot.metrics.len(),
        }
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        let len = self.rows_len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        self.detail_open = false;
        self.body_scroll = 0;
        self.confirm_decision = false;
        self.decision_option = 0;
    }

    /// Whether a Board box is expanded, which is when the arrows scroll its body instead of moving
    /// the selection.
    pub(crate) fn board_body_expanded(&self) -> bool {
        self.detail_open && self.tab == OperationsTab::Board
    }

    /// Scroll the expanded body, clamped to the bottom the last frame measured.
    pub(crate) fn scroll_board_body(&mut self, delta: isize) {
        let max = self.body_scroll_max.get() as isize;
        self.body_scroll = (self.body_scroll as isize + delta).clamp(0, max) as usize;
    }

    /// What the Board pane is focused on, for the frame about to be painted.
    pub(crate) fn board_focus(&self) -> BoardFocus<'_> {
        BoardFocus {
            selected: self.selected,
            expanded: self.detail_open,
            body_scroll: self.body_scroll,
            bottom: &self.body_scroll_max,
        }
    }

    pub(crate) fn selected_decision(&self) -> Option<&BoardDecisionView> {
        (self.tab == OperationsTab::Decisions)
            .then(|| self.snapshot.decisions.get(self.selected))
            .flatten()
    }

    /// First Enter arms the explicit confirmation; only the second returns a mutation request.
    pub(crate) fn confirm_selected_decision(&mut self) -> Option<(String, String)> {
        let decision = self.selected_decision()?;
        if decision.status != "open" || decision.options.is_empty() {
            return None;
        }
        if !self.confirm_decision {
            self.confirm_decision = true;
            return None;
        }
        let option = decision.options[self.decision_option.min(decision.options.len() - 1)]
            .id
            .clone();
        Some((decision.decision_ref.clone(), option))
    }

    /// Route one key while the overlay is open, returning what the event loop still owes.
    ///
    /// Every effect on presentation state is applied here; the returned command is the only way an
    /// interaction reaches the [`FleetBoardSource`], and [`OperationsCommand::Decide`] is the only
    /// variant that reaches a write. Routing lives beside the state rather than inline in the
    /// terminal event loop so the Board pane's read-only invariant (C-623) is drivable by a test.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OperationsCommand {
        match key.code {
            KeyCode::Esc if self.confirm_decision => self.confirm_decision = false,
            KeyCode::Esc if self.detail_open => {
                self.detail_open = false;
                self.confirm_decision = false;
            }
            KeyCode::Esc | KeyCode::F(2) | KeyCode::Char('q') => {
                self.open = false;
                self.detail_open = false;
                self.confirm_decision = false;
            }
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                let tab = self.tab.cycle(-1);
                self.select_tab(tab);
            }
            KeyCode::Tab => {
                let tab = self.tab.cycle(1);
                self.select_tab(tab);
            }
            KeyCode::Char('1') => self.select_tab(OperationsTab::Overview),
            KeyCode::Char('2') => self.select_tab(OperationsTab::Board),
            KeyCode::Char('3') => self.select_tab(OperationsTab::Workers),
            KeyCode::Char('4') => self.select_tab(OperationsTab::Decisions),
            KeyCode::Char('5') => self.select_tab(OperationsTab::Stats),
            // C-621: while a Board box is expanded the arrows scroll the story body inside it, so a
            // long body never pushes the rest of the board away. These arms must precede the plain
            // movement arms below — a guard only wins by being matched first.
            KeyCode::Up if self.board_body_expanded() => self.scroll_board_body(-1),
            KeyCode::Down if self.board_body_expanded() => self.scroll_board_body(1),
            KeyCode::PageUp if self.board_body_expanded() => self.scroll_board_body(-10),
            KeyCode::PageDown if self.board_body_expanded() => self.scroll_board_body(10),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Left if self.detail_open && self.tab == OperationsTab::Decisions => {
                self.decision_option = self.decision_option.saturating_sub(1);
                self.confirm_decision = false;
            }
            KeyCode::Right if self.detail_open && self.tab == OperationsTab::Decisions => {
                let options = self
                    .selected_decision()
                    .map_or(0, |decision| decision.options.len());
                self.decision_option = (self.decision_option + 1).min(options.saturating_sub(1));
                self.confirm_decision = false;
            }
            KeyCode::Enter if self.detail_open && self.tab == OperationsTab::Decisions => {
                if let Some((decision_ref, outcome)) = self.confirm_selected_decision() {
                    return OperationsCommand::Decide {
                        decision_ref,
                        outcome,
                    };
                }
            }
            KeyCode::Enter => {
                // Expanding always starts at the top of the body, so a box reopened after being
                // scrolled does not come back mid-paragraph.
                if !self.detail_open {
                    self.body_scroll = 0;
                }
                self.detail_open = true;
            }
            KeyCode::Char('r') => return OperationsCommand::Refresh,
            _ => {}
        }
        OperationsCommand::None
    }

    /// Route one mouse event while the overlay is open. Scrolling moves the selection and nothing
    /// else, so no mouse interaction on any tab can ask for a mutation.
    pub(crate) fn handle_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            _ => {}
        }
    }
}

/// What an overlay interaction still owes the event loop once surface state is updated.
///
/// The Board pane can only ever produce [`Self::None`] or [`Self::Refresh`] — both reads. A status,
/// priority or any other planning field changes through the Board CLI, which validates the
/// transition; the surface offers no bypass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum OperationsCommand {
    /// Surface-local state changed; nothing durable to do.
    #[default]
    None,
    /// Re-read the durable projection.
    Refresh,
    /// A twice-confirmed Board decision — the overlay's single durable write.
    Decide {
        decision_ref: String,
        outcome: String,
    },
}

/// Terminal rows one collapsed Board box occupies: top border, one content row, bottom border.
pub(crate) const BOARD_BOX_ROWS: usize = 3;

/// Rows an expanded box spends on structure rather than story text: both borders, the title row,
/// and the one metadata row.
pub(crate) const BOARD_EXPANDED_CHROME_ROWS: usize = 4;

/// Most body rows an expanded box shows at once. A story body is unbounded; the box is not, so a
/// long body scrolls inside it instead of pushing the rest of the board off screen.
pub(crate) const BOARD_EXPANDED_BODY_ROWS: usize = 14;

/// What the Board pane is showing: which box is selected, whether it is expanded, and how far its
/// body is scrolled. `bottom` is an out-parameter — the renderer records the deepest scroll the
/// body has, because the wrap width it needs to know that exists only during rendering.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoardFocus<'a> {
    pub selected: usize,
    pub expanded: bool,
    pub body_scroll: usize,
    pub bottom: &'a Cell<usize>,
}

/// Body rows an expanded box may claim in a viewport `height` rows tall.
fn expanded_body_rows(height: usize) -> usize {
    BOARD_EXPANDED_BODY_ROWS
        .min(height.saturating_sub(BOARD_EXPANDED_CHROME_ROWS))
        .max(1)
}

/// One status group of the Board pane, as a span over [`BoardPane`]'s ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoardGroup {
    pub status: String,
    pub start: usize,
    pub len: usize,
}

/// A grouped, priority-ordered *index* over the snapshot's Board items.
///
/// Only indices are materialized — one `usize` per item, never a box — and boxes are formatted for
/// the visible window alone. The board carries >1000 items, so eager construction of a box per item
/// would be paid on every frame.
pub(crate) struct BoardPane<'a> {
    items: &'a [BoardItemView],
    order: Vec<usize>,
    groups: Vec<BoardGroup>,
}

impl<'a> BoardPane<'a> {
    pub(crate) fn new(items: &'a [BoardItemView]) -> Self {
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by(|left, right| {
            let left = &items[*left];
            let right = &items[*right];
            story_status_order(&left.status)
                .cmp(&story_status_order(&right.status))
                // Statuses the Board CLI does not rank still group together rather than interleave.
                .then_with(|| left.status.cmp(&right.status))
                // `board next` order inside a group: ascending priority, unprioritized work last,
                // then natural id order so `C-99` precedes `C-100`.
                .then_with(|| {
                    left.priority
                        .unwrap_or(i64::MAX)
                        .cmp(&right.priority.unwrap_or(i64::MAX))
                })
                .then_with(|| natural_ref_cmp(&left.board_ref, &right.board_ref))
        });
        let mut groups: Vec<BoardGroup> = Vec::new();
        for (position, index) in order.iter().enumerate() {
            let status = items[*index].status.as_str();
            match groups.last_mut() {
                Some(group) if group.status == status => group.len += 1,
                _ => groups.push(BoardGroup {
                    status: status.to_string(),
                    start: position,
                    len: 1,
                }),
            }
        }
        Self {
            items,
            order,
            groups,
        }
    }

    /// The item at `position` in board order, not in snapshot order.
    #[cfg(test)]
    pub(crate) fn item(&self, position: usize) -> Option<&'a BoardItemView> {
        self.order.get(position).map(|index| &self.items[*index])
    }

    /// First terminal row of the box at `position`, counting the group headings above it.
    fn row_of(&self, position: usize) -> usize {
        let headings = self
            .groups
            .iter()
            .take_while(|group| group.start <= position)
            .count();
        headings + position * BOARD_BOX_ROWS
    }

    /// Rows the box at `position` occupies — three when collapsed, chrome plus the bounded body
    /// window when it is the expanded one.
    fn box_rows(&self, position: usize, focus: &BoardFocus<'_>, height: usize) -> usize {
        if focus.expanded && position == focus.selected {
            BOARD_EXPANDED_CHROME_ROWS + expanded_body_rows(height)
        } else {
            BOARD_BOX_ROWS
        }
    }

    /// Scroll so the selected box is fully visible; selection is the only paging state the pane
    /// needs, so a refresh cannot leave the viewport somewhere the operator never scrolled to. An
    /// expanded box keeps its top row on screen even when it claims the whole viewport — its body
    /// scrolls inside it, so the header must never be the part that goes.
    fn first_visible_row(&self, focus: &BoardFocus<'_>, height: usize) -> usize {
        if focus.selected >= self.order.len() {
            return 0;
        }
        let top = self.row_of(focus.selected);
        let bottom = top + self.box_rows(focus.selected, focus, height);
        top.min(bottom.saturating_sub(height))
    }

    /// Render at most `height` rows: group headings plus the boxes that intersect the viewport.
    /// Items above or below it cost index arithmetic only.
    pub(crate) fn window_lines(
        &self,
        focus: &BoardFocus<'_>,
        snapshot: &FleetBoardSnapshot,
        theme: &Theme,
        width: usize,
        height: usize,
    ) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        if width == 0 || height == 0 {
            return lines;
        }
        let first = self.first_visible_row(focus, height);
        let mut row = 0usize;
        'groups: for group in &self.groups {
            if row >= first {
                if lines.len() >= height {
                    break 'groups;
                }
                lines.push(group_heading(&group.status, group.len, theme, width));
            }
            row += 1;
            let skipped = if first > row {
                ((first - row) / BOARD_BOX_ROWS).min(group.len)
            } else {
                0
            };
            row += skipped * BOARD_BOX_ROWS;
            for position in (group.start + skipped)..(group.start + group.len) {
                if lines.len() >= height {
                    break 'groups;
                }
                let top = row;
                row += self.box_rows(position, focus, height);
                let item = &self.items[self.order[position]];
                let wave = fleet_wave_in_flight(snapshot, &item.board_ref);
                let selected = position == focus.selected;
                let painted = if selected && focus.expanded {
                    expanded_box(
                        item,
                        wave.as_deref(),
                        theme,
                        width,
                        expanded_body_rows(height),
                        focus,
                    )
                } else {
                    collapsed_box(item, wave.as_deref(), selected, theme, width)
                };
                for (offset, line) in painted.into_iter().enumerate() {
                    if top + offset < first {
                        continue;
                    }
                    if lines.len() >= height {
                        break 'groups;
                    }
                    lines.push(line);
                }
            }
        }
        lines
    }
}

/// Status group order, mirroring the Board projection's own ranking.
fn story_status_order(status: &str) -> u8 {
    match status {
        "in-progress" => 0,
        "ready" => 1,
        "blocked" => 2,
        "backlog" => 3,
        "done" => 4,
        _ => 5,
    }
}

/// Compare digit runs numerically so `flux/C-99` sorts ahead of `flux/C-100`, matching the natural
/// id tiebreak `board next` applies after priority.
fn natural_ref_cmp(left: &str, right: &str) -> Ordering {
    fn digits(chars: &mut Peekable<Chars<'_>>) -> u128 {
        let mut value: u128 = 0;
        while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
            value = value.saturating_mul(10).saturating_add(u128::from(digit));
            chars.next();
        }
        value
    }
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                match digits(&mut left).cmp(&digits(&mut right)) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            (Some(a), Some(b)) => match a.cmp(&b) {
                Ordering::Equal => {
                    left.next();
                    right.next();
                }
                other => return other,
            },
        }
    }
}

/// Whether the Fleet currently has this Board item in flight, and under which wave.
///
/// Only Fleet state answers this: a live worker's assignment, or membership of the active wave
/// (which the projection already filters to in-flight statuses). A Board status of `in-progress` is
/// not evidence that anything is running right now.
fn fleet_wave_in_flight(snapshot: &FleetBoardSnapshot, board_ref: &str) -> Option<String> {
    let working = snapshot.workers.iter().find(|worker| {
        worker.board_ref.as_deref() == Some(board_ref)
            && matches!(worker.status.as_str(), "working" | "running" | "active")
    });
    if let Some(worker) = working {
        return Some(
            worker
                .wave
                .clone()
                .unwrap_or_else(|| "in flight".to_string()),
        );
    }
    let wave = snapshot.active_wave.as_ref()?;
    wave.items
        .iter()
        .any(|item| item == board_ref)
        .then(|| wave.id.clone())
}

fn group_heading(status: &str, count: usize, theme: &Theme, width: usize) -> Line<'static> {
    Line::styled(
        truncate(&format!("{status} · {count} item(s)"), width),
        theme.accent_style().add_modifier(Modifier::BOLD),
    )
}

/// One collapsed item: a bordered box carrying id, status, priority, title, and — when Fleet state
/// says so — the in-flight wave. Nothing else, so the count of items on screen stays the point.
fn collapsed_box(
    item: &BoardItemView,
    wave: Option<&str>,
    selected: bool,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(2);
    let selected_style = selected_box_style(theme);
    let border = if selected {
        selected_style
    } else {
        theme.muted_style()
    };
    let head = if selected {
        selected_style
    } else {
        theme.accent_style()
    };
    let body = if selected {
        selected_style
    } else {
        theme.panel_style()
    };
    let flight = if selected {
        selected_style
    } else {
        theme.warn_style()
    };

    vec![
        box_top_line(item, wave, border, head, flight, inner),
        box_row(
            vec![Span::styled(
                truncate(&item.title, inner.saturating_sub(2)),
                body,
            )],
            border,
            body,
            inner,
        ),
        box_bottom_line(None, border, border, inner),
    ]
}

/// One expanded item: the collapsed chrome, the item's own planning metadata, and the story body
/// rendered as markdown inside the box. The body window is bounded and scrolls in place, so the
/// rest of the board keeps its rows.
fn expanded_box(
    item: &BoardItemView,
    wave: Option<&str>,
    theme: &Theme,
    width: usize,
    body_rows: usize,
    focus: &BoardFocus<'_>,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(2);
    let text = inner.saturating_sub(2);
    let border = selected_box_style(theme);
    let pad = theme.panel_style();

    let mut lines = vec![
        box_top_line(item, wave, border, border, border, inner),
        box_row(
            vec![Span::styled(truncate(&item.title, text), border)],
            border,
            border,
            inner,
        ),
        box_row(
            vec![Span::styled(
                truncate(
                    &format!(
                        "epic {} · design {} · depends on {}",
                        item.epic.as_deref().unwrap_or("—"),
                        item.design.as_deref().unwrap_or("—"),
                        display_list(&item.dependencies)
                    ),
                    text,
                ),
                theme.muted_style(),
            )],
            border,
            pad,
            inner,
        ),
    ];
    // Width-aware by construction. The story markdown wraps to the *text* column — the pane less
    // both borders and both gutters — and a hard wrap catches what the layout could not break, such
    // as a URL longer than the box. Wrapping to the pane width, or to the box interior, is exactly
    // the `area_width`-versus-usable-columns off-by-one the transcript carried: it costs one
    // character at every row boundary.
    let rendered = match item
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(body) => crate::wrap_styled_lines(
            crate::markdown::render(body, inner as u16).lines,
            inner as u16,
        ),
        None => vec![Line::styled(
            truncate("this projection carries no story body", text),
            theme.muted_style(),
        )],
    };
    // The box is bounded; the body is not. Everything past the window scrolls in place, and the
    // renderer is the only party that knows the wrap width, so it reports the bottom back.
    let deepest = rendered.len().saturating_sub(body_rows);
    focus.bottom.set(deepest);
    let start = focus.body_scroll.min(deepest);
    for line in rendered.iter().skip(start).take(body_rows) {
        lines.push(box_row(line.spans.clone(), border, pad, inner));
    }
    for _ in rendered.len().saturating_sub(start)..body_rows {
        lines.push(box_row(Vec::new(), border, pad, inner));
    }
    let hint = (deepest > 0).then(|| {
        format!(
            " rows {}–{} of {} ",
            start + 1,
            (start + body_rows).min(rendered.len()),
            rendered.len()
        )
    });
    lines.push(box_bottom_line(hint, border, theme.muted_style(), inner));
    lines
}

fn selected_box_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.accent)
        .bg(theme.sel_bg)
        .add_modifier(Modifier::BOLD)
}

/// The bordered top of a box: `id · status · priority` on the left, the Fleet in-flight marker
/// flush right, box rule between them.
fn box_top_line(
    item: &BoardItemView,
    wave: Option<&str>,
    border: Style,
    head: Style,
    flight: Style,
    inner: usize,
) -> Line<'static> {
    let label = truncate(
        &format!(
            " {} · {} · {} ",
            item.board_ref,
            item.status,
            item.priority
                .map_or_else(|| "p—".to_string(), |value| format!("p{value}"))
        ),
        inner,
    );
    let marker = wave
        .map(|wave| truncate(&format!("◆ {wave} "), inner.saturating_sub(label.width())))
        .filter(|marker| !marker.is_empty());
    let fill = inner
        .saturating_sub(label.width())
        .saturating_sub(marker.as_deref().map_or(0, UnicodeWidthStr::width));

    let mut top = vec![Span::styled("┌", border), Span::styled(label, head)];
    if fill > 0 {
        top.push(Span::styled("─".repeat(fill), border));
    }
    if let Some(marker) = marker {
        top.push(Span::styled(marker, flight));
    }
    top.push(Span::styled("┐", border));
    Line::from(top)
}

/// One bordered content row: `spans` padded to the box's inner width, with a space of gutter on
/// each side so no glyph ever touches a border. Every row a box paints is exactly `width` columns,
/// which is what keeps a wrapped body from overflowing the pane.
fn box_row(spans: Vec<Span<'static>>, border: Style, pad: Style, inner: usize) -> Line<'static> {
    let used: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let fill = inner.saturating_sub(2).saturating_sub(used);
    let mut row = vec![Span::styled("│", border), Span::styled(" ", pad)];
    row.extend(spans);
    row.push(Span::styled(" ".repeat(fill + 1), pad));
    row.push(Span::styled("│", border));
    Line::from(row)
}

/// The bordered bottom of a box, optionally carrying a right-aligned hint (the expanded body's
/// position in a body too long to show at once).
fn box_bottom_line(
    hint: Option<String>,
    border: Style,
    hint_style: Style,
    inner: usize,
) -> Line<'static> {
    let hint = hint
        .map(|hint| truncate(&hint, inner))
        .filter(|hint| !hint.is_empty());
    let fill = inner.saturating_sub(hint.as_deref().map_or(0, UnicodeWidthStr::width));
    let mut row = vec![Span::styled("└", border)];
    if fill > 0 {
        row.push(Span::styled("─".repeat(fill), border));
    }
    if let Some(hint) = hint {
        row.push(Span::styled(hint, hint_style));
    }
    row.push(Span::styled("┘", border));
    Line::from(row)
}

pub(crate) fn split_chat_area(area: Rect, state: &ChatState) -> (Rect, Option<Rect>) {
    if state.operations.is_none() || area.width < ATTENTION_RAIL_MIN_FRAME {
        return (area, None);
    }
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(48),
            Constraint::Length(ATTENTION_RAIL_WIDTH),
        ])
        .split(area);
    (chunks[0], Some(chunks[1]))
}

pub(crate) fn render_attention_rail(frame: &mut Frame, state: &ChatState, area: Rect) {
    let Some(ops) = state.operations.as_ref() else {
        return;
    };
    let snapshot = &ops.snapshot;
    let theme = &state.theme;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!(
                    "{} {}",
                    if snapshot.can_send() { "●" } else { "○" },
                    snapshot.connection_label()
                ),
                if snapshot.can_send() {
                    theme.ok_style()
                } else {
                    theme.warn_style()
                },
            ),
            Span::styled(format!("  r{}", snapshot.revision), theme.muted_style()),
        ]),
        Line::styled(
            truncate(&snapshot.root, area.width.saturating_sub(2) as usize),
            theme.muted_style(),
        ),
    ];
    match ops.projection_status {
        ProjectionStatus::Loading => lines.push(Line::styled(
            "Board/Fleet projection loading…",
            theme.muted_style(),
        )),
        ProjectionStatus::Error => lines.push(Line::styled(
            "Board/Fleet projection unavailable",
            theme.err_style(),
        )),
        ProjectionStatus::Ready | ProjectionStatus::Stale => {}
    }
    if let Some(wave) = &snapshot.active_wave {
        lines.push(Line::styled(
            format!("wave {} · {}", wave.id, wave.status),
            theme.panel_style(),
        ));
    } else {
        lines.push(Line::styled("wave —", theme.muted_style()));
    }
    let desired = snapshot
        .capacity
        .desired
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into());
    let draining = snapshot
        .capacity
        .draining
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into());
    lines.push(Line::styled(
        format!(
            "workers {}/{} · want {} · drain {}",
            snapshot.capacity.active, snapshot.capacity.configured, desired, draining
        ),
        theme.panel_style(),
    ));
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} decision", snapshot.open_decisions()),
            if snapshot.open_decisions() > 0 {
                theme.warn_style()
            } else {
                theme.muted_style()
            },
        ),
        Span::styled(
            format!(" · {} blocked", snapshot.blocked_items),
            theme.muted_style(),
        ),
        Span::styled(
            format!(" · {} red", snapshot.failures_total),
            if snapshot.failures_total > 0 {
                theme.err_style()
            } else {
                theme.muted_style()
            },
        ),
    ]));
    lines.push(Line::raw(""));
    for worker in snapshot.workers.iter().take(5) {
        let status = worker.status.as_str();
        let style = match status {
            "working" | "running" | "active" => theme.ok_style(),
            "failed" | "parked" => theme.err_style(),
            "cancelled" | "draining" => theme.warn_style(),
            _ => theme.muted_style(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", status_badge(status)), style),
            Span::styled(
                truncate(&worker.id, area.width.saturating_sub(6) as usize),
                theme.panel_style(),
            ),
        ]));
        if let Some(board_ref) = worker.board_ref.as_deref() {
            lines.push(Line::styled(
                format!(
                    "  {}",
                    truncate(board_ref, area.width.saturating_sub(4) as usize)
                ),
                theme.muted_style(),
            ));
        }
    }
    if snapshot.workers_total > snapshot.workers.len().min(5) {
        lines.push(Line::styled(
            format!(
                "… {} more",
                snapshot.workers_total - snapshot.workers.len().min(5)
            ),
            theme.muted_style(),
        ));
    }
    if let Some(error) = ops.refresh_error.as_deref() {
        lines.push(Line::styled(
            format!(
                "stale · {}",
                truncate(error, area.width.saturating_sub(4) as usize)
            ),
            theme.err_style(),
        ));
    }
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.muted_style())
        .title(Line::styled(" Fleet · F2 ", theme.accent_style()));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.panel_style()),
        area,
    );
}

fn status_badge(status: &str) -> &'static str {
    match status {
        "working" | "running" | "active" => "●",
        "failed" | "parked" => "×",
        "completed" | "done" => "✓",
        "draining" => "↓",
        _ => "○",
    }
}

pub(crate) fn render_overlay(frame: &mut Frame, state: &ChatState) {
    let Some(ops) = state.operations.as_ref().filter(|ops| ops.open) else {
        return;
    };
    let area = frame.area();
    let margin_x = if area.width >= 100 { 3 } else { 1 };
    let margin_y = if area.height >= 28 { 2 } else { 0 };
    let inner = Rect {
        x: area.x + margin_x,
        y: area.y + margin_y,
        width: area.width.saturating_sub(margin_x * 2),
        height: area.height.saturating_sub(margin_y * 2),
    };
    frame.render_widget(Clear, inner);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(state.theme.accent_style())
        .style(state.theme.panel_style())
        .title(Line::from(vec![
            Span::styled(" Board + Fleet ", state.theme.accent_style()),
            Span::styled(
                "· F2/Esc close · Tab views · r refresh ",
                state.theme.muted_style(),
            ),
        ]));
    let body = block.inner(inner);
    frame.render_widget(block, inner);
    if body.height == 0 || body.width == 0 {
        return;
    }
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(body);
    let tabs = OperationsTab::ALL
        .iter()
        .flat_map(|tab| {
            let active = *tab == ops.tab;
            [
                Span::styled(
                    format!(" {} ", tab.label()),
                    if active {
                        state
                            .theme
                            .accent_style()
                            .bg(state.theme.sel_bg)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        state.theme.muted_style()
                    },
                ),
                Span::raw(" "),
            ]
        })
        .collect::<Vec<_>>();
    let status = format!(
        "{} · main {} · r{} · session {}",
        ops.snapshot.connection_label(),
        ops.snapshot.main_status,
        ops.snapshot.revision,
        ops.snapshot.main_session.as_deref().unwrap_or("—")
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(tabs),
            Line::styled(status, state.theme.muted_style()),
        ]),
        chunks[0],
    );
    let lines = overlay_lines(
        ops,
        &state.theme,
        chunks[1].width as usize,
        chunks[1].height as usize,
    );
    frame.render_widget(
        Paragraph::new(lines).style(state.theme.panel_style()),
        chunks[1],
    );
}

fn overlay_lines(
    ops: &OperationsState,
    theme: &Theme,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if matches!(
        ops.projection_status,
        ProjectionStatus::Loading | ProjectionStatus::Error
    ) {
        let mut lines = vec![Line::styled(
            if ops.projection_status == ProjectionStatus::Loading {
                " loading Board and Fleet projection…"
            } else {
                " Board and Fleet projection unavailable"
            },
            if ops.projection_status == ProjectionStatus::Loading {
                theme.muted_style()
            } else {
                theme.err_style()
            },
        )];
        if let Some(error) = ops.refresh_error.as_deref() {
            lines.push(Line::styled(
                truncate(&format!(" {error}"), width),
                theme.err_style(),
            ));
        }
        return lines;
    }
    // C-621: the Board expands in place, inside the selected box, so it never leaves the list for
    // a full-pane detail view the way the other tabs do.
    if ops.detail_open && !ops.board_body_expanded() {
        return detail_lines(ops, theme, width);
    }
    let selected_style = Style::default()
        .fg(theme.accent)
        .bg(theme.sel_bg)
        .add_modifier(Modifier::BOLD);
    // Rows the ack/stale trailer claims below the list, so the Board pane pages within what is left.
    let trailer =
        usize::from(ops.last_ack.is_some()) * 2 + usize::from(ops.refresh_error.is_some());
    let mut rows = match ops.tab {
        OperationsTab::Overview => overview_lines(ops, theme, width),
        OperationsTab::Board => BoardPane::new(&ops.snapshot.items).window_lines(
            &ops.board_focus(),
            &ops.snapshot,
            theme,
            width,
            height.saturating_sub(trailer),
        ),
        OperationsTab::Workers => ops
            .snapshot
            .workers
            .iter()
            .enumerate()
            .map(|(index, worker)| {
                let text = format!(
                    " {} {} {:<11} {:<24} {}",
                    if index == ops.selected { "▸" } else { " " },
                    status_badge(&worker.status),
                    worker.status,
                    worker.id,
                    worker.board_ref.as_deref().unwrap_or("unassigned")
                );
                Line::styled(
                    truncate(&text, width),
                    if index == ops.selected {
                        selected_style
                    } else {
                        theme.panel_style()
                    },
                )
            })
            .collect(),
        OperationsTab::Decisions => ops
            .snapshot
            .decisions
            .iter()
            .enumerate()
            .map(|(index, decision)| {
                let text = format!(
                    " {} {:<10} {:<24} {}",
                    if index == ops.selected { "▸" } else { " " },
                    decision.status,
                    decision.decision_ref,
                    decision.question
                );
                Line::styled(
                    truncate(&text, width),
                    if index == ops.selected {
                        selected_style
                    } else {
                        theme.panel_style()
                    },
                )
            })
            .collect(),
        OperationsTab::Stats => ops
            .snapshot
            .metrics
            .iter()
            .enumerate()
            .map(|(index, metric)| {
                let value = match (metric.done, metric.remaining, metric.total, metric.percent) {
                    (Some(done), Some(remaining), Some(total), Some(percent)) => {
                        format!("{done} done · {remaining} left · {total} total · {percent:.1}%")
                    }
                    _ => "unavailable".into(),
                };
                Line::styled(
                    truncate(
                        &format!(
                            " {} {:<22} {}",
                            if index == ops.selected { "▸" } else { " " },
                            metric.name,
                            value
                        ),
                        width,
                    ),
                    if index == ops.selected {
                        selected_style
                    } else {
                        theme.panel_style()
                    },
                )
            })
            .collect(),
    };
    if rows.is_empty() {
        rows.push(Line::styled(" no records", theme.muted_style()));
    }
    if let Some(ack) = ops.last_ack.as_ref() {
        rows.push(Line::raw(""));
        rows.push(Line::styled(
            format!("ack {} · {} · r{}", ack.id, ack.level, ack.revision),
            if ack.level == "failed" {
                theme.err_style()
            } else {
                theme.ok_style()
            },
        ));
    }
    if let Some(error) = ops.refresh_error.as_deref() {
        rows.push(Line::styled(
            format!("stale snapshot · {error}"),
            theme.err_style(),
        ));
    }
    rows
}

fn overview_lines(ops: &OperationsState, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let snapshot = &ops.snapshot;
    let desired = snapshot
        .capacity
        .desired
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".into());
    let draining = snapshot
        .capacity
        .draining
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".into());
    let mut lines = vec![
        Line::styled(
            truncate(&format!("root  {}", snapshot.root), width),
            theme.panel_style(),
        ),
        Line::styled(
            format!(
                "capacity  configured {} · desired {} · active {} · draining {} · registered {}",
                snapshot.capacity.configured,
                desired,
                snapshot.capacity.active,
                draining,
                snapshot.capacity.registered
            ),
            theme.panel_style(),
        ),
        Line::styled(
            format!(
                "attention  {} decision(s) · {} blocked · {} failure(s)",
                snapshot.open_decisions(),
                snapshot.blocked_items,
                snapshot.failures_total
            ),
            if snapshot.attention_required {
                theme.warn_style()
            } else {
                theme.muted_style()
            },
        ),
    ];
    if let Some(wave) = snapshot.active_wave.as_ref() {
        lines.push(Line::styled(
            format!(
                "wave  {} · {} · {} item(s)",
                wave.id,
                wave.status,
                wave.items.len()
            ),
            theme.panel_style(),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "recent coordinator intake",
        theme.accent_style(),
    ));
    if snapshot.intake.is_empty() {
        lines.push(Line::styled("  none recorded", theme.muted_style()));
    } else {
        for intake in snapshot.intake.iter().rev().take(8) {
            lines.push(Line::styled(
                truncate(
                    &format!(
                        "  {} · {} · {}",
                        intake.id, intake.acknowledgement, intake.summary
                    ),
                    width,
                ),
                if intake.acknowledgement == "failed" {
                    theme.err_style()
                } else {
                    theme.panel_style()
                },
            ));
        }
        if snapshot.intake_total > snapshot.intake.len() {
            lines.push(Line::styled(
                format!(
                    "  … {} earlier record(s)",
                    snapshot.intake_total - snapshot.intake.len()
                ),
                theme.muted_style(),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("goals", theme.accent_style()));
    if snapshot.goals.is_empty() {
        lines.push(Line::styled(
            "  unavailable / none recorded",
            theme.muted_style(),
        ));
    } else {
        for goal in snapshot.goals.iter().take(8) {
            lines.push(Line::styled(
                truncate(
                    &format!("  {}/{}  {}", goal.scope, goal.name, goal.statement),
                    width,
                ),
                theme.panel_style(),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("planning documents", theme.accent_style()));
    if snapshot.documents.is_empty() {
        lines.push(Line::styled(
            "  unavailable / none found",
            theme.muted_style(),
        ));
    } else {
        for document in snapshot.documents.iter().take(8) {
            lines.push(Line::styled(
                truncate(
                    &format!("  {} · {} · {}", document.kind, document.id, document.path),
                    width,
                ),
                theme.panel_style(),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("failures", theme.accent_style()));
    if snapshot.failures.is_empty() {
        lines.push(Line::styled("  none", theme.muted_style()));
    } else {
        for failure in snapshot.failures.iter().take(8) {
            lines.push(Line::styled(
                truncate(
                    &format!("  × {} · {}", failure.subject, failure.message),
                    width,
                ),
                theme.err_style(),
            ));
            if let Some(candidate) = failure.candidate.as_deref() {
                lines.push(Line::styled(
                    truncate(&format!("    candidate {candidate}"), width),
                    theme.muted_style(),
                ));
            }
            if let Some(evidence) = failure.evidence.as_deref() {
                lines.push(Line::styled(
                    truncate(&format!("    evidence {evidence}"), width),
                    theme.muted_style(),
                ));
            }
        }
    }
    lines
}

fn detail_lines(ops: &OperationsState, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(" Esc back to list", theme.muted_style()),
        Line::raw(""),
    ];
    match ops.tab {
        OperationsTab::Overview => return overview_lines(ops, theme, width),
        // C-621: the Board's detail is its expanded box, painted in place by [`BoardPane`].
        OperationsTab::Board => return lines,
        OperationsTab::Workers => {
            let Some(worker) = ops.snapshot.workers.get(ops.selected) else {
                return lines;
            };
            lines.extend([
                Line::styled(worker.id.clone(), theme.accent_style()),
                Line::styled(
                    format!("role/status  {} / {}", worker.role, worker.status),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!(
                        "assignment  {}",
                        worker.board_ref.as_deref().unwrap_or("unassigned")
                    ),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!(
                        "wave/session  {} / {}",
                        worker.wave.as_deref().unwrap_or("—"),
                        worker.session.as_deref().unwrap_or("—")
                    ),
                    theme.panel_style(),
                ),
                Line::styled(
                    truncate(
                        &format!(
                            "worktree  {}",
                            worker.worktree.as_deref().unwrap_or("unavailable")
                        ),
                        width,
                    ),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!(
                        "handoff  {}",
                        worker.handoff.as_deref().unwrap_or("unavailable")
                    ),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!(
                        "review  {}",
                        worker.review.as_deref().unwrap_or("unavailable")
                    ),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!(
                        "rework  {}",
                        worker
                            .rework_round
                            .map_or("unavailable".into(), |v| v.to_string())
                    ),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!(
                        "activity  {}",
                        worker.last_activity.as_deref().unwrap_or("unavailable")
                    ),
                    theme.panel_style(),
                ),
            ]);
            if !worker.activity.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled("bounded activity log", theme.accent_style()));
                for event in &worker.activity {
                    lines.push(Line::styled(
                        truncate(&format!("  {event}"), width),
                        theme.panel_style(),
                    ));
                }
            }
            if let Some(error) = worker.error.as_deref() {
                lines.push(Line::styled(format!("error  {error}"), theme.err_style()));
            }
        }
        OperationsTab::Decisions => {
            let Some(decision) = ops.snapshot.decisions.get(ops.selected) else {
                return lines;
            };
            lines.extend([
                Line::styled(decision.decision_ref.clone(), theme.accent_style()),
                Line::styled(decision.title.clone(), theme.panel_style()),
                Line::styled(
                    format!("question  {}", decision.question),
                    theme.panel_style(),
                ),
                Line::styled(
                    format!("blocks  {}", display_list(&decision.blocks)),
                    theme.panel_style(),
                ),
            ]);
            for (index, option) in decision.options.iter().enumerate() {
                lines.push(Line::styled(
                    truncate(
                        &format!(
                            " {} {}{}  {}",
                            if index == ops.decision_option {
                                "▸"
                            } else {
                                " "
                            },
                            option.id,
                            if option.recommended {
                                " · recommended"
                            } else {
                                ""
                            },
                            option.tradeoff.as_deref().unwrap_or("")
                        ),
                        width,
                    ),
                    if index == ops.decision_option {
                        theme.accent_style().bg(theme.sel_bg)
                    } else {
                        theme.panel_style()
                    },
                ));
            }
            if decision.status == "open" && !decision.options.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    if ops.confirm_decision {
                        "confirm decision: Enter applies · Esc cancels"
                    } else {
                        "Left/Right choose · Enter review confirmation"
                    },
                    if ops.confirm_decision {
                        theme.warn_style()
                    } else {
                        theme.muted_style()
                    },
                ));
            }
        }
        OperationsTab::Stats => {
            let Some(metric) = ops.snapshot.metrics.get(ops.selected) else {
                return lines;
            };
            lines.push(Line::styled(metric.name.clone(), theme.accent_style()));
            lines.push(Line::styled(
                format!("schema  {}", metric.schema),
                theme.panel_style(),
            ));
            match (metric.done, metric.remaining, metric.total, metric.percent) {
                (Some(done), Some(remaining), Some(total), Some(percent)) => {
                    lines.push(Line::styled(
                        format!(
                            "{done} done · {remaining} remaining · {total} total · {percent:.2}%"
                        ),
                        theme.panel_style(),
                    ));
                }
                _ => lines.push(Line::styled("unavailable", theme.muted_style())),
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("stats source  {}", ops.snapshot.metrics_schema),
                theme.muted_style(),
            ));
            if !ops.snapshot.status_counts.is_empty() {
                lines.push(Line::styled("status histogram", theme.accent_style()));
                lines.push(Line::styled(
                    truncate(
                        &ops.snapshot
                            .status_counts
                            .iter()
                            .map(|(status, count)| format!("{status}={count}"))
                            .collect::<Vec<_>>()
                            .join(" · "),
                        width,
                    ),
                    theme.panel_style(),
                ));
            }
            for (name, value) in &ops.snapshot.stats_facts {
                lines.push(Line::styled(
                    truncate(&format!("{name}  {value}"), width),
                    theme.panel_style(),
                ));
            }
            if !ops.snapshot.history.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled("recent history", theme.accent_style()));
                for (date, added, removed, completed) in ops.snapshot.history.iter().rev().take(8) {
                    lines.push(Line::styled(
                        format!("{date}  +{added} / -{removed} / ✓{completed}"),
                        theme.panel_style(),
                    ));
                }
            }
        }
    }
    lines
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "—".into()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn snapshot() -> FleetBoardSnapshot {
        FleetBoardSnapshot {
            schema: "flux.tui-board-fleet/v1".into(),
            root: "/workspace".into(),
            running: true,
            main_status: "running".into(),
            main_session: Some("s_7".into()),
            revision: 9,
            goals_revision: 2,
            goals: Vec::new(),
            active_wave: None,
            capacity: FleetCapacityView {
                configured: 5,
                desired: None,
                active: 1,
                draining: None,
                registered: 3,
            },
            workers: Vec::new(),
            workers_total: 0,
            items: Vec::new(),
            items_total: 0,
            decisions: Vec::new(),
            decisions_total: 0,
            documents: Vec::new(),
            documents_total: 0,
            metrics_schema: "flux.board-stats/v1".into(),
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

    #[test]
    fn only_a_running_main_accepts_conversation_input() {
        let mut value = snapshot();
        assert!(value.can_send());
        assert_eq!(value.connection_label(), "connected");
        value.running = false;
        assert!(!value.can_send());
        assert_eq!(value.connection_label(), "stopped");
        value.running = true;
        value.main_status = "failed".into();
        assert!(!value.can_send());
        assert_eq!(value.connection_label(), "failed");
    }

    #[test]
    fn tabs_wrap_and_reset_detail_selection() {
        let mut state = OperationsState::new(snapshot());
        assert_eq!(OperationsTab::Overview.cycle(-1), OperationsTab::Stats);
        state.detail_open = true;
        state.select_tab(OperationsTab::Workers);
        assert_eq!(state.selected, 0);
        assert!(!state.detail_open);
    }

    #[test]
    fn projection_failures_distinguish_unavailable_startup_from_stale_data() {
        let initial = snapshot();
        let mut state = OperationsState::loading(initial.clone());
        assert_eq!(state.projection_status, ProjectionStatus::Loading);

        state.refresh_failed("initial read failed".into());
        assert_eq!(state.projection_status, ProjectionStatus::Error);
        assert_eq!(state.snapshot, initial);

        let mut current = snapshot();
        current.revision = 10;
        state.refresh(current.clone());
        assert_eq!(state.projection_status, ProjectionStatus::Ready);
        assert_eq!(state.snapshot, current);
        assert_eq!(state.refresh_error, None);

        state.refresh_failed("later read failed".into());
        assert_eq!(state.projection_status, ProjectionStatus::Stale);
        assert_eq!(state.snapshot.revision, 10);
    }

    #[test]
    fn a_decision_requires_a_review_enter_before_it_can_mutate() {
        let mut state = OperationsState::new(attention_snapshot());
        state.select_tab(OperationsTab::Decisions);
        state.detail_open = true;

        assert_eq!(state.confirm_selected_decision(), None);
        assert!(state.confirm_decision);
        assert_eq!(
            state.confirm_selected_decision(),
            Some(("workspace/D-7".into(), "strict".into()))
        );
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn attention_snapshot() -> FleetBoardSnapshot {
        let mut value = snapshot();
        value.active_wave = Some(FleetWaveView {
            id: "wave-7".into(),
            status: "working".into(),
            items: vec!["flux/C-556".into()],
        });
        value.workers = vec![FleetWorkerView {
            id: "worker-C-556".into(),
            role: "writer".into(),
            status: "working".into(),
            board_ref: Some("flux/C-556".into()),
            wave: Some("wave-7".into()),
            session: Some("s_worker".into()),
            worktree: Some("/worktrees/C-556".into()),
            handoff: None,
            review: None,
            rework_round: Some(0),
            last_activity: Some("agent.turn.delivered".into()),
            activity: vec!["tool_result · read · ok".into()],
            error: None,
        }];
        value.workers_total = 1;
        value.items = vec![BoardItemView {
            board_ref: "flux/C-556".into(),
            title: "Fleet main shell".into(),
            status: "in-progress".into(),
            priority: Some(1),
            dependencies: Vec::new(),
            design: Some("board-fleet-tui".into()),
            epic: Some("board-fleet-tui".into()),
            body: None,
        }];
        value.items_total = 1;
        value.decisions = vec![BoardDecisionView {
            decision_ref: "workspace/D-7".into(),
            board: "workspace".into(),
            id: "D-7".into(),
            title: "Choose review posture".into(),
            question: "Which review posture should the Fleet use?".into(),
            status: "open".into(),
            blocks: vec!["flux/C-557".into()],
            options: vec![DecisionOptionView {
                id: "strict".into(),
                tradeoff: Some("more evidence".into()),
                recommended: true,
            }],
            outcome: None,
            rationale: None,
            path: Some("docs/decisions/D-7.md".into()),
        }];
        value.decisions_total = 1;
        value.blocked_items = 1;
        value.failures = vec![FleetFailureView {
            subject: "wave-7/flux".into(),
            kind: "red gate".into(),
            message: "exit 101".into(),
            candidate: Some("abc123".into()),
            evidence: Some("cargo test --workspace".into()),
        }];
        value.failures_total = 1;
        value.attention_required = true;
        value
    }

    #[test]
    fn wide_chat_keeps_the_coordinator_primary_and_shows_the_attention_rail() {
        let mut state = ChatState::new("mock".into());
        state.operations = Some(OperationsState::new(attention_snapshot()));
        state.push_user("coordinate the active wave");
        let mut terminal = Terminal::new(TestBackend::new(140, 24)).unwrap();

        terminal.draw(|frame| crate::render(frame, &state)).unwrap();
        let content = screen(&terminal);

        assert!(content.contains("coordinate the active wave"), "{content}");
        assert!(content.contains("Fleet · F2"), "{content}");
        assert!(content.contains("worker-C-556"), "{content}");
        assert!(content.contains("1 decision · 1 blocked"), "{content}");
        assert!(content.contains("1 red"), "{content}");
    }

    #[test]
    fn empty_snapshot_renders_a_safe_operations_overview() {
        let mut state = ChatState::new("mock".into());
        state.operations = Some(OperationsState::new(snapshot()));
        state.operations.as_mut().unwrap().open = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();

        terminal.draw(|frame| crate::render(frame, &state)).unwrap();
        let content = screen(&terminal);

        assert!(content.contains("Board + Fleet"), "{content}");
        assert!(content.contains("none recorded"), "{content}");
        assert!(content.contains("failures"), "{content}");
        assert!(content.contains("none"), "{content}");
    }

    #[test]
    fn narrow_chat_uses_the_header_and_full_screen_operations_fallback() {
        let mut state = ChatState::new("mock".into());
        state.operations = Some(OperationsState::new(attention_snapshot()));
        let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();

        terminal.draw(|frame| crate::render(frame, &state)).unwrap();
        let compact = screen(&terminal);
        assert!(
            compact.contains("Fleet main · connected · r9 · F2"),
            "{compact}"
        );
        assert!(!compact.contains("Fleet · F2"), "{compact}");

        state.operations.as_mut().unwrap().open = true;
        state.operations.as_mut().unwrap().tab = OperationsTab::Decisions;
        terminal.draw(|frame| crate::render(frame, &state)).unwrap();
        let overlay = screen(&terminal);
        assert!(overlay.contains("Board + Fleet"), "{overlay}");
        assert!(overlay.contains("workspace/D-7"), "{overlay}");
        assert!(overlay.contains("Which review posture"), "{overlay}");
        assert!(overlay.contains("open"), "{overlay}");
    }

    /// A fixture board whose input order deliberately contradicts the board order the pane owes:
    /// status groups first, then ascending priority, then natural id order inside a group.
    fn grouped_board_snapshot() -> FleetBoardSnapshot {
        fn item(board_ref: &str, status: &str, priority: Option<i64>) -> BoardItemView {
            BoardItemView {
                board_ref: board_ref.into(),
                title: format!("collapsed box for {board_ref}"),
                status: status.into(),
                priority,
                dependencies: Vec::new(),
                design: None,
                epic: None,
                body: None,
            }
        }
        let mut value = snapshot();
        value.items = vec![
            item("flux/C-31", "done", Some(3)),
            item("flux/C-100", "ready", Some(20)),
            item("flux/C-7", "ready", None),
            item("flux/C-43", "in-progress", Some(6)),
            item("flux/C-620", "blocked", Some(1)),
            item("flux/C-99", "ready", Some(20)),
            item("flux/C-8", "backlog", Some(2)),
            item("flux/C-42", "in-progress", Some(5)),
            item("flux/C-11", "ready", Some(10)),
        ];
        value.items_total = value.items.len();
        // Fleet state, not the Board, says what is in flight: only C-42 is a member of the wave.
        value.active_wave = Some(FleetWaveView {
            id: "wave-7".into(),
            status: "working".into(),
            items: vec!["flux/C-42".into()],
        });
        value
    }

    fn board_pane_screen(snapshot: FleetBoardSnapshot, width: u16, height: u16) -> String {
        let mut state = ChatState::new("mock".into());
        let mut ops = OperationsState::new(snapshot);
        ops.open = true;
        ops.tab = OperationsTab::Board;
        state.operations = Some(ops);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::render(frame, &state)).unwrap();
        screen(&terminal)
    }

    #[test]
    fn board_pane_groups_by_status_and_orders_by_priority() {
        let content = board_pane_screen(grouped_board_snapshot(), 96, 44);
        let at = |needle: &str| {
            content
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing from\n{content}"))
        };

        // Collapsed items are bordered boxes, not table rows.
        assert!(content.contains('┌'), "{content}");
        assert!(content.contains('└'), "{content}");

        // Group order first: in-progress, ready, blocked, backlog, done. Blocked C-620 carries the
        // lowest priority on the board and still sorts behind every ready item.
        // Inside the ready group the order is `board next` order: priority ascending, then natural
        // id order, so C-99 precedes C-100 and the unprioritized C-7 sorts last.
        for (earlier, later) in [
            ("flux/C-42", "flux/C-43"),
            ("flux/C-43", "flux/C-11"),
            ("flux/C-11", "flux/C-99"),
            ("flux/C-99", "flux/C-100"),
            ("flux/C-100", "flux/C-7"),
            ("flux/C-7", "flux/C-620"),
            ("flux/C-620", "flux/C-8"),
            ("flux/C-8", "flux/C-31"),
        ] {
            assert!(
                at(earlier) < at(later),
                "{earlier} before {later}\n{content}"
            );
        }
    }

    #[test]
    fn board_pane_collapsed_box_shows_id_title_status_priority_and_fleet_wave_marker() {
        let content = board_pane_screen(grouped_board_snapshot(), 96, 44);

        assert!(content.contains("flux/C-11 · ready · p10"), "{content}");
        assert!(content.contains("collapsed box for flux/C-11"), "{content}");
        // No priority renders as an explicit gap rather than a guess.
        assert!(content.contains("flux/C-7 · ready · p—"), "{content}");

        // The in-flight marker is Fleet-sourced: C-42 is a wave member, C-43 shares its
        // `in-progress` Board status and is not marked.
        assert_eq!(content.matches("◆ wave-7").count(), 1, "{content}");
        let marker = content
            .lines()
            .find(|line| line.contains("◆ wave-7"))
            .unwrap_or_else(|| panic!("no marked box in\n{content}"));
        assert!(marker.contains("flux/C-42"), "{content}");
    }

    fn pane_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn board_pane_pages_a_board_far_larger_than_the_viewport() {
        let mut value = snapshot();
        value.items = (0..1_200)
            .map(|index| BoardItemView {
                board_ref: format!("flux/C-{index}"),
                title: format!("item {index:04}"),
                status: if index % 2 == 0 { "ready" } else { "backlog" }.into(),
                priority: Some(index),
                dependencies: Vec::new(),
                design: None,
                epic: None,
                body: None,
            })
            .collect();
        value.items_total = value.items.len();
        let state = ChatState::new("mock".into());
        let pane = BoardPane::new(&value.items);
        assert_eq!(pane.order.len(), 1_200);
        // Board order, not snapshot order: the lowest-priority ready item leads.
        assert_eq!(
            pane.item(0).map(|item| item.board_ref.as_str()),
            Some("flux/C-0")
        );
        assert_eq!(
            pane.groups
                .iter()
                .map(|group| (group.status.as_str(), group.len))
                .collect::<Vec<_>>(),
            vec![("ready", 600), ("backlog", 600)]
        );

        // A 24-row viewport builds 24 rows, not 1200 boxes.
        let bottom = Cell::new(0);
        let head = pane.window_lines(&focus(0, false, 0, &bottom), &value, &state.theme, 80, 24);
        assert_eq!(head.len(), 24, "the viewport bounds the rows built");
        let head_text = pane_text(&head);
        assert!(head_text.contains("item 0000"), "{head_text}");
        assert!(!head_text.contains("item 0100"), "{head_text}");

        // Selecting deep into the board pages to it rather than materializing everything above.
        let deep = pane.window_lines(&focus(400, false, 0, &bottom), &value, &state.theme, 80, 24);
        assert_eq!(deep.len(), 24);
        let deep_text = pane_text(&deep);
        assert!(deep_text.contains("item 0800"), "{deep_text}");
        assert!(!deep_text.contains("item 0000"), "{deep_text}");
    }

    /// A board carrying work in every status *and* an open decision, so the single mutation the
    /// overlay owns is genuinely reachable — from the Decisions tab. The Board pane must not be
    /// able to spend it.
    fn read_only_snapshot() -> FleetBoardSnapshot {
        let mut value = grouped_board_snapshot();
        let decisions = attention_snapshot().decisions;
        value.decisions_total = decisions.len();
        value.decisions = decisions;
        value
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Every key the Board pane can receive: its own bindings, the overlay bindings reachable while
    /// it is focused, and every printable character a future binding might claim.
    fn board_pane_keys() -> Vec<KeyEvent> {
        let mut keys: Vec<KeyEvent> = [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::F(2),
            KeyCode::F(5),
        ]
        .into_iter()
        .map(key)
        .collect();
        keys.push(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        keys.extend((0x20u8..=0x7e).map(|byte| key(KeyCode::Char(char::from(byte)))));
        keys
    }

    /// C-623: the Board pane is read-only. Every interaction it accepts, driven through the same
    /// routing the event loop uses, leaves planning state untouched: no command that reaches
    /// [`FleetBoardSource`]'s one mutation, and an unchanged snapshot — same revision, same status
    /// and priority on every item. Status transitions stay the Board CLI's, which validates them.
    #[test]
    fn no_board_pane_interaction_mutates_planning_state() {
        let before = read_only_snapshot();

        for detail_open in [false, true] {
            for event in board_pane_keys() {
                let mut state = OperationsState::new(before.clone());
                state.open = true;
                state.select_tab(OperationsTab::Board);
                state.selected = 1;
                state.detail_open = detail_open;
                // Pre-arm the confirmation the Decisions tab spends on its second Enter: the Board
                // pane must not be a second, unvalidated way to spend it.
                state.confirm_decision = true;

                let command = state.handle_key(event);

                assert!(
                    !matches!(command, OperationsCommand::Decide { .. }),
                    "board pane key {event:?} requested a planning mutation"
                );
                assert_eq!(
                    state.snapshot, before,
                    "board pane key {event:?} changed planning state"
                );
            }
        }

        // The same keys as one uninterrupted session, plus the pane's mouse interactions. A
        // tab-switching key leaves the pane, so return to it and keep going.
        let mut state = OperationsState::new(before.clone());
        state.open = true;
        state.select_tab(OperationsTab::Board);
        for event in board_pane_keys() {
            let command = state.handle_key(event);
            assert!(
                !matches!(command, OperationsCommand::Decide { .. }),
                "board pane key {event:?} requested a planning mutation"
            );
            state.open = true;
            if state.tab != OperationsTab::Board {
                state.select_tab(OperationsTab::Board);
            }
            state.handle_mouse(MouseEventKind::ScrollDown);
            state.handle_mouse(MouseEventKind::ScrollUp);
            state.handle_mouse(MouseEventKind::Moved);
            assert_eq!(
                state.snapshot, before,
                "board pane interaction {event:?} changed planning state"
            );
        }
        assert_eq!(state.snapshot.revision, before.revision);
    }

    /// The negative control for the invariant above: the same driver does observe the overlay's one
    /// mutation where it legitimately lives — the Decisions tab, after an explicit second Enter.
    #[test]
    fn the_decisions_tab_still_reaches_the_one_confirmed_mutation() {
        let mut state = OperationsState::new(read_only_snapshot());
        state.open = true;
        state.select_tab(OperationsTab::Decisions);

        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            OperationsCommand::None
        );
        assert!(state.detail_open);
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            OperationsCommand::None
        );
        assert!(state.confirm_decision);
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            OperationsCommand::Decide {
                decision_ref: "workspace/D-7".into(),
                outcome: "strict".into(),
            }
        );
    }

    /// The pane focus for one rendering, with the scroll bottom the renderer writes back.
    fn focus(
        selected: usize,
        expanded: bool,
        body_scroll: usize,
        bottom: &Cell<usize>,
    ) -> BoardFocus<'_> {
        BoardFocus {
            selected,
            expanded,
            body_scroll,
            bottom,
        }
    }

    /// One item carrying a story body, which is the only thing the expanded box renders.
    fn item_with_body(board_ref: &str, body: &str) -> BoardItemView {
        BoardItemView {
            board_ref: board_ref.into(),
            title: format!("story {board_ref}"),
            status: "ready".into(),
            priority: Some(21),
            dependencies: vec!["flux/C-620".into()],
            design: Some("docs/designs/tui-board-surface.md".into()),
            epic: Some("tui-board-surface".into()),
            body: Some(body.into()),
        }
    }

    /// Display columns one painted row occupies.
    fn row_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    /// A box row's text, borders and gutters removed.
    fn box_text(line: &Line<'_>) -> Option<String> {
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let inner = text.strip_prefix('│')?.strip_suffix('│')?;
        Some(inner.trim().to_string())
    }

    /// Failing first (C-621): a body whose wrapped rows land *exactly* on the box's usable width is
    /// where the transcript's `area_width`-versus-usable-columns off-by-one dropped one character
    /// per row. Sixteen six-column words wrap to two rows of exactly 55 columns inside a 59-column
    /// pane, so any confusion between the pane width, the box interior and the text column loses a
    /// character at the row boundary or overflows the border.
    #[test]
    fn an_expanded_body_wraps_on_the_box_boundary_without_losing_a_character() {
        const WIDTH: usize = 59;
        let words: Vec<String> = (0..16).map(|index| format!("w{index:05}")).collect();
        let body = words.join(" ");
        let mut value = snapshot();
        value.items = vec![item_with_body("flux/C-621", &body)];
        value.items_total = 1;
        let state = ChatState::new("mock".into());
        let pane = BoardPane::new(&value.items);
        let bottom = Cell::new(0);
        let lines = pane.window_lines(&focus(0, true, 0, &bottom), &value, &state.theme, WIDTH, 24);

        // No horizontal overflow and no short row: every row the box paints is exactly the pane.
        for line in &lines {
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            if text.starts_with(['┌', '│', '└']) {
                assert_eq!(
                    row_width(line),
                    WIDTH,
                    "box row is the pane width: {text:?}"
                );
            }
        }

        // Continuity across the row boundary: the words survive in order, none clipped or dropped.
        let flowed = lines
            .iter()
            .filter_map(box_text)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flowed.contains(&body),
            "the wrapped body is continuous across rows:\n{flowed}"
        );
    }

    #[test]
    fn an_expanded_body_renders_headings_lists_checkboxes_and_code_as_markdown() {
        let body = "## Goal\n\nplain prose sentence\n\n## Acceptance\n\n- [x] first is done\n- [ ] second is open\n\n```text\nfenced code\n```\n";
        let mut value = snapshot();
        value.items = vec![item_with_body("flux/C-621", body)];
        value.items_total = 1;
        let state = ChatState::new("mock".into());
        let pane = BoardPane::new(&value.items);
        let bottom = Cell::new(0);
        let lines = pane.window_lines(&focus(0, true, 0, &bottom), &value, &state.theme, 72, 30);
        let text = pane_text(&lines);

        // Acceptance checkboxes keep the state that decides whether the story is complete.
        assert!(text.contains("[x] first is done"), "{text}");
        assert!(text.contains("[ ] second is open"), "{text}");
        // Fenced code keeps its gutter, so it is visually distinct from prose.
        assert!(text.contains("▎"), "{text}");

        let row = |needle: &str| {
            lines
                .iter()
                .find(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                        .contains(needle)
                })
                .unwrap_or_else(|| panic!("{needle} missing from\n{text}"))
        };
        // The style of a row's first story glyph — past the border and its gutter.
        let content_style = |line: &Line<'_>| {
            line.spans
                .iter()
                .find(|span| !span.content.contains('│') && !span.content.trim().is_empty())
                .map(|span| span.style)
                .unwrap()
        };
        // A heading is styled, prose is not: the body reads as structure rather than as a wall.
        let heading = content_style(row("Goal"));
        let prose = content_style(row("plain prose sentence"));
        assert_ne!(heading, prose, "heading and prose share a style\n{text}");
        assert!(
            heading.add_modifier.contains(Modifier::BOLD),
            "heading is emphasized\n{text}"
        );
    }

    #[test]
    fn a_long_expanded_body_scrolls_inside_the_box_instead_of_pushing_the_board_off_screen() {
        let body = (0..80)
            .map(|index| format!("line {index:03} of the story body"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut value = snapshot();
        value.items = vec![
            item_with_body("flux/C-621", &body),
            item_with_body("flux/C-622", "the next story"),
        ];
        value.items_total = 2;
        let state = ChatState::new("mock".into());
        let pane = BoardPane::new(&value.items);
        let bottom = Cell::new(0);
        let top = pane.window_lines(&focus(0, true, 0, &bottom), &value, &state.theme, 72, 26);
        let top_text = pane_text(&top);

        // The box is bounded: the item below the expanded one still has its rows.
        assert!(top.len() <= 26, "the viewport bounds the rows built");
        assert!(top_text.contains("flux/C-622"), "{top_text}");
        assert!(top_text.contains("line 000"), "{top_text}");
        assert!(!top_text.contains("line 079"), "{top_text}");

        // The renderer reports a real bottom, and scrolling moves the window inside the box only.
        assert!(
            bottom.get() > 0,
            "a longer body than the box records a bottom"
        );
        let scrolled = pane.window_lines(
            &focus(0, true, bottom.get(), &bottom),
            &value,
            &state.theme,
            72,
            26,
        );
        let scrolled_text = pane_text(&scrolled);
        assert_eq!(scrolled.len(), top.len(), "the box keeps its height");
        assert!(scrolled_text.contains("line 079"), "{scrolled_text}");
        assert!(!scrolled_text.contains("line 000"), "{scrolled_text}");
        assert!(scrolled_text.contains("flux/C-622"), "{scrolled_text}");
    }

    #[test]
    fn scrolling_an_expanded_body_stops_at_the_bottom_the_last_frame_measured() {
        let mut state = OperationsState::new(attention_snapshot());
        state.select_tab(OperationsTab::Board);
        state.detail_open = true;
        assert!(state.board_body_expanded());

        state.body_scroll_max.set(3);
        state.scroll_board_body(10);
        assert_eq!(
            state.body_scroll, 3,
            "scrolling stops at the measured bottom"
        );
        state.scroll_board_body(-10);
        assert_eq!(state.body_scroll, 0, "and at the top");

        // Moving the selection collapses the box and rewinds its body.
        state.body_scroll = 2;
        state.move_selection(1);
        assert!(!state.detail_open);
        assert_eq!(state.body_scroll, 0);
    }
}
