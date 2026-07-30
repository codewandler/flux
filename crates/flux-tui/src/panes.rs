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
    open: Vec<AgentPane>,
}

impl PaneStore {
    /// Apply one command. Bounds are enforced here so nothing downstream has to trust the caller:
    /// an `open` past [`MAX_PANES`] for an unknown id is dropped, and an `open` for an id already
    /// present replaces it in place (keeping its position, so the layout does not jump).
    ///
    /// An `update` whose payload is a different [`PaneKind`](flux_runtime::PaneKind) than the open
    /// pane re-derives the kind from the data instead of rejecting it — the two can then never
    /// disagree, which is the same invariant `PaneSpec::new` enforces at the contract end.
    pub(crate) fn apply(&mut self, command: PaneCommand) {
        match command {
            PaneCommand::Open(spec) => {
                // `project` is rejected at the reporter (C-220) and has no store here; a spec that
                // reached the surface anyway is dropped rather than silently treated as `session`.
                if spec.lifetime == PaneLifetime::Project {
                    return;
                }
                match self.open.iter().position(|p| p.spec().id == spec.id) {
                    Some(at) => self.open[at] = AgentPane::sanitized(spec),
                    None if self.open.len() < MAX_PANES => {
                        self.open.push(AgentPane::sanitized(spec))
                    }
                    None => {}
                }
            }
            PaneCommand::Update { id, data } => {
                if let Some(pane) = self.open.iter_mut().find(|p| p.spec().id == id) {
                    pane.update(data);
                }
            }
            PaneCommand::Close { id } => self.open.retain(|p| p.spec().id != id),
        }
    }

    /// Drop the [`PaneLifetime::Turn`] panes. Called at every turn-termination path the surface
    /// owns, so a turn-scoped pane cannot outlive the turn that opened it.
    pub(crate) fn end_turn(&mut self) {
        self.open
            .retain(|p| p.spec().lifetime != PaneLifetime::Turn);
    }

    /// Drop every pane. Used when the surface projects a different session (`/resume`): panes are
    /// session-scoped, and carrying them across would attribute one session's panes to another.
    pub(crate) fn clear(&mut self) {
        self.open.clear();
    }

    /// The panes asking for `slot`, oldest first.
    fn in_slot(&self, slot: PaneSlot) -> Vec<&AgentPane> {
        self.open.iter().filter(|p| p.spec().slot == slot).collect()
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
        self.open.iter().map(|p| p.spec().id.as_str()).collect()
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
        .map(|p| PANE_CHROME_ROWS + body_rows(p).min(MAX_PANE_ROWS))
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
    let Some(pane) = panes.last() else {
        return;
    };
    let t = &state.theme;
    let width = frame.area().width.min(OVERLAY_PANE_WIDTH);
    let mut body = body_lines(pane, t, width);
    let total = body.len();
    let shown = total.min(MAX_PANE_ROWS as usize);
    body.truncate(shown);
    rendering::render_overlay_panel(
        frame,
        t,
        trust::agent_overlay_header(t, &pane.spec().title, width),
        body,
        (total > shown).then_some((shown, total)),
        width,
    );
}

/// Body rows a pane's payload wants, before the surface's cap applies.
fn body_rows(pane: &AgentPane) -> u16 {
    let count = match &pane.spec().data {
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
fn render_column(frame: &mut Frame, state: &ChatState, panes: &[&AgentPane], area: Rect) {
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
fn render_row(frame: &mut Frame, state: &ChatState, panes: &[&AgentPane], area: Rect) {
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

/// One pane: the surface's trust chrome ([`trust::agent_block`] — themed border, agent mark, the
/// pane's own title as text), and a body truncated to the rect with an explicit elision marker
/// when the cap bites.
fn render_pane(frame: &mut Frame, state: &ChatState, pane: &AgentPane, area: Rect) {
    if area.width < MIN_PANE_WIDTH || area.height < PANE_CHROME_ROWS + 1 {
        return;
    }
    let t = &state.theme;
    let block = trust::agent_block(t, &pane.spec().title, area.width);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let budget = inner.height.min(MAX_PANE_ROWS);
    let mut lines = body_lines(pane, t, inner.width);
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
fn body_lines(pane: &AgentPane, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let cols = width as usize;
    match &pane.spec().data {
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

    /// The glyph vocabulary this surface draws its **own** chrome from: the borders of every
    /// bordered block (the approval sheet, a pane, the transcript's C-149 rail), the bars and the
    /// `▍` reason cursor, the agent mark and the `▸`/`●` selection marks, and the sheet's own
    /// `⚠`/`↑↓` affordances. Nothing a payload writes may land in this alphabet — that is what
    /// makes the counterfeit unbuildable rather than merely discouraged.
    fn is_chrome_glyph(ch: char) -> bool {
        matches!(ch,
            '\u{2500}'..='\u{257F}'      // box drawing
            | '\u{2580}'..='\u{259F}'    // block elements
            | '\u{25A0}'..='\u{25FF}'    // geometric shapes
            | '⚠' | '↑' | '↓')
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
    /// Three things are asserted, in every slot, with the real sheet pending:
    ///
    /// 1. **The payload paints no chrome.** Every cell on screen holding a chrome glyph is one the
    ///    *surface* chose — identical, cell for cell, to the same frame whose pane holds plain
    ///    letters of the same shape.
    /// 2. **Nothing is interpreted.** No control byte reaches a cell.
    /// 3. **The sheet is untouched.** Its rows are byte-identical, styles included, to the same
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

        for (width, height) in [
            (PANE_MIN_TRANSCRIPT_WIDTH - 1, 24), // panes suppressed entirely
            (PANE_MIN_TRANSCRIPT_WIDTH, 24),     // exactly one side column fits
            (80, PANE_MIN_HEIGHT),               // the shortest frame that carries a pane
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
        let md_lines = body_lines(&md, &theme, 30);
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
        let tree_text = flat(&body_lines(&tree, &theme, 30));
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
}
