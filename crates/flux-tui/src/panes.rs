//! Host-pushed pane slots (C-221): the surface-side store and renderer for the L2 `SurfaceSink`
//! vocabulary C-220 fixed (`PaneCommand` / `PaneSpec` / `PaneData`).
//!
//! Two properties hold this module together, and both are structural rather than conventional:
//!
//! - **Panes are bounded by surface constants, never by their payload.** Count, body rows, column
//!   width and the bottom strip's height are all decided here from the terminal's geometry. A pane
//!   whose content exceeds a bound is truncated by the surface; it can never push the transcript
//!   below [`MIN_TRANSCRIPT_WIDTH`], and below [`PANE_MIN_TRANSCRIPT_WIDTH`] nothing is drawn at
//!   all — the same narrow-width posture `EMPTY_CARD_MIN_WIDTH` (C-157) and C-102's header/footer
//!   bars established.
//! - **Panes are outside the transcript.** They never enter [`ChatState::transcript_viewport`], so
//!   they take no layout-cache entry (C-149), no focus index (C-111), no scroll bookkeeping
//!   (C-106) and are not found by transcript search (C-108) — exactly the rule
//!   `render_empty_state_card`'s doc comment states for the orientation card, for the same reason:
//!   anything drawn into the transcript area that is not a transcript row must stay entirely out
//!   of that machinery.
//!
//! Every colour and modifier comes from the [`Theme`]; [`PaneSpec`](flux_runtime::PaneSpec)
//! carries no style-bearing field and must not grow one. Trust chrome — the agent-region mark,
//! the border style, and the rule that a payload can never draw either — lives in
//! [`crate::trust`] (C-222), which is also why this store holds [`AgentPane`]s rather than raw
//! specs: an unsanitized payload cannot be rendered because it cannot be stored. This module
//! guarantees the ordering that invariant rests on: panes draw before the approval sheet, always.
//!
//! # Two owners, one store (C-224)
//!
//! A pane is either agent-authored or host-owned ([`Pane`]), and the difference is visible: only the
//! agent's carries the ` ◆ agent ` mark. The host's own pane — the sub-agent fleet view — is
//! ordinary harness chrome, and marking it would make the mark a lie in the one place a user checks
//! it. In the other direction the discriminator is structural, which is what makes it worth
//! anything: an agent pane cannot suppress its mark, and a payload cannot draw one, because
//! [`trust::AGENT_MARK`] is itself a reserved glyph.
//!
//! **The fleet pane is why `PaneData` is not the whole vocabulary, and deliberately still is not
//! widened.** C-224 asked whether `kind: rows` can express a fleet honestly, and it cannot: the
//! operational question is "is that worker working or hung?", and answering it needs a *live running
//! indicator* and a *tint on the stalled row*. A payload can have neither by construction — every
//! `rows` cell renders in one `panel_style()` (C-220 gives the model no style field, on purpose),
//! and the spinner is Braille, which C-222 reserves precisely so a payload cannot fake a running
//! indicator. The answer is **not** a new [`PaneData`] variant: the fleet's content is
//! surface-derived from A-79's typed stream, never model-authored, so it does not belong in the
//! model-facing payload type at all. [`Pane::Fleet`] carries *no data* and reads
//! [`ChatState::fleet_rows`] at render time instead. `PaneData` therefore reaches C-223 exactly as
//! C-220 fixed it, and the host keeps the one thing the model must not have: a region it can style.

use super::*;

use flux_runtime::{PaneCommand, PaneData, PaneLifetime, PaneSlot};
use trust::AgentPane;

/// The transcript's own width floor. Narrower than this the conversation stops being readable, and
/// the surface would be trading the thing the user came for against a side panel.
const MIN_TRANSCRIPT_WIDTH: u16 = 44;

/// The narrowest a side pane can be and still carry a border plus a useful line of content.
const MIN_PANE_WIDTH: u16 = 24;

/// The widest a single side column grows to, however much room is left. Panes are an aside; past
/// this the extra columns are worth more to the transcript.
const MAX_PANE_WIDTH: u16 = 36;

/// Total share of the terminal both side columns together may take, in percent.
const MAX_SIDE_WIDTH_PCT: u16 = 50;

/// Below this total width **no pane is drawn at all** — the transcript keeps the full row and the
/// frame is identical to a session with no panes. It is the sum of the two floors above rather
/// than a taste call: any narrower and a side column could only exist by starving the transcript.
pub(crate) const PANE_MIN_TRANSCRIPT_WIDTH: u16 = MIN_TRANSCRIPT_WIDTH + MIN_PANE_WIDTH;

/// Below this total height panes are suppressed too: the vertical layout already owes rows to the
/// header, steering strip, composer and footer, and a shorter terminal has none to spare.
const PANE_MIN_HEIGHT: u16 = 14;

/// How many panes the surface holds at once. Once full, an `open` for a *new* id is refused rather
/// than evicting an existing pane — a runaway caller must not be able to push a pane the user is
/// reading off the screen. `update`/`close` for an id already open keep working.
const MAX_PANES: usize = 4;

/// Body rows rendered inside one pane before the surface truncates and marks the elision.
const MAX_PANE_ROWS: u16 = 12;

/// Rows the bottom strip may take off the vertical layout, further capped at a third of the frame.
const MAX_BOTTOM_ROWS: u16 = 8;

/// Rows of a pane that are chrome rather than content: the top and bottom border.
const PANE_CHROME_ROWS: u16 = 2;

/// The reserved id of the host's sub-agent fleet pane (C-224).
///
/// Reserved rather than merely taken: [`PaneStore::apply`] refuses **every** command naming it, so
/// the model can neither replace, repaint nor close the pane, and cannot shadow it by claiming the
/// id before any child is live.
pub(crate) const FLEET_PANE_ID: &str = "host:fleet";

/// The fleet pane's heading. Surface-authored, like the rest of its chrome.
const FLEET_PANE_TITLE: &str = "sub-agents";

/// Where the fleet pane sits. A side column, because the fleet is an aside to the conversation and
/// the bottom strip's rows are the ones the composer competes for.
const FLEET_PANE_SLOT: PaneSlot = PaneSlot::Right;

/// Workers listed in the fleet pane before the rest are summarized in one line. Two rows each, so
/// this is [`MAX_PANE_ROWS`] halved — the surface's cap, applied in the unit the body is built in.
const MAX_FLEET_WORKERS: usize = (MAX_PANE_ROWS / 2) as usize;

/// One pane in the store, together with **who owns it** — the distinction the model cannot cross.
///
/// C-221 had one kind of pane and could hold a bare [`AgentPane`]. C-224 adds a region the *host*
/// owns, and the two must not be confusable: the agent's carries C-222's ` ◆ agent ` mark and the
/// host's must not, or the mark stops being evidence of anything.
#[derive(Debug)]
pub(crate) enum Pane {
    /// Agent-authored: a sanitized payload, drawn inside the C-222 trust chrome.
    Agent(AgentPane),
    /// The host's sub-agent fleet pane (C-224).
    ///
    /// **This variant deliberately carries no data at all.** Its body is read from
    /// [`ChatState::fleet_rows`] at render time, which is what lets the surface draw a live running
    /// indicator and a stalled/failed tint — see the module docs on why `rows` cannot. Having no
    /// field is also the whole trust argument for it: there is nothing here for a model-supplied
    /// character to inhabit, which is stronger than sanitizing a field would be.
    Fleet,
}

impl Pane {
    /// The id this pane is addressed by.
    fn id(&self) -> &str {
        match self {
            Pane::Agent(pane) => &pane.spec().id,
            Pane::Fleet => FLEET_PANE_ID,
        }
    }

    /// The slot this pane occupies. The fleet pane's is surface-chosen, not proposed.
    fn slot(&self) -> PaneSlot {
        match self {
            Pane::Agent(pane) => pane.spec().slot,
            Pane::Fleet => FLEET_PANE_SLOT,
        }
    }

    fn is_fleet(&self) -> bool {
        matches!(self, Pane::Fleet)
    }
}

/// Pane commands the agent has pushed but the event loop has not drawn yet. Deliberately generous:
/// the bound exists so a runaway caller cannot grow the surface's memory without limit, not to
/// shape behaviour — a turn that legitimately opens and repaints panes stays far below it.
const MAX_PENDING_COMMANDS: usize = 1024;

/// The TUI's [`SurfaceSink`](flux_runtime::SurfaceSink) — the L6 half of C-220's contract, and the
/// thing whose *existence at assembly time* surfaces the `pane.*` vocabulary at all (C-305).
///
/// It is a queue rather than a direct call into [`ChatState`] because of when it has to exist:
/// `run_tui` assembles the agent **before** the terminal (and its `ChatState`) are created, and the
/// surfacing decision must be taken once, at assembly. So the channel is minted first, handed to the
/// agent, and connected to the state that drains it afterwards. That is the same shape A-94's
/// `SteeringQueue` uses in the other direction, for the same reason.
///
/// [`SurfaceSink::emit`](flux_runtime::SurfaceSink::emit) is called from inside a running tool and
/// must not block, so this only ever locks a mutex to push. Nothing here renders, and nothing here
/// sanitizes: sanitizing is [`PaneStore::apply`]'s job through [`AgentPane`], so a payload still
/// cannot be drawn without having been filtered.
#[derive(Debug, Default)]
pub struct PaneQueue {
    pending: std::sync::Mutex<Pending>,
}

/// The queued commands and the tally of the ones the queue refused, behind **one** lock.
///
/// They live together rather than as a `VecDeque` plus an atomic because `emit` decides to drop and
/// records the drop in the same critical section: a drain can therefore never take the commands and
/// miss a refusal that happened alongside them, nor report a refusal twice.
#[derive(Debug, Default)]
struct Pending {
    commands: std::collections::VecDeque<PaneCommand>,
    /// Commands refused since the last [`PaneQueue::drain`], reset by it.
    dropped: usize,
}

/// What one [`PaneQueue::drain`] took: the commands to apply, and how many the channel refused
/// while they were waiting.
pub(crate) struct Drained {
    pub(crate) commands: Vec<PaneCommand>,
    pub(crate) dropped: usize,
}

impl PaneQueue {
    /// A fresh channel, ready to be handed to an agent assembly. `Arc` because the sink half lives
    /// inside the runtime's `ToolContext` while the draining half stays with the surface.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Take everything queued so far — and everything refused so far — leaving the channel empty
    /// and open.
    pub(crate) fn drain(&self) -> Drained {
        let mut pending = self.pending.lock().unwrap();
        Drained {
            commands: pending.commands.drain(..).collect(),
            dropped: std::mem::take(&mut pending.dropped),
        }
    }
}

