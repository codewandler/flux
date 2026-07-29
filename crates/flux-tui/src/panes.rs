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
//! Every colour and modifier comes from the [`Theme`]; [`PaneSpec`] carries no style-bearing field
//! and must not grow one. Trust chrome (the agent-region mark, the border style, the rule that a
//! pane never occludes the approval sheet) is C-222's story — this module only guarantees the
//! ordering it needs: panes draw before the sheet, always.

use super::*;

use flux_runtime::{PaneCommand, PaneData, PaneLifetime, PaneSlot, PaneSpec};

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

/// Placeholder agent-region mark. C-222 owns making this load-bearing and proving it; it is a
/// glyph plus a modifier rather than a tint so it survives `Theme::MONO`, where every colour role
/// resolves to `Color::Reset`.
const AGENT_MARK: &str = "◆";

/// Host-pushed panes, addressed by id and rendered in the order they were opened.
///
/// A `Vec` rather than a map on purpose: ids address a pane, but *render order must be
/// deterministic*, and a hash map's iteration order is not. Insertion order also gives the
/// [`MAX_PANES`] cap an obvious meaning.
#[derive(Debug, Default)]
pub(crate) struct PaneStore {
    open: Vec<PaneSpec>,
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
                match self.open.iter().position(|p| p.id == spec.id) {
                    Some(at) => self.open[at] = spec,
                    None if self.open.len() < MAX_PANES => self.open.push(spec),
                    None => {}
                }
            }
            PaneCommand::Update { id, data } => {
                if let Some(pane) = self.open.iter_mut().find(|p| p.id == id) {
                    pane.kind = data.kind();
                    pane.data = data;
                }
            }
            PaneCommand::Close { id } => self.open.retain(|p| p.id != id),
        }
    }

    /// Drop the [`PaneLifetime::Turn`] panes. Called at every turn-termination path the surface
    /// owns, so a turn-scoped pane cannot outlive the turn that opened it.
    pub(crate) fn end_turn(&mut self) {
        self.open.retain(|p| p.lifetime != PaneLifetime::Turn);
    }

    /// Drop every pane. Used when the surface projects a different session (`/resume`): panes are
    /// session-scoped, and carrying them across would attribute one session's panes to another.
    pub(crate) fn clear(&mut self) {
        self.open.clear();
    }

    /// The panes asking for `slot`, oldest first.
    fn in_slot(&self, slot: PaneSlot) -> Vec<&PaneSpec> {
        self.open.iter().filter(|p| p.slot == slot).collect()
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
        self.open.iter().map(|p| p.id.as_str()).collect()
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
        format!(
            " {AGENT_MARK} {} ",
            truncate(&pane.title, width.saturating_sub(6) as usize)
        ),
        body,
        (total > shown).then_some((shown, total)),
        width,
    );
}

/// Body rows a pane's payload wants, before the surface's cap applies.
fn body_rows(pane: &PaneSpec) -> u16 {
    let count = match &pane.data {
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
fn render_column(frame: &mut Frame, state: &ChatState, panes: &[&PaneSpec], area: Rect) {
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
fn render_row(frame: &mut Frame, state: &ChatState, panes: &[&PaneSpec], area: Rect) {
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

/// One pane: themed border, the placeholder agent mark plus the pane's own title, and a body
/// truncated to the rect with an explicit elision marker when the cap bites.
fn render_pane(frame: &mut Frame, state: &ChatState, pane: &PaneSpec, area: Rect) {
    if area.width < MIN_PANE_WIDTH || area.height < PANE_CHROME_ROWS + 1 {
        return;
    }
    let t = &state.theme;
    let title = format!(
        " {AGENT_MARK} {} ",
        truncate(&pane.title, area.width.saturating_sub(6) as usize)
    );
    let block = Block::bordered()
        .border_style(t.muted_style())
        .title(Span::styled(
            title,
            t.muted_style().add_modifier(Modifier::BOLD),
        ));
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
fn body_lines(pane: &PaneSpec, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let cols = width as usize;
    match &pane.data {
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
        PaneData::Markdown { text } => crate::markdown::render(text, width).lines,
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
    use flux_runtime::PaneNode;

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

        let md = PaneSpec::new(
            "m",
            "m",
            PaneSlot::Right,
            PaneLifetime::Session,
            PaneData::Markdown {
                text: "# Title\n\nsome **bold** prose\n".into(),
            },
        );
        let md_lines = body_lines(&md, &theme, 30);
        assert!(flat(&md_lines).contains("Title"));
        assert!(
            md_lines.iter().any(|l| l.spans.len() > 1),
            "flux-markdown produced styled spans, not one flat span"
        );

        let tree = PaneSpec::new(
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
        );
        let tree_text = flat(&body_lines(&tree, &theme, 30));
        assert!(
            tree_text.contains("root") && tree_text.contains("second"),
            "{tree_text}"
        );
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