impl flux_runtime::SurfaceSink for PaneQueue {
    fn emit(&self, command: PaneCommand) {
        let mut pending = self.pending.lock().unwrap();
        if pending.commands.len() >= MAX_PENDING_COMMANDS {
            // Drop the *newest*: it keeps the panes the user is already looking at rather than the
            // flood behind them, and evicting the oldest would throw away an `Open` while leaving
            // `Update`s that `PaneStore` then discards anyway — strictly worse.
            //
            // **C-324: the drop is counted, not silent.** The `pane.*` op that pushed this command
            // has already been told it succeeded, and there is no way to un-tell it: `SurfaceSink`
            // is send-only by construction (L2 cannot know a surface exists, let alone wait on
            // one), so the model's view of this call is fixed before we get here. The surface is
            // still the party that knows, so the surface is the party that reports — to the
            // **operator**, through the transcript, in `ChatState::apply_pending_panes`. That is
            // the same posture `flux-tools`' surface module already states for this seam's sibling
            // failure ("a clear op failure, never a silent success"), honoured on the one channel
            // this failure actually has.
            //
            // Telling the *model* was considered and rejected as disproportionate: it would mean
            // `SurfaceSink::emit` reporting acceptance back, which is a breaking change to a
            // published L2 trait and every implementor of it, to close a hole that needs 1024
            // pending commands inside one 62 ms frame. The operator can see the pane is missing and
            // now learns why; the model can already re-check reality with `pane.list`.
            pending.dropped = pending.dropped.saturating_add(1);
            return;
        }
        pending.commands.push_back(command);
    }
}

/// One open pane as the surface reports it to a `pane.list` query (C-224).
///
/// `host_owned` is the field that keeps the model from duplicating the fleet pane: it can see the
/// pane is up and that the commands it has are not what put it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneListing {
    pub id: String,
    pub title: String,
    /// Whether the surface opened this pane itself. A host-owned pane ignores every `pane.*`
    /// command addressed at it.
    pub host_owned: bool,
}

/// The id a command addresses, whichever command it is.
fn addressed_id(command: &PaneCommand) -> &str {
    match command {
        PaneCommand::Open(spec) => &spec.id,
        PaneCommand::Update { id, .. } | PaneCommand::Close { id } => id,
    }
}

/// Host-pushed panes, addressed by id and rendered in the order they were opened.
///
/// A `Vec` rather than a map on purpose: ids address a pane, but *render order must be
/// deterministic*, and a hash map's iteration order is not. Insertion order also gives the
/// [`MAX_PANES`] cap an obvious meaning.
///
/// The element type is [`AgentPane`], not [`PaneSpec`](flux_runtime::PaneSpec): the store is the
/// one door into the surface, and [`AgentPane`]'s only constructor sanitizes (C-222). A payload
/// therefore cannot be rendered without having been filtered, because it cannot be *held* without
/// having been filtered.
#[derive(Debug, Default)]
pub(crate) struct PaneStore {
    open: Vec<Pane>,
}

impl PaneStore {
    /// Apply one command. Bounds are enforced here so nothing downstream has to trust the caller:
    /// an `open` past [`MAX_PANES`] for an unknown id is dropped, and an `open` for an id already
    /// present replaces it in place (keeping its position, so the layout does not jump).
    ///
    /// An `update` whose payload is a different [`PaneKind`](flux_runtime::PaneKind) than the open
    /// pane re-derives the kind from the data instead of rejecting it — the two can then never
    /// disagree, which is the same invariant `PaneSpec::new` enforces at the contract end.
    ///
    /// **Nothing that arrives here can touch a host-owned pane** (C-224). The check is on the
    /// command's id and happens before anything else, so `open`, `update` and `close` are refused
    /// alike — including an `open` sent before the fleet pane exists, which would otherwise let the
    /// model squat the reserved id and have the host adopt its payload.
    pub(crate) fn apply(&mut self, command: PaneCommand) {
        if addressed_id(&command) == FLEET_PANE_ID {
            return;
        }
        match command {
            PaneCommand::Open(spec) => {
                // `project` is rejected at the reporter (C-220) and has no store here; a spec that
                // reached the surface anyway is dropped rather than silently treated as `session`.
                if spec.lifetime == PaneLifetime::Project {
                    return;
                }
                match self.open.iter().position(|p| p.id() == spec.id) {
                    Some(at) => self.open[at] = Pane::Agent(AgentPane::sanitized(spec)),
                    // The cap counts *agent* panes only: the host's own pane is not admitted
                    // through this path and must not consume the model's budget either, or opening
                    // MAX_PANES panes would be a way to suppress the surface's fleet view.
                    None if self.agent_len() < MAX_PANES => {
                        self.open.push(Pane::Agent(AgentPane::sanitized(spec)))
                    }
                    None => {}
                }
            }
            PaneCommand::Update { id, data } => {
                if let Some(Pane::Agent(pane)) =
                    self.open.iter_mut().find(|p| p.id() == id && !p.is_fleet())
                {
                    pane.update(data);
                }
            }
            PaneCommand::Close { id } => self.open.retain(|p| p.is_fleet() || p.id() != id),
        }
    }

    /// Drop the [`PaneLifetime::Turn`] panes. Called at every turn-termination path the surface
    /// owns, so a turn-scoped pane cannot outlive the turn that opened it.
    ///
    /// The fleet pane is not turn-scoped and survives: its lifetime is the fleet's own, so a wave
    /// that finished as the turn ended stays readable for its retention window rather than
    /// vanishing at the exact moment the user wants to see how it ended.
    pub(crate) fn end_turn(&mut self) {
        self.open.retain(|p| match p {
            Pane::Fleet => true,
            Pane::Agent(pane) => pane.spec().lifetime != PaneLifetime::Turn,
        });
    }

    /// Drop every pane. Used when the surface projects a different session (`/resume`): panes are
    /// session-scoped, and carrying them across would attribute one session's panes to another.
    pub(crate) fn clear(&mut self) {
        self.open.clear();
    }

    /// Raise the host's fleet pane, if it is not already up (C-224).
    pub(crate) fn raise_fleet(&mut self) {
        if !self.has_fleet() {
            self.open.push(Pane::Fleet);
        }
    }

    /// Retire the host's fleet pane. Driven by the projection emptying, not by the turn ending.
    pub(crate) fn retire_fleet(&mut self) {
        self.open.retain(|p| !p.is_fleet());
    }

    pub(crate) fn has_fleet(&self) -> bool {
        self.open.iter().any(Pane::is_fleet)
    }

    /// Agent-authored panes only — what [`MAX_PANES`] bounds.
    fn agent_len(&self) -> usize {
        self.open.iter().filter(|p| !p.is_fleet()).count()
    }

    /// Every open pane, labelled with who owns it — the surface-side answer to `pane.list` (C-223).
    pub(crate) fn listing(&self) -> Vec<PaneListing> {
        self.open
            .iter()
            .map(|pane| PaneListing {
                id: pane.id().to_string(),
                title: match pane {
                    Pane::Agent(agent) => agent.spec().title.clone(),
                    Pane::Fleet => FLEET_PANE_TITLE.to_string(),
                },
                host_owned: pane.is_fleet(),
            })
            .collect()
    }

    /// The panes asking for `slot`, oldest first.
    fn in_slot(&self, slot: PaneSlot) -> Vec<&Pane> {
        self.open.iter().filter(|p| p.slot() == slot).collect()
    }

    /// Whether anything is open at all — the cheap check that keeps a pane-less session on exactly
    /// the code path it had before C-221.
    pub(crate) fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.open.len()
    }

    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<&str> {
        self.open.iter().map(Pane::id).collect()
    }
}

/// Whether this **frame** is large enough to carry panes at all. Suppression is total and decided
/// once from the whole frame — not per slot — so a narrow or short terminal drops every slot
/// together and renders byte-identically to the same session with no panes.
fn frame_fits_panes(state: &ChatState, frame: Rect) -> bool {
    !state.panes.is_empty()
        && frame.width >= PANE_MIN_TRANSCRIPT_WIDTH
        && frame.height >= PANE_MIN_HEIGHT
}

/// Rects the side slots resolved to for one frame, plus what is left for the transcript.
pub(crate) struct PaneAreas {
    pub(crate) left: Option<Rect>,
    pub(crate) right: Option<Rect>,
    /// The transcript's rect after the side columns took their share — the **full** input rect
    /// whenever no side pane is drawn, which is what keeps a pane-less session unchanged.
    pub(crate) transcript: Rect,
}

/// Rows the `bottom` slot needs off the vertical layout, or `0` when it takes none.
///
/// Computed from the whole frame *before* the vertical split, because the bottom strip is one
/// extra vertical constraint rather than a carve-out of the transcript row. `0` means the caller
/// omits the constraint entirely instead of adding a zero-length one — the layout a pane-less
/// session solves is then the exact list it solved before C-221.
pub(crate) fn bottom_rows(state: &ChatState, frame: Rect) -> u16 {
    if !frame_fits_panes(state, frame) {
        return 0;
    }
    let panes = state.panes.in_slot(PaneSlot::Bottom);
    if panes.is_empty() {
        return 0;
    }
    // Bottom panes sit side by side, so the strip is as tall as the tallest of them rather than
    // their sum — a second bottom pane costs width, never more of the transcript's height.
    let want = panes
        .iter()
        .map(|p| PANE_CHROME_ROWS + body_rows(state, p).min(MAX_PANE_ROWS))
        .max()
        .unwrap_or(0);
    want.min(MAX_BOTTOM_ROWS).min(frame.height / 3)
}

/// Resolve `row` (the transcript row of `frame`) into `left` / transcript / `right`.
///
/// The width budget is bounded three ways at once — a percentage of the frame, a per-column
/// maximum, and the transcript's own floor — and the tightest wins. When two columns are asked for
/// and the budget cannot give both at least [`MIN_PANE_WIDTH`], neither is drawn: half a pane is
/// worse than no pane, and silently dropping one slot would make placement look arbitrary.
pub(crate) fn split_transcript(state: &ChatState, frame: Rect, row: Rect) -> PaneAreas {
    let none = PaneAreas {
        left: None,
        right: None,
        transcript: row,
    };
    if !frame_fits_panes(state, frame) {
        return none;
    }
    let area = row;
    let wants_left = !state.panes.in_slot(PaneSlot::Left).is_empty();
    let wants_right = !state.panes.in_slot(PaneSlot::Right).is_empty();
    let columns = u16::from(wants_left) + u16::from(wants_right);
    if columns == 0 {
        return none;
    }

    let budget = (area.width * MAX_SIDE_WIDTH_PCT / 100)
        .min(area.width.saturating_sub(MIN_TRANSCRIPT_WIDTH))
        .min(MAX_PANE_WIDTH * columns);
    let each = budget / columns;
    if each < MIN_PANE_WIDTH {
        return none;
    }

    let left_w = if wants_left { each } else { 0 };
    let right_w = if wants_right { each } else { 0 };
    let chunks = Layout::horizontal([
        Constraint::Length(left_w),
        Constraint::Min(MIN_TRANSCRIPT_WIDTH),
        Constraint::Length(right_w),
    ])
    .split(area);
    PaneAreas {
        left: wants_left.then_some(chunks[0]),
        right: wants_right.then_some(chunks[2]),
        transcript: chunks[1],
    }
}

/// Draw every resolved slot. Called from `render` **before** the approval sheet so a pane can
/// never paint over it (C-222 owns proving that invariant; this is the ordering it rests on).
pub(crate) fn render_panes(
    frame: &mut Frame,
    state: &ChatState,
    areas: &PaneAreas,
    bottom: Option<Rect>,
) {
    if let Some(area) = areas.left {
        render_column(frame, state, &state.panes.in_slot(PaneSlot::Left), area);
    }
    if let Some(area) = areas.right {
        render_column(frame, state, &state.panes.in_slot(PaneSlot::Right), area);
    }
    if let Some(area) = bottom {
        render_row(frame, state, &state.panes.in_slot(PaneSlot::Bottom), area);
    }
}

/// Widest an `overlay`-slot pane's centred panel grows to — the same 76 the queue, session-picker
/// and help overlays pass to [`rendering::render_overlay_panel`].
const OVERLAY_PANE_WIDTH: u16 = 76;

/// Draw the `overlay` slot through the shared [`rendering::render_overlay_panel`] chrome (C-152)
/// rather than a second overlay shape of our own.
///
/// Only the newest overlay pane is shown: that helper draws one centred panel, so a second would
/// simply hide behind the first. The caller places this **before** the surface's own overlays and
/// the approval sheet, so surface chrome always paints over an agent pane.
///
/// This is the slot where the agent mark earns its keep: the shared chrome is the *same*
/// borderless centred panel the help, `/usage`, queue and session-picker overlays use, so
/// [`trust::agent_overlay_header`] is the only thing separating an agent pane from a host one.
pub(crate) fn render_overlay_pane(frame: &mut Frame, state: &ChatState) {
    if !frame_fits_panes(state, frame.area()) {
        return;
    }
    let panes = state.panes.in_slot(PaneSlot::Overlay);
    // Always an agent pane: the host's fleet pane is pinned to `FLEET_PANE_SLOT` and never asks for
    // the overlay slot, so this destructure is what keeps the header below unambiguously the mark.
    let Some(pane @ Pane::Agent(agent)) = panes.last().copied() else {
        return;
    };
    let t = &state.theme;
    let width = frame.area().width.min(OVERLAY_PANE_WIDTH);
    let mut body = body_lines(state, pane, t, width);
    let total = body.len();
    let shown = total.min(MAX_PANE_ROWS as usize);
    body.truncate(shown);
    rendering::render_overlay_panel(
        frame,
        t,
        trust::agent_overlay_header(t, &agent.spec().title, width),
        body,
        (total > shown).then_some((shown, total)),
        width,
    );
}

/// Body rows a pane's payload wants, before the surface's cap applies.
fn body_rows(state: &ChatState, pane: &Pane) -> u16 {
    let agent = match pane {
        Pane::Agent(agent) => agent,
        // Built in the same unit the body is, so the two cannot disagree about the fleet's height.
        Pane::Fleet => return fleet_rows_wanted(state) as u16,
    };
    let count = match &agent.spec().data {
        PaneData::Rows { header, rows } => usize::from(!header.is_empty()) + rows.len(),
        PaneData::Kv { pairs } => pairs.len(),
        PaneData::Log { lines } => lines.len(),
        PaneData::Progress { .. } => 2,
        PaneData::Tree { roots } => tree_rows(roots, 0),
        // Markdown wraps to the pane width, which is not known here; the cap is what bounds it, so
        // asking for the maximum is both cheap and correct.
        PaneData::Markdown { .. } => MAX_PANE_ROWS as usize,
    };
    count.min(u16::MAX as usize) as u16
}

/// Rows a [`PaneData::Tree`] forest occupies at the surface's depth cap.
fn tree_rows(nodes: &[flux_runtime::PaneNode], depth: usize) -> usize {
    if depth >= plan::MAX_TREE_DEPTH {
        return 0;
    }
    nodes
        .iter()
        .map(|n| 1 + tree_rows(&n.children, depth + 1))
        .sum()
}

/// Stack a slot's panes vertically inside its column, giving each an equal share and dropping the
/// ones that no longer clear the minimum pane height.
fn render_column(frame: &mut Frame, state: &ChatState, panes: &[&Pane], area: Rect) {
    let fit = (area.height / (PANE_CHROME_ROWS + 1)).min(panes.len() as u16);
    if fit == 0 {
        return;
    }
    let chunks =
        Layout::vertical(vec![Constraint::Ratio(1, u32::from(fit)); fit as usize]).split(area);
    for (pane, chunk) in panes.iter().zip(chunks.iter()) {
        render_pane(frame, state, pane, *chunk);
    }
}

/// Lay a slot's panes side by side across a strip, giving each an equal share.
fn render_row(frame: &mut Frame, state: &ChatState, panes: &[&Pane], area: Rect) {
    let fit = (area.width / MIN_PANE_WIDTH).min(panes.len() as u16);
    if fit == 0 {
        return;
    }
    let chunks =
        Layout::horizontal(vec![Constraint::Ratio(1, u32::from(fit)); fit as usize]).split(area);
    for (pane, chunk) in panes.iter().zip(chunks.iter()) {
        render_pane(frame, state, pane, *chunk);
    }
}

/// One pane: its chrome, and a body truncated to the rect with an explicit elision marker when the
/// cap bites.
///
/// **Which chrome depends on who owns it, and that is load-bearing.** An agent pane gets
/// [`trust::agent_block`] — themed border plus C-222's ` ◆ agent ` mark. The host's fleet pane gets
/// an ordinary bordered block with a plain title and **no mark**, because the mark is the user's
/// evidence that a region was authored by the model: putting it on harness chrome would be a false
/// claim and would teach the user that ` ◆ agent ` means nothing in particular. The discriminator
/// stays structural in the other direction too — an agent pane cannot drop its mark, and a payload
/// cannot draw one, since [`trust::AGENT_MARK`] is itself a reserved glyph.
fn render_pane(frame: &mut Frame, state: &ChatState, pane: &Pane, area: Rect) {
    if area.width < MIN_PANE_WIDTH || area.height < PANE_CHROME_ROWS + 1 {
        return;
    }
    let t = &state.theme;
    let block = match pane {
        Pane::Agent(agent) => trust::agent_block(t, &agent.spec().title, area.width),
        Pane::Fleet => Block::bordered()
            .border_style(t.muted_style())
            .title(Span::styled(
                format!(" {FLEET_PANE_TITLE} "),
                t.muted_style(),
            )),
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let budget = inner.height.min(MAX_PANE_ROWS);
    let mut lines = body_lines(state, pane, t, inner.width);
    let total = lines.len();
    if total > budget as usize {
        let keep = budget.saturating_sub(1) as usize;
        lines.truncate(keep);
        lines.push(Line::styled(
            format!(" … {} more", total - keep),
            t.muted_style(),
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The pane's payload as styled rows, per [`PaneKind`](flux_runtime::PaneKind).
///
/// Every kind goes through machinery the TUI already owns — `markdown` through the transcript's
/// own [`crate::markdown`] (`flux-markdown`), `tree` through [`plan::render_nodes`] — so no widget
/// dependency is added under the standing `ratatui` 0.29 hold.
///
/// Every `Style` below is read off the [`Theme`]; the payload contributes characters and counts
/// and nothing else. `markdown` gets a second pass through [`trust::sanitize_lines`] because it is
/// the one kind whose *renderer* turns payload text into glyphs (a thematic break, a table) —
/// everywhere else the glyphs are the surface's own.
fn body_lines(state: &ChatState, pane: &Pane, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let cols = width as usize;
    let agent = match pane {
        Pane::Agent(agent) => agent,
        Pane::Fleet => return fleet_lines(state, theme, cols),
    };
    match &agent.spec().data {
        PaneData::Rows { header, rows } => {
            let widths = column_widths(header, rows, cols);
            let mut out = Vec::new();
            if !header.is_empty() {
                out.push(Line::styled(
                    truncate(&pad_row(header, &widths), cols),
                    theme.accent_style().add_modifier(Modifier::BOLD),
                ));
            }
            out.extend(rows.iter().map(|row| {
                Line::styled(truncate(&pad_row(row, &widths), cols), theme.panel_style())
            }));
            out
        }
        PaneData::Kv { pairs } => {
            let key_w = pairs
                .iter()
                .map(|(k, _)| UnicodeWidthStr::width(k.as_str()))
                .max()
                .unwrap_or(0)
                .min(cols / 2);
            pairs
                .iter()
                .map(|(key, value)| {
                    Line::from(vec![
                        Span::styled(
                            format!("{:<key_w$}  ", truncate(key, key_w)),
                            theme.muted_style(),
                        ),
                        Span::styled(
                            truncate(value, cols.saturating_sub(key_w + 2)),
                            theme.panel_style(),
                        ),
                    ])
                })
                .collect()
        }
        PaneData::Log { lines } => lines
            .iter()
            .map(|line| Line::styled(truncate(line, cols), theme.panel_style()))
            .collect(),
        PaneData::Progress { label, done, total } => {
            // The same `█`/`░` bar the `/usage` overlay draws, sized to the pane rather than to the
            // payload — `done`/`total` choose a ratio, never a width.
            let bar_w = cols.saturating_sub(2).clamp(4, 24);
            let ratio = if *total == 0 {
                0.0
            } else {
                (*done as f64 / *total as f64).clamp(0.0, 1.0)
            };
            let filled = (ratio * bar_w as f64).round() as usize;
            vec![
                Line::styled(truncate(label, cols), theme.panel_style()),
                Line::from(vec![
                    Span::styled(
                        format!("{}{}", "█".repeat(filled), "░".repeat(bar_w - filled)),
                        theme.accent_style(),
                    ),
                    Span::styled(format!(" {done}/{total}"), theme.muted_style()),
                ]),
            ]
        }
        PaneData::Tree { roots } => plan::render_nodes(roots, theme, cols),
        PaneData::Markdown { text } => {
            trust::sanitize_lines(crate::markdown::render(text, width).lines)
        }
    }
}

/// Workers the fleet pane lists, and whether a summary line is owed for the rest.
fn fleet_shown(state: &ChatState) -> (&[crate::fleet::WorkerRow], usize) {
    let rows = state.fleet_rows.as_slice();
    let shown = rows.len().min(MAX_FLEET_WORKERS);
    (&rows[..shown], rows.len() - shown)
}

/// Rows the fleet pane's body wants: two per listed worker, plus a line for any remainder.
fn fleet_rows_wanted(state: &ChatState) -> usize {
    let (shown, rest) = fleet_shown(state);
    shown.len() * 2 + usize::from(rest > 0 || state.fleet.dropped() > 0)
}

/// The fleet pane's body: two rows per live worker, drawn from typed [`crate::fleet::WorkerRow`]s.
///
/// ```text
/// ⠹ implementor         1m2s
///   running · read        3s
/// ```
///
/// The first row identifies the worker and how long it has been going; the second says what it is
/// doing and how long since it last said anything — the hung-versus-working signal, which is the
/// operational question a fleet surface exists to answer.
///
/// **Everything here is surface-drawn from typed values**, which is the whole reason this is a host
/// pane rather than a `rows` payload:
///
/// - the leading glyph is a live [`crate::SPINNER`] frame for a worker that is working, so "this
///   worker is running" is shown rather than asserted in text. A payload could never carry it:
///   `SPINNER` is Braille and Braille is reserved (C-222) precisely so a payload cannot fake it.
/// - a stalled worker is `warn`-styled and a failed one `err`-styled. A `rows` payload renders every
///   cell in one `panel_style()`, so it can state a worker is stuck but cannot *show* it.
/// - the status word comes from [`crate::fleet::WorkerStatus::label`] — the closed set A-79's design
///   requires a customer surface to derive fixed or allowlisted labels from. Only the operation
///   *name* is interpolated, already sanitized and length-bounded by `crate::fleet`. The child's
///   tool input and observation data are never read, here or anywhere on this path.
fn fleet_lines(state: &ChatState, theme: &Theme, cols: usize) -> Vec<Line<'static>> {
    use crate::fleet::WorkerStatus;

    let (shown, rest) = fleet_shown(state);
    let mut out = Vec::with_capacity(shown.len() * 2 + 1);
    for row in shown {
        // The mark, the tint and the frame are all chosen here from the typed status — never from
        // anything a worker or the model wrote.
        let (mark, style) = match (&row.status, row.stalled) {
            // A quiet worker is the one an operator has to notice, so it loses its animation: a
            // frozen mark plus a warn tint, rather than a spinner that suggests progress.
            (_, true) => ("◌", theme.warn_style()),
            (WorkerStatus::Finished { is_error: true }, _) => ("●", theme.err_style()),
            (WorkerStatus::Finished { is_error: false }, _) => ("●", theme.ok_style()),
            (WorkerStatus::Idle, _) => ("◌", theme.muted_style()),
            // Starting / Planning / Running: working, so it animates off its own age.
            (_, false) => (
                SPINNER[(row.elapsed.as_millis() / 80) as usize % SPINNER.len()],
                theme.accent_style(),
            ),
        };

        // Line 1: mark, role, and total age flushed right.
        let age = fmt_elapsed(row.elapsed);
        let role_cols = cols.saturating_sub(2 + age.len() + 1);
        out.push(Line::from(vec![
            Span::styled(format!("{mark} "), style),
            Span::styled(
                format!("{:<role_cols$}", truncate(&row.role, role_cols)),
                if row.stalled {
                    theme.warn_style()
                } else {
                    theme.panel_style()
                },
            ),
            Span::styled(format!(" {age}"), theme.muted_style()),
        ]));

        // Line 2: the closed-set status label, the op name when there is one, and how long the
        // worker has been quiet. `stalled` is carried as a **word** and not only as the warn tint,
        // for the reason C-149/C-154 give: under `Theme::MONO` every colour role is `Color::Reset`,
        // so a tint-only signal is no signal at all — and this is the signal an operator is here for.
        let mut activity = String::new();
        // `stalled` leads, so that when the line is truncated the op name is what gets dropped and
        // not the signal: `stalled · running…` is useful, `running · read sta…` is the defect.
        if row.stalled {
            activity.push_str("stalled · ");
        }
        activity.push_str(row.status.label());
        if let Some(op) = row.status.op() {
            activity.push_str(" · ");
            activity.push_str(op);
        }
        // Labelled, because line 1 already right-flushes an age and two bare numbers in a column
        // cannot be told apart.
        let idle = format!("quiet {}", fmt_elapsed(row.idle));
        let activity_cols = cols.saturating_sub(2 + idle.len() + 1);
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<activity_cols$}", truncate(&activity, activity_cols)),
                style,
            ),
            Span::styled(format!(" {idle}"), theme.muted_style()),
        ]));
    }

    // Refusals and overflow are reported rather than hidden — a fleet surface that silently shows
    // a subset is worse than one that says it is showing a subset.
    let untracked = state.fleet.dropped();
    if rest > 0 || untracked > 0 {
        let mut note = String::new();
        if rest > 0 {
            note.push_str(&format!(" … {rest} more"));
        }
        if untracked > 0 {
            note.push_str(&format!(" · {untracked} untracked"));
        }
        out.push(Line::styled(truncate(&note, cols), theme.muted_style()));
    }
    out
}

/// Per-column display widths for a `rows` payload, bounded so a single wide cell cannot decide the
/// whole layout.
fn column_widths(header: &[String], rows: &[Vec<String>], cols: usize) -> Vec<usize> {
    let count = rows
        .iter()
        .map(Vec::len)
        .chain([header.len()])
        .max()
        .unwrap_or(0);
    let cap = (cols / count.max(1)).max(4);
    (0..count)
        .map(|i| {
            std::iter::once(header)
                .chain(rows.iter().map(Vec::as_slice))
                .filter_map(|row| row.get(i))
                .map(|cell| UnicodeWidthStr::width(cell.as_str()))
                .max()
                .unwrap_or(0)
                .min(cap)
        })
        .collect()
}

/// One `rows` row padded to `widths`, two spaces between columns.
fn pad_row(row: &[String], widths: &[usize]) -> String {
    row.iter()
        .zip(widths.iter())
        .map(|(cell, w)| format!("{:<w$}", truncate(cell, *w)))
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_runtime::{PaneNode, PaneSpec};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use trust::AGENT_MARK;

    /// Frame the C-222 screen assertions draw into: wide and tall enough that a side column, a
    /// bottom strip, an overlay panel and the approval sheet all fit at once.
    const TRUST_W: u16 = 100;
    const TRUST_H: u16 = 30;

    /// Every slot a pane can ask for — the trust invariant has to hold in all four, including
    /// `overlay`, which shares its chrome with the surface's own overlays.
    const ALL_SLOTS: [PaneSlot; 4] = [
        PaneSlot::Left,
        PaneSlot::Right,
        PaneSlot::Bottom,
        PaneSlot::Overlay,
    ];

    /// **C-305.** The agent's channel is the model's one door into the pane store, and it must
    /// deliver through the *same* bounds and the same trust chrome a host-pushed pane goes through.
    ///
    /// Ordering is the load-bearing part and is asserted rather than assumed: a queue that replayed
    /// commands out of order would turn an `open` + `update` + `close` triple into a pane the user
    /// cannot get rid of.
    #[test]
    fn commands_emitted_on_the_agents_channel_reach_the_store_in_order() {
        use flux_runtime::SurfaceSink;

        let queue = PaneQueue::new();
        let sink: Arc<dyn SurfaceSink> = queue.clone();
        let mut state = ChatState::for_session("m".into(), "s".into()).with_pane_queue(queue);

        assert_eq!(
            state.apply_pending_panes(),
            0,
            "an idle channel applies none"
        );

        sink.emit(PaneCommand::Open(PaneSpec::new(
            "build",
            "Build",
            PaneSlot::Right,
            PaneLifetime::Session,
            PaneData::Log {
                lines: vec!["first".into()],
            },
        )));
        sink.emit(PaneCommand::Update {
            id: "build".into(),
            data: PaneData::Log {
                lines: vec!["second".into()],
            },
        });

        assert_eq!(state.apply_pending_panes(), 2);
        let listing = state.open_panes();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].id, "build");
        assert!(
            !listing[0].host_owned,
            "an agent-authored pane must never be labelled host-owned"
        );

        // The `update` landed, which is only true if the two commands were applied in order — an
        // `update` seen before its `open` is a silent host-side drop.
        let mut terminal = Terminal::new(TestBackend::new(TRUST_W, TRUST_H)).unwrap();
        terminal.draw(|f| crate::render(f, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("second"), "the update is drawn:\n{screen}");
        assert!(
            screen.contains(AGENT_MARK),
            "an agent-authored pane keeps its trust mark whichever door it came through:\n{screen}"
        );

        sink.emit(PaneCommand::Close { id: "build".into() });
        assert_eq!(state.apply_pending_panes(), 1);
        assert!(state.open_panes().is_empty(), "the close landed too");
    }

    /// The channel is bounded by the surface, like every other pane resource: a runaway caller
    /// cannot grow it without limit while the event loop is between frames.
    #[test]
    fn the_agents_channel_is_bounded_by_the_surface() {
        use flux_runtime::SurfaceSink;

        let queue = PaneQueue::new();
        let sink: Arc<dyn SurfaceSink> = queue.clone();
        for index in 0..(MAX_PENDING_COMMANDS + 50) {
            sink.emit(PaneCommand::Close {
                id: format!("p{index}"),
            });
        }
        assert_eq!(
            queue.drain().commands.len(),
            MAX_PENDING_COMMANDS,
            "the pending channel must be capped by the surface, not by its caller"
        );
    }

    /// **C-324.** Overflowing the channel drops the newest command — and the operator is told, in
    /// the transcript and on the frame.
    ///
    /// The drop is what makes this worth a test at all: the `pane.*` op has already returned ok by
    /// the time the queue refuses, so nothing about the op's *return value* differs before and
    /// after this story. The observable is the surface's own record of the refusal, which is why
    /// the assertions are on the transcript and the drawn frame rather than on a `Result`.
    #[test]
    fn a_dropped_pane_command_is_reported_to_the_operator() {
        use flux_runtime::SurfaceSink;

        let queue = PaneQueue::new();
        let sink: Arc<dyn SurfaceSink> = queue.clone();
        let mut state = ChatState::for_session("m".into(), "s".into()).with_pane_queue(queue);

        for index in 0..(MAX_PENDING_COMMANDS + 3) {
            sink.emit(PaneCommand::Close {
                id: format!("p{index}"),
            });
        }
        assert_eq!(
            state.apply_pending_panes(),
            MAX_PENDING_COMMANDS,
            "drop-newest is preserved: the queue still delivers exactly its cap"
        );

        let notices: Vec<&String> = state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                crate::Entry::Notice { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            notices.len(),
            1,
            "an overflowing pane channel is reported exactly once, not per dropped command: \
             {notices:?}"
        );
        assert!(
            notices[0].contains('3'),
            "the notice names how many commands were dropped: {:?}",
            notices[0]
        );

        // A sustained flood is one condition, not one notice per frame — the operator is told when
        // it starts and is not drowned while it lasts.
        for index in 0..(MAX_PENDING_COMMANDS + 7) {
            sink.emit(PaneCommand::Close {
                id: format!("q{index}"),
            });
        }
        state.apply_pending_panes();
        assert_eq!(
            state
                .entries
                .iter()
                .filter(|e| matches!(e, crate::Entry::Notice { .. }))
                .count(),
            1,
            "a still-overflowing channel does not re-notify every frame"
        );

        let mut terminal = Terminal::new(TestBackend::new(TRUST_W, TRUST_H)).unwrap();
        terminal.draw(|f| crate::render(f, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            screen.contains("dropped"),
            "the drop reaches the frame the operator is looking at:\n{screen}"
        );
    }

    /// A pending **destructive** approval: the top risk tier (C-154), and therefore the chrome a
    /// counterfeit would most want to wear.
    fn destructive_view() -> crate::controller::ApprovalView {
        crate::controller::ApprovalView {
            request: crate::controller::ApprovalRequest {
                tool: "bash".into(),
                subjects: vec!["$ rm -rf ~/work".into()],
                summary: Some("high · destructive".into()),
                destructive: true,
                mutating: true,
            },
            scroll: 0,
            reason: None,
        }
    }

    /// One rendered frame: a one-entry transcript, optionally one `log` pane, optionally a pending
    /// approval sheet.
    fn trust_frame_at(
        width: u16,
        height: u16,
        theme: Theme,
        pane: Option<(PaneSlot, String, Vec<String>)>,
        approval: bool,
    ) -> Buffer {
        let mut state = ChatState::new("mock".into());
        state.theme = theme;
        state.push(Entry::Notice {
            text: "a transcript row".into(),
            sev: Sev::Info,
        });
        if let Some((slot, title, lines)) = pane {
            state.apply_pane_command(PaneCommand::Open(PaneSpec::new(
                "counterfeit",
                title,
                slot,
                PaneLifetime::Session,
                PaneData::Log { lines },
            )));
        }
        if approval {
            state.approval = Some(destructive_view());
        }
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn trust_frame(pane: Option<(PaneSlot, String, Vec<String>)>, approval: bool) -> Buffer {
        trust_frame_at(TRUST_W, TRUST_H, Theme::default(), pane, approval)
    }

    fn row_cells(buf: &Buffer, y: u16) -> Vec<Cell> {
        let w = buf.area.width as usize;
        buf.content[y as usize * w..(y as usize + 1) * w].to_vec()
    }

    /// The rows the approval sheet occupies: it is the only **full-width** bordered block on the
    /// frame, so they are exactly the rows whose last cell is one of its right-hand border glyphs.
    fn sheet_rows(buf: &Buffer) -> Vec<u16> {
        (0..buf.area.height)
            .filter(|y| {
                let last = row_cells(buf, *y).pop().expect("a non-empty row");
                matches!(last.symbol(), "┐" | "│" | "┘")
            })
            .collect()
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        row_cells(buf, y).iter().map(|c| c.symbol()).collect()
    }

    /// The row `y` a cell index falls on.
    fn row_of(buf: &Buffer, index: usize) -> u16 {
        (index / buf.area.width as usize) as u16
    }

    fn screen_text(buf: &Buffer) -> String {
        buf.content.iter().map(|c| c.symbol()).collect()
    }

    /// **The adversarial corpus: glyphs someone builds harness chrome out of, and what each one
    /// imitates.**
    ///
    /// This list is deliberately **not** derived from [`trust::is_reserved`], and it is deliberately
    /// **not** a second copy of its ranges. It was grown by probing the shipped rule for
    /// pass-throughs, because that is the only way a test can see a gap the implementation has: a
    /// mirrored range list shares the implementation's blind spot *exactly*, so a counterfeit built
    /// out of a block the rule forgot scores zero forged cells and the test reports success. That is
    /// the "guard tested against its own assumptions" failure, and an earlier revision of this file
    /// shipped it — the first four groups below all passed through a rule that reserved only box
    /// drawing, block elements and geometric shapes.
    ///
    /// Add to this list whenever a new lookalike turns up. Never rewrite it as a range check.
    const CHROME_LOOKALIKES: &[(&str, &str)] = &[
        // Box-drawing variants — the reason the rule has to be range-shaped and not a list of the
        // six glyphs the sheet happens to use.
        ("┏", "heavy top-left"),
        ("┓", "heavy top-right"),
        ("┗", "heavy bottom-left"),
        ("┛", "heavy bottom-right"),
        ("━", "heavy horizontal"),
        ("┃", "heavy vertical"),
        ("╔", "double top-left"),
        ("╗", "double top-right"),
        ("═", "double horizontal"),
        ("║", "double vertical"),
        ("╭", "rounded top-left"),
        ("╯", "rounded bottom-right"),
        // Misc technical: Unicode's own names for these are box and scan lines, and the bracket
        // pieces tile into a frame edge. One block BELOW the reserved box-drawing range.
        ("\u{23b8}", "left vertical box line"),
        ("\u{23b9}", "right vertical box line"),
        ("\u{23ba}", "horizontal scan line 1"),
        ("\u{23bb}", "horizontal scan line 3"),
        ("\u{23bc}", "horizontal scan line 7"),
        ("\u{23bd}", "horizontal scan line 9"),
        ("\u{239c}", "left parenthesis extension"),
        ("\u{239f}", "right parenthesis extension"),
        // Braille tiles densely enough to stand in for the bars — and the spinner IS Braille.
        ("\u{28ff}", "full 8-dot cell, as a block fill"),
        ("\u{2847}", "left-column dots, as a vertical rule"),
        ("\u{283f}", "bottom dots, as a horizontal rule"),
        // Fullwidth and halfwidth rule forms.
        ("\u{ffe8}", "halfwidth forms light vertical"),
        ("\u{ff5c}", "fullwidth vertical line"),
        ("\u{ffe3}", "fullwidth macron, as an overline rule"),
        ("\u{fe31}", "presentation form vertical em dash"),
        ("\u{fe33}", "presentation form vertical low line"),
        // Legacy computing: sextants and half-blocks exist for terminal graphics.
        ("\u{1fb70}", "vertical one-eighth block"),
        ("\u{1fb00}", "sextant"),
        // Long rules that are punctuation rather than drawing.
        ("\u{2015}", "horizontal bar"),
        ("\u{2e3a}", "two-em dash"),
        // Fills, arrows and attention marks.
        ("\u{2b1b}", "large black square"),
        ("\u{2b06}", "upwards black arrow"),
        ("\u{21d1}", "upwards double arrow"),
        ("\u{26a1}", "high voltage, as a warning mark"),
        ("\u{2757}", "heavy exclamation, as a warning mark"),
        ("\u{1f53a}", "red triangle up, as a risk badge"),
        ("\u{1f7e5}", "red square, as a risk badge"),
        ("\u{1f536}", "orange diamond, as a risk badge"),
        ("\u{2666}", "black diamond suit, imitating the agent mark"),
    ];

    /// What a *user* reads as harness chrome: the blocks whose glyphs draw frames, rules and fills,
    /// plus every glyph in [`CHROME_LOOKALIKES`].
    ///
    /// The ranges here are the narrow, obvious ones; the corpus is what gives this predicate reach
    /// beyond them, and it is the half that is independent of the implementation.
    fn is_chrome_glyph(ch: char) -> bool {
        matches!(ch,
            '\u{2500}'..='\u{257F}'      // box drawing
            | '\u{2580}'..='\u{259F}'    // block elements
            | '\u{25A0}'..='\u{25FF}'    // geometric shapes
            | '⚠' | '↑' | '↓')
            || CHROME_LOOKALIKES
                .iter()
                .any(|(glyph, _)| glyph.chars().any(|c| c == ch))
    }

    /// Where the chrome alphabet appears on a frame, as `(x, y, glyph)` — a position-aware
    /// fingerprint of everything on screen that reads as harness chrome.
    fn chrome_cells(buf: &Buffer) -> std::collections::BTreeSet<(u16, u16, String)> {
        let w = buf.area.width as usize;
        buf.content
            .iter()
            .enumerate()
            .filter(|(_, c)| c.symbol().chars().any(is_chrome_glyph))
            .map(|(i, c)| ((i % w) as u16, (i / w) as u16, c.symbol().to_string()))
            .collect()
    }

    /// C-222 (the story's failing-first test): **an agent pane cannot be mistaken for the approval
    /// sheet, even when its payload *is* the approval sheet.**
    ///
    /// The impersonation payload is not hand-written. It is the sheet the surface itself just drew,
    /// read straight back off the buffer and handed to a pane as content — so it stays verbatim
    /// accurate as the sheet evolves, where a hand-copied string would rot silently and the test
    /// would keep passing. Two more primitives a payload would reach for ride along: an ANSI
    /// sequence, and the surface's own agent mark.
    ///
    /// A second counterfeit rides along, and it is the one that catches a *gap in the rule* rather
    /// than a gap in its enforcement: the same frame rebuilt from [`CHROME_LOOKALIKES`], the corpus
    /// grown by probing rather than copied from [`trust::is_reserved`]'s ranges. Against the rule as
    /// first shipped — box drawing, block elements, geometric shapes — that payload rendered a
    /// complete frame and scored **zero** forged cells.
    ///
    /// Four things are asserted, in every slot, with the real sheet pending:
    ///
    /// 1. **The payload paints no chrome.** Every cell on screen holding a chrome glyph is one the
    ///    *surface* chose — identical, cell for cell, to the same frame whose pane holds plain
    ///    letters of the same shape.
    /// 2. **No lookalike survives.** Not one glyph of the corpus reaches a cell.
    /// 3. **Nothing is interpreted.** No control byte reaches a cell.
    /// 4. **The sheet is untouched.** Its rows are byte-identical, styles included, to the same
    ///    sheet drawn with no pane open at all — a pane draws first, the sheet `Clear`s and draws
    ///    over it, always.
    #[test]
    fn an_agent_pane_cannot_imitate_the_approval_sheet() {
        let real = trust_frame(None, true);
        let sheet = sheet_rows(&real);
        assert!(
            sheet.len() >= 4,
            "the sheet was located: {sheet:?}\n{}",
            screen_text(&real)
        );

        let mut counterfeit: Vec<String> = sheet
            .iter()
            .map(|y| row_text(&real, *y).trim_end().to_string())
            .collect();
        counterfeit.push("\u{1b}[7m approve bash? \u{1b}[0m".into());
        counterfeit.push(format!("{AGENT_MARK} agent"));
        // The corpus, packed a few glyphs to a row so that every one of them clears the surface's
        // row cap and is actually drawn — an elided lookalike would pass this test for free.
        let lookalikes: Vec<String> = CHROME_LOOKALIKES
            .chunks(6)
            .map(|chunk| chunk.iter().map(|(glyph, _)| *glyph).collect())
            .collect();
        assert!(
            lookalikes.len() <= MAX_PANE_ROWS as usize,
            "the corpus must fit the surface's row cap to be provably drawn: {} rows",
            lookalikes.len()
        );
        let title = counterfeit[0].clone();
        // The benign twin: the same shape and the same char counts, drawn from an alphabet with no
        // chrome in it. Any chrome cell present in one frame and not the other came from a payload.
        let benign: Vec<String> = counterfeit
            .iter()
            .map(|line| "x".repeat(line.chars().count()))
            .collect();
        let benign_title = "x".repeat(title.chars().count());

        for slot in ALL_SLOTS {
            let bad = trust_frame(Some((slot, title.clone(), counterfeit.clone())), true);
            let good = trust_frame(Some((slot, benign_title.clone(), benign.clone())), true);

            let forged: Vec<_> = chrome_cells(&bad)
                .symmetric_difference(&chrome_cells(&good))
                .cloned()
                .collect();
            assert!(
                forged.is_empty(),
                "{slot:?}: {} cell(s) of chrome the surface did not draw: {:?}",
                forged.len(),
                &forged[..forged.len().min(12)]
            );
            assert!(
                !bad.content
                    .iter()
                    .any(|c| c.symbol().chars().any(char::is_control)),
                "{slot:?}: a control byte reached a cell",
            );

            // The gap-catching pass: the corpus, rendered as a pane, in this slot.
            let probe = trust_frame(Some((slot, lookalikes.join(" "), lookalikes.clone())), true);
            let probe_text = screen_text(&probe);
            let leaked: Vec<&str> = CHROME_LOOKALIKES
                .iter()
                .filter(|(glyph, _)| probe_text.contains(glyph))
                .map(|(_, what)| *what)
                .collect();
            assert!(
                leaked.is_empty(),
                "{slot:?}: {} lookalike(s) reached a cell — a payload can still draw chrome \
                 this rule does not reserve: {:?}",
                leaked.len(),
                leaked
            );
            for y in &sheet {
                assert_eq!(
                    row_cells(&probe, *y),
                    row_cells(&real, *y),
                    "{slot:?}: the lookalike pane reached sheet row {y}\n{probe_text}"
                );
            }
            for y in &sheet {
                assert_eq!(
                    row_cells(&bad, *y),
                    row_cells(&real, *y),
                    "{slot:?}: sheet row {y} differs from the sheet with no pane open\n{}",
                    screen_text(&bad)
                );
            }
            // The invariant neutralizes the counterfeit chrome; it does not silence the pane. The
            // sheet's words still render inside it, as text.
            assert!(
                screen_text(&bad).matches("approval · destructive").count() >= 2,
                "{slot:?}: the pane still renders its title as text\n{}",
                screen_text(&bad)
            );
        }
    }

    /// C-222 / acceptance 4: **draw order is explicit and tested.** Panes render before the
    /// approval sheet, always — so the sheet's rows come out byte-identical, styles included,
    /// whether or not a pane is open, at every width from the suppression threshold up, at every
    /// height, in every slot including `overlay`. A pane that cannot change one cell of the sheet
    /// cannot occlude it.
    #[test]
    fn a_pane_can_never_occlude_the_approval_sheet_at_any_width_or_slot() {
        // A payload that wants every row and column it can get: tall enough to overflow the body
        // cap, wide enough to overflow any column.
        let greedy: Vec<String> = (0..40)
            .map(|i| format!("row-{i}-{}", "W".repeat(300)))
            .collect();
        let title = "W".repeat(300);

        // Below the suppression threshold the sheet-row comparison is vacuous — no pane is drawn, so
        // of course the sheet is unchanged. Assert the stronger thing that actually holds there: the
        // WHOLE frame is identical, which is C-221's narrow-width posture still holding with a
        // pending approval on screen.
        for slot in ALL_SLOTS {
            let narrow = PANE_MIN_TRANSCRIPT_WIDTH - 1;
            assert_eq!(
                trust_frame_at(
                    narrow,
                    24,
                    Theme::default(),
                    Some((slot, title.clone(), greedy.clone())),
                    true
                ),
                trust_frame_at(narrow, 24, Theme::default(), None, true),
                "{slot:?}: at {narrow} columns a pane must not change the frame at all"
            );
        }

        for (width, height) in [
            (PANE_MIN_TRANSCRIPT_WIDTH, 24), // exactly one side column fits
            (80, PANE_MIN_HEIGHT),           // the shortest frame that carries a pane
            (80, 24),
            (100, 40),
            (132, 30),
            (240, 60),
        ] {
            let bare = trust_frame_at(width, height, Theme::default(), None, true);
            let sheet = sheet_rows(&bare);
            assert!(
                !sheet.is_empty(),
                "{width}x{height}: the sheet was located\n{}",
                screen_text(&bare)
            );
            for slot in ALL_SLOTS {
                let with = trust_frame_at(
                    width,
                    height,
                    Theme::default(),
                    Some((slot, title.clone(), greedy.clone())),
                    true,
                );
                for y in &sheet {
                    assert_eq!(
                        row_cells(&with, *y),
                        row_cells(&bare, *y),
                        "{width}x{height} {slot:?}: the pane reached sheet row {y}\n{}",
                        screen_text(&with)
                    );
                }
            }
        }
    }

    /// C-222 / acceptance 2: the mark reaches the **screen** under `Theme::MONO`, where every
    /// colour role resolves to `Color::Reset`. It therefore has to be a glyph plus a modifier
    /// rather than a tint — the reasoning C-149 used for the transcript gutter rail and C-154 for
    /// the approval risk tiers. It appears exactly once per drawn pane, and it is named, because a
    /// glyph on its own asks the user to have learnt it.
    #[test]
    fn the_agent_mark_survives_mono_in_every_slot() {
        for slot in ALL_SLOTS {
            let buf = trust_frame_at(
                TRUST_W,
                TRUST_H,
                Theme::MONO,
                // The payload tries to place the mark itself, and to tint it.
                Some((
                    slot,
                    format!("{AGENT_MARK} agent"),
                    vec![format!("\u{1b}[7m{AGENT_MARK} agent\u{1b}[0m")],
                )),
                false,
            );
            let marks: Vec<usize> = buf
                .content
                .iter()
                .enumerate()
                .filter(|(_, c)| c.symbol() == AGENT_MARK)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                marks.len(),
                1,
                "{slot:?}: the mark is the surface's, and appears once\n{}",
                screen_text(&buf)
            );
            let cell = &buf.content[marks[0]];
            assert!(
                cell.modifier.contains(Modifier::REVERSED)
                    && cell.modifier.contains(Modifier::BOLD),
                "{slot:?}: the mark carries modifiers, not colour: {:?}",
                cell.modifier
            );
            assert_eq!(cell.fg, Color::Reset, "{slot:?}: MONO leaves no colour");
            assert_eq!(cell.bg, Color::Reset, "{slot:?}: MONO leaves no colour");
            assert!(
                row_text(&buf, row_of(&buf, marks[0])).contains("agent"),
                "{slot:?}: the mark is named\n{}",
                screen_text(&buf)
            );
        }
    }

    /// C-222 / acceptance 1: a payload cannot inject styling **through content** either. C-220
    /// pinned the type — no field of `PaneSpec` reaches a `Style` — and this is the other half:
    /// every colour on a frame carrying an adversarial pane is one of the theme's own roles.
    #[test]
    fn a_pane_paints_only_colours_the_theme_defines() {
        let theme = Theme::LIGHT_RGB;
        let palette = [
            Color::Reset,
            theme.user,
            theme.assistant,
            theme.tool,
            theme.ok,
            theme.err,
            theme.warn,
            theme.muted,
            theme.accent,
            theme.sel_bg,
            theme.composer_bg,
            theme.panel_bg,
            theme.text,
            theme.base_bg,
        ];
        for slot in ALL_SLOTS {
            let buf = trust_frame_at(
                TRUST_W,
                TRUST_H,
                theme,
                Some((
                    slot,
                    "\u{1b}[1;31m approval · destructive \u{1b}[0m".into(),
                    vec![
                        "\u{1b}[48;5;196m\u{1b}[38;2;255;0;0m approve bash? \u{1b}[0m".into(),
                        "\u{1b}]8;;http://evil\u{1b}\\[y] allow\u{1b}]8;;\u{1b}\\".into(),
                    ],
                )),
                true,
            );
            for (index, cell) in buf.content.iter().enumerate() {
                assert!(
                    palette.contains(&cell.fg) && palette.contains(&cell.bg),
                    "{slot:?}: cell {index} is painted off-palette: fg {:?} bg {:?}",
                    cell.fg,
                    cell.bg
                );
            }
        }
    }

    fn log_pane(id: &str, slot: PaneSlot, lifetime: PaneLifetime) -> PaneCommand {
        PaneCommand::Open(PaneSpec::new(
            id,
            format!("{id} title"),
            slot,
            lifetime,
            PaneData::Log {
                lines: vec![format!("{id} body")],
            },
        ))
    }

    /// The store is bounded by [`MAX_PANES`], and the cap refuses a NEW id rather than evicting an
    /// open pane — a runaway caller must not be able to scroll the user's panes off the surface.
    #[test]
    fn pane_count_is_capped_and_the_cap_refuses_the_newcomer() {
        let mut store = PaneStore::default();
        for i in 0..MAX_PANES + 3 {
            store.apply(log_pane(
                &format!("p{i}"),
                PaneSlot::Right,
                PaneLifetime::Session,
            ));
        }
        assert_eq!(store.len(), MAX_PANES);
        assert_eq!(store.ids(), vec!["p0", "p1", "p2", "p3"]);

        // An id already open still updates in place, and keeps its position.
        store.apply(PaneCommand::Update {
            id: "p0".into(),
            data: PaneData::Log {
                lines: vec!["replaced".into()],
            },
        });
        assert_eq!(store.len(), MAX_PANES);
        assert_eq!(store.ids()[0], "p0");
    }

    /// `turn` panes are cleared at turn end; `session` panes survive it. `project` never enters the
    /// store at all — the reporter rejects it (C-220) and the surface has no store for it.
    #[test]
    fn turn_panes_clear_at_turn_end_and_session_panes_do_not() {
        let mut store = PaneStore::default();
        store.apply(log_pane("t", PaneSlot::Right, PaneLifetime::Turn));
        store.apply(log_pane("s", PaneSlot::Right, PaneLifetime::Session));
        store.apply(log_pane("p", PaneSlot::Right, PaneLifetime::Project));
        assert_eq!(store.ids(), vec!["t", "s"], "project is not stored");

        store.end_turn();
        assert_eq!(store.ids(), vec!["s"]);

        store.clear();
        assert!(store.is_empty());
    }

    /// The side columns are bounded by surface constants at every width — a share of the frame, a
    /// per-column maximum, and the transcript's own floor, tightest wins — and the transcript never
    /// drops below [`MIN_TRANSCRIPT_WIDTH`]. At the suppression threshold a pair of side panes
    /// cannot both clear [`MIN_PANE_WIDTH`], so neither is drawn rather than one being dropped
    /// arbitrarily.
    #[test]
    fn side_columns_are_bounded_and_never_starve_the_transcript() {
        /// Total width the side columns took, after asserting every bound at `width`.
        fn sides(slots: &[PaneSlot], width: u16) -> u16 {
            let mut state = ChatState::new("mock".into());
            for (i, slot) in slots.iter().enumerate() {
                state.apply_pane_command(log_pane(&format!("p{i}"), *slot, PaneLifetime::Session));
            }
            let frame = Rect {
                x: 0,
                y: 0,
                width,
                height: 24,
            };
            let row = Rect {
                y: 1,
                height: 20,
                ..frame
            };
            let areas = split_transcript(&state, frame, row);
            let total: u16 = [areas.left, areas.right]
                .into_iter()
                .flatten()
                .map(|r| {
                    assert!(r.width >= MIN_PANE_WIDTH, "{width}: pane below its floor");
                    assert!(r.width <= MAX_PANE_WIDTH, "{width}: pane past its cap");
                    r.width
                })
                .sum();
            assert!(
                total <= width * MAX_SIDE_WIDTH_PCT / 100,
                "{width}: side columns past the total-width fraction"
            );
            assert!(
                areas.transcript.width >= MIN_TRANSCRIPT_WIDTH,
                "{width}: transcript starved"
            );
            assert_eq!(
                areas.transcript.width + total,
                width,
                "{width}: the row is fully accounted for"
            );
            total
        }

        // The bounds hold at every width, for one column and for two.
        for width in [PANE_MIN_TRANSCRIPT_WIDTH, 80, 100, 132, 200, 400] {
            sides(&[PaneSlot::Right], width);
            sides(&[PaneSlot::Left, PaneSlot::Right], width);
        }

        // The threshold is exactly what it claims: at it, ONE column fits (and takes the minimum,
        // leaving the transcript its floor); one column narrower, nothing is drawn at all.
        assert_eq!(
            sides(&[PaneSlot::Right], PANE_MIN_TRANSCRIPT_WIDTH),
            MIN_PANE_WIDTH
        );
        assert_eq!(sides(&[PaneSlot::Right], PANE_MIN_TRANSCRIPT_WIDTH - 1), 0);

        // A PAIR needs room for two floors plus the transcript's, so it stays suppressed well past
        // the single-column threshold rather than halving each column below legibility.
        assert_eq!(sides(&[PaneSlot::Left, PaneSlot::Right], 80), 0);
        assert_eq!(
            sides(&[PaneSlot::Left, PaneSlot::Right], 200),
            MAX_PANE_WIDTH * 2,
            "given room, both columns cap out and the transcript keeps the rest"
        );
    }

    /// Each `kind` renders through machinery the TUI already owns — `markdown` through
    /// `flux-markdown` (the transcript's own renderer) and `tree` through `plan.rs`'s connectors —
    /// so no widget dependency is added under the `ratatui` 0.29 hold.
    #[test]
    fn markdown_and_tree_render_through_machinery_the_tui_already_owns() {
        let theme = Theme::default();
        // Only the agent-pane bodies are under test here; the fleet body reads `state` instead.
        let state = ChatState::new("mock".into());
        let flat = |lines: &[Line<'static>]| -> String {
            lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect()
        };

        let md = AgentPane::sanitized(PaneSpec::new(
            "m",
            "m",
            PaneSlot::Right,
            PaneLifetime::Session,
            PaneData::Markdown {
                text: "# Title\n\nsome **bold** prose\n".into(),
            },
        ));
        let md_lines = body_lines(&state, &Pane::Agent(md), &theme, 30);
        assert!(flat(&md_lines).contains("Title"));
        assert!(
            md_lines.iter().any(|l| l.spans.len() > 1),
            "flux-markdown produced styled spans, not one flat span"
        );

        let tree = AgentPane::sanitized(PaneSpec::new(
            "t",
            "t",
            PaneSlot::Right,
            PaneLifetime::Session,
            PaneData::Tree {
                roots: vec![PaneNode {
                    label: "root".into(),
                    children: vec![
                        PaneNode {
                            label: "first".into(),
                            children: Vec::new(),
                        },
                        PaneNode {
                            label: "second".into(),
                            children: Vec::new(),
                        },
                    ],
                }],
            },
        ));
        let tree_text = flat(&body_lines(&state, &Pane::Agent(tree), &theme, 30));
        assert!(
            tree_text.contains("root") && tree_text.contains("second"),
            "{tree_text}"
        );
        // C-222: the connectors are drawn by the SURFACE from the theme, so the reserved-glyph
        // rule that neutralizes a payload's own box drawing leaves them untouched.
        assert!(
            tree_text.contains("├─") && tree_text.contains("└─"),
            "plan.rs's connectors: {tree_text}"
        );
    }

    /// A tree deeper than the surface's cap stops at the cap instead of rendering unboundedly deep
    /// content the payload chose the size of.
    #[test]
    fn tree_depth_is_capped_by_the_surface() {
        let mut node = PaneNode {
            label: "leaf".into(),
            children: Vec::new(),
        };
        for i in 0..plan::MAX_TREE_DEPTH * 2 {
            node = PaneNode {
                label: format!("n{i}"),
                children: vec![node],
            };
        }
        let roots = vec![node];
        assert_eq!(tree_rows(&roots, 0), plan::MAX_TREE_DEPTH);
        assert_eq!(
            plan::render_nodes(&roots, &Theme::default(), 40).len(),
            plan::MAX_TREE_DEPTH
        );
    }

    // ---- C-224: the host-owned sub-agent fleet pane -------------------------------------------

    /// One `subagent.activity` event as it actually reaches this surface: through the engine's
    /// turn-owned `AgentSinkSpawnActivitySink`, which forwards it into the parent [`AgentSink`] as
    /// an observation. Built here the same way, so the test exercises the real decode path rather
    /// than a hand-made `UiEvent`.
    fn child_event(
        spawn_id: u64,
        role: &str,
        event: flux_runtime::SpawnActivityEvent,
    ) -> flux_runtime::SpawnActivity {
        flux_runtime::SpawnActivity {
            spawn_id,
            role: role.into(),
            // Deliberately the SAME session id for both children: a fresh storeless event store
            // hands every child `s_1`. That is exactly why the surface must not pair on it.
            child_session_id: "s_1".into(),
            parent_session: Some("s_parent".into()),
            depth: 1,
            event,
        }
    }

    /// Feed one child event down the real path — `ChannelSink::observation` → the `UiEvent`
    /// channel — and fold whatever comes back into `state`, exactly as the event loop does.
    fn feed(
        state: &mut ChatState,
        activity: &flux_runtime::SpawnActivity,
        now: std::time::Instant,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel::<crate::controller::UiEvent>();
        let mut sink = crate::controller::ChannelSink { tx, action_id: 1 };
        <crate::controller::ChannelSink as AgentSink>::observation(
            &mut sink,
            &activity.to_observation(),
        );
        let event = rx
            .try_recv()
            .expect("the surface decoded `subagent.activity`");
        let inner = match event {
            crate::controller::UiEvent::Tagged { event, .. } => *event,
            other => other,
        };
        match inner {
            crate::controller::UiEvent::SpawnActivity(activity) => {
                state.record_spawn_activity(&activity, now)
            }
            _ => panic!("`subagent.activity` decoded to some other event"),
        }
    }

    /// The story's named failing-first test.
    ///
    /// Two concurrent children of the **same role**, running the **same op**, sharing the **same
    /// child session id** — the shape A-79's correlation exists to disambiguate. The surface must
    /// pair each child's events to its own row: when one resolves its call, the *other* child must
    /// still be shown running. Pairing on role, on op or on `child_session_id` all produce a
    /// visibly wrong pane here, which is the point.
    #[test]
    fn two_children_of_one_role_pair_to_their_own_rows() {
        use flux_runtime::SpawnActivityEvent;

        let mut state = ChatState::new("mock".into());
        let t0 = std::time::Instant::now();

        // Both children open a call. Same role, same op name, same session id.
        feed(
            &mut state,
            &child_event(
                1,
                "implementor",
                SpawnActivityEvent::ToolCall {
                    call_id: 1,
                    name: "read".into(),
                    input: serde_json::json!({ "path": "/etc/passwd" }),
                },
            ),
            t0,
        );
        feed(
            &mut state,
            &child_event(
                2,
                "implementor",
                SpawnActivityEvent::ToolCall {
                    call_id: 1,
                    name: "read".into(),
                    input: serde_json::json!({ "path": "/etc/passwd" }),
                },
            ),
            t0,
        );
        // Only child 1 resolves. Its `call_id` is 1 — the same number child 2 is still waiting on.
        feed(
            &mut state,
            &child_event(
                1,
                "implementor",
                SpawnActivityEvent::ToolResult {
                    call_id: 1,
                    name: "read".into(),
                    is_error: false,
                },
            ),
            t0,
        );

        // The pane is open, and it is the host's — not something the model asked for.
        assert!(
            state.panes.has_fleet(),
            "live children must raise the host fleet pane"
        );
        let listing = state.open_panes();
        assert_eq!(listing.len(), 1, "one pane: {listing:?}");
        assert!(
            listing[0].host_owned,
            "the fleet pane reports as host-owned so the model does not duplicate it: {listing:?}"
        );

        // Two rows, correlated by spawn id, with the right status on each.
        let rows = &state.fleet_rows;
        assert_eq!(rows.len(), 2, "one row per child: {rows:?}");
        let first = rows.iter().find(|r| r.spawn_id == 1).expect("child 1");
        let second = rows.iter().find(|r| r.spawn_id == 2).expect("child 2");
        assert_eq!(
            first.status,
            crate::fleet::WorkerStatus::Idle,
            "child 1 resolved its read"
        );
        assert_eq!(
            second.status,
            crate::fleet::WorkerStatus::Running { op: "read".into() },
            "child 2's read must NOT be closed by child 1's identically-numbered result"
        );

        // And the frame says so: two `implementor` rows, one running and one idle.
        let mut terminal = Terminal::new(TestBackend::new(TRUST_W, TRUST_H)).unwrap();
        terminal.draw(|f| crate::render(f, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert_eq!(
            screen.matches("implementor").count(),
            2,
            "both children are on the surface:\n{screen}"
        );
        assert!(
            screen.contains("running"),
            "the still-working child is shown running:\n{screen}"
        );
        assert!(
            screen.contains("idle"),
            "the finished-its-call child is shown idle:\n{screen}"
        );
    }

    /// One live worker, as the shortest way to get the pane up.
    fn with_one_worker(state: &mut ChatState, now: std::time::Instant) {
        feed(
            state,
            &child_event(
                7,
                "worker",
                flux_runtime::SpawnActivityEvent::ToolCall {
                    call_id: 1,
                    name: "bash".into(),
                    input: serde_json::json!({}),
                },
            ),
            now,
        );
    }

    /// The pane is the **host's**: every `pane.*` command naming it is refused, so the model can
    /// neither repaint it into something else nor take it down. The `open` case matters most and is
    /// the least obvious — it is checked both while the pane is up (no adoption) and before it
    /// exists (no squatting the reserved id and having the host inherit the payload).
    #[test]
    fn the_model_can_neither_close_repaint_nor_shadow_the_host_fleet_pane() {
        let mut state = ChatState::new("mock".into());
        let t0 = std::time::Instant::now();

        // Before any child: the model claims the reserved id. Refused outright.
        state.apply_pane_command(PaneCommand::Open(PaneSpec::new(
            FLEET_PANE_ID,
            "not the fleet",
            PaneSlot::Right,
            PaneLifetime::Session,
            PaneData::Log {
                lines: vec!["squatted".into()],
            },
        )));
        assert!(state.panes.is_empty(), "the reserved id cannot be claimed");

        with_one_worker(&mut state, t0);
        assert!(state.panes.has_fleet());

        // Now the pane is up: update and close are refused too.
        state.apply_pane_command(PaneCommand::Update {
            id: FLEET_PANE_ID.into(),
            data: PaneData::Log {
                lines: vec!["repainted".into()],
            },
        });
        state.apply_pane_command(PaneCommand::Close {
            id: FLEET_PANE_ID.into(),
        });
        assert!(
            state.panes.has_fleet(),
            "the model cannot close a host-owned pane"
        );
        assert_eq!(state.panes.len(), 1, "and did not add one either");

        let mut terminal = Terminal::new(TestBackend::new(TRUST_W, TRUST_H)).unwrap();
        terminal.draw(|f| crate::render(f, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            !screen.contains("repainted") && !screen.contains("squatted"),
            "no model payload reached the host pane:\n{screen}"
        );
        assert!(
            screen.contains("worker"),
            "the real fleet body did:\n{screen}"
        );
    }

    /// The host pane is not spendable from the model's budget. `MAX_PANES` exists so a runaway
    /// caller cannot push a pane the user is reading off the screen; if it also capped the surface's
    /// own regions, opening the full quota would be a way to suppress the fleet view.
    #[test]
    fn a_model_at_its_pane_quota_cannot_suppress_the_fleet_pane() {
        let mut state = ChatState::new("mock".into());
        for i in 0..MAX_PANES {
            state.apply_pane_command(log_pane(
                &format!("agent{i}"),
                PaneSlot::Left,
                PaneLifetime::Session,
            ));
        }
        assert_eq!(state.panes.len(), MAX_PANES);

        with_one_worker(&mut state, std::time::Instant::now());
        assert!(
            state.panes.has_fleet(),
            "the fleet pane came up over a full model quota: {:?}",
            state.panes.ids()
        );
        // And the model's own budget is unchanged — it still gets exactly MAX_PANES, no more.
        state.apply_pane_command(log_pane(
            "one-too-many",
            PaneSlot::Left,
            PaneLifetime::Session,
        ));
        assert!(
            !state.panes.ids().contains(&"one-too-many"),
            "the host pane must not have widened the model's quota: {:?}",
            state.panes.ids()
        );
    }

    /// **The host pane must not wear the agent mark.** C-222 makes ` ◆ agent ` unforgeable so a user
    /// can trust it as evidence that a region was authored by the model; putting it on harness
    /// chrome would make it evidence of nothing. The check runs both ways so the two cannot converge.
    #[test]
    fn the_host_fleet_pane_carries_no_agent_mark_and_an_agent_pane_still_does() {
        let render_screen = |state: &ChatState| -> String {
            let mut terminal = Terminal::new(TestBackend::new(TRUST_W, TRUST_H)).unwrap();
            terminal.draw(|f| crate::render(f, state)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        let mut fleet_only = ChatState::new("mock".into());
        with_one_worker(&mut fleet_only, std::time::Instant::now());
        let screen = render_screen(&fleet_only);
        assert!(
            screen.contains("sub-agents"),
            "the host pane is drawn:\n{screen}"
        );
        assert!(
            !screen.contains(AGENT_MARK),
            "host chrome must not claim to be agent-authored:\n{screen}"
        );

        let mut with_agent = ChatState::new("mock".into());
        with_agent.apply_pane_command(log_pane("a", PaneSlot::Left, PaneLifetime::Session));
        assert!(
            render_screen(&with_agent).contains(AGENT_MARK),
            "an agent pane still carries the mark"
        );
    }

    /// A-79's contract has an internal half a customer surface must default-deny: the child's tool
    /// input and its observation data. This is the same corpus `crate::fleet` uses, fed through the
    /// **whole surface path** and asserted against the rendered frame — the projection proving it
    /// never reads those fields is one thing, the terminal never showing them is the claim a user
    /// cares about. Child prose and thinking need no case here: A-79 gives them no variant to
    /// travel in, and this story adds no other route.
    #[test]
    fn no_worker_secret_reaches_the_rendered_fleet_pane() {
        // C-325: joined from fragments at compile time, split inside the vendor prefix — same
        // corpus, same bytes at run time, nothing on disk for a forge's scanner to block.
        const CORPUS: &[&str] = &[
            concat!("sk-ant-", "api03-REALLOOKINGKEYMATERIAL"),
            concat!("ghp", "_0123456789abcdefghijklmnopqrstuvwxyz"),
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "postgres://fleet:hunter2@db.internal:5432/prod",
            "hunter2",
        ];
        let mut state = ChatState::new("mock".into());
        let t0 = std::time::Instant::now();
        for (index, secret) in CORPUS.iter().enumerate() {
            // Values, nested values, array members and JSON *keys* — as if the emitter-side
            // redactor seam had failed open.
            let mut input = serde_json::json!({
                "url": format!("https://api.example.com?token={secret}"),
                "headers": { "Authorization": format!("Bearer {secret}") },
                "argv": ["curl", secret],
            });
            input[secret.to_string()] = serde_json::json!("a secret used as a JSON key");
            feed(
                &mut state,
                &child_event(
                    1,
                    "worker",
                    flux_runtime::SpawnActivityEvent::ToolCall {
                        call_id: index as u64 + 1,
                        name: "http_request".into(),
                        input,
                    },
                ),
                t0,
            );
            feed(
                &mut state,
                &child_event(
                    1,
                    "worker",
                    flux_runtime::SpawnActivityEvent::Observation {
                        observation: flux_evidence::Observation::new(
                            "plugin.audit",
                            flux_evidence::Phase::ToolFollowup,
                            serde_json::json!({ "credential": secret }),
                        ),
                    },
                ),
                t0,
            );
        }

        let mut terminal = Terminal::new(TestBackend::new(TRUST_W, TRUST_H)).unwrap();
        terminal.draw(|f| crate::render(f, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        for secret in CORPUS {
            assert!(
                !screen.contains(secret),
                "`{secret}` reached the terminal:\n{screen}"
            );
        }
        // Not a vacuous pass: the allowlisted structural fields did arrive. The op name is asserted
        // on the projection rather than the screen because a 34-column pane truncates it — the
        // point is that it crossed at all, while nothing from `input`/`observation.data` did.
        assert!(
            screen.contains("worker") && screen.contains("running"),
            "the pane rendered its permitted fields:\n{screen}"
        );
        assert_eq!(
            state.fleet_rows[0].status.op(),
            Some("http_request"),
            "the operation NAME is permitted; only its input is not"
        );
    }

    /// The pane is bounded like any other and suppressed with them. Narrow or short frames drop
    /// every slot together (C-221's posture), and a fleet larger than the surface's cap is
    /// truncated by the surface with the remainder counted, never grown to fit.
    #[test]
    fn the_fleet_pane_is_bounded_and_suppressed_with_every_other_slot() {
        let mut state = ChatState::new("mock".into());
        let t0 = std::time::Instant::now();
        for spawn_id in 0..(MAX_FLEET_WORKERS as u64 + 5) {
            feed(
                &mut state,
                &child_event(
                    spawn_id,
                    &format!("role{spawn_id}"),
                    flux_runtime::SpawnActivityEvent::Planning { active: true },
                ),
                t0,
            );
        }

        // The body is capped in workers, and says how many it is not showing.
        let lines = fleet_lines(&state, &Theme::default(), 34);
        assert_eq!(
            lines.len(),
            MAX_FLEET_WORKERS * 2 + 1,
            "two rows per listed worker plus the remainder line"
        );
        assert_eq!(body_rows(&state, &Pane::Fleet) as usize, lines.len());
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(flat.contains("5 more"), "the remainder is counted: {flat}");

        // Wide frame: drawn. Narrow and short frames: nothing, exactly as with no panes at all.
        let screen = |w: u16, h: u16| -> String {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| crate::render(f, &state)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect()
        };
        assert!(screen(TRUST_W, TRUST_H).contains("sub-agents"));
        assert!(
            !screen(PANE_MIN_TRANSCRIPT_WIDTH - 1, TRUST_H).contains("sub-agents"),
            "suppressed below the width floor"
        );
        assert!(
            !screen(TRUST_W, PANE_MIN_HEIGHT - 1).contains("sub-agents"),
            "suppressed below the height floor"
        );
    }

    /// The pane's lifetime is the **fleet's**, not the turn's and not the model's: it survives the
    /// turn that spawned the children (so a wave's outcome stays readable through its retention
    /// window), retires once the projection empties, and goes away with the session.
    #[test]
    fn the_fleet_pane_retires_on_its_own_lifetime_rules() {
        let mut state = ChatState::new("mock".into());
        let t0 = std::time::Instant::now();
        // A turn-lifetime agent pane alongside, to show end_turn discriminates rather than skipping.
        state.apply_pane_command(log_pane("turnly", PaneSlot::Left, PaneLifetime::Turn));
        feed(
            &mut state,
            &child_event(
                1,
                "worker",
                flux_runtime::SpawnActivityEvent::Finished {
                    usage: None,
                    is_error: false,
                },
            ),
            t0,
        );

        state.panes.end_turn();
        assert!(
            state.panes.has_fleet(),
            "the fleet pane is not turn-scoped: {:?}",
            state.panes.ids()
        );
        assert!(
            !state.panes.ids().contains(&"turnly"),
            "a turn-lifetime agent pane still goes"
        );

        // Still inside the finished worker's retention: shown, so the wave's outcome is readable.
        state.refresh_fleet(t0 + Duration::from_secs(5));
        assert!(state.panes.has_fleet());

        // Past it: the projection empties and the pane retires with it.
        state.refresh_fleet(t0 + Duration::from_secs(600));
        assert!(
            !state.panes.has_fleet(),
            "an empty fleet retires its pane: {:?}",
            state.panes.ids()
        );
        assert!(state.fleet_rows.is_empty());
    }

    /// `pane.list` (C-223) reports the fleet pane labelled host-owned, so the model does not open a
    /// second fleet pane it would then be unable to keep in sync.
    #[test]
    fn pane_list_labels_the_fleet_pane_host_owned_and_agent_panes_not() {
        let mut state = ChatState::new("mock".into());
        state.apply_pane_command(log_pane("mine", PaneSlot::Left, PaneLifetime::Session));
        with_one_worker(&mut state, std::time::Instant::now());

        let listing = state.open_panes();
        let agent = listing.iter().find(|p| p.id == "mine").expect("agent pane");
        let host = listing
            .iter()
            .find(|p| p.id == FLEET_PANE_ID)
            .expect("host pane");
        assert!(!agent.host_owned, "the model's own pane is not host-owned");
        assert!(host.host_owned, "the fleet pane is: {listing:?}");
        assert_eq!(host.title, FLEET_PANE_TITLE);
    }

    /// A stalled worker stays legible at the narrowest pane the surface will draw, and under
    /// `Theme::MONO` where the warn tint does not exist.
    ///
    /// Both halves were real defects found by eye: the activity line originally ended with
    /// `stalled`, so a worker with a long op name rendered `running · read sta…` — truncating away
    /// the one word an operator is scanning for, in the one theme that has nothing else to show it.
    #[test]
    fn a_stalled_worker_says_so_at_the_narrowest_width_and_under_mono() {
        let mut state = ChatState::new("mock".into());
        let t0 = std::time::Instant::now();
        feed(
            &mut state,
            &child_event(
                1,
                "implementor",
                flux_runtime::SpawnActivityEvent::ToolCall {
                    call_id: 1,
                    name: "a_very_long_operation_name".into(),
                    input: serde_json::json!({}),
                },
            ),
            t0,
        );
        state.refresh_fleet(t0 + crate::fleet::DEFAULT_STALL_AFTER + Duration::from_secs(1));
        assert!(state.fleet_rows[0].stalled, "the worker is stalled");

        for theme in [Theme::MONO, Theme::default()] {
            // MIN_PANE_WIDTH is the narrowest column that gets drawn at all, minus its two borders.
            let lines = fleet_lines(&state, &theme, (MIN_PANE_WIDTH - 2) as usize);
            let flat: String = lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect();
            assert!(
                flat.contains("stalled"),
                "the stalled word must survive truncation, not the op name: {flat:?}"
            );
        }
    }
}
